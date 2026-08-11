//! Tests for the trial chokepoint, including E7.
//!
//! The engine's own arithmetic is tested in `crates/engine`. What is tested
//! here is the property that only exists at this boundary: that a trial is
//! appended on every path, and that the Sharpe reaching the log is the one the
//! engine produced.

use super::*;
use std::fs;

use engine::BacktestConfig;
use ingest::adjusted::AdjustedBar;
use ingest::schema::{CloseKind, PermanentId, SessionScope};
use ingest::universe::UniverseEntry;
use jiff::civil::{Date, date};
use rust_decimal::Decimal;

/// The genesis line, copied rather than read from `trials/trials.jsonl`, so
/// these tests never touch the real scientific record.
const GENESIS: &str = r#"{"config_hash":"0000000000000000000000000000000000000000000000000000000000000000","entry_hash":"bbdc8721955c4bb3f0ba915f1306070895d598345f3f5308d09cd78093af6db5","prev_hash":"0000000000000000000000000000000000000000000000000000000000000000","program":"genesis","sharpe":null,"timestamp":"2026-08-08T00:00:00Z"}"#;

fn temp_log(name: &str) -> String {
    let path = std::env::temp_dir().join(format!("trials-{name}.jsonl"));
    fs::write(&path, format!("{GENESIS}\n")).expect("seeding a temp log");
    path.to_string_lossy().into_owned()
}

fn declared(config: &str) -> Strategy<'_> {
    Strategy::Declared(config)
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

    run(&path, "test-program", None, &declared("some=config")).expect("backtest runs");

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
    run(&path, "prog", None, &declared("first")).expect("first runs");
    run(&path, "prog", None, &declared("second")).expect("second runs");

    let log = TrialLog::load(&path).expect("reload");
    assert_eq!(log.lifetime_count(), 2);
    assert_eq!(log.count_for("prog"), 2);
    log.verify().expect("two appends leave a verifying chain");
}

/// A program name outside the documented charset is refused, and refusing
/// it leaves the log exactly as it was.
///
/// The second half is the part worth testing. A boundary check that rejects
/// after writing something is not a boundary check.
#[test]
fn a_non_ascii_program_is_refused_and_writes_nothing() {
    let path = temp_log("nonascii");
    let before = fs::read_to_string(&path).expect("read");

    let error = run(&path, "\u{3bc}structure-v1", None, &declared("some=config"))
        .expect_err("a non-ASCII program must be refused");

    // `{:#}` prints the whole chain of causes, which is what the binary
    // prints, so this asserts on what a person would actually see.
    let message = format!("{error:#}");
    assert!(
        message.contains("\u{3bc}structure-v1"),
        "the error must name the value it rejected, got {message}"
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        before,
        "a refused trial changed the log"
    );
}

