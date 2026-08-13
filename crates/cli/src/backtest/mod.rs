//! `backtest`. Runs the engine, then records the trial whatever happened.
//!
//! # Why the counter cannot be skipped
//!
//! `CLAUDE.md` rule 2 puts the increment in the harness so it cannot be
//! forgotten, and there is deliberately no flag to skip it. A trial that was
//! attempted is a trial, whether or not it produced a number, because what
//! inflates a best-of Sharpe is the number of configurations looked at rather
//! than the number that finished.
//!
//! # Why the engine runs before the append, which is not a bypass
//!
//! The stub this replaced appended first, because it had nothing to run. The
//! real engine cannot work that way: the Sharpe is a *field of the entry* and
//! the log is append-only, so a hash chained over a placeholder could never be
//! amended with the answer.
//!
//! The guarantee is therefore stated differently, and it is stronger than
//! ordering. **No path through [`run`] reaches the end without appending.** The
//! engine's result is captured as a value rather than propagated with `?`, so a
//! failure becomes an [`Outcome::Failed`] that is recorded with a null Sharpe
//! and reported, not an early return. `an_engine_failure_still_records_a_trial`
//! is the test that holds it, and it fails the moment an error is allowed to
//! escape ahead of the append.
//!
//! Input validation still refuses before the append, and that is correct. A
//! program name the log will not accept was never a trial, and refusing it
//! leaves the file untouched.

mod report;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use bytes::Bytes;
use engine::{BacktestConfig, Panel, Report};
use ingest::schema::AssetKey;
use rigor::{ConfigHash, TrialLog};

/// What the command was asked to evaluate.
#[derive(Debug)]
pub enum Strategy<'a> {
    /// A configuration put forward with no engine behind it. Records the
    /// attempt with a null Sharpe, which is accurate: something was tried and
    /// no figure came back.
    Declared(&'a str),
    /// The engine over a curated price file and a universe file, and
    /// optionally a curated actions file whose cash dividends reach the
    /// returns.
    ///
    /// Which strategy runs comes from `--program`, not from here, because the
    /// research program is what scopes `N` and the two have to agree. The
    /// variant keeps its momentum-era name so that the momentum regression
    /// tests are untouched by this round; renaming it is deferred, not
    /// forgotten.
    Momentum {
        prices: &'a Path,
        universe: &'a Path,
        actions: Option<&'a Path>,
        /// Curated delistings, which classify the exits the engine detects.
        delistings: Option<&'a Path>,
        /// Curated market caps, which the size screen ranks on and which carry
        /// the share-count leg of net payout yield.
        marketcap: Option<&'a Path>,
        /// Curated filings, which carry the book equity the value signal reads.
        filings: Option<&'a Path>,
    },
}

