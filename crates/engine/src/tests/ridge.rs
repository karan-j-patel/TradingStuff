//! The ridge import boundary: what the engine refuses, and which way it ranks.
//!
//! The signal here is not computed from anything on disk, it is READ. That
//! moves every question this file asks: not "is the arithmetic right" but "is
//! this the file the trial says it is, ranked the way the program says, over
//! the months the model actually forecast".
//!
//! Every fixture is synthetic.

use ingest::PredictionRow;
use jiff::civil::Date;
use rust_decimal::Decimal;

use super::{asset, dec, month_ends, panel_of};
use ingest::parquet::{PanelProvenance, PredictionsProvenance};

use crate::config::{BacktestConfig, RIDGE_PROGRAM, Strategy};
use crate::error::EngineError;
use crate::panel::Panel;
use crate::{momentum, run};

/// The placeholder digest the fixture panels attach predictions under.
const PREDICTIONS_SHA256: &str = "pppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppp";

/// What the fit recorded about the data it trained on. The digests match
/// [`ridge_config`], so a run under that configuration binds cleanly and any
/// test that wants a mismatch changes exactly one of them.
fn fitted_on() -> PredictionsProvenance {
    PredictionsProvenance {
        panel: PanelProvenance {
            config_hash: "c".repeat(64),
            universe_sha256: "0".repeat(64),
            prices_sha256: "7".repeat(64),
            // Four digests the fixture run attaches nothing for. They are not
            // comparable and deliberately do not match anything.
            actions_sha256: "a".repeat(64),
            delistings_sha256: "d".repeat(64),
            marketcap_sha256: "m".repeat(64),
            filings_sha256: "f".repeat(64),
        },
        panel_sha256: "8".repeat(64),
    }
}

/// Three names at a flat price, so nothing but the attached prediction can
/// order them.
///
/// Flat deliberately. Every other strategy would rank these identically and the
/// tie-break would decide, so a held set that follows the predictions is
/// following the predictions and not the prices.
fn priced_panel() -> Panel {
    let m = month_ends();
    panel_of(&[
        (
            asset("AAA", 1),
            m.iter().map(|day| (*day, dec("100"))).collect(),
        ),
        (
            asset("BBB", 2),
            m.iter().map(|day| (*day, dec("100"))).collect(),
        ),
        (
            asset("CCC", 3),
            m.iter().map(|day| (*day, dec("100"))).collect(),
        ),
    ])
}

fn prediction(ticker: &str, permaticker: u64, month_end: Date, value: &str) -> PredictionRow {
    PredictionRow {
        asset: asset(ticker, permaticker),
        month_end,
        predicted_return_1m: dec(value),
    }
}

/// Predictions from `m[3]` onwards, so the first three month-ends have none.
///
/// `m[3]` and not `m[2]`, deliberately. The fixture's lead-in is two months, so
/// predictions starting at `m[2]` would begin exactly where the schedule
/// already starts and the ridge clamp would be a no-op that no test could see
/// fail. Starting a month later is what makes the clamp's absence visible.
///
/// CCC is the model's favourite everywhere, AAA its least. The ordering is the
/// same at every month-end so a direction test does not depend on which
/// formation it looks at.
fn predictions() -> Vec<PredictionRow> {
    let m = month_ends();
    let mut rows = Vec::new();
    for month_end in &m[3..] {
        rows.push(prediction("AAA", 1, *month_end, "-0.05"));
        rows.push(prediction("BBB", 2, *month_end, "0.01"));
        rows.push(prediction("CCC", 3, *month_end, "0.09"));
    }
    rows
}

fn with_predictions(panel: Panel, rows: &[PredictionRow]) -> Panel {
    panel
        .with_predictions(rows, PREDICTIONS_SHA256, &fitted_on())
        .expect("fixture predictions attach")
}

/// The ridge rules over a two-month lead-in, wired to the fixture digest.
fn ridge_config() -> BacktestConfig {
    BacktestConfig {
        strategy: Strategy::Ridge,
        signal_lookback_months: 2,
        predictions_sha256: Some(PREDICTIONS_SHA256.to_string()),
        // The fixture panel attaches no dividends, delistings, market caps or
        // filings, so the configuration names none of them either. Universe and
        // prices are the two digests every run has, and they are what the
        // binding below compares.
        prices_sha256: Some("7".repeat(64)),
        ..BacktestConfig::momentum_v0("0".repeat(64))
    }
}

