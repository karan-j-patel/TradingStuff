//! `export`. Writes the characteristic panel, and records no trial.
//!
//! # Why this is top-level rather than under `diagnose`
//!
//! `diagnose` measures the machinery. This produces an artifact: a file that
//! outlives the command and that something else reads later. The two share the
//! claim that no trial is recorded and nothing else, and filing an artifact
//! under a measurement's subcommand would make the tree say the wrong thing
//! about what this is.
//!
//! # Why every dataset is required rather than optional
//!
//! The panel writes a column per dataset. An export run without filings would
//! write a book-to-market column that is null from the first row to the last,
//! and nothing downstream could tell that apart from a universe in which no
//! company had ever filed. The engine refuses that state
//! (`EngineError::ExportMissingInputs`), and the arguments being required is
//! what turns the refusal into a message about a missing flag rather than a
//! failed run.
//!
//! The trial log is read here, for the count printed beside the export, and
//! never written. `x_r6` is the test that holds it.

#[cfg(test)]
mod tests;

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use engine::BacktestConfig;
use ingest::PanelProvenance;
use rigor::TrialLog;

use crate::backtest::{Attachments, build_panel, read_universe, with_digests};

/// Eight paths, each a distinct file the export reads or writes. Bundling them
/// into a struct would move the argument list rather than shorten it, which is
/// the rule `backtest::Strategy::from_args` already follows.
#[allow(clippy::too_many_arguments)]
pub fn run(
    trials: &str,
    prices: &Path,
    universe: &Path,
    actions: &Path,
    delistings: &Path,
    marketcap: &Path,
    filings: &Path,
    out: &Path,
) -> anyhow::Result<ExitCode> {
    println!(
        "{}",
        panel_report(
            trials, prices, universe, actions, delistings, marketcap, filings, out
        )?
    );
    Ok(ExitCode::SUCCESS)
}

/// The whole export as text, with the file written as a side effect.
///
/// Returned rather than printed, on the rule `diagnose::turnover_report`
/// follows, so a test can assert on what it says without capturing stdout.
#[allow(clippy::too_many_arguments)]
pub(crate) fn panel_report(
    trials: &str,
    prices: &Path,
    universe: &Path,
    actions: &Path,
    delistings: &Path,
    marketcap: &Path,
    filings: &Path,
    out: &Path,
) -> anyhow::Result<String> {
    // Read, never written. The count says how much searching stands behind the
    // data this artifact is cut from, which is context a reader of the file
    // wants, and is the only reason the log is opened at all.
    let log =
        TrialLog::load(trials).with_context(|| format!("loading the trial log from {trials}"))?;

    // Read before anything is resolved, because the digests the configuration
    // records, and which then travel into the file's own metadata, come out of
    // these same buffers.
    let attachments = Attachments::read(
        prices,
        Some(actions),
        Some(delistings),
        Some(marketcap),
        Some(filings),
        // The export produces the panel a fit reads; it ranks on nothing.
        None,
    )?;
    let (universe_sha256, members) = read_universe(universe)?;
    let config = with_digests(
        BacktestConfig::panel_export(universe_sha256),
        Some(delistings),
        &attachments,
    );
    let config_hash = config.config_hash()?;

    let prices_sha256 = attachments.prices.sha256.clone();
    let panel = build_panel(attachments, &members)?;
    let rows = engine::export::characteristics(&panel, &config)?;

    let provenance = PanelProvenance {
        config_hash: config_hash.as_str().to_owned(),
        universe_sha256: config.universe_sha256.clone(),
        prices_sha256,
        actions_sha256: digest("actions", &config.actions_sha256)?,
        delistings_sha256: digest("delistings", &config.delistings_sha256)?,
        marketcap_sha256: digest("market cap", &config.marketcap_sha256)?,
        filings_sha256: digest("filings", &config.filings_sha256)?,
    };

    // Summarised before the write, which consumes the rows. The counts are
    // taken as values rather than as a set of borrows for that reason:
    // `AssetKey` is hashable rather than ordered, and a `HashSet` of references
    // into `rows` would still be alive when `write_panel` takes ownership.
    let month_ends: std::collections::BTreeSet<_> = rows.iter().map(|row| row.month_end).collect();
    let securities = rows
        .iter()
        .map(|row| &row.asset)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let first = month_ends.iter().next().copied();
    let last = month_ends.iter().next_back().copied();
    let months = month_ends.len();

    let written = ingest::parquet::write_panel(rows, out, &provenance)
        .with_context(|| format!("writing the characteristic panel to {}", out.display()))?;

    Ok([
        "Characteristic panel export".to_string(),
        format!("  universe file sha256:  {}", provenance.universe_sha256),
        format!("  exporting config hash: {}", provenance.config_hash),
        format!(
            "  trials standing behind this data: lifetime {}",
            log.lifetime_count()
        ),
        String::new(),
        format!("  {}", engine::export::NOT_A_TRIAL),
        String::new(),
        format!("Wrote {written} rows to {}.", out.display()),
        format!(
            "  {securities} securities across {months} month-ends{}",
            match (first, last) {
                (Some(first), Some(last)) => format!(", {first} to {last}"),
                _ => String::new(),
            }
        ),
        "  a characteristic that could not be computed is NULL and was not imputed;".to_string(),
        "  imputation is a modelling choice and belongs in the fit script".to_string(),
    ]
    .join("\n"))
}

/// One recorded digest, refused rather than blanked when it is absent.
///
/// Unreachable while every attachment is required, since `with_digests` fills
/// each field from an attachment that was read. It exists because the
/// alternative shape, defaulting to an empty string, writes a file whose
/// provenance says nothing while looking complete, and the reader would refuse
/// it later at a point much further from the cause.
fn digest(dataset: &str, recorded: &Option<String>) -> anyhow::Result<String> {
    recorded.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "the export configuration records no {dataset} digest, so the panel's provenance \
             could not name the file the column came from"
        )
    })
}