/// Different configurations must hash differently, or the log would record
/// that a trial happened while losing what was tried.
#[test]
fn the_configuration_reaches_the_log_as_a_hash() {
    let path = temp_log("confighash");
    run(&path, "prog", None, &declared("lookback=12")).expect("runs");
    run(&path, "prog", None, &declared("lookback=6")).expect("runs");

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

// ---------------------------------------------------------------------------
// A synthetic curated dataset, written to a temp directory.
//
// Sixteen month-ends so a twelve-month lead-in still leaves three months of
// holding, which is the minimum that has a Sharpe at all. Prices are synthetic;
// vendor licences forbid redistributing rows.
// ---------------------------------------------------------------------------

fn month_days() -> Vec<Date> {
    (1i8..=12)
        .map(|month| date(2020, month, 28))
        .chain((1i8..=4).map(|month| date(2021, month, 28)))
        .collect()
}

fn fixture_bars() -> Vec<AdjustedBar> {
    let rising = [
        "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "20", "21", "22", "20", "24",
        "26",
    ];
    let mut bars = Vec::new();
    for (day, price) in month_days().iter().zip(rising) {
        for (ticker, permaticker, close) in [("AAA", 1u64, price), ("BBB", 2, "10")] {
            let close = Decimal::from_str_exact(close).expect("literal");
            bars.push(AdjustedBar {
                asset: AssetKey {
                    ticker: ticker.to_string(),
                    permanent: Some(PermanentId::Sharadar(permaticker)),
                },
                date: *day,
                open: close,
                high: close,
                low: close,
                close,
                volume: Decimal::from(1000u64),
                close_unadjusted: close,
                session: SessionScope::RegularHours,
                close_kind: CloseKind::ClosingAuction,
            });
        }
    }
    bars
}

/// Write the price and universe files, returning their paths.
fn fixture_dataset(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("backtest-cli-{name}"));
    fs::create_dir_all(&dir).expect("creating the fixture directory");
    let prices = dir.join("prices.parquet");
    let universe = dir.join("universe.jsonl");

    ingest::parquet::write_prices(fixture_bars(), &prices, "Synthetic")
        .expect("writing the fixture prices");

    let entries: Vec<UniverseEntry> = [("AAA", 1u64), ("BBB", 2)]
        .into_iter()
        .map(|(ticker, permaticker)| UniverseEntry {
            asset: AssetKey {
                ticker: ticker.to_string(),
                permanent: Some(PermanentId::Sharadar(permaticker)),
            },
            name: None,
            exchange: None,
            is_delisted: false,
            first_price_date: date(2020, 1, 28),
            last_price_date: Some(date(2021, 4, 28)),
            outcome: None,
        })
        .collect();
    fs::write(
        &universe,
        ingest::universe::to_jsonl(&entries).expect("serialising the fixture universe"),
    )
    .expect("writing the fixture universe");

    (prices, universe)
}

/// E7. One run appends exactly one entry, and its `sharpe` field is the figure
/// the engine produced rather than an approximation of it.
///
/// The expected value is re-derived by running the engine directly on the same
/// bars. Asserting against a literal copied out of a previous run would only
/// prove the number had not changed, not that it was ever right.
#[test]
fn e7_a_run_appends_one_entry_carrying_the_engine_sharpe() {
    let path = temp_log("engine-sharpe");
    let (prices, universe) = fixture_dataset("engine-sharpe");
    let before = TrialLog::load(&path).expect("load").lifetime_count();

    let text = fs::read_to_string(&universe).expect("read universe");
    let config = BacktestConfig::momentum_v0(rigor::hash_bytes(text.as_bytes()));
    let panel = Panel::from_bars(fixture_bars()).expect("panel builds");
    let expected = engine::backtest(&panel, &config)
        .expect("the fixture backtests")
        .sharpe_annualised
        .round_dp(10);

    let code = run(
        &path,
        engine::PROGRAM,
        None,
        &Strategy::Momentum {
            prices: &prices,
            universe: &universe,
            actions: None,
        },
    )
    .expect("the momentum backtest runs");
    assert_eq!(code, ExitCode::SUCCESS);

    let log = TrialLog::load(&path).expect("reload");
    assert_eq!(
        log.lifetime_count(),
        before + 1,
        "a run must append exactly one entry"
    );
    log.verify().expect("the chain verifies");

    let entry = log.entries().last().expect("an entry was appended");
    assert_eq!(entry.program, engine::PROGRAM);
    assert_eq!(
        entry.sharpe,
        Some(expected),
        "the recorded Sharpe is not the one the engine produced"
    );
    assert_eq!(
        entry.config_hash,
        config.config_hash().expect("hashes").as_str(),
        "the recorded configuration hash is not the one the run was configured by"
    );
}

