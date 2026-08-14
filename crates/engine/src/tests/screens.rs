//! E10, engine half. The two tradability screens and the arithmetic under one
//! of them.
//!
//! Eligibility is a mandatory-test path under `CLAUDE.md`, so every expected
//! figure below is derived in its comment and the derivation is the test.
//!
//! # Why the thin name is also the calm one, and first in identity order
//!
//! Li, Sullivan and Garcia-Feijoo (2014) report the low-volatility anomaly's
//! abnormal return concentrated in low-liquidity names, so the case worth
//! testing is exactly the one where the screen and the signal disagree. A
//! fixture whose thinnest name was somewhere in the middle of the volatility
//! ranking would be removed by the screen without changing the portfolio, and
//! the test would pass whether or not the screen ran.
//!
//! `THIN` is permaticker 1, hence index 0, so the ties in the ranking break
//! towards it. An implementation that scored every name the same, which is the
//! shape of a stub, would hold `THIN`, and so would one whose screen was a
//! no-op. Both are the failure this file exists to catch.

use std::collections::BTreeSet;

use super::{asset, bar, dec, lowvol_config, month_ends, panel_of};
use crate::config::{
    BacktestConfig, CONSERVATIVE_PROGRAM, LOWVOL_PROGRAM, PROGRAM, RUNNABLE, VALUE_PROGRAM,
    VARIANT_LIQUIDITY_SCREENED, VARIANT_PRICE_FLOOR_10, VARIANT_VALUE_WEIGHTED,
};
use crate::liquidity;
use crate::momentum;
use crate::panel::Panel;
use crate::run;
use ingest::adjusted::AdjustedBar;
use ingest::schema::AssetKey;
use jiff::civil::{Date, date};

/// Eighteen daily closes per name, with a per-name daily volume.
///
/// [`super::daily_panel`] fixes the volume at 1000 for every bar, which is what
/// every other fixture wants and exactly what a liquidity screen cannot be
/// tested against. This is that helper with the volume opened up.
fn panel_with_volumes(series: &[(AssetKey, [&str; 18], &str)]) -> Panel {
    let days = super::trading_days();
    let bars: Vec<AdjustedBar> = series
        .iter()
        .flat_map(|(key, closes, volume)| {
            days.iter()
                .zip(closes)
                .map(move |(day, close)| AdjustedBar {
                    // Struct update rather than assignment after construction: the
                    // Rust habit is to build the value you want in one expression
                    // rather than to build a wrong one and repair it.
                    volume: dec(volume),
                    ..bar(key, *day, dec(close), dec(close))
                })
        })
        .collect();
    Panel::from_bars(bars).expect("fixture panel builds")
}