/// The program is ranked DESCENDING: the model's highest forecast is held.
///
/// # Why the fixture flips the portfolio rather than perturbing a number
///
/// All three names are priced identically at every month-end, so no other
/// signal in this engine can tell them apart and the tie-break would hold AAA
/// on identity order. Only the predictions order them. Reversing the sort holds
/// AAA instead of CCC, which is a different portfolio rather than a different
/// number, and that is what the assertion reads.
#[test]
fn the_ridge_signal_is_ranked_descending() {
    let panel = with_predictions(priced_panel(), &predictions());
    let config = ridge_config();

    let rebalance = momentum::rebalance_at(&panel, &config, 3).expect("the fixture forms");
    let held: Vec<&str> = rebalance
        .chosen
        .iter()
        .map(|position| panel.securities()[*position].asset.ticker.as_str())
        .collect();

    // Three eligible names, a quintile of `max(3 / 5, 1)` = 1, so exactly the
    // model's favourite is held.
    assert_eq!(
        held,
        vec!["CCC"],
        "the ridge program held the name the model liked LEAST, so it is ranking \
         ascending and buying the forecasts it should be avoiding"
    );
    // And the signal really is the attached number rather than anything derived
    // from the flat prices.
    assert_eq!(
        rebalance
            .signals
            .iter()
            .map(|(_, value)| *value)
            .collect::<Vec<_>>(),
        vec![dec("-0.05"), dec("0.01"), dec("0.09")],
    );
}

/// A formation before the model's first forecast never happens.
///
/// The predictions start at `m[3]` while the configuration's own lead-in is two
/// months, so without the clamp the schedule would form at `m[2]` and rank a
/// month the model never forecast. The gap between the two is the whole point
/// of the fixture: an equal pair would make the clamp invisible.
#[test]
fn a_ridge_run_forms_no_portfolio_before_its_first_prediction() {
    let m = month_ends();
    let panel = with_predictions(priced_panel(), &predictions());

    let schedule = run::schedule(&panel, &ridge_config()).expect("the fixture schedules");
    assert_eq!(
        schedule[0].date, m[3],
        "the ridge run formed a portfolio before its first prediction, so it ranked \
         a month the model never forecast"
    );
    assert!(schedule.iter().all(|rebalance| rebalance.date >= m[3]));

    // Every other program still starts at its own lead-in over the same panel,
    // so the clamp is the ridge program's and not a change to the schedule for
    // everyone. The two dates differ, which is what makes the assertion above
    // a statement about the clamp rather than about the lead-in.
    let momentum = BacktestConfig {
        signal_lookback_months: 2,
        ..BacktestConfig::momentum_v0("0".repeat(64))
    };
    assert_eq!(
        run::schedule(&priced_panel(), &momentum).expect("momentum schedules")[0].date,
        m[2],
        "the fixture no longer distinguishes the two lead-ins"
    );
}

/// Predictions that do not reach the panel are refused rather than producing an
/// empty run.
#[test]
fn predictions_that_do_not_overlap_the_panel_are_refused() {
    let panel = with_predictions(
        priced_panel(),
        &[prediction("AAA", 1, Date::constant(2031, 1, 31), "0.02")],
    );

    let error = run::schedule(&panel, &ridge_config())
        .expect_err("predictions after the panel must be refused");
    assert!(
        matches!(error, EngineError::PredictionsOutsidePanel { .. }),
        "got {error:?}"
    );
}

