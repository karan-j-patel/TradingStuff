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
    let strategy = &report.strategy;
    // The research program rather than a hard-coded strategy name. There is
    // more than one strategy now, and a heading that named the wrong one would
    // mislabel every figure under it.
    println!(
        "{program}, {} to {}, {} monthly observations",
        report.first_rebalance, report.last_rebalance, report.months
    );
    println!("  universe file sha256:  {}", report.config.universe_sha256);
    println!(
        "  eligible names:        {} to {} per rebalance",
        report.eligible_min, report.eligible_max
    );
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
        "  mean one-way turnover:   {}   per rebalance",
        show(report.mean_one_way_turnover)
    );
    println!("  delisting exits:         {}", strategy.delisting_exits);
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
        engine::baseline::LINEAR_BASELINE_NOTE
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

/// The caveat block this report is published under.
///
/// A named function rather than the call inlined above, so the selection can be
/// asserted without capturing stdout. The two blocks make different claims
/// about which way the remaining bias runs, and a printer that ignored the flag
/// would attach the wrong claim to a real figure.
pub(super) fn caveat_block(report: &Report) -> &'static str {
    engine::caveats(report.dividends_applied)
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