/// The E10a fixture. Five names whose daily closes are flat inside each month
/// and step on its last day, so that every formation window sees the same shape
/// and the ranking is the same at every rebalance.
///
/// Name `k` grows by a factor `g_k` per month. Written out, with `C_j` the
/// month-end close of month `j` and `C_j = 100 * g^j`:
///
/// ```text
///   month j      day 1     day 2     day 3 (month end)
///                C_{j-1}   C_{j-1}   C_j
/// ```
///
/// So January is flat at 100 for every name, and the steps begin in February.
///
/// ```text
///   name      g        volume    month-end closes 100 * g^j
///   THIN      1.00       100     100    100    100        100
///   KEPT      1.01       200     101    102.01 103.0301   ...
///   WIDE      1.02       300     102    104.04 106.1208   ...
///   WIDER     1.04       400     104    108.16 112.4864   ...
///   WIDEST    1.08       500     108    116.64 125.9712   ...
/// ```
///
/// **Volatility.** Every formation window holds four closes, three of them
/// equal and the last one a factor `g` higher, so the daily returns are
/// `0, 0, g - 1`. The sample deviation of that is `(g - 1) / sqrt(3)`, which is
/// increasing in `g`, so the volatility ranking is `THIN < KEPT < WIDE <
/// WIDER < WIDEST` at every rebalance. `THIN` is the calmest name.
///
/// **Dollar volume.** Every dollar-volume window holds three bars, priced
/// `C_{j-1}, C_{j-1}, C_j`, so the median is `C_{j-1}` times the name's volume.
/// At the first rebalance that is `100 * volume`, giving 10,000 for `THIN`
/// against 20,000 for `KEPT` and more for the rest. `THIN` never grows, so it
/// stays at 10,000 while every other name's median rises, and it is the
/// thinnest name at every rebalance. `THIN` is the least liquid name.
///
/// The two rankings therefore point at the same name, which is the whole point.
fn thin_and_calm() -> Panel {
    fn path(growth: [&'static str; 6]) -> [&'static str; 18] {
        // Month j prints the previous month's end twice and then its own.
        let [c0, c1, c2, c3, c4, c5] = growth;
        [
            "100", "100", c0, c0, c0, c1, c1, c1, c2, c2, c2, c3, c3, c3, c4, c4, c4, c5,
        ]
    }

    panel_with_volumes(&[
        (
            asset("THIN", 1),
            path(["100", "100", "100", "100", "100", "100"]),
            "100",
        ),
        (
            asset("KEPT", 2),
            path([
                "100",
                "101",
                "102.01",
                "103.0301",
                "104.060401",
                "105.10100501",
            ]),
            "200",
        ),
        (
            asset("WIDE", 3),
            path([
                "100",
                "102",
                "104.04",
                "106.1208",
                "108.243216",
                "110.40808032",
            ]),
            "300",
        ),
        (
            asset("WIDER", 4),
            path([
                "100",
                "104",
                "108.16",
                "112.4864",
                "116.985856",
                "121.66529024",
            ]),
            "400",
        ),
        (
            asset("WIDEST", 5),
            path([
                "100",
                "108",
                "116.64",
                "125.9712",
                "136.048896",
                "146.93280768",
            ]),
            "500",
        ),
    ])
}

/// E10a. The liquidity screen removes the thinnest names before the ranking,
/// and the portfolio changes because of it.
///
/// Five eligible names and a fraction of 0.2 exclude `floor(5 * 0.2) = 1` name,
/// which is `THIN`. Four names remain, so the quintile is `max(4 / 5, 1) = 1`
/// and exactly one is held: `KEPT`, the calmest of the survivors.
///
/// Without the screen five names remain, the quintile is `max(5 / 5, 1) = 1`,
/// and the held name is `THIN`.
///
/// The two portfolios and their monthly series, to the penny. `THIN` never
/// moves, so its gross series is three zeros and its net series is the cost of
/// getting in and the cost of getting out. `KEPT` compounds at exactly one
/// percent a month, because `C_{j+1} / C_j = 1.01` for every `j`.
///
/// ```text
///   unscreened, holding THIN
///     m2->m3   gross 0, traded 1.0 in, cost 0.0010
///              net = (1 - 0.0010)(1 + 0) - 1              = -0.001
///     m3->m4   gross 0, nothing traded                    =  0
///     m4->m5   gross 0, nothing traded, then the final
///              liquidation lands its 0.0010 on it         = -0.001
///
///   screened, holding KEPT
///     m2->m3   gross 103.0301 / 102.01 - 1 = 0.01
///              net = (1 - 0.0010)(1.01) - 1 = 1.00899 - 1 =  0.00899
///     m3->m4   gross 104.060401 / 103.0301 - 1 = 0.01     =  0.01
///     m4->m5   gross 105.10100501 / 104.060401 - 1 = 0.01
///              then the liquidation: (1 - 0.0010)(1.01) - 1 =  0.00899
/// ```
#[test]
fn e10a_the_liquidity_screen_excludes_the_thinnest_names() {
    let panel = thin_and_calm();
    let unscreened = lowvol_config(2);
    let screened = BacktestConfig {
        liquidity_floor_fraction: Some(dec("0.2")),
        ..unscreened.clone()
    };

    let base = momentum::rebalance_at(&panel, &unscreened, 2).expect("rebalance forms");
    assert_eq!(
        base.eligible,
        vec![0, 1, 2, 3, 4],
        "every name clears the price floor and the coverage rule"
    );
    assert_eq!(
        base.chosen,
        vec![0],
        "the calmest name is not THIN, so the fixture does not put the thinnest \
         name at the calm end of the ranking"
    );

    let with_screen = momentum::rebalance_at(&panel, &screened, 2).expect("rebalance forms");
    assert_eq!(
        with_screen.eligible,
        vec![1, 2, 3, 4],
        "the screen did not remove exactly the thinnest name"
    );
    assert_eq!(
        with_screen.chosen,
        vec![1],
        "the held name is not KEPT, so the screen ran after the ranking rather \
         than before it, or did not run"
    );

    // The penny assertion. Two portfolios that differ only because one name was
    // screened out produce two different monthly series, and both are derived
    // in the doc comment rather than copied from a run.
    let without = run::backtest(&panel, &unscreened).expect("the fixture backtests");
    assert_eq!(
        without.strategy.gross_monthly,
        vec![dec("0"), dec("0"), dec("0")]
    );
    assert_eq!(
        without.strategy.net_monthly,
        vec![dec("-0.001"), dec("0"), dec("-0.001")],
        "the unscreened run is not the one holding THIN produces"
    );

    let with = run::backtest(&panel, &screened).expect("the fixture backtests");
    assert_eq!(
        with.strategy.gross_monthly,
        vec![dec("0.01"), dec("0.01"), dec("0.01")]
    );
    assert_eq!(
        with.strategy.net_monthly,
        vec![dec("0.00899"), dec("0.01"), dec("0.00899")],
        "the screened run is not the one holding KEPT produces"
    );
}

/// E10b. The median is pinned as a value on an even-count window where the
/// median and the mean differ, and where the values arrive out of order.
///
/// ```text
///   date        close   volume   dollar volume
///   2020-01-31    500        2           1000   <- outside the window
///   2020-02-25     30        1             30
///   2020-02-26     10        1             10
///   2020-02-27    100        1            100
///   2020-02-28     20        1             20
///
///   sorted           10   20   30   100
///   median   (20 + 30) / 2                 = 25
///   mean     (10 + 20 + 30 + 100) / 4      = 40
/// ```
///
/// Three separate mistakes fail this and pass a fixture that is sorted, has an
/// odd count, or is closed at the start:
///
/// - the mean instead of the median gives 40
/// - the middle pair taken without sorting first gives `(10 + 100) / 2 = 55`
/// - a window closed at `2020-01-31` admits the 1000 and gives a median of 30
#[test]
fn e10b_the_median_dollar_volume_is_pinned() {
    let name = asset("ONE", 1);
    let bars: Vec<AdjustedBar> = [
        (date(2020, 1, 31), "500", "2"),
        (date(2020, 2, 25), "30", "1"),
        (date(2020, 2, 26), "10", "1"),
        (date(2020, 2, 27), "100", "1"),
        (date(2020, 2, 28), "20", "1"),
    ]
    .into_iter()
    .map(|(day, close, volume)| AdjustedBar {
        volume: dec(volume),
        ..bar(&name, day, dec(close), dec(close))
    })
    .collect();
    let panel = Panel::from_bars(bars).expect("fixture panel builds");

    let observed = liquidity::median_dollar_volume(&panel, 0, date(2020, 1, 31), date(2020, 2, 28))
        .expect("the arithmetic holds");
    assert_eq!(
        observed,
        Some(dec("25")),
        "the median is wrong; the mean of these four is 40, the unsorted middle \
         pair averages 55, and including the January bar gives 30"
    );
}

/// The E10f fixture. A cheap calm name and a dear volatile one.
///
/// `CHEAP` trades flat at $7, so its volatility is zero and it is the calmest
/// name available. `DEAR` doubles and halves inside February and then sits at
/// $40. Both clear a $5 floor; only `DEAR` clears a $10 one.
///
/// `CHEAP` is permaticker 1, hence index 0, so an implementation that ignored
/// the floor entirely and held the calmest name would hold `CHEAP` under both
/// configurations, which is what the second half of the test refuses.
fn cheap_and_dear() -> Panel {
    super::daily_panel(&[
        (
            asset("CHEAP", 1),
            [
                "7", "7", "7", "7", "7", "7", "7", "7", "7", "7", "7", "7", "7", "7", "7", "7",
                "7", "7",
            ],
        ),
        (
            asset("DEAR", 2),
            [
                "20", "20", "20", "40", "20", "40", "40", "40", "40", "40", "40", "40", "40", "40",
                "40", "40", "40", "40",
            ],
        ),
    ])
}

/// E10f. The $10 floor refuses a name the $5 floor admits, and the variant that
/// carries it differs from the base configuration in that one field.
///
/// The formation window at the rebalance is January's month-end through
/// February's. `CHEAP` never moves over it, so its sample deviation is zero and
/// it is the calmest name. `DEAR`'s daily returns over the same window are
/// `0, +1.0, -0.5, +1.0`, so it is not close.
///
/// Under the base configuration both names clear the $5 floor and the calm one
/// is held. Under `price-floor-10` the $7 name is not eligible at all, so the
/// only name left is the volatile one and it is held instead.
#[test]
fn e10f_the_ten_dollar_floor_excludes_what_the_base_admits() {
    let universe = "0".repeat(64);
    let base = BacktestConfig::for_program(LOWVOL_PROGRAM, None, &universe)
        .expect("lowvol-v0 is a runnable program");
    let raised =
        BacktestConfig::for_program(LOWVOL_PROGRAM, Some(VARIANT_PRICE_FLOOR_10), &universe)
            .expect("price-floor-10 is a runnable variant of lowvol-v0");

    // The variant is the base configuration with one field moved, and the
    // assertion says so field by field rather than by checking the one field
    // this test cares about. A variant that also changed the cost model would
    // be a different hypothesis wearing a robustness label.
    assert_eq!(
        raised,
        BacktestConfig {
            price_floor: dec("10"),
            ..base.clone()
        },
        "price-floor-10 is not lowvol-v0 with the floor raised to $10"
    );

    // Shortened to the fixture's two-month lead-in, taking the floor from the
    // resolved variant rather than writing 10 in here, so a resolution that
    // handed back the base configuration fails this test rather than passing it.
    let panel = cheap_and_dear();
    let short = |config: &BacktestConfig| BacktestConfig {
        signal_lookback_months: 2,
        ..config.clone()
    };

    let admitted = momentum::rebalance_at(&panel, &short(&base), 2).expect("rebalance forms");
    assert_eq!(
        admitted.eligible,
        vec![0, 1],
        "$7 clears a $5 floor and must be admitted"
    );
    assert_eq!(
        admitted.chosen,
        vec![0],
        "the calmest name is not CHEAP, so the fixture does not put the cheap \
         name at the calm end of the ranking"
    );

    let refused = momentum::rebalance_at(&panel, &short(&raised), 2).expect("rebalance forms");
    assert_eq!(
        refused.eligible,
        vec![1],
        "$7 is below a $10 floor and must be refused"
    );
    assert_eq!(
        refused.chosen,
        vec![1],
        "the $10 floor left the cheap name in the portfolio"
    );
}

/// The door resolves exactly the pairs the registry lists, in both directions.
///
/// # Why this exists on top of the registry walk in the CLI tests
///
/// `e10d` walks the registry and checks that every listed pair runs and records
/// distinctly. That covers one direction. The other direction is a pair the
/// door serves that the registry does not list, and a walk over the registry is
/// structurally incapable of noticing one: it never asks about a pair that is
/// not in the list it is walking.
///
/// That gap was measured rather than reasoned about. Removing
/// `liquidity-screened` from [`RUNNABLE`] and removing the registry check from
/// [`BacktestConfig::for_program`] at the same time left the entire suite green
/// while `--variant liquidity-screened` went on running from the command line
/// untested. This is the test that fails on it.
///
/// # Why the probes are written out rather than taken from the registry
///
/// The first version of this test built its probe set from [`RUNNABLE`], and it
/// was measured green under the very mutation it was written for. A probe set
/// drawn from the registry can only ask about pairs the registry lists, which
/// is the same blind spot `e10d` has, so it inherited the gap instead of
/// closing it. The names below are therefore the constants this crate declares,
/// which is what the door's match arms are written against, and the loops
/// underneath refuse a registry entry naming a program or a variant absent from
/// them.
///
/// Both halves of a pair are written out for the same reason. A program the
/// door serves that the registry does not list is the identical failure one
/// column over, and a walk over the registry's programs cannot see it either.
///
/// What that leaves uncovered is a match arm added together with a constant and
/// listed in neither place. That one is visible in the diff and nowhere else,
/// and pretending otherwise would be worse than saying so.
#[test]
fn the_door_resolves_exactly_the_pairs_the_registry_lists() {
    let universe = "0".repeat(64);
    let listed = |program: &str, variant: Option<&str>| {
        RUNNABLE
            .iter()
            .any(|(runnable, entry)| *runnable == program && *entry == variant)
    };

    let variants = [
        None,
        Some(VARIANT_PRICE_FLOOR_10),
        Some(VARIANT_LIQUIDITY_SCREENED),
        Some(VARIANT_VALUE_WEIGHTED),
        Some("no-such-variant"),
    ];
    let declared = [
        PROGRAM,
        LOWVOL_PROGRAM,
        CONSERVATIVE_PROGRAM,
        VALUE_PROGRAM,
        "no-such-program",
    ];
    for (program, variant) in RUNNABLE {
        assert!(
            variants.contains(variant),
            "the registry lists {program} --variant {variant:?} and this test never \
             probes that variant, so the pair is covered in one direction only"
        );
        assert!(
            declared.contains(program),
            "the registry lists {program} and this test never probes that program, \
             so the pair is covered in one direction only"
        );
    }

    // `BTreeSet` deduplicates and fixes the order, so a failure names the same
    // pair on every run.
    let programs: BTreeSet<&str> = declared.into_iter().collect();
    for program in &programs {
        for variant in variants {
            let resolved = BacktestConfig::for_program(program, variant, &universe).is_some();
            assert_eq!(
                resolved,
                listed(program, variant),
                "--program {program} --variant {variant:?} resolves to {}, and the \
                 registry says it should resolve to {}",
                if resolved {
                    "a configuration"
                } else {
                    "nothing"
                },
                if listed(program, variant) {
                    "a configuration"
                } else {
                    "nothing"
                }
            );
        }
    }
}

/// A name with no bar in the window has no median, and is unrankable rather
/// than infinitely thin or infinitely liquid.
///
/// Zero would be the worst possible value on this ranking and would put the
/// name straight into the excluded tail; a missing value excludes it too, but
/// without pretending a measurement was taken.
#[test]
fn a_window_with_no_bars_has_no_median_dollar_volume() {
    let m = month_ends();
    let name = asset("ONE", 1);
    // A second name trading every month, so the PANEL's calendar is
    // consecutive while ONE's own bars still skip the window under test. A
    // month in which nothing traded anywhere is a missing fetch that
    // `Panel::from_bars` refuses; one security going quiet is not. It sorts
    // after ONE on identity, so ONE is still security 0.
    let keeper = asset("KEEP", 2);
    let panel = panel_of(&[
        (name, vec![(m[0], dec("100")), (m[4], dec("110"))]),
        (keeper, m.iter().map(|day| (*day, dec("50"))).collect()),
    ]);

    assert_eq!(
        liquidity::median_dollar_volume(&panel, 0, m[1], m[2]).expect("the arithmetic holds"),
        None,
        "a window the name did not trade in produced a dollar volume"
    );
}

/// A fraction outside `[0, 1)` is refused rather than clamped.
///
/// `CLAUDE.md` is explicit that malformed input errors rather than being
/// normalised. At 1.0 the screen would exclude every eligible name and the
/// rebalance would hold nothing, which is a configuration mistake that should
/// stop the run rather than quietly produce a flat series.
#[test]
fn a_fraction_outside_the_unit_interval_is_refused() {
    let panel = thin_and_calm();
    let (from, through): (Date, Date) = (month_ends()[0], month_ends()[1]);

    for fraction in ["-0.1", "1", "1.5"] {
        assert!(
            liquidity::screened(&panel, dec(fraction), &[0, 1, 2, 3, 4], from, through).is_err(),
            "a liquidity fraction of {fraction} was accepted"
        );
    }
    assert!(
        liquidity::screened(&panel, dec("0.2"), &[0, 1, 2, 3, 4], from, through).is_ok(),
        "0.2 is a valid fraction and must be accepted"
    );
}