/// A month the model forecast nothing for is an empty field, not an error.
///
/// The distinction the spec draws. A missing prediction for a NAME drops that
/// name; a month missing them entirely drops every name and the formation holds
/// nothing. Both are ordinary states of a run whose test window is bounded by
/// what the fit produced, and both are visible in the census rather than
/// stopping the run.
#[test]
fn a_month_with_no_predictions_is_an_empty_field_rather_than_an_error() {
    let m = month_ends();
    // Everything except m[4], which the model skipped entirely.
    let sparse: Vec<PredictionRow> = predictions()
        .into_iter()
        .filter(|row| row.month_end != m[4])
        .collect();
    let panel = with_predictions(priced_panel(), &sparse);
    let config = ridge_config();

    let skipped = momentum::rebalance_at(&panel, &config, 4).expect("the formation still forms");
    assert_eq!(
        skipped.census.eligible, 0,
        "a month the model forecast nothing for still admitted names to the ranking"
    );
    assert!(skipped.chosen.is_empty());

    // The months around it are unaffected, which is what makes the zero above a
    // measurement rather than a broken fixture.
    for index in [3usize, 5] {
        assert_eq!(
            momentum::rebalance_at(&panel, &config, index)
                .expect("the fixture forms")
                .census
                .eligible,
            3,
            "month index {index} lost names it has predictions for"
        );
    }

    // And one name missing at one month drops only that name.
    let one_gone: Vec<PredictionRow> = predictions()
        .into_iter()
        .filter(|row| !(row.month_end == m[5] && row.asset.ticker == "CCC"))
        .collect();
    let panel = with_predictions(priced_panel(), &one_gone);
    let rebalance = momentum::rebalance_at(&panel, &config, 5).expect("the fixture forms");
    assert_eq!(rebalance.census.eligible, 2);
    assert_eq!(
        rebalance
            .chosen
            .iter()
            .map(|position| panel.securities()[*position].asset.ticker.as_str())
            .collect::<Vec<_>>(),
        vec!["BBB"],
        "with the model's favourite unforecast this month, the next-best name is held"
    );
}

/// A ridge run with no predictions attached refuses, and a run whose recorded
/// digest is not the panel's refuses too.
///
/// The first is the strategy's own requirement: the configuration and the panel
/// AGREE when neither names a predictions file, so nothing before this point
/// refuses a ridge run with no signal at all. The second is the wiring guard
/// one dataset over, and it matters more here than anywhere else because the
/// signal IS the file rather than something computed from it.
#[test]
fn a_ridge_run_refuses_missing_and_mismatched_predictions() {
    let bare = priced_panel();
    let unwired = BacktestConfig {
        predictions_sha256: None,
        ..ridge_config()
    };
    // Refused at BOTH doors, and they are genuinely separate. `check_wiring`
    // carries the strategy's requirement alongside the conservative formula's
    // and the value program's; `schedule` refuses independently because it has
    // no first prediction month to start from. A ridge run reaches one or the
    // other whichever way it is invoked.
    for error in [
        run::check_wiring(&bare, &unwired).expect_err("the wiring guard must refuse"),
        run::schedule(&bare, &unwired).expect_err("the schedule must refuse"),
    ] {
        assert!(
            matches!(error, EngineError::RidgeMissingPredictions),
            "got {error:?}"
        );
    }

    // Attached but named as a different file.
    let panel = with_predictions(priced_panel(), &predictions());
    let error = run::check_wiring(
        &panel,
        &BacktestConfig {
            predictions_sha256: Some("1".repeat(64)),
            ..ridge_config()
        },
    )
    .expect_err("a digest disagreement must refuse");
    assert!(
        matches!(
            error,
            EngineError::DatasetDigestMismatch {
                dataset: "predictions",
                ..
            }
        ),
        "got {error:?}"
    );

    // And presence disagreeing either way is refused before that.
    let error = run::check_wiring(&panel, &unwired)
        .expect_err("a panel with predictions under a config naming none must refuse");
    assert!(
        matches!(error, EngineError::PredictionsWiringMismatch { .. }),
        "got {error:?}"
    );
}

