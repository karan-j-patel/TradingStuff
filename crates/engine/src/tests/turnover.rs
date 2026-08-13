//! X-T2 through X-T5, the engine half of the turnover diagnostic.
//!
//! X-T1 lives in the CLI tests, because the trial log is only reachable there.
//! Everything else is here, including the partition rule: membership is
//! arithmetic on an identifier, and asserting it through a fixture panel would
//! put a great deal of price data between the test and the property.
//!
//! Turnover is cost-adjacent: it is the quantity the cost model multiplies, so a
//! turnover figure that disagrees with the run it describes would misprice every
//! comparison drawn from it. X-T2 therefore gets the mandatory-tier treatment,
//! with its expected value re-derived from the backtest rather than pinned to a
//! literal copied out of a run.

use rust_decimal::Decimal;

use super::{dec, test_config, two_asset_panel};
use crate::config::BacktestConfig;
use crate::error::EngineError;
use crate::turnover::{self, Replay};
use crate::{portfolio, run};

/// A replay carrying only the two fields the fit reads.
fn point(mean_book: &str, mean_turnover: &str) -> Replay {
    Replay {
        modulus: 1,
        residue: 0,
        securities: 0,
        formations: 0,
        mean_book: dec(mean_book),
        mean_turnover: dec(mean_turnover),
    }
}

/// X-T2. The diagnostic's full-universe turnover is the backtest's own figure,
/// to the last decimal.
///
/// # Why exact equality rather than a tolerance
///
/// The two numbers are not two measurements of one quantity, they are one
/// quantity computed once. The replay calls `run::schedule` and
/// `run::run_schedule`, the same two functions `run::backtest` calls, and both
/// average through `portfolio::mean_one_way_turnover`. Anything less than exact
/// equality would mean a second implementation had appeared somewhere, which is
/// the thing this test exists to forbid.
///
/// The expected value is re-derived by running the backtest here rather than
/// pinned to a literal, so this test cannot pass by having been updated to
/// whatever the code last produced.
///
/// # Why the divisor is 1, which was measured rather than assumed
///
/// The first version of this test used the default quintile, which on a
/// two-name fixture holds exactly one name. A one-name book has weight 1.0 and
/// stays there, so drifted weights and target weights are the same vector and
/// the drift is invisible. The mutation this test exists for, measuring turnover
/// target-against-target and skipping the drift, ran green against it.
///
/// Holding both names is what makes drift observable: they return +0.1 and +1.0
/// over the first month, so the book drifts away from equal weight and the next
/// formation has to trade back to it. Under the mutation that trade is zero.
/// The guard below asserts exactly that non-triviality, so a future fixture edit
/// that collapses the book back to one name fails here rather than going quiet.
#[test]
fn x_t2_the_replay_turnover_equals_the_backtest_turnover_exactly() {
    let panel = two_asset_panel();
    let config = BacktestConfig {
        quintile_divisor: 1,
        ..test_config(2)
    };

    let report = run::backtest(&panel, &config).expect("the fixture backtests");
    let replay = turnover::replay(&panel, &config, 1, 0).expect("the fixture replays");

    assert_eq!(
        replay.mean_turnover, report.mean_one_way_turnover,
        "the diagnostic's turnover is not the figure the backtest reports for \
         the identical configuration, so one of them is a second implementation"
    );

    // The fixture has to have moved some notional, or the assertion above would
    // hold at zero against any implementation at all.
    assert_ne!(
        replay.mean_turnover,
        Decimal::ZERO,
        "the fixture traded nothing, so the parity assertion is vacuous"
    );

    // Drift has to be observable, or the parity holds against an implementation
    // that never drifts. The book is more than one name, and a formation after
    // the first trades a non-zero amount purely to restore the target weights.
    assert!(
        report.formations.iter().all(|census| census.held > 1),
        "the fixture holds a single name, so drifted and target weights are the \
         same vector and this test cannot see the drift"
    );
    assert_ne!(
        report.strategy.traded[1],
        Decimal::ZERO,
        "no formation after the first trades anything, so a turnover measured \
         target-against-target would agree with this one"
    );

    assert_eq!(replay.formations, report.formations.len());
}