/// The configuration hash the log records moves when a constant moves, so two
/// different runs cannot be recorded as the same trial.
///
/// The engine-side version of this covers every field. This one checks that the
/// hash the CLI writes is the configuration's own, by changing the universe
/// file and watching the recorded hash change with it.
#[test]
fn e7_the_recorded_config_hash_follows_the_universe_file() {
    let path = temp_log("confighash-universe");
    let (prices, universe) = fixture_dataset("confighash-universe");

    run(
        &path,
        engine::PROGRAM,
        None,
        &Strategy::Momentum {
            prices: &prices,
            universe: &universe,
            actions: None,
        },
    )
    .expect("first run");

    // Naming one member changes the bytes, and therefore the SHA-256, while
    // leaving the file valid and its membership identical. A change that made
    // the file unparseable would also change the recorded hash, by falling back
    // to the unresolved-request hash, and would prove nothing about the
    // configuration reaching the log.
    let text = fs::read_to_string(&universe).expect("read");
    let renamed = text.replacen(r#""name":null"#, r#""name":"Renamed Inc""#, 1);
    assert_ne!(
        renamed, text,
        "the fixture universe file has no name field to change"
    );
    fs::write(&universe, &renamed).expect("rewrite");

    run(
        &path,
        engine::PROGRAM,
        None,
        &Strategy::Momentum {
            prices: &prices,
            universe: &universe,
            actions: None,
        },
    )
    .expect("second run");

    let log = TrialLog::load(&path).expect("reload");
    let entries = log.trials();
    assert_eq!(entries.len(), 2);
    // Both runs must have produced a figure. Two failed runs would also record
    // two different hashes, through the unresolved-request fallback, so without
    // this the assertion below would pass for the wrong reason.
    assert!(
        entries[0].sharpe.is_some() && entries[1].sharpe.is_some(),
        "a run failed, so the recorded hashes are the fallback rather than the configuration"
    );
    assert_ne!(
        entries[0].config_hash, entries[1].config_hash,
        "a different universe file recorded as the same configuration"
    );
    assert_eq!(
        entries[1].config_hash,
        BacktestConfig::momentum_v0(rigor::hash_bytes(renamed.as_bytes()))
            .config_hash()
            .expect("hashes")
            .as_str(),
        "the recorded hash is not the hash of the configuration the second run used"
    );
}

/// The guarantee this module exists for. An engine that fails still leaves a
/// trial behind.
///
/// This is the test X4 breaks: routing the engine's error out with `?` ahead of
/// the append makes the log stop growing, and the count below stops advancing.
#[test]
fn an_engine_failure_still_records_a_trial() {
    let path = temp_log("engine-failure");
    let (_, universe) = fixture_dataset("engine-failure");
    let missing = std::env::temp_dir().join("backtest-cli-no-such-file.parquet");
    let _ = fs::remove_file(&missing);
    let before = TrialLog::load(&path).expect("load").lifetime_count();

    let code = run(
        &path,
        engine::PROGRAM,
        None,
        &Strategy::Momentum {
            prices: &missing,
            universe: &universe,
            actions: None,
        },
    )
    .expect("a failed engine is still a completed command");
    assert_eq!(
        code,
        ExitCode::FAILURE,
        "a run that produced no number must not exit successfully"
    );

    let log = TrialLog::load(&path).expect("reload");
    assert_eq!(
        log.lifetime_count(),
        before + 1,
        "the engine failed and the trial was not recorded, so the counter can be \
         bypassed by making the engine fail"
    );
    assert_eq!(
        log.entries().last().expect("an entry").sharpe,
        None,
        "a failed run recorded a Sharpe it did not have"
    );
    log.verify().expect("the chain verifies");
}

/// An unreadable universe file is still a trial, hashed over the request.
#[test]
fn an_unresolvable_configuration_still_records_a_trial() {
    let path = temp_log("no-universe");
    let missing = std::env::temp_dir().join("backtest-cli-no-such-universe.jsonl");
    let _ = fs::remove_file(&missing);

    run(
        &path,
        engine::PROGRAM,
        None,
        &Strategy::Momentum {
            prices: &missing,
            universe: &missing,
            actions: None,
        },
    )
    .expect("a failed engine is still a completed command");

    let log = TrialLog::load(&path).expect("reload");
    assert_eq!(log.lifetime_count(), 1);
    log.verify().expect("the chain verifies");
}

/// The argument combinations the command accepts.
#[test]
fn the_strategy_arguments_are_either_a_config_or_a_dataset() {
    let prices = PathBuf::from("prices.parquet");
    let universe = PathBuf::from("universe.jsonl");

    let actions = PathBuf::from("actions.parquet");

    assert!(matches!(
        Strategy::from_args(Some("free text"), None, None, None, None),
        Ok(Strategy::Declared(_))
    ));
    assert!(matches!(
        Strategy::from_args(None, None, Some(&prices), Some(&universe), None),
        Ok(Strategy::Momentum { actions: None, .. })
    ));
    assert!(matches!(
        Strategy::from_args(None, None, Some(&prices), Some(&universe), Some(&actions)),
        Ok(Strategy::Momentum {
            actions: Some(_),
            ..
        })
    ));
    for bad in [
        Strategy::from_args(None, None, None, None, None),
        Strategy::from_args(None, None, Some(&prices), None, None),
        Strategy::from_args(
            Some("free text"),
            None,
            Some(&prices),
            Some(&universe),
            None,
        ),
        // Dividends with no engine behind them. Accepting this would record a
        // hash saying an actions file was used by a run that never opened one.
        Strategy::from_args(Some("free text"), None, None, None, Some(&actions)),
        // A variant with no engine behind it, which clap does not refuse on its
        // own. Measured 2026-08-11: `--config` conflicts with `--prices`, and
        // that conflict suppresses the `requires = "prices"` on `--variant`,
        // so the pair reached this function and a Declared trial was recorded
        // naming a variant that nothing ever applied.
        Strategy::from_args(Some("free text"), Some("price-floor-10"), None, None, None),
    ] {
        assert!(bad.is_err(), "an impossible combination was accepted");
    }
}

// --- E8, the dividend wiring at this boundary -------------------------------

/// A curated actions file beside the fixture dataset, with one cash dividend.
///
/// Dated inside the panel, on a security the universe holds, so it reaches a
/// holding return rather than only the unmatched census.
fn fixture_actions(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("backtest-cli-{name}"));
    fs::create_dir_all(&dir).expect("creating the fixture directory");
    let path = dir.join("actions.parquet");

    let record = ingest::ActionRecord {
        asset: AssetKey {
            ticker: "AAA".to_string(),
            permanent: Some(PermanentId::Sharadar(1)),
        },
        effective: date(2021, 2, 15),
        action: ingest::CorporateAction::Dividend {
            amount: Decimal::from_str_exact("0.5").expect("literal"),
            kind: ingest::DividendKind::Cash,
        },
        source: "Synthetic".to_string(),
    };
    ingest::parquet::write_actions(vec![record], &path, "Synthetic")
        .expect("writing the fixture actions");
    path
}

