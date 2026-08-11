//! E9. The low-volatility strategy: which tail is held, what the formation
//! window may see, and the arithmetic underneath both.
//!
//! Selection and lookahead are mandatory-test paths under `CLAUDE.md`, so every
//! expected figure below is derived in its comment and the derivation is the
//! test. A number updated to match what the code produced would turn this file
//! into a transcript of a past run.
//!
//! # Why the calm name is second in identity order
//!
//! The panel sorts securities by permaticker, so `WILD` at 1 is index 0 and
//! `CALM` at 2 is index 1. That ordering is deliberate. Ties in the ranking
//! break on identity order, so an implementation that returns the same
//! volatility for every name, which is exactly the shape of a stub, holds
//! `WILD`. Putting the calm name first would have let such a stub pass these
//! tests for the wrong reason.

use super::{asset, daily_panel, dec, lowvol_config, month_ends, panel_of, test_config};
use crate::lowvol;
use crate::momentum;
use crate::run;
use jiff::civil::date;

/// The E9a fixture, eighteen daily closes per name.
///
/// ```text
///   month      Jan            Feb              Mar              Apr
///   WILD   100 100 100    200 100 200      400 200 400      800 400 800
///   CALM   100 100 100    100 100 125      125 125 100      100 100 125
///
///   month      May                 Jun
///   WILD   1600 800 1600      3200 1600 3200
///   CALM    125 125 100        100 100 125
/// ```
///
/// `WILD` doubles and halves within every month and doubles month over month.
/// `CALM` sits still for two days and then steps by a quarter, and its month
/// ends alternate between 100 and 125 so that it goes nowhere.
///
/// The two things this fixture has to separate, at every rebalance:
///
/// - the LOW tail from the HIGH tail. `WILD`'s daily returns in any formation
///   month are `+1.0, -0.5, +1.0` and `CALM`'s are `0, 0, +0.25` or
///   `0, 0, -0.2`, so `CALM` is the calmer name by a wide margin every time.
/// - volatility from return. `CALM` has the LOWER trailing return at every
///   rebalance, 0.25 against 1.0 in the up months and -0.2 against 1.0 in the
///   down ones, so a signal that ranked on return would hold `WILD`.
fn wild_and_calm() -> crate::panel::Panel {
    daily_panel(&[
        (
            asset("WILD", 1),
            [
                "100", "100", "100", "200", "100", "200", "400", "200", "400", "800", "400", "800",
                "1600", "800", "1600", "3200", "1600", "3200",
            ],
        ),
        (
            asset("CALM", 2),
            [
                "100", "100", "100", "100", "100", "125", "125", "125", "100", "100", "100", "125",
                "125", "125", "100", "100", "100", "125",
            ],
        ),
    ])
}

/// E9a. The lowest-volatility quintile is what gets held.
///
/// Two eligible names give a quintile of `max(2 / 5, 1) = 1`, so exactly one
/// name is held and the choice is unambiguous. `CALM` is index 1.
///
/// ```text
///   rebalances at m2 = 2020-03-31, m3 = 04-30, m4 = 05-29, m5 = 06-30
///   holding CALM, whose month-end closes are 100, 125, 100, 125
///
///   m2->m3:  125 / 100 - 1                                  =  0.25
///     traded 1.0 into the position, cost 0.0010
///     net = (1 - 0.0010)(1.25) - 1 = 1.24875 - 1             =  0.24875
///   m3->m4:  100 / 125 - 1, nothing traded                  = -0.2
///   m4->m5:  125 / 100 - 1, nothing traded                  =  0.25
///     then the final liquidation lands its 0.0010 on it
///     net = (1 - 0.0010)(1.25) - 1                          =  0.24875
/// ```
///
/// Holding `WILD` instead, which is what the inverted tail selects, gives
/// month-end closes of 400, 800, 1600, 3200 and a gross series of
/// `1.0, 1.0, 1.0`. Nothing about the two answers is close.
#[test]
fn e9a_the_lowest_volatility_quintile_is_held() {
    let panel = wild_and_calm();
    let config = lowvol_config(2);

    let rebalance = momentum::rebalance_at(&panel, &config, 2).expect("rebalance forms");
    assert_eq!(
        rebalance.eligible,
        vec![0, 1],
        "both names clear the price floor and the coverage rule"
    );
    assert_eq!(
        rebalance.chosen,
        vec![1],
        "the held name is not CALM, so the ranking took the wrong tail"
    );

    // The control that makes the fixture a discriminating one. Ranking the same
    // window on return holds WILD, so a low-volatility strategy that quietly
    // ranked on return would fail the assertion above rather than pass it.
    assert_eq!(
        momentum::rebalance_at(&panel, &test_config(2), 2)
            .expect("rebalance forms")
            .chosen,
        vec![0],
        "the fixture does not separate volatility from return"
    );

    let report = run::backtest(&panel, &config).expect("the fixture backtests");
    assert_eq!(
        report.strategy.gross_monthly,
        vec![dec("0.25"), dec("-0.2"), dec("0.25")],
        "the monthly series is not the one holding the calm name produces"
    );
    assert_eq!(
        report.strategy.net_monthly,
        vec![dec("0.24875"), dec("-0.2"), dec("0.24875")]
    );
}

