//! `backtest`. Records the trial first, then admits there is no engine.
//!
//! # Why the counter moves before anything else
//!
//! `CLAUDE.md` rule 2 puts the increment in the harness so it cannot be
//! forgotten, and there is deliberately no flag to skip it. A trial that was
//! attempted is a trial, whether or not it produced a number, because what
//! inflates a best-of Sharpe is the number of configurations looked at rather
//! than the number that finished.
//!
//! # Why recording a trial with no engine is correct rather than a stub
//!
//! The alternative is a command that runs nothing and records nothing, which
//! trains the habit of running backtests outside the counter. Recording the
//! attempt with a null Sharpe is accurate. It says a configuration was put
//! forward and no figure came back.

use std::process::ExitCode;

use anyhow::Context as _;
use rigor::{TrialLog, hash_bytes};

pub fn run(trials_path: &str, program: &str, config: &str) -> anyhow::Result<ExitCode> {
    let mut log = TrialLog::load(trials_path)
        .with_context(|| format!("loading the trial log from {trials_path}"))?;

    // `append` verifies the existing chain before writing, so a broken log
    // stops this here rather than growing a longer broken log.
    let config_hash = hash_bytes(config.as_bytes());
    let entry = log
        .append(program, &config_hash, None)
        .context("recording the trial before running anything")?;

    println!("Trial recorded.");
    println!("  program:     {}", entry.program);
    println!("  timestamp:   {}", entry.timestamp);
    println!("  config_hash: {}", entry.config_hash);
    println!("  entry_hash:  {}", entry.entry_hash);
    println!("  prev_hash:   {}", entry.prev_hash);
    println!("  sharpe:      null, because no figure was produced");
    println!();

    // Counts are read after the append, so they include the trial just made.
    println!("Counts after this trial");
    println!("  lifetime N: {}", log.lifetime_count());
    println!(
        "  scoped N:   {}   (research program {program})",
        log.count_for(program)
    );
    println!();

    println!("No backtest ran. There is no engine yet, so nothing was evaluated and no");
    println!("Sharpe exists to report. The trial is recorded regardless, because the");
    println!("configuration was put forward and that is what a trial counts.");

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The genesis line, copied rather than read from `trials/trials.jsonl`, so
    /// these tests never touch the real scientific record.
    const GENESIS: &str = r#"{"config_hash":"0000000000000000000000000000000000000000000000000000000000000000","entry_hash":"bbdc8721955c4bb3f0ba915f1306070895d598345f3f5308d09cd78093af6db5","prev_hash":"0000000000000000000000000000000000000000000000000000000000000000","program":"genesis","sharpe":null,"timestamp":"2026-08-08T00:00:00Z"}"#;

    fn temp_log(name: &str) -> String {
        let path = std::env::temp_dir().join(format!("trials-{name}.jsonl"));
        fs::write(&path, format!("{GENESIS}\n")).expect("seeding a temp log");
        path.to_string_lossy().into_owned()
    }

    /// Rule 2 says the harness increments the counter so that it cannot be
    /// forgotten. This is the test that holds that.
    ///
    /// It exists because of a hole found by deliberately bypassing the append:
    /// nothing failed, because this crate had no tests at all. A property that
    /// the whole enforcement design rests on was resting on nobody moving a
    /// line of code.
    ///
    /// It fails if the append is removed, made conditional, or moved after any
    /// early return, because in every one of those cases the log does not grow.
    #[test]
    fn a_trial_is_recorded_even_though_no_engine_ran() {
        let path = temp_log("recorded");
        let before = TrialLog::load(&path).expect("load").lifetime_count();

        run(&path, "test-program", "some=config").expect("backtest runs");

        let log = TrialLog::load(&path).expect("reload");
        assert_eq!(
            log.lifetime_count(),
            before + 1,
            "the trial counter did not advance, so the enforcement path was bypassed"
        );
        assert_eq!(log.count_for("test-program"), 1, "scoped count is wrong");
        log.verify()
            .expect("the chain still verifies after the append");
    }

    /// A second trial must link to the first rather than to genesis. A chain
    /// that appends without linking would still grow, so counting alone would
    /// not catch it.
    #[test]
    fn consecutive_trials_stay_linked() {
        let path = temp_log("linked");
        run(&path, "prog", "first").expect("first runs");
        run(&path, "prog", "second").expect("second runs");

        let log = TrialLog::load(&path).expect("reload");
        assert_eq!(log.lifetime_count(), 2);
        assert_eq!(log.count_for("prog"), 2);
        log.verify().expect("two appends leave a verifying chain");
    }

    /// Different configurations must hash differently, or the log would record
    /// that a trial happened while losing what was tried.
    #[test]
    fn the_configuration_reaches_the_log_as_a_hash() {
        let path = temp_log("confighash");
        run(&path, "prog", "lookback=12").expect("runs");
        run(&path, "prog", "lookback=6").expect("runs");

        let text = fs::read_to_string(&path).expect("read");
        let hashes: Vec<&str> = text
            .lines()
            .skip(1)
            .filter_map(|line| line.split("\"config_hash\":\"").nth(1))
            .filter_map(|rest| rest.split('"').next())
            .collect();
        assert_eq!(hashes.len(), 2);
        assert_ne!(
            hashes[0], hashes[1],
            "two different configurations hashed the same"
        );
    }
}