/// E8g. Supplying an actions file moves the recorded hash, and both runs
/// produce a figure.
///
/// Two entries rather than one, because a run with cash dividends and the same
/// run without are two hypotheses about returns, not one measured twice. If the
/// hash did not move, the log would record the second run as a repeat of the
/// first and the deflated Sharpe's denominator would be short by one.
#[test]
fn e8g_the_recorded_hash_moves_when_actions_are_supplied() {
    let path = temp_log("actions-hash");
    let (prices, universe) = fixture_dataset("actions-hash");
    let actions = fixture_actions("actions-hash");

    for supplied in [None, Some(&actions)] {
        let code = run(
            &path,
            engine::PROGRAM,
            None,
            &Strategy::Momentum {
                prices: &prices,
                universe: &universe,
                actions: supplied.map(PathBuf::as_path),
            },
        )
        .expect("the momentum backtest runs");
        assert_eq!(code, ExitCode::SUCCESS);
    }

    let log = TrialLog::load(&path).expect("reload");
    log.verify().expect("the chain verifies");

    let entries: Vec<_> = log.entries().iter().skip(1).collect();
    assert_eq!(entries.len(), 2, "each run must record its own trial");
    assert_ne!(
        entries[0].config_hash, entries[1].config_hash,
        "the same hash for a run with dividends and one without would record two \
         different strategies as a single trial"
    );
    assert!(
        entries.iter().all(|entry| entry.sharpe.is_some()),
        "both runs must have produced a figure, or the hashes differ for the \
         uninteresting reason that one of them failed"
    );
    assert_ne!(
        entries[0].sharpe, entries[1].sharpe,
        "the dividend reached the hash but not the returns"
    );
}

