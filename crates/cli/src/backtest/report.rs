//! What a backtest prints.
//!
//! Separated from the chokepoint so that the file holding the trial append
//! holds nothing else. Rules 1, 3, and 5 are all about what appears next to a
//! number, so the formatting is where they are visible or not.

use engine::Report;
use rigor::{DeflatedSharpe, TrialEntry, TrialLog};
use rust_decimal::Decimal;

/// Decimal places for anything printed. The recorded Sharpe is rounded to ten
/// before it reaches here and is printed unrounded, so the log and the report
/// cannot disagree.
const DISPLAY_DP: u32 = 6;

fn show(value: Decimal) -> String {
    value.round_dp(DISPLAY_DP).normalize().to_string()
}

fn show_option(value: Option<Decimal>, absent: &str) -> String {
    match value {
        Some(value) => show(value),
        None => absent.to_string(),
    }
}

pub fn print_entry(entry: &TrialEntry, sharpe: Option<Decimal>) {
    println!("Trial recorded.");
    println!("  program:     {}", entry.program);
    println!("  timestamp:   {}", entry.timestamp);
    println!("  config_hash: {}", entry.config_hash);
    println!("  entry_hash:  {}", entry.entry_hash);
    println!("  prev_hash:   {}", entry.prev_hash);
    match sharpe {
        Some(value) => println!("  sharpe:      {value}   (annualised, net, diagnostic)"),
        None => println!("  sharpe:      null, because no figure was produced"),
    }
    println!();
}

pub fn print_counts(log: &TrialLog, program: &str) {
    println!("Counts after this trial");
    println!("  lifetime N: {}", log.lifetime_count());
    println!(
        "  scoped N:   {}   (research program {program})",
        log.count_for(program)
    );
    println!();
}

pub fn print_outcome(
    log: &TrialLog,
    program: &str,
    outcome: &super::Outcome,
    recorded: Option<Decimal>,
) {
    match outcome {
        super::Outcome::Declared => {
            println!("No backtest ran. A configuration was put forward with no engine behind");
            println!("it, so nothing was evaluated and no Sharpe exists to report. The trial");
            println!("is recorded regardless, because that is what a trial counts.");
        }
        super::Outcome::Failed(error) => {
            println!("The engine produced no defensible number.");
            println!("  {error}");
            println!();
            println!("The trial is recorded with a null Sharpe. An aborted run that consumed a");
            println!("look at the data is still a trial, and not counting it would understate");
            println!("N for every figure computed after it.");
        }
        super::Outcome::Ran(report) => print_report(log, program, report, recorded),
    }
}

fn print_report(log: &TrialLog, program: &str, report: &Report, recorded: Option<Decimal>) {
    // The research program rather than a hard-coded strategy name. There is
    // more than one strategy now, and a heading that named the wrong one would
    // mislabel every figure under it.
    println!(
        "{program}, {} to {}, {} monthly observations",
        report.first_rebalance, report.last_rebalance, report.months
    );
    println!("  universe file sha256:  {}", report.config.universe_sha256);
    println!("{}", census_block(report));
    println!();

    println!("Result, net of costs. Rule 1: a Sharpe is a diagnostic, never performance.");
    println!(
        "  net Sharpe, annualised:  {}   diagnostic",
        show_option(recorded, "none")
    );
    println!(
        "  net Sharpe, monthly:     {}   diagnostic, and the figure the DSR takes",
        show(report.sharpe_monthly)
    );
    println!(
        "  total net return:        {}",
        show(report.total_net_return)
    );
    println!("  max drawdown:            {}", show(report.max_drawdown));
    println!(
        "  formations:              {}   every {} month(s)",
        report.formations.len(),
        report.config.rebalance_every_months
    );
    println!(
        "  mean one-way turnover:   {}   per formation, single-counted",
        show(report.mean_one_way_turnover)
    );
    println!("{}", exit_block(report));
    println!();

    println!("{}", dsr_block(log, program, report));

    println!("Baselines, rule 5.");
    println!(
        "  equal-weight buy-and-hold:  total {}   annualised Sharpe {}",
        show(report.buy_and_hold.total_net_return),
        show_option(report.buy_and_hold.annualised_sharpe, "none")
    );
    println!(
        "  random ranking, {} seeds:   mean {}   min {}   max {}   spread {}",
        report.config.random_draws,
        show_option(report.random.mean_sharpe, "none"),
        show_option(report.random.min_sharpe, "none"),
        show_option(report.random.max_sharpe, "none"),
        show_option(report.random.spread, "none"),
    );
    println!(
        "  linear model:               {}",
        engine::baseline::linear_baseline_note(report.config.strategy)
    );
    println!();

    println!("Diagnostics. Rule 3: there is no gross-returns mode and this is not performance.");
    println!(
        "  total gross return:      {}   diagnostic",
        show(report.total_gross_return)
    );
    println!();

    println!("Caveats, recorded with the result.");
    println!("  {}", caveat_block(report));
    println!();
}