impl<'a> Strategy<'a> {
    /// Resolve the command-line arguments into one strategy.
    ///
    /// Clap already refuses the impossible combinations. This is the check that
    /// does not depend on the argument attributes staying correct.
    /// Eight paths, and clap has already refused the impossible combinations.
    /// Bundling them into a struct would move the argument list rather than
    /// shorten it, since every one is a distinct optional file.
    #[allow(clippy::too_many_arguments)]
    pub fn from_args(
        config: Option<&'a str>,
        variant: Option<&'a str>,
        prices: Option<&'a PathBuf>,
        universe: Option<&'a PathBuf>,
        actions: Option<&'a PathBuf>,
        delistings: Option<&'a PathBuf>,
        marketcap: Option<&'a PathBuf>,
        filings: Option<&'a PathBuf>,
    ) -> anyhow::Result<Self> {
        match (config, prices, universe) {
            // `--actions`, `--delistings` and `--variant` are refused here
            // rather than ignored. A run that was asked for dividends and
            // quietly produced price returns is the exact mislabelling the
            // engine's wiring guard exists to prevent, and it would arrive with
            // a hash saying no actions were used. A delistings file asked for
            // and never applied is the same failure one dataset over. A variant
            // asked for and never applied is the same failure again: the
            // recorded hash would be the config string's, and nothing in the
            // log would say a variant had been requested.
            //
            // Clap does not catch any of them, which was measured rather than
            // assumed. All three arguments carry `requires = "prices"`, and
            // `--config` conflicts with `--prices`, which suppresses the
            // requirement instead of failing on it.
            (Some(config), None, None)
                if actions.is_none()
                    && delistings.is_none()
                    && marketcap.is_none()
                    && filings.is_none()
                    && variant.is_none() =>
            {
                Ok(Strategy::Declared(config))
            }
            (Some(_), None, None) if filings.is_some() => anyhow::bail!(
                "--filings carries the book equity a value run reads, and --config declares a \
                 configuration with no engine behind it, so the two cannot go together"
            ),
            (Some(_), None, None) if marketcap.is_some() => anyhow::bail!(
                "--marketcap ranks the universe by size in an engine run, and --config \
                 declares a configuration with no engine behind it, so the two cannot go \
                 together"
            ),
            (Some(_), None, None) if variant.is_some() => anyhow::bail!(
                "--variant selects a robustness variant of a program the engine runs, and \
                 --config declares a configuration with no engine behind it, so the two \
                 cannot go together"
            ),
            (Some(_), None, None) if delistings.is_some() => anyhow::bail!(
                "--delistings imputes delisting returns in an engine run, and --config \
                 declares a configuration with no engine behind it, so the two cannot go \
                 together"
            ),
            (Some(_), None, None) => anyhow::bail!(
                "--actions applies cash dividends to an engine run, and --config declares a \
                 configuration with no engine behind it, so the two cannot go together"
            ),
            (None, Some(prices), Some(universe)) => Ok(Strategy::Momentum {
                prices: prices.as_path(),
                universe: universe.as_path(),
                actions: actions.map(PathBuf::as_path),
                delistings: delistings.map(PathBuf::as_path),
                marketcap: marketcap.map(PathBuf::as_path),
                filings: filings.map(PathBuf::as_path),
            }),
            _ => anyhow::bail!(
                "give either --config, for a configuration with no engine behind it, or \
                 --prices together with --universe to run the momentum engine"
            ),
        }
    }
}

/// One optional dataset file, read exactly once.
///
/// # Why the bytes are carried rather than the path
///
/// The recorded digest and the parsed records have to describe the same bytes.
/// Hashing one `read` of a path and parsing a second `read` of it does not
/// guarantee that: between the two the file can be replaced, by a concurrent
/// refetch or by a hand, and the run then produces results from data B under a
/// hash recording data A. That was the shape of the finding this type exists to
/// close, and it applied to all three attachments equally.
///
/// So the file is read once here and both consumers are served from the buffer.
pub(crate) struct Attachment {
    pub(crate) sha256: String,
    pub(crate) bytes: Bytes,
}

impl Attachment {
    /// Read `path` once, keeping the bytes and their digest together.
    pub(crate) fn read(path: &Path, dataset: &str) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading the {dataset} file {}", path.display()))?;
        // Hashed over the bytes on disk, so the recorded value is what `shasum`
        // prints for the same file, and a refetch that changed nothing records
        // the same trial.
        let sha256 = rigor::hash_bytes(&bytes);
        Ok(Attachment {
            sha256,
            bytes: Bytes::from(bytes),
        })
    }
}

/// The three optional attachments, each read once.
///
/// Gathered into one value rather than threaded as six arguments, so the
/// read-once rule is stated in one place and a fourth dataset joins without
/// growing every signature between here and the panel.
#[derive(Default)]
pub(crate) struct Attachments {
    pub(crate) actions: Option<Attachment>,
    pub(crate) delistings: Option<Attachment>,
    pub(crate) marketcap: Option<Attachment>,
    pub(crate) filings: Option<Attachment>,
}

impl Attachments {
    pub(crate) fn read(
        actions: Option<&Path>,
        delistings: Option<&Path>,
        marketcap: Option<&Path>,
        filings: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Ok(Attachments {
            actions: actions
                .map(|path| Attachment::read(path, "actions"))
                .transpose()?,
            delistings: delistings
                .map(|path| Attachment::read(path, "delistings"))
                .transpose()?,
            marketcap: marketcap
                .map(|path| Attachment::read(path, "market cap"))
                .transpose()?,
            filings: filings
                .map(|path| Attachment::read(path, "filings"))
                .transpose()?,
        })
    }

