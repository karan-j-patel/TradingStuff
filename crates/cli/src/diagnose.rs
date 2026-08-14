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
use engine::{deletions, turnover};
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

    /// Probe whether names dropping out of a synthetic large-cap set rebound
    ///
    /// Not a backtest. It computes returns against a matched control and no
    /// Sharpe, and records no trial: the hypothesis is declared separately
    /// through the backtest command's --config door.
    Deletions {
        #[arg(long, default_value = rigor::DEFAULT_PATH)]
        trials: String,
        /// Research program whose universe and cost constants frame the probe.
        /// Nothing is backtested; this only resolves the dataset wiring
        #[arg(long)]
        program: String,
        #[arg(long)]
        prices: PathBuf,
        #[arg(long)]
        universe: PathBuf,
        #[arg(long)]
        actions: PathBuf,
        #[arg(long)]
        delistings: PathBuf,
        #[arg(long)]
        marketcap: PathBuf,
    },
}

pub fn run(what: &What) -> anyhow::Result<ExitCode> {
    let text = match what {
        What::Turnover {
            trials,
            program,
            variant,
            prices,
            universe,
            actions,
            delistings,
            marketcap,
        } => turnover_report(
            trials,
            program,
            variant.as_deref(),
            prices,
            universe,
            actions.as_deref(),
            delistings.as_deref(),
            marketcap.as_deref(),
        )?,
        What::Deletions {
            trials,
            program,
            prices,
            universe,
            actions,
            delistings,
            marketcap,
        } => deletions_report(
            trials, program, prices, universe, actions, delistings, marketcap,
        )?,
    };
    println!("{text}");
    Ok(ExitCode::SUCCESS)
}

/// The deletion probe as text.
///
/// Returned rather than printed, on the rule `turnover_report` follows, so a
/// test can assert on what it says without capturing stdout.
///
/// All four datasets are required rather than optional. The ranking needs market
/// caps, the returns need dividends to be total returns, and the delisting
/// treatment is what makes the sensitivity split mean anything. A probe run
/// without one of them would answer a different question under the same name.
#[allow(clippy::too_many_arguments)]
pub(crate) fn deletions_report(
    trials: &str,
    program: &str,
    prices: &Path,
    universe: &Path,
    actions: &Path,
    delistings: &Path,
    marketcap: &Path,
) -> anyhow::Result<String> {
    // Read, never written. `x_d5` is the test that holds it.
    let log =
        TrialLog::load(trials).with_context(|| format!("loading the trial log from {trials}"))?;

    let attachments = Attachments::read(Some(actions), Some(delistings), Some(marketcap), None)?;
    let (config, members) = resolve(program, None, universe, Some(delistings), &attachments)?;
    let panel = build_panel(prices, attachments, &members)?;
    let probe = deletions::probe(&panel)?;

    let mut lines = vec![
        format!("Index-deletion probe, universe of {program}"),
        format!("  universe file sha256:  {}", config.universe_sha256),
        format!(
            "  trials standing behind this log: lifetime {}, {program} {}",
            log.lifetime_count(),
            log.count_for(program)
        ),
        String::new(),
        format!("  {}", deletions::NOT_A_TRIAL),
        String::new(),
        format!("  {}", deletions::POWER_CEILING),
        String::new(),
        format!(
            "Synthetic set: top {} by market cap, recomputed monthly. A deletion is a \
             name ranked",
            deletions::TOP_N
        ),
        format!(
            "  {} or better at m-1 and past {} at m, still trading at m. Controls come from \
             ranks",
            deletions::TOP_N,
            deletions::BUFFER_RANK
        ),
        format!(
            "  {}..={} at m-1, matched on the trailing {}-month return.",
            deletions::CONTROL_RANKS.start(),
            deletions::CONTROL_RANKS.end(),
            deletions::TRAILING_MONTHS
        ),
        String::new(),
        format!("  deletion events:                 {}", probe.events.len()),
        format!(
            "  exits by delisting, not deletions: {}",
            probe.delisting_exits
        ),
        String::new(),
        "Excess return over the matched control, diagnostic. Both sides of the".to_string(),
        "sensitivity split are shown; neither is the headline.".to_string(),
    ];

    for months in deletions::WINDOWS {
        for (label, include) in [("excluding", false), ("including", true)] {
            let summary = probe.summarise(*months, include);
            lines.push(format!(
                "  (m, m+{months}], {label} in-window delistings: n {}   mean {}   median {}   \
                 spread {}",
                summary.events,
                show_option(summary.mean),
                show_option(summary.median),
                show_option(summary.spread),
            ));
        }
    }

    lines.push(String::new());
    lines.push("Every event, so the aggregates above can be checked against them.".to_string());
    if probe.events.is_empty() {
        lines.push("  none".to_string());
    }
    for event in &probe.events {
        let windows: Vec<String> = event
            .windows
            .iter()
            .map(|window| {
                format!(
                    "+{}m event {} control {}{}",
                    window.months,
                    show_option(window.event),
                    show_option(window.control),
                    if window.delisted_in_window {
                        " (delisted in window)"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        lines.push(format!(
            "  {} {:<8} rank {} -> {}   control {:<8} {}",
            event.date,
            event.ticker,
            event.rank_before,
            event.rank_after,
            event.control.as_deref().unwrap_or("none"),
            windows.join("   ")
        ));
    }

    Ok(lines.join("\n"))
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

/// A figure that may not exist, printed as absent rather than as a zero.
///
/// Substituting a number here would be inventing the one thing this project
/// exists to stop being invented, and on a probe with small counts the absent
/// case is common rather than exotic.
fn show_option(value: Option<rust_decimal::Decimal>) -> String {
    value.map_or_else(|| "none".to_string(), show)
}
