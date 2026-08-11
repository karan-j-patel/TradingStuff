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
    },
}

impl<'a> Strategy<'a> {
    /// Resolve the command-line arguments into one strategy.
    ///
    /// Clap already refuses the impossible combinations. This is the check that
    /// does not depend on the argument attributes staying correct.
    pub fn from_args(
        config: Option<&'a str>,
        prices: Option<&'a PathBuf>,
        universe: Option<&'a PathBuf>,
        actions: Option<&'a PathBuf>,
    ) -> anyhow::Result<Self> {
        match (config, prices, universe) {
            // `--actions` is refused here rather than ignored. A run that was
            // asked for dividends and quietly produced price returns is the
            // exact mislabelling the engine's wiring guard exists to prevent,
            // and it would arrive with a hash saying no actions were used.
            (Some(config), None, None) if actions.is_none() => Ok(Strategy::Declared(config)),
            (Some(_), None, None) => anyhow::bail!(
                "--actions applies cash dividends to an engine run, and --config declares a \
                 configuration with no engine behind it, so the two cannot go together"
            ),
            (None, Some(prices), Some(universe)) => Ok(Strategy::Momentum {
                prices: prices.as_path(),
                universe: universe.as_path(),
                actions: actions.map(PathBuf::as_path),
            }),
            _ => anyhow::bail!(
                "give either --config, for a configuration with no engine behind it, or \
                 --prices together with --universe to run the momentum engine"
            ),
        }
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

pub fn run(trials_path: &str, program: &str, strategy: &Strategy<'_>) -> anyhow::Result<ExitCode> {
    let mut log = TrialLog::load(trials_path)
        .with_context(|| format!("loading the trial log from {trials_path}"))?;

    // Nothing between here and the append may use `?` on the engine. See the
    // module documentation.
    let (config_hash, outcome) = evaluate(program, strategy);

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
fn evaluate(program: &str, strategy: &Strategy<'_>) -> (ConfigHash, Outcome) {
    match strategy {
        Strategy::Declared(config) => (ConfigHash::of(config.as_bytes()), Outcome::Declared),
        Strategy::Momentum {
            prices,
            universe,
            actions,
        } => engine_run(program, prices, universe, *actions),
    }
}

fn engine_run(
    program: &str,
    prices: &Path,
    universe: &Path,
    actions: Option<&Path>,
) -> (ConfigHash, Outcome) {
    // Used only when the configuration itself cannot be resolved, which happens
    // when the universe file cannot be read or the program names no
    // configuration. The attempt still consumed a look at the data and is still
    // recorded, so it still needs a hash.
    let unresolved = || {
        ConfigHash::of(
            format!(
                "{program} unresolved prices={} universe={}",
                prices.display(),
                universe.display()
            )
            .as_bytes(),
        )
    };

    let (config, members) = match resolve(program, universe, actions) {
        Ok(resolved) => resolved,
        Err(error) => return (unresolved(), Outcome::Failed(format!("{error:#}"))),
    };
    let config_hash = match config.config_hash() {
        Ok(hash) => hash,
        Err(error) => return (unresolved(), Outcome::Failed(format!("{error:#}"))),
    };

    match execute(prices, actions, &members, &config) {
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
fn resolve(
    program: &str,
    universe: &Path,
    actions: Option<&Path>,
) -> anyhow::Result<(BacktestConfig, HashSet<AssetKey>)> {
    let text = std::fs::read_to_string(universe)
        .with_context(|| format!("reading the universe file {}", universe.display()))?;
    // Hashed over the bytes on disk, so the recorded value is the one `shasum`
    // prints for the same file.
    let sha256 = rigor::hash_bytes(text.as_bytes());
    let entries = ingest::universe::from_jsonl(&text)
        .map_err(|error| anyhow::anyhow!("universe file line {}: {}", error.line, error.source))?;
    let members = entries.into_iter().map(|entry| entry.asset).collect();

    let actions_sha256 = match actions {
        None => None,
        Some(path) => {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading the actions file {}", path.display()))?;
            Some(rigor::hash_bytes(&bytes))
        }
    };

    let config = BacktestConfig::for_program(program, sha256).ok_or_else(|| {
        anyhow::anyhow!(
            "--program {program} names no configuration this engine can run. The engine \
             programs are {} and {}",
            engine::PROGRAM,
            engine::LOWVOL_PROGRAM
        )
    })?;

    Ok((
        BacktestConfig {
            actions_sha256,
            ..config
        },
        members,
    ))
}

fn execute(
    prices: &Path,
    actions: Option<&Path>,
    members: &HashSet<AssetKey>,
    config: &BacktestConfig,
) -> anyhow::Result<Report> {
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
    let panel = match actions {
        None => panel,
        Some(path) => {
            let records = ingest::parquet::read_actions(path)
                .with_context(|| format!("reading curated actions from {}", path.display()))?;
            let read = records.len();
            let panel = panel.with_dividends(&records)?;
            println!("Read {read} corporate actions from {}.", path.display());
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

    Ok(engine::backtest(&panel, config)?)
}