    fn sha256(attachment: &Option<Attachment>) -> Option<String> {
        attachment
            .as_ref()
            .map(|attachment| attachment.sha256.clone())
    }
}

/// What the engine did, which is recorded either way.
#[derive(Debug)]
pub enum Outcome {
    /// No engine was asked for.
    Declared,
    /// A figure was produced. Boxed because `Report` is large and an enum is as
    /// big as its largest variant.
    Ran(Box<Report>),
    /// The engine was asked for and could not produce a defensible number. The
    /// string is the full cause chain, printed rather than swallowed.
    Failed(String),
}

/// `variant` is a robustness rerun of `program` rather than a program of its
/// own, so it never reaches the recorded program string. It changes the
/// configuration, and therefore the hash, which is where the log carries the
/// difference. Keeping it out of the program string is what makes the scoped
/// `N` of rule 2 group a hypothesis with its own reruns.
pub fn run(
    trials_path: &str,
    program: &str,
    variant: Option<&str>,
    strategy: &Strategy<'_>,
) -> anyhow::Result<ExitCode> {
    let mut log = TrialLog::load(trials_path)
        .with_context(|| format!("loading the trial log from {trials_path}"))?;

    // Nothing between here and the append may use `?` on the engine. See the
    // module documentation.
    let (config_hash, outcome) = evaluate(program, variant, strategy);

    let sharpe = match &outcome {
        // Rounded once, here, and this exact value is both recorded and
        // printed. A Sharpe carried to `Decimal`'s full precision is noise
        // past the tenth place, and printing a different rounding from the one
        // recorded would make the log disagree with the report it came from.
        Outcome::Ran(report) => Some(report.sharpe_annualised.round_dp(10)),
        Outcome::Declared | Outcome::Failed(_) => None,
    };

    // `append` verifies the existing chain before writing, so a broken log
    // stops this here rather than growing a longer broken log.
    // `ConfigHash` is the only thing `append` accepts, so the configuration
    // cannot reach the field named for its hash unhashed. See `rigor::ConfigHash`.
    let entry = log
        .append(program, &config_hash, sharpe)
        .context("recording the trial")?;

    report::print_entry(entry, sharpe);
    // Counts are read after the append, so they include the trial just made.
    report::print_counts(&log, program);
    report::print_outcome(&log, program, &outcome, sharpe);

    Ok(match outcome {
        // A failed engine is a recorded trial and a failed command. Exiting
        // successfully would let a scripted run treat "no number" as a number.
        Outcome::Failed(_) => ExitCode::FAILURE,
        Outcome::Declared | Outcome::Ran(_) => ExitCode::SUCCESS,
    })
}