/// Where the cross-section went, stage by stage, across every formation.
///
/// Reported as the range each stage spanned rather than as one line per
/// formation. The question a reader has is whether the funnel behaved, and a
/// hundred rows answers it no better than its two ends while burying the rest
/// of the report. `Report::formations` carries every row for a caller that
/// wants them.
///
/// Gated on the size screen, which only the conservative formula runs. On the
/// other programs three of the five stages do not exist, and printing them as
/// zeros would claim a screen ran and removed nothing.
pub(super) fn census_block(report: &Report) -> String {
    if report.config.size_floor_fraction.is_none() {
        return format!(
            "  eligible names:        {} to {} per rebalance",
            report.eligible_min, report.eligible_max
        );
    }
    let span = |stage: fn(&engine::FormationCensus) -> usize| {
        let counts: Vec<usize> = report.formations.iter().map(stage).collect();
        format!(
            "{} to {}",
            counts.iter().copied().min().unwrap_or(0),
            counts.iter().copied().max().unwrap_or(0)
        )
    };
    [
        "  formation census, per formation, low to high".to_string(),
        format!(
            "    eligible:                  {}",
            span(|row| row.eligible)
        ),
        format!(
            "    removed by the size screen: {}",
            span(|row| row.size_screened)
        ),
        format!(
            "    kept by the calm half:      {}",
            span(|row| row.vol_half)
        ),
        format!(
            "    dropped, no payout yield:   {}",
            span(|row| row.npy_ineligible)
        ),
        format!("    held:                       {}", span(|row| row.held)),
    ]
    .join("\n")
}

/// What the report says about held names that stopped trading.
///
/// Two shapes, selected on whether a delistings file was attached, because the
/// three-state split is vocabulary the dataset supplies. On a run with no such
/// file every exit lands in `unexplained` by construction, and printing that
/// word claims a dataset looked and found nothing when no dataset was consulted
/// at all. The bare run therefore keeps the single line it printed before this
/// round existed.
///
/// Only the printing is gated. The census is still accumulated on every run,
/// because the pre-round engine counted and printed delisting exits
/// unconditionally and X-D4 is what holds that arithmetic byte-identical.
///
/// Returned as text rather than printed, so a test can assert on both shapes
/// without capturing stdout, which is the same reason [`dsr_block`] and
/// [`caveat_block`] exist.
pub(super) fn exit_block(report: &Report) -> String {
    let strategy = &report.strategy;
    let total = format!("  delisting exits:         {}", strategy.delisting_exits);
    if !report.delistings_imputed {
        return total;
    }
    // The three states printed apart, because the total says how many holdings
    // stopped trading and only the split says how much of the figure above
    // rests on a published convention rather than on data. The unexplained
    // count is the one the caveat block points a reader at.
    [
        total,
        format!(
            "    imputed at the convention:   {}",
            strategy.exits.imputed
        ),
        format!(
            "    observed from data:          {}",
            strategy.exits.observed
        ),
        format!(
            "    unexplained, at last close:  {}",
            strategy.exits.unexplained
        ),
    ]
    .join("\n")
}

/// The caveat block this report is published under.
///
/// A named function rather than the call inlined above, so the selection can be
/// asserted without capturing stdout. The blocks make different claims about
/// which way the remaining bias runs, and a printer that ignored either flag
/// would attach the wrong claim to a real figure.
pub(super) fn caveat_block(report: &Report) -> &'static str {
    engine::caveats(report.dividends_applied, report.delistings_imputed)
}

/// What `N` is, and is not, printed beside every DSR figure.
///
/// This is not conditional on the strategy or on whether a figure exists. A
/// deflated Sharpe corrects for the trials *this log* has counted, and a factor
/// lifted out of the published literature arrives having already survived a
/// search nobody here ran. Against a small local `N` the correction is
/// therefore too gentle, and the direction of that error is flattering, which
/// is the direction worth stating out loud.
pub(super) const N_CAVEAT: &str = "\
N counts this log's trials, not the field's. A factor taken from the
  literature arrives with the field's thousands of tests behind it, and a
  small N here flatters it.";

/// The Deflated Sharpe Ratio against both trial counts, rule 2.
///
/// Returned as text rather than printed, so a test can assert on what the block
/// says without capturing stdout. That is the same reason [`caveat_block`]
/// exists, and it is the only way the caveat's presence is testable at all.
///
/// `observed` is the **monthly** Sharpe. Feeding the annualised figure produces
/// a confident wrong probability with nothing visibly failing, which is why the
/// annualised value is not in scope in this function.
pub(super) fn dsr_block(log: &TrialLog, program: &str, report: &Report) -> String {
    let mut lines =
        vec!["Deflated Sharpe Ratio. Rule 2 requires both readings, always.".to_string()];
    for (label, sharpes, trials) in [
        (
            "scoped  ",
            log.recorded_sharpes_for(program),
            log.count_for(program),
        ),
        ("lifetime", log.recorded_sharpes(), log.lifetime_count()),
    ] {
        let probability = rigor::sigma_sr(&sharpes).and_then(|sigma_sr| {
            DeflatedSharpe {
                observed: report.sharpe_monthly,
                periods: report.months,
                skewness: report.skewness,
                kurtosis: report.kurtosis,
                sigma_sr,
                trials,
            }
            .probability()
        });
        lines.push(match probability {
            Some(value) => format!("  {label} (N={trials}):  {}", show(value)),
            // A missing figure is printed as missing. Substituting a number
            // here would be inventing the one thing the whole crate exists to
            // stop being invented.
            None => format!(
                "  {label} (N={trials}):  no figure exists. The DSR needs at least two \
                 recorded Sharpes to estimate their spread, at least two return periods, \
                 and a positive variance term. One of those is absent."
            ),
        });
    }
    // Unconditional, and inside the block rather than beside the call, so that
    // no caller can print a DSR figure without it.
    lines.push(String::new());
    lines.push(format!("  {N_CAVEAT}"));
    lines.push(String::new());
    lines.join("\n")
}