/// E8h, CLI half. The printer selects the caveat block on the report's flag.
///
/// The engine half, that the two blocks differ in the ways that matter, is in
/// `crates/engine/src/tests/dividends.rs`. What is only testable here is that
/// the printer consults the flag at all.
#[test]
fn e8h_the_printed_caveats_follow_the_dividend_flag() {
    let (prices, universe) = fixture_dataset("caveat-flag");
    let actions = fixture_actions("caveat-flag");

    let text = fs::read_to_string(&universe).expect("read universe");
    let sha256 = rigor::hash_bytes(text.as_bytes());
    let bars = ingest::parquet::read_prices(&prices).expect("read prices");

    let without = engine::backtest(
        &Panel::from_bars(bars.clone()).expect("panel builds"),
        &BacktestConfig::momentum_v0(&sha256),
    )
    .expect("the fixture backtests");

    let records = ingest::parquet::read_actions(&actions).expect("read actions");
    let with = engine::backtest(
        &Panel::from_bars(bars)
            .expect("panel builds")
            .with_dividends(&records)
            .expect("dividends attach"),
        &BacktestConfig {
            actions_sha256: Some(rigor::hash_bytes(
                &fs::read(&actions).expect("read actions bytes"),
            )),
            ..BacktestConfig::momentum_v0(&sha256)
        },
    )
    .expect("the fixture backtests");

    assert_eq!(report::caveat_block(&without), engine::CAVEATS);
    assert_eq!(
        report::caveat_block(&with),
        engine::CAVEATS_WITH_DIVIDENDS,
        "the printer published the wrong bias statement beside a real figure"
    );
}

// --- E9, the program door ----------------------------------------------------

/// `--program` picks the configuration, end to end, and the log records which
/// one ran.
///
/// The gap this closes is narrow and expensive. If the door resolved
/// `lowvol-v0` to the momentum constructor, every other test in this repository
/// would still pass, the run would report a figure, and the trial log would
/// carry a second momentum result filed under a low-volatility program. Nothing
/// downstream could tell.
#[test]
fn the_program_selects_the_configuration_the_run_is_recorded_under() {
    let path = temp_log("program-door");
    let (prices, universe) = fixture_dataset("program-door");
    let sha256 = rigor::hash_bytes(fs::read_to_string(&universe).expect("read").as_bytes());

    for program in [engine::PROGRAM, engine::LOWVOL_PROGRAM] {
        let code = run(
            &path,
            program,
            None,
            &Strategy::Momentum {
                prices: &prices,
                universe: &universe,
                actions: None,
            },
        )
        .expect("the backtest runs");
        assert_eq!(code, ExitCode::SUCCESS, "{program} produced no figure");
    }

    let log = TrialLog::load(&path).expect("reload");
    let entries = log.trials();
    assert_eq!(entries.len(), 2);
    for (entry, expected) in entries.iter().zip([
        BacktestConfig::momentum_v0(&sha256),
        BacktestConfig::lowvol_v0(&sha256),
    ]) {
        assert_eq!(
            entry.config_hash,
            expected.config_hash().expect("hashes").as_str(),
            "the trial recorded under {} is not the configuration that program \
             names",
            entry.program
        );
    }
    assert_ne!(
        entries[0].config_hash, entries[1].config_hash,
        "the two programs recorded as the same configuration"
    );
}

/// A program the engine has no configuration for is refused rather than run as
/// momentum by default.
///
/// The trial is still recorded, with no Sharpe, because the attempt happened.
/// What must not happen is a figure appearing under a name that does not
/// describe what produced it.
#[test]
fn an_unknown_program_produces_no_figure() {
    let path = temp_log("unknown-program");
    let (prices, universe) = fixture_dataset("unknown-program");

    let code = run(
        &path,
        "momentum-v99",
        None,
        &Strategy::Momentum {
            prices: &prices,
            universe: &universe,
            actions: None,
        },
    )
    .expect("a refused configuration is still a completed command");
    assert_eq!(
        code,
        ExitCode::FAILURE,
        "an unrecognised program ran something and called it a success"
    );

    let log = TrialLog::load(&path).expect("reload");
    assert_eq!(
        log.entries().last().expect("an entry").sharpe,
        None,
        "an unrecognised program produced a figure, so it resolved to some \
         configuration after all"
    );
    log.verify().expect("the chain verifies");
}

// --- E10, the variant door and the registry ----------------------------------