/// The predictions must have been fitted on the data this run reads.
///
/// # Why this is not covered by any digest guard that already existed
///
/// The existing wiring guard compares the CONFIGURATION's recorded digest
/// against the PANEL's, which says the run read what it says it read. It is
/// silent about whether the model was fitted on the same thing. A fit over
/// snapshot A ranking a backtest over snapshot B passes every other check in
/// this engine: both files are valid, both digests are recorded, the returns
/// look ordinary, and the coefficients were learned from data the run never
/// touched.
///
/// The comparison is digest by digest rather than on `config_hash`, which could
/// never match: a ridge run's configuration differs from the export's by
/// construction.
#[test]
fn predictions_fitted_on_other_data_are_refused() {
    let panel = |provenance| {
        priced_panel()
            .with_predictions(&predictions(), PREDICTIONS_SHA256, &provenance)
            .expect("fixture predictions attach")
    };

    // Each of the two digests this fixture's run actually has, moved in turn.
    for (dataset, moved) in [
        (
            "universe",
            PredictionsProvenance {
                panel: PanelProvenance {
                    universe_sha256: "9".repeat(64),
                    ..fitted_on().panel
                },
                ..fitted_on()
            },
        ),
        (
            "prices",
            PredictionsProvenance {
                panel: PanelProvenance {
                    prices_sha256: "9".repeat(64),
                    ..fitted_on().panel
                },
                ..fitted_on()
            },
        ),
    ] {
        let error = run::check_wiring(&panel(moved), &ridge_config())
            .expect_err("predictions fitted on other data must be refused");
        match error {
            EngineError::PredictionsProvenanceMismatch { dataset: named, .. } => {
                assert_eq!(named, dataset)
            }
            other => panic!("got {other:?} for a moved {dataset} digest"),
        }
    }

    // The matching block passes, so the guard refuses a mismatch rather than
    // refusing everything. Note what it does NOT refuse: `fitted_on` carries
    // four digests for datasets this run attaches nothing for, and they match
    // nothing at all. A dataset the run does not read is not comparable, which
    // is what keeps ridge-v0 runnable without filings.
    assert!(run::check_wiring(&panel(fitted_on()), &ridge_config()).is_ok());
    assert_ne!(fitted_on().panel.filings_sha256, "0".repeat(64));

    // And a run with no predictions attached is not bound to anything.
    assert!(
        run::check_wiring(
            &priced_panel(),
            &BacktestConfig {
                predictions_sha256: None,
                strategy: Strategy::Momentum,
                ..ridge_config()
            }
        )
        .is_ok()
    );
}

/// Two predictions for one name on one month-end are refused at the panel door.
///
/// The curated writer refuses this shape, so a pair arriving here came around
/// it. Which value the ranking would use depends on caller order, which is a
/// portfolio nobody chose.
#[test]
fn a_duplicate_prediction_is_refused_at_the_panel_door() {
    let m = month_ends();
    let error = priced_panel()
        .with_predictions(
            &[
                prediction("AAA", 1, m[2], "0.01"),
                prediction("AAA", 1, m[2], "0.09"),
            ],
            PREDICTIONS_SHA256,
            &fitted_on(),
        )
        .expect_err("a duplicate prediction must be refused");
    assert!(
        matches!(error, EngineError::DuplicatePrediction { .. }),
        "got {error:?}"
    );
}

/// The ridge program is in the registry and hashes as its own configuration.
#[test]
fn the_ridge_program_is_runnable_and_distinct() {
    let universe = "0".repeat(64);
    let ridge = BacktestConfig::for_program(RIDGE_PROGRAM, None, &universe)
        .expect("the registry serves the ridge program");
    assert_eq!(ridge.strategy, Strategy::Ridge);
    // Every constant that is not the signal is momentum's, which is what makes
    // the comparison between them a comparison of signals.
    let momentum = BacktestConfig::momentum_v0(&universe);
    assert_eq!(ridge.price_floor, momentum.price_floor);
    assert_eq!(ridge.min_coverage, momentum.min_coverage);
    assert_eq!(ridge.quintile_divisor, momentum.quintile_divisor);
    assert_eq!(ridge.rebalance_every_months, 1);
    assert_eq!(ridge.weighting, momentum.weighting);
    assert_ne!(
        ridge.config_hash().expect("hashes").as_str(),
        momentum.config_hash().expect("hashes").as_str(),
    );
    // Decimal literals rather than a repeat of the constructor, so a changed
    // default fails here instead of being copied into the expectation.
    assert_eq!(ridge.price_floor, Decimal::from(5u64));
    assert_eq!(ridge.min_coverage, dec("0.80"));
}
