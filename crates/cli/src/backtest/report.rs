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
    println!(
        "Momentum v0, {} to {}, {} monthly observations",
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

    print_dsr(log, program, report);

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
    println!("  {}", engine::CAVEATS);
    println!();
}

/// The Deflated Sharpe Ratio against both trial counts, rule 2.
///
/// `observed` is the **monthly** Sharpe. Feeding the annualised figure produces
/// a confident wrong probability with nothing visibly failing, which is why the
/// annualised value is not in scope in this function.
fn print_dsr(log: &TrialLog, program: &str, report: &Report) {
    println!("Deflated Sharpe Ratio. Rule 2 requires both readings, always.");
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
        match probability {
            Some(value) => println!("  {label} (N={trials}):  {}", show(value)),
            // A missing figure is printed as missing. Substituting a number
            // here would be inventing the one thing the whole crate exists to
            // stop being invented.
            None => println!(
                "  {label} (N={trials}):  no figure exists. The DSR needs at least two \
                 recorded Sharpes to estimate their spread, at least two return periods, \
                 and a positive variance term. One of those is absent."
            ),
        }
    }
    println!();
}