/// E10c. A variant reaches the log as the bare program, with the difference
/// carried by the configuration hash.
///
/// This is the division of labour rule 2 needs. Scoped `N` counts trials within
/// a research program, and a robustness rerun of one hypothesis belongs to that
/// hypothesis. Recording `lowvol-v0 --variant liquidity-screened` under a
/// program string of its own would give each variant a scoped `N` of one and a
/// deflated Sharpe computed as though no search had happened.
///
/// The recorded hash is checked against a configuration written out here rather
/// than against "not the base hash". A resolution that handed back some third
/// configuration would differ from the base too.
#[test]
fn e10c_a_variant_reaches_the_log_as_the_bare_program() {
    let path = temp_log("variant-door");
    let (prices, universe) = fixture_dataset("variant-door");
    let sha256 = rigor::hash_bytes(fs::read_to_string(&universe).expect("read").as_bytes());

    for variant in [None, Some(engine::VARIANT_LIQUIDITY_SCREENED)] {
        let code = run(
            &path,
            engine::LOWVOL_PROGRAM,
            variant,
            &Strategy::Momentum {
                prices: &prices,
                universe: &universe,
                actions: None,
            },
        )
        .expect("the backtest runs");
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "{variant:?} produced no figure, so the hashes below would differ only \
             because one run failed"
        );
    }

    let log = TrialLog::load(&path).expect("reload");
    log.verify().expect("the chain verifies");
    let entries = log.trials();
    assert_eq!(entries.len(), 2);

    for entry in entries {
        assert_eq!(
            entry.program,
            engine::LOWVOL_PROGRAM,
            "a variant was recorded under a program string of its own, so the \
             scoped N no longer groups the variants of one hypothesis"
        );
    }
    assert!(
        entries.iter().all(|entry| entry.sharpe.is_some()),
        "a run recorded no figure"
    );
    assert_ne!(
        entries[0].config_hash, entries[1].config_hash,
        "the variant recorded as the same configuration as the base run"
    );
    assert_eq!(
        entries[1].config_hash,
        BacktestConfig {
            liquidity_floor_fraction: Some(Decimal::from_str_exact("0.2").expect("literal")),
            ..BacktestConfig::lowvol_v0(&sha256)
        }
        .config_hash()
        .expect("hashes")
        .as_str(),
        "the recorded hash is not lowvol-v0 with the liquidity screen engaged"
    );
}

/// E10d. Every runnable pair records a distinct configuration under the bare
/// program name.
///
/// The walk is over `engine::RUNNABLE` rather than over a list written here, so
/// a pair added to the registry later is covered by this test the moment it
/// exists rather than when somebody remembers. That is the point of the
/// registry: the class of near-miss it closes is a configuration reachable from
/// the command line that nothing ever checked records as its own trial.
#[test]
fn e10d_every_runnable_pair_hashes_distinctly() {
    let path = temp_log("registry-walk");
    let (prices, universe) = fixture_dataset("registry-walk");

    for (program, variant) in engine::RUNNABLE {
        let code = run(
            &path,
            program,
            *variant,
            &Strategy::Momentum {
                prices: &prices,
                universe: &universe,
                actions: None,
            },
        )
        .expect("the backtest runs");
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "--program {program} --variant {variant:?} is in the registry and \
             produced no figure, so the door and the registry disagree"
        );
    }

    let log = TrialLog::load(&path).expect("reload");
    log.verify().expect("the chain verifies");
    let entries = log.trials();
    assert_eq!(
        entries.len(),
        engine::RUNNABLE.len(),
        "the walk recorded a different number of trials from the registry's length"
    );

    // Without this the distinctness assertion below would pass for the
    // uninteresting reason that a failed run hashes over its request instead of
    // over a configuration.
    assert!(
        entries.iter().all(|entry| entry.sharpe.is_some()),
        "a run in the walk produced no figure, so its recorded hash is the \
         unresolved-request fallback rather than a configuration"
    );

    for (entry, (program, _)) in entries.iter().zip(engine::RUNNABLE) {
        assert_eq!(
            entry.program, *program,
            "a variant was recorded under something other than its bare program"
        );
    }

    let mut hashes: Vec<&str> = entries
        .iter()
        .map(|entry| entry.config_hash.as_str())
        .collect();
    hashes.sort_unstable();
    let distinct = hashes.len();
    hashes.dedup();
    assert_eq!(
        hashes.len(),
        distinct,
        "two runnable pairs recorded the same configuration hash, so the trial \
         log cannot tell them apart"
    );
}