/// The E9b fixture, in two versions differing only inside the rebalance month.
///
/// ```text
///   month        Jan            Feb              Mar           Apr onward
///   WILD     100 100 100    200 100 200      200 200 200      200 every day
///   CALM     100 100 100    100 100 100      violent          100 every day
/// ```
///
/// `violent` is `1000, 100, 1000`, which is a set of daily returns of
/// `+9.0, -0.9, +9.0` placed entirely inside March, the month the rebalance at
/// index 2 happens in. The control replaces those three closes with
/// `100, 100, 100` and changes nothing else.
fn calm_with_march(march: [&str; 3]) -> crate::panel::Panel {
    daily_panel(&[
        (
            asset("WILD", 1),
            [
                "100", "100", "100", "200", "100", "200", "200", "200", "200", "200", "200", "200",
                "200", "200", "200", "200", "200", "200",
            ],
        ),
        (
            asset("CALM", 2),
            [
                "100", "100", "100", "100", "100", "100", march[0], march[1], march[2], "100",
                "100", "100", "100", "100", "100", "100", "100", "100",
            ],
        ),
    ])
}

/// E9b. The formation window cannot reach into the rebalance month.
///
/// The rebalance is at index 2, which is 2020-03-31. With a two-month lookback
/// and one month skipped the window runs from the January month-end to the
/// February one, so March is not in it.
///
/// Over that window `CALM` never moves and `WILD` doubles and halves, so `CALM`
/// is held. March is then loaded with the most violent move in the fixture and
/// given to the calm name, and the assertion is that neither the signals nor
/// the choice notice.
///
/// Widening the window by one month to end at the rebalance month instead
/// reverses it. `CALM` would carry daily returns of `+9.0, -0.9, +9.0` on top
/// of three flat ones, a sample deviation near 4.8, against `WILD`'s 0.61, and
/// the violent name would be selected as the calm one.
#[test]
fn e9b_the_volatility_window_cannot_see_the_rebalance_month() {
    let config = lowvol_config(2);
    let violent = calm_with_march(["1000", "100", "1000"]);
    let control = calm_with_march(["100", "100", "100"]);

    let observed = momentum::rebalance_at(&violent, &config, 2).expect("rebalance forms");
    let unchanged = momentum::rebalance_at(&control, &config, 2).expect("rebalance forms");

    assert_eq!(
        observed.date,
        date(2020, 3, 31),
        "the rebalance under test is not the one the violent move was placed in"
    );
    assert_eq!(
        observed.signals, unchanged.signals,
        "a move inside the rebalance month changed the formation signals, so the \
         window reaches past the month it is allowed to see"
    );
    assert_eq!(
        observed.chosen,
        vec![1],
        "the calm name was not held, so the rebalance month's own returns reached \
         the ranking"
    );
}

/// E9c. The sample standard deviation is pinned as a value, not as a rank.
///
/// A constant factor cannot change a ranking, so no selection test can tell
/// `n` from `n - 1`. This one asserts the number.
///
/// Five bars, four daily returns, chosen so the whole derivation is exact:
///
/// ```text
///   closes    100      130      117     105.3    94.77
///   returns        +0.3     -0.1     -0.1     -0.1
///
///   mean = (0.3 - 0.1 - 0.1 - 0.1) / 4                        = 0
///   squared deviations   0.09 + 0.01 + 0.01 + 0.01            = 0.12
///
///   sample variance  0.12 / (4 - 1) = 0.04   sqrt             = 0.2
///   population       0.12 / 4       = 0.03   sqrt             = 0.17320508...
/// ```
///
/// The two answers differ in the first decimal place, so the assertion below
/// cannot pass under a population denominator however the square root rounds.
#[test]
fn e9c_the_volatility_arithmetic_is_pinned() {
    let name = asset("ONE", 1);
    let panel = panel_of(&[(
        name,
        vec![
            (date(2020, 1, 31), dec("100")),
            (date(2020, 2, 25), dec("130")),
            (date(2020, 2, 26), dec("117")),
            (date(2020, 2, 27), dec("105.3")),
            (date(2020, 2, 28), dec("94.77")),
        ],
    )]);

    let observed = lowvol::volatility(&panel, 0, date(2020, 1, 31), date(2020, 2, 28))
        .expect("five bars give four returns, which is enough for a sample deviation");
    assert_eq!(
        observed,
        dec("0.2"),
        "the sample standard deviation is wrong; a population denominator would \
         give 0.1732050807568877293527446342 here"
    );
}

/// One daily return is not a sample. The `n - 1` denominator is zero there, and
/// a name with no estimate is excluded rather than given a volatility of zero,
/// which would be the most attractive value this strategy can see.
#[test]
fn a_window_with_one_return_has_no_volatility() {
    let m = month_ends();
    let name = asset("ONE", 1);
    let panel = panel_of(&[(name, vec![(m[0], dec("100")), (m[1], dec("110"))])]);

    assert_eq!(
        lowvol::volatility(&panel, 0, m[0], m[1]),
        None,
        "a single return produced a volatility, so the denominator is not n - 1"
    );
}
