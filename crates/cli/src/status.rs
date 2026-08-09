//! `status`. What the trial log says, and what the gates permit.
//!
//! Everything printed here is either computed from the log or explicitly marked
//! unavailable with the reason. There is no line that looks like a number and
//! is not one.

use std::process::ExitCode;

use anyhow::Context as _;
use rigor::{TrialLog, threshold};

use crate::gates::{Checkability, GATES, GateItem};

pub fn run(trials_path: &str, program: Option<&str>) -> anyhow::Result<ExitCode> {
    let log = TrialLog::load(trials_path)
        .with_context(|| format!("loading the trial log from {trials_path}"))?;

    println!("Trial log: {}", log.path().display());

    // The chain verdict comes first, because every count below it is only worth
    // reading if the log has not been edited.
    let chain_ok = report_chain(&log);
    println!();

    report_counts(&log, program);
    println!();

    report_thresholds(&log, program);
    println!();

    report_gates(chain_ok);

    // A broken chain is a failure the caller should be able to notice from a
    // script, not only from reading the output.
    Ok(if chain_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Verify the chain and say so. Returns whether it held.
fn report_chain(log: &TrialLog) -> bool {
    match log.verify() {
        Ok(()) => {
            let entries = log.entries().len();
            let plural = if entries == 1 { "entry" } else { "entries" };
            println!("Chain:     verifies, {entries} {plural} from genesis forward");
            true
        }
        Err(error) => {
            println!("Chain:     BROKEN");
            println!("           {error}");
            println!(
                "           The chain is tamper evident, not tamper proof. This says an entry"
            );
            println!("           changed after it was written. It does not say who or why.");
            false
        }
    }
}

/// Both figures rule 2 requires, lifetime and scoped.
fn report_counts(log: &TrialLog, program: Option<&str>) {
    println!("Trials");
    println!(
        "  lifetime N:  {}   (every trial ever run against this dataset)",
        log.lifetime_count()
    );

    match program {
        Some(program) => println!(
            "  scoped N:    {}   (research program {program})",
            log.count_for(program)
        ),
        None => println!(
            "  scoped N:    not requested, pass --program to scope the count and the threshold"
        ),
    }

    let counts = log.program_counts();
    if counts.is_empty() {
        println!("  per program: none, the log holds only the genesis entry");
    } else {
        println!("  per program:");
        for (name, count) in counts {
            println!("    {count:>6}  {name}");
        }
    }

    let recorded = log.recorded_sharpes().len();
    let missing = log.lifetime_count() - recorded;
    println!(
        "  Sharpes recorded: {recorded} of {} trials",
        log.lifetime_count()
    );
    if missing > 0 {
        println!(
            "    {missing} trial(s) recorded no Sharpe, so they count toward N but not sigma_SR"
        );
    }
}

/// `sigma_SR` and the bar a result would have to clear, or why neither exists.
fn report_thresholds(log: &TrialLog, program: Option<&str>) {
    println!("Selection bias");
    println!("  A raw Sharpe is a diagnostic and never performance. The figure below is the");
    println!("  Sharpe the luckiest of N trials would be expected to reach with no edge at all,");
    println!("  approximated as sigma_SR * sqrt(2 * ln(N)). Clearing it is necessary and is");
    println!("  nowhere near sufficient. DECISION_CRITERIA.md gates on the Deflated Sharpe.");

    report_one_threshold("lifetime", &log.recorded_sharpes(), log.lifetime_count());

    if let Some(program) = program {
        report_one_threshold(
            &format!("scoped to {program}"),
            &log.recorded_sharpes_for(program),
            log.count_for(program),
        );
    }
}

fn report_one_threshold(label: &str, sharpes: &[rust_decimal::Decimal], trials: usize) {
    let (sigma, required) = threshold::threshold_for(sharpes, trials);

    println!("  {label}:");
    match sigma {
        // Four decimals on both figures. Decimal keeps far more than that and
        // printing all of it suggests a precision the underlying estimate does
        // not have.
        Some(sigma) => println!("    sigma_SR:        {sigma:.4}"),
        None => println!(
            "    sigma_SR:        not computable, {} recorded Sharpe(s) and at least 2 are needed",
            sharpes.len()
        ),
    }
    match required {
        Some(required) => println!("    Sharpe to clear: {required:.4}  at N = {trials}"),
        None if sigma.is_none() => println!(
            "    Sharpe to clear: not computable, it is a multiple of sigma_SR and sigma_SR is unknown"
        ),
        None => println!(
            "    Sharpe to clear: not computable, N = {trials} and at least 2 trials are needed \
             before selection bias exists to correct for"
        ),
    }
}

/// Every criterion in `DECISION_CRITERIA.md`, grouped by gate.
///
/// All of them, not a selection. A gate list that shows fewer criteria than the
/// document holds reads as a shorter path to trading than there is, and
/// `crates/cli/src/gates.rs` carries a test that fails the build if the two
/// ever fall out of step.
fn report_gates(chain_ok: bool) {
    println!("Gates");
    println!("  DECISION_CRITERIA.md governs. This is a reading of it, not the thing itself.");
    println!("  [ -- ] is not an unticked box. It means the question cannot be asked yet, and");
    println!("  the line below each one says why.");

    for gate in GATES {
        println!();
        println!("{}", gate.heading);
        for item in gate.items {
            print_gate_item(item, chain_ok);
        }
    }

    let automated = GATES
        .iter()
        .flat_map(|gate| gate.items)
        .filter(|item| item.checkability == Checkability::Automated)
        .count();
    let total: usize = GATES.iter().map(|gate| gate.items.len()).sum();
    println!();
    println!("  {automated} of {total} criteria are machine-checkable today.");
}

fn print_gate_item(item: &GateItem, chain_ok: bool) {
    match item.checkability {
        // The one automated item is the chain, so its verdict is the chain's
        // verdict. A second automated item would need its own check rather than
        // this one reused, which is why the verdict is not threaded in as a
        // per-item closure. There is nothing yet to thread.
        Checkability::Automated if chain_ok => {
            println!("  [pass]  {:<6}{}", item.id, item.summary);
        }
        Checkability::Automated => println!("  [FAIL]  {:<6}{}", item.id, item.summary),
        Checkability::BlockedOnCode(reason) => {
            println!("  [ -- ]  {:<6}{}", item.id, item.summary);
            println!("                not checkable yet, {reason}");
        }
        Checkability::NeedsHuman(reason) => {
            println!("  [ -- ]  {:<6}{}", item.id, item.summary);
            println!("                not machine-checkable, {reason}");
        }
    }
}