/// E10e. A variant the engine has no configuration for is refused rather than
/// run as the base program.
///
/// Both shapes of the mistake: a variant nobody defined, and a real variant
/// asked for on a program that has none. The trial is still recorded, with no
/// Sharpe, because the attempt happened.
#[test]
fn e10e_an_unknown_variant_produces_no_figure() {
    let path = temp_log("unknown-variant");
    let (prices, universe) = fixture_dataset("unknown-variant");

    let attempts = [
        (engine::LOWVOL_PROGRAM, "price-floor-99"),
        // A variant that exists, on a program it is not a variant of.
        (engine::PROGRAM, engine::VARIANT_LIQUIDITY_SCREENED),
    ];
    for (program, variant) in attempts {
        let code = run(
            &path,
            program,
            Some(variant),
            &Strategy::Momentum {
                prices: &prices,
                universe: &universe,
                actions: None,
            },
        )
        .expect("a refused configuration is still a completed command");
        assert_eq!(
            code,
            ExitCode::FAILURE,
            "--program {program} --variant {variant} ran something and called it \
             a success"
        );
    }

    let log = TrialLog::load(&path).expect("reload");
    log.verify().expect("the chain verifies");
    let entries = log.trials();
    assert_eq!(
        entries.len(),
        attempts.len(),
        "each attempt is still a trial"
    );
    assert!(
        entries.iter().all(|entry| entry.sharpe.is_none()),
        "an unrecognised variant produced a figure, so it resolved to some \
         configuration after all"
    );
}

// --- E9e, the N caveat -------------------------------------------------------

/// E9e. The `N` caveat is part of the DSR block, not a decoration on it.
///
/// Both states are checked, because the failure worth catching is a caveat that
/// only appears beside a figure. A scoped `N` of one is exactly when the
/// deflated Sharpe is honestly absent and exactly when a reader is most likely
/// to reach for the raw number instead, so the line about what `N` counts
/// matters more there, not less.
#[test]
fn e9e_the_n_caveat_prints_beside_every_dsr_block() {
    let panel = Panel::from_bars(fixture_bars()).expect("panel builds");
    let report = engine::backtest(&panel, &BacktestConfig::momentum_v0("0".repeat(64)))
        .expect("the fixture backtests");

    // One trial in the program. `sigma_SR` needs at least two recorded Sharpes
    // to estimate their spread, so the scoped reading has no figure.
    let thin = temp_log("n-caveat-thin");
    let mut log = TrialLog::load(&thin).expect("load");
    log.append(
        engine::LOWVOL_PROGRAM,
        &ConfigHash::of(b"only"),
        Some(Decimal::from_str_exact("0.4").expect("literal")),
    )
    .expect("append");
    let absent = report::dsr_block(&log, engine::LOWVOL_PROGRAM, &report);
    assert!(
        absent.contains("no figure exists"),
        "the fixture does not reach the absent-figure branch, got {absent}"
    );
    assert!(
        absent.contains(report::N_CAVEAT),
        "the caveat is missing from a DSR block with no figure in it, got {absent}"
    );

    // Two recorded Sharpes in the program, so both readings produce a figure.
    let full = temp_log("n-caveat-full");
    let mut log = TrialLog::load(&full).expect("load");
    for (config, sharpe) in [(&b"first"[..], "0.4"), (&b"second"[..], "0.9")] {
        log.append(
            engine::LOWVOL_PROGRAM,
            &ConfigHash::of(config),
            Some(Decimal::from_str_exact(sharpe).expect("literal")),
        )
        .expect("append");
    }
    let present = report::dsr_block(&log, engine::LOWVOL_PROGRAM, &report);
    assert!(
        !present.contains("no figure exists"),
        "the fixture does not reach the figure branch, got {present}"
    );
    assert!(
        present.contains(report::N_CAVEAT),
        "the caveat is missing from a DSR block with a figure in it, got {present}"
    );
}