/// The replay reports the book that was held, not the one the final formation
/// computed and never bought.
///
/// The last formation sells and buys nothing, so its `chosen` never becomes a
/// position. Averaging it in would report a book size the run did not carry, and
/// on a short fixture that is a visible fraction of the mean.
#[test]
fn the_mean_book_covers_the_formations_that_were_actually_held() {
    let panel = two_asset_panel();
    let config = test_config(2);

    let rebalances = run::schedule(&panel, &config).expect("the schedule resolves");
    let replay = turnover::replay(&panel, &config, 1, 0).expect("the fixture replays");

    // Every formation on this fixture holds exactly one name, so the mean is
    // one whichever set is averaged. What discriminates is the count.
    assert_eq!(replay.formations, rebalances.len());
    assert_eq!(replay.mean_book, dec("1"));
    assert!(
        rebalances.len() > 1,
        "a single-formation fixture cannot tell the two averages apart"
    );
}

/// X-T3. The seven sub-universes partition the universe: disjoint, and between
/// them exhaustive.
///
/// # Why this is asserted as arithmetic rather than through a panel
///
/// Membership is `permaticker % modulus == residue` and nothing else. Running it
/// through a fixture panel would test the same rule with a great deal of price
/// data in the way, and the property worth pinning is about the identifiers.
///
/// Both halves matter and they fail to different mutations. `<= residue` in
/// place of `== residue` keeps every name in some sub-universe and breaks
/// disjointness; a residue set that missed one value keeps them disjoint and
/// breaks exhaustiveness.
#[test]
fn x_t3_the_sub_universes_partition_the_universe() {
    // A range wide enough to hit every residue of 4 several times, and starting
    // at 1 because the vendor's identifiers do.
    let universe: Vec<u64> = (1..=40).collect();

    for modulus in [2u64, 4] {
        let split: Vec<Vec<u64>> = turnover::SUB_UNIVERSES
            .iter()
            .filter(|(each, _)| *each == modulus)
            .map(|(_, residue)| {
                universe
                    .iter()
                    .copied()
                    .filter(|id| turnover::in_sub_universe(*id, modulus, *residue))
                    .collect()
            })
            .collect();
        assert_eq!(
            split.len() as u64,
            modulus,
            "the registry does not list every residue of {modulus}, so the \
             sub-universes cannot be exhaustive"
        );

        // Disjoint. Every name lands in exactly one part.
        for id in &universe {
            let homes = split.iter().filter(|part| part.contains(id)).count();
            assert_eq!(
                homes, 1,
                "permaticker {id} belongs to {homes} of the {modulus} \
                 sub-universes, so they are not a partition"
            );
        }

        // Exhaustive. The parts put back together are the universe.
        let mut union: Vec<u64> = split.concat();
        union.sort_unstable();
        assert_eq!(
            union, universe,
            "the {modulus} sub-universes do not reassemble into the full universe"
        );

        // Each part is non-empty, or a "partition" of one part and some empty
        // ones would satisfy everything above.
        assert!(
            split.iter().all(|part| !part.is_empty()),
            "a sub-universe of modulus {modulus} is empty"
        );
    }

    // The full-universe entry keeps everything, which is what makes it the
    // point the halves and quarters are compared against.
    assert_eq!(turnover::SUB_UNIVERSES[0], (1, 0));
    assert!(
        universe
            .iter()
            .all(|id| turnover::in_sub_universe(*id, 1, 0)),
        "the full-universe rule dropped a name"
    );
    assert_eq!(
        turnover::SUB_UNIVERSES.len(),
        7,
        "the diagnostic replays a different number of sub-universes from the \
         seven the fit is specified over"
    );
}

/// X-T4. The fit recovers the constants the points were generated from.
///
/// Points are built from a known `a` and `b` at book sizes chosen so every
/// `1 / K` terminates: 2, 4, 5, 10 and 20 all divide a power of ten. A fixture
/// whose reciprocals repeated would force the expected values to be whatever the
/// code produced, which is not a test.
///
/// ```text
///   a = 0.30, b = 1.20
///   K =  2   1/K = 0.5     turnover = 0.30 + 0.60  = 0.90
///   K =  4   1/K = 0.25    turnover = 0.30 + 0.30  = 0.60
///   K =  5   1/K = 0.2     turnover = 0.30 + 0.24  = 0.54
///   K = 10   1/K = 0.1     turnover = 0.30 + 0.12  = 0.42
///   K = 20   1/K = 0.05    turnover = 0.30 + 0.06  = 0.36
/// ```
///
/// The points are exactly collinear in `1 / K`, so the residuals are all zero
/// and the recovery is exact. Regressing on `K` instead of on `1 / K` fits a
/// different curve through the same five points and misses every one of them.
#[test]
fn x_t4_the_fit_recovers_the_constants_its_points_were_built_from() {
    let points = [
        point("2", "0.90"),
        point("4", "0.60"),
        point("5", "0.54"),
        point("10", "0.42"),
        point("20", "0.36"),
    ];

    let fit = turnover::fit(&points).expect("the fixture fits");
    assert_eq!(
        fit.a,
        dec("0.30"),
        "the intercept is not the book-size-free turnover the points were built \
         from, so the regressor is not 1/K"
    );
    assert_eq!(fit.b, dec("1.20"), "the slope is not the boundary churn");
    assert_eq!(
        fit.residuals,
        vec![Decimal::ZERO; 5],
        "the points are exactly collinear in 1/K and the fit did not pass \
         through them"
    );
}