/// Run whatever was asked for, converting every failure into a value.
fn evaluate(
    program: &str,
    variant: Option<&str>,
    strategy: &Strategy<'_>,
) -> (ConfigHash, Outcome) {
    match strategy {
        Strategy::Declared(config) => (ConfigHash::of(config.as_bytes()), Outcome::Declared),
        Strategy::Momentum {
            prices,
            universe,
            actions,
            delistings,
            marketcap,
            filings,
        } => engine_run(
            program,
            variant,
            prices,
            universe,
            *actions,
            *delistings,
            *marketcap,
            *filings,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn engine_run(
    program: &str,
    variant: Option<&str>,
    prices: &Path,
    universe: &Path,
    actions: Option<&Path>,
    delistings: Option<&Path>,
    marketcap: Option<&Path>,
    filings: Option<&Path>,
) -> (ConfigHash, Outcome) {
    // Used only when the configuration itself cannot be resolved, which happens
    // when the universe file cannot be read or the program names no
    // configuration. The attempt still consumed a look at the data and is still
    // recorded, so it still needs a hash.
    let unresolved = || {
        ConfigHash::of(
            format!(
                "{program} variant={} unresolved prices={} universe={}",
                variant.unwrap_or("none"),
                prices.display(),
                universe.display()
            )
            .as_bytes(),
        )
    };

    // Read before anything is resolved, because the digests the configuration
    // records come out of these same buffers.
    let attachments = match Attachments::read(actions, delistings, marketcap, filings) {
        Ok(attachments) => attachments,
        Err(error) => return (unresolved(), Outcome::Failed(format!("{error:#}"))),
    };

    let (config, members) = match resolve(program, variant, universe, delistings, &attachments) {
        Ok(resolved) => resolved,
        Err(error) => return (unresolved(), Outcome::Failed(format!("{error:#}"))),
    };
    let config_hash = match config.config_hash() {
        Ok(hash) => hash,
        Err(error) => return (unresolved(), Outcome::Failed(format!("{error:#}"))),
    };

    match execute(prices, attachments, &members, &config) {
        Ok(report) => (config_hash, Outcome::Ran(Box::new(report))),
        Err(error) => (config_hash, Outcome::Failed(format!("{error:#}"))),
    }
}

/// Read the universe file once, for the hash the trial records and for the
/// membership the panel is filtered to.
///
/// The actions file is hashed here too, over its bytes on disk. Hashing the
/// file rather than the records it decodes to means a refetch that changes a
/// byte without changing what the engine sees records as a new trial. That is
/// over-counting, and over-counting is the safe direction: it makes the
/// deflated Sharpe's denominator larger and every reported probability more
/// conservative.
/// The research program picks the configuration. An unrecognised one is
/// refused rather than defaulted, because a run recorded under a program name
/// that does not describe what ran is a mislabelled trial, and the config hash
/// exists to make exactly that impossible.
///
/// The delistings file contributes a convention name rather than a hash of its
/// bytes. What changes the arithmetic is the rule, and two runs under the same
/// rule over a refetched file that classifies one more name are the same
/// hypothesis measured against slightly more data. The unexplained-exit count
/// printed beside the result is what makes that difference visible.
pub(crate) fn resolve(
    program: &str,
    variant: Option<&str>,
    universe: &Path,
    delistings: Option<&Path>,
    attachments: &Attachments,
) -> anyhow::Result<(BacktestConfig, HashSet<AssetKey>)> {
    let text = std::fs::read_to_string(universe)
        .with_context(|| format!("reading the universe file {}", universe.display()))?;
    // Hashed over the bytes on disk, so the recorded value is the one `shasum`
    // prints for the same file.
    let sha256 = rigor::hash_bytes(text.as_bytes());
    let entries = ingest::universe::from_jsonl(&text)
        .map_err(|error| anyhow::anyhow!("universe file line {}: {}", error.line, error.source))?;
    let members = entries.into_iter().map(|entry| entry.asset).collect();

    // All three datasets, one rule, and the digests come from the buffers the
    // records will be parsed out of rather than from a second read of the path.
    let actions_sha256 = Attachments::sha256(&attachments.actions);
    let delistings_sha256 = Attachments::sha256(&attachments.delistings);
    let marketcap_sha256 = Attachments::sha256(&attachments.marketcap);
    let filings_sha256 = Attachments::sha256(&attachments.filings);

    let config = BacktestConfig::for_program(program, variant, sha256).ok_or_else(|| {
        // Listed from the registry rather than written out, so a pair added
        // there cannot go missing from the message that says what is runnable.
        let runnable: Vec<String> = engine::RUNNABLE
            .iter()
            .map(|(program, variant)| match variant {
                None => format!("--program {program}"),
                Some(variant) => format!("--program {program} --variant {variant}"),
            })
            .collect();
        anyhow::anyhow!(
            "--program {program}{} names no configuration this engine can run. The \
             runnable combinations are: {}",
            variant
                .map(|v| format!(" --variant {v}"))
                .unwrap_or_default(),
            runnable.join("; ")
        )
    })?;

    Ok((
        BacktestConfig {
            actions_sha256,
            delisting_convention: delistings.map(|_| engine::DELISTING_CONVENTION.to_string()),
            delistings_sha256,
            marketcap_sha256,
            filings_sha256,
            ..config
        },
        members,
    ))
}

pub(crate) fn build_panel(
    prices: &Path,
    attachments: Attachments,
    members: &HashSet<AssetKey>,
) -> anyhow::Result<Panel> {
    let bars = ingest::parquet::read_prices(prices)
        .with_context(|| format!("reading curated prices from {}", prices.display()))?;

    // The universe file, not the price file, defines the run. Filtering here is
    // what makes the recorded SHA-256 mean something: a price file that has
    // grown extra securities since the universe was drawn cannot silently widen
    // the cross-section a recorded configuration was measured over.
    let total = bars.len();
    let kept: Vec<_> = bars
        .into_iter()
        .filter(|bar| members.contains(&bar.asset))
        .collect();
    if kept.len() != total {
        println!(
            "Dropped {} of {total} bars belonging to securities outside the universe file.",
            total - kept.len()
        );
    }

    let panel = Panel::from_bars(kept)?;

    // Attached before the run, and only when an actions file was named. The
    // engine refuses a panel whose dividend state disagrees with the
    // configuration, so a mistake here fails loudly rather than producing a
    // number labelled as something it is not.
    let panel = match attachments.actions {
        None => panel,
        Some(attachment) => {
            // Parsed from the bytes already read and already hashed, so the
            // records and the recorded digest describe the same file.
            let records = ingest::parquet::read_actions_from_bytes(attachment.bytes)
                .context("reading curated actions")?;
            let read = records.len();
            let panel = panel.with_dividends(&records, &attachment.sha256)?;
            println!(
                "Read {read} corporate actions, sha256 {}.",
                attachment.sha256
            );
            println!(
                "  cash dividends applied to holdings, on their ex-dates, held uninvested \
                 until the next rebalance"
            );
            // Both censuses printed whatever their value, so a run that applied
            // almost nothing says so rather than looking identical to one that
            // applied everything.
            println!(
                "  {} named a security outside the universe file",
                panel.unmatched_dividends()
            );
            println!(
                "  {} were not cash dividends and changed no return",
                panel.non_cash_actions()
            );
            panel
        }
    };

    // Attached on the same rule and in the same place. The engine refuses a
    // panel whose delisting state disagrees with the configuration, so a
    // mistake here fails loudly rather than publishing an imputation nothing
    // recorded.
    let panel = match attachments.delistings {
        None => panel,
        Some(attachment) => {
            let records = ingest::parquet::read_delistings_from_bytes(attachment.bytes)
                .context("reading curated delistings")?;
            let read = records.len();
            let panel = panel.with_delistings(&records, &attachment.sha256)?;
            println!("Read {read} delistings, sha256 {}.", attachment.sha256);
            println!(
                "  performance-related exits are imputed at the {} convention; \
                 mergers exit at their last close",
                engine::DELISTING_CONVENTION
            );
            println!(
                "  {} named a security outside the universe file",
                panel.unmatched_delistings()
            );
            panel
        }
    };

    // The third attachment, on the same rule as the first two. The engine
    // refuses a panel whose market cap state disagrees with the configuration,
    // and the conservative formula refuses to run without one at all.
    let panel = match attachments.marketcap {
        None => panel,
        Some(attachment) => {
            let records = ingest::parquet::read_marketcap_from_bytes(attachment.bytes)
                .context("reading curated market caps")?;
            let read = records.len();
            let panel = panel.with_marketcaps(&records, &attachment.sha256)?;
            println!("Read {read} market caps, sha256 {}.", attachment.sha256);
            println!(
                "  ranked by size at each formation, and divided by the adjusted close for \
                 the share-count leg of net payout yield"
            );
            println!(
                "  {} named a security outside the universe file",
                panel.unmatched_marketcaps()
            );
            panel
        }
    };

    // The fourth attachment, on the same rule as the first three. A restated
    // row is refused inside `with_filings` rather than filtered here.
    let panel = match attachments.filings {
        None => panel,
        Some(attachment) => {
            let records = ingest::parquet::read_filings_from_bytes(attachment.bytes)
                .context("reading curated filings")?;
            let read = records.len();
            let panel = panel.with_filings(&records, &attachment.sha256)?;
            println!("Read {read} filings, sha256 {}.", attachment.sha256);
            println!(
                "  book equity is read as of each formation, from the latest filing \
                 published on or before it"
            );
            println!(
                "  {} named a security outside the universe file",
                panel.unmatched_filings()
            );
            panel
        }
    };

    Ok(panel)
}

fn execute(
    prices: &Path,
    attachments: Attachments,
    members: &HashSet<AssetKey>,
    config: &BacktestConfig,
) -> anyhow::Result<Report> {
    Ok(engine::backtest(
        &build_panel(prices, attachments, members)?,
        config,
    )?)
}
