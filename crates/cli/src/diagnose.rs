//! `diagnose`. Tools that measure the machinery without producing a result.
//!
//! # Why this is not under `backtest`
//!
//! That module's documented job is the trial chokepoint: no path through it
//! reaches the end without appending. This module's job is the opposite claim,
//! that no path through it appends at all. They share the dataset plumbing and
//! nothing else, so the plumbing is reused and the two chokepoints stay apart.
//!
//! The trial log is read here, for the counts printed beside the diagnostic, and
//! never written. `x_t1` is the test that holds it.

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Subcommand;
use engine::turnover;
use rigor::TrialLog;

use crate::backtest::{Attachments, build_panel, resolve};

#[derive(Debug, Subcommand)]
pub enum What {
    /// Replay formations at several book sizes and estimate the turnover a
    /// book-size-free strategy would pay
    ///
    /// Not a backtest. It computes no return series and no Sharpe, and records
    /// no trial.
    Turnover {
        #[arg(long, default_value = rigor::DEFAULT_PATH)]
        trials: String,
        /// Research program whose configuration is replayed
        #[arg(long)]
        program: String,
        /// Robustness variant of that program, when it has one
        #[arg(long)]
        variant: Option<String>,
        #[arg(long)]
        prices: PathBuf,
        #[arg(long)]
        universe: PathBuf,
        #[arg(long)]
        actions: Option<PathBuf>,
        #[arg(long)]
        delistings: Option<PathBuf>,
        #[arg(long)]
        marketcap: Option<PathBuf>,
    },
}

pub fn run(what: &What) -> anyhow::Result<ExitCode> {
    let What::Turnover {
        trials,
        program,
        variant,
        prices,
        universe,
        actions,
        delistings,
        marketcap,
    } = what;

    let text = turnover_report(
        trials,
        program,
        variant.as_deref(),
        prices,
        universe,
        actions.as_deref(),
        delistings.as_deref(),
        marketcap.as_deref(),
    )?;
    println!("{text}");
    Ok(ExitCode::SUCCESS)
}

/// The whole diagnostic as text.
///
/// Returned rather than printed so a test can assert on what it says without
/// capturing stdout, which is the rule `report::dsr_block` and its siblings
/// already follow.
#[allow(clippy::too_many_arguments)]
pub(crate) fn turnover_report(
    trials: &str,
    program: &str,
    variant: Option<&str>,
    prices: &Path,
    universe: &Path,
    actions: Option<&Path>,
    delistings: Option<&Path>,
    marketcap: Option<&Path>,
) -> anyhow::Result<String> {
    // Read, never written. The counts say how much searching stands behind the
    // configuration being diagnosed, which is context a reader of this output
    // wants and is the only reason the log is opened at all.
    let log =
        TrialLog::load(trials).with_context(|| format!("loading the trial log from {trials}"))?;

    // No filings. The turnover diagnostic replays formations of whatever
    // program it is pointed at, and a value program pointed here would refuse
    // for want of them, which is the correct answer rather than a gap.
    let attachments = Attachments::read(actions, delistings, marketcap, None)?;
    let (config, members) = resolve(program, variant, universe, delistings, &attachments)?;
    let panel = build_panel(prices, attachments, &members)?;

    // The engine carves each sub-universe out of the full panel itself and
    // refuses a security it cannot place. The filtered panel never crosses the
    // crate boundary: `Panel::retaining` keeps its parent's attachment digests,
    // which is right for this diagnostic and a mislabelled trial anywhere else.
    let mut replays = Vec::with_capacity(turnover::SUB_UNIVERSES.len());
    for (modulus, residue) in turnover::SUB_UNIVERSES {
        replays.push(
            turnover::replay(&panel, &config, *modulus, *residue).with_context(|| {
                format!("replaying the sub-universe permaticker % {modulus} == {residue}")
            })?,
        );
    }
    let fit = turnover::fit(&replays)?;

    let mut lines = vec![
        format!(
            "Turnover diagnostic, {program}{}",
            variant
                .map(|v| format!(" --variant {v}"))
                .unwrap_or_default()
        ),
        format!("  universe file sha256:  {}", config.universe_sha256),
        format!(
            "  trials standing behind this configuration: lifetime {}, {program} {}",
            log.lifetime_count(),
            log.count_for(program)
        ),
        String::new(),
        format!("  {}", turnover::NOT_A_TRIAL),
        String::new(),
        "Formation replays, one per sub-universe.".to_string(),
    ];
    for replay in &replays {
        lines.push(format!(
            "  {:<24}  securities {:>5}   formations {:>4}   K {:>8}   turnover {}",
            replay.label(),
            replay.securities,
            replay.formations,
            show(replay.mean_book),
            show(replay.mean_turnover)
        ));
    }
    lines.push(String::new());
    // One call, one string. The estimate and the residuals are assembled
    // together and there is no way to print one of them.
    lines.push(turnover::block(&replays, &fit));

    Ok(lines.join("\n"))
}

fn show(value: rust_decimal::Decimal) -> String {
    value.round_dp(6).normalize().to_string()
}