/// A fit that cannot be determined is refused rather than approximated.
///
/// Every point at one book size leaves the slope free, and a slope invented
/// there would be the most flattering number this module could emit: it would
/// let any residual gap be attributed to book size.
#[test]
fn a_fit_with_no_spread_in_book_size_is_refused() {
    let flat = [point("10", "0.40"), point("10", "0.50")];
    assert!(
        matches!(
            turnover::fit(&flat),
            Err(EngineError::TurnoverFitUnderdetermined { .. })
        ),
        "a fit with no spread in K produced a slope"
    );
    assert!(
        matches!(
            turnover::fit(&flat[..1]),
            Err(EngineError::TurnoverFitUnderdetermined { .. })
        ),
        "a single point produced a line"
    );
    // The control. With spread restored the same shape fits.
    assert!(turnover::fit(&[point("10", "0.40"), point("20", "0.35")]).is_ok());
}

/// X-T5. The block carrying the estimate carries every residual with it.
///
/// The failure this forbids is a printer that emits the book-size-free figure
/// and then, on some path, does not reach the residuals. A reader given `a`
/// alone cannot tell a fit that passed through its points from one that missed
/// all of them by ten points of turnover, and the second one is worthless.
#[test]
fn x_t5_the_estimate_never_appears_without_its_residuals() {
    // Deliberately not collinear, so the residuals are non-zero and a printer
    // that dropped them would be hiding something rather than hiding zeros.
    let points = [
        point("2", "0.90"),
        point("4", "0.50"),
        point("10", "0.45"),
        point("20", "0.20"),
    ];
    let fit = turnover::fit(&points).expect("the fixture fits");
    assert!(
        fit.residuals
            .iter()
            .any(|residual| *residual != Decimal::ZERO),
        "the fixture is collinear, so a dropped residual would be invisible"
    );

    let block = turnover::block(&points, &fit);
    assert!(
        block.contains("Book-size-free turnover estimate"),
        "the block does not carry the estimate at all, got {block}"
    );

    // Every residual, by its printed value, and every point by its label.
    for (point, residual) in points.iter().zip(&fit.residuals) {
        let shown = residual.round_dp(6).normalize().to_string();
        assert!(
            block.contains(&shown),
            "the block carries the estimate and not the residual {shown} for \
             {}, got {block}",
            point.label()
        );
    }
    assert_eq!(
        block.matches("residual").count(),
        points.len(),
        "the block does not carry exactly one residual line per point, got {block}"
    );
}

/// The averaging helper both the report and the diagnostic quote is
/// single-counted, so a complete rotation reads as 1.0 rather than 2.0.
///
/// The fixtures divide exactly. A first draft used three formations, where the
/// mean is 4/3 and halving it rounds differently from writing 2/3 directly, and
/// the expected value had to be either a transcript of what the code produced or
/// a fixture with no repeating decimal in it. This is the second.
#[test]
fn mean_one_way_turnover_halves_the_two_way_notional() {
    // One formation, a complete rotation: two-way 2, one-way 1.
    assert_eq!(
        portfolio::mean_one_way_turnover(&[dec("2")]).expect("averages"),
        dec("1"),
        "a complete rotation must read as 1.0 one-way, not 2.0"
    );

    // Rotation, nothing, rotation, nothing: two-way 4 over four formations is
    // 1, halved to 0.5.
    let traded = [dec("2"), dec("0"), dec("2"), dec("0")];
    assert_eq!(
        portfolio::mean_one_way_turnover(&traded).expect("averages"),
        dec("0.5")
    );

    assert!(
        portfolio::mean_one_way_turnover(&[]).is_err(),
        "averaging over no formations produced a number"
    );
}
