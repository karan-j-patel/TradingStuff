//! E1 and E5. What the signal may see, and who is allowed into the field.

use super::{asset, bar, dec, month_ends, panel_of, test_config};
use crate::momentum;
use crate::panel::Panel;
use jiff::civil::{Date, date};

/// E1. The signal cannot reach the rebalance month or anything after it.
///
/// ```text
///          m0     m1     m2     m3
///   AAA    10     20      5      5
///   BBB    10     10     40   1000
/// ```
///
/// The rebalance is m2. With one month skipped the window runs from m0 to m1:
///
/// ```text
///   AAA  20/10 - 1 =  1.0     <- wins
///   BBB  10/10 - 1 =  0.0
/// ```
///
/// Both later prices are placed to flip that if they were ever reached. Reading
/// through to the rebalance month gives AAA 5/10-1 = -0.5 against BBB
/// 40/10-1 = 3.0, and BBB's m3 print of 1000 would swamp anything. Neither is
/// visible, so AAA is held.
#[test]
fn e1_the_signal_cannot_see_the_rebalance_month_or_later() {
    let m = month_ends();
    let panel = panel_of(&[
        (
            asset("AAA", 1),
            vec![
                (m[0], dec("10")),
                (m[1], dec("20")),
                (m[2], dec("5")),
                (m[3], dec("5")),
            ],
        ),
        (
            asset("BBB", 2),
            vec![
                (m[0], dec("10")),
                (m[1], dec("10")),
                (m[2], dec("40")),
                (m[3], dec("1000")),
            ],
        ),
    ]);
    let config = test_config(2);

    let rebalance = momentum::rebalance_at(&panel, &config, 2).expect("rebalance forms");
    assert_eq!(rebalance.date, m[2]);
    assert_eq!(rebalance.eligible, vec![0, 1]);

    assert_eq!(
        rebalance.signals,
        vec![(0, dec("1")), (1, dec("0"))],
        "the signal window is not m0 to m1"
    );
    assert_eq!(
        rebalance.chosen,
        vec![0],
        "the ranking used a price at or after the rebalance date"
    );
}

/// Build the E5 fixture. Three securities over three months of five trading
/// days each, so the coverage rule has a denominator bigger than one.
///
/// - `GOOD` trades every day at $10.
/// - `CHEAP` trades every day, at `cheap_price` unadjusted on the rebalance
///   date and $10 otherwise.
/// - `SPARSE` trades every day of the first and last month, and only
///   `sparse_february_days` of the middle one, always including its last day so
///   that the signal itself is computable.
fn eligibility_panel(cheap_price: &str, sparse_february_days: usize) -> Panel {
    let january = [27, 28, 29, 30, 31].map(|day| date(2020, 1, day));
    let february = [24, 25, 26, 27, 28].map(|day| date(2020, 2, day));
    let march = [25, 26, 27, 30, 31].map(|day| date(2020, 3, day));
    let rebalance_date = march[4];

    // The last February day is kept whatever the count, so what varies between
    // the two variants is coverage alone and not whether a signal exists.
    let sparse_february: Vec<Date> = february[..sparse_february_days.saturating_sub(1)]
        .iter()
        .copied()
        .chain(std::iter::once(february[4]))
        .collect();

    let good = asset("GOOD", 1);
    let cheap = asset("CHEAP", 2);
    let sparse = asset("SPARSE", 3);
    let ten = dec("10");

    let mut bars = Vec::new();
    for day in january.iter().chain(&february).chain(&march) {
        bars.push(bar(&good, *day, ten, ten));
        let unadjusted = if *day == rebalance_date {
            dec(cheap_price)
        } else {
            ten
        };
        bars.push(bar(&cheap, *day, ten, unadjusted));
    }
    for day in january.iter().chain(&sparse_february).chain(&march) {
        bars.push(bar(&sparse, *day, ten, ten));
    }

    Panel::from_bars(bars).expect("fixture panel builds")
}

/// E5. The $5 floor and the 80% coverage rule each exclude exactly the name
/// built to offend them, and neither excludes the other's.
///
/// The lead-in window is February, which has five trading days, so the coverage
/// requirement is `ceil(0.8 * 5) = 4` bars. `SPARSE` at three is excluded and
/// at four is not.
#[test]
fn e5_the_price_floor_and_the_coverage_rule_each_exclude_one_name() {
    let config = test_config(2);

    // Both offenders offending. GOOD is index 0, CHEAP index 1, SPARSE index 2,
    // by permaticker.
    let both = eligibility_panel("4.99", 3);
    assert_eq!(
        both.trading_days_in(date(2020, 1, 31), date(2020, 2, 28)),
        5,
        "the coverage denominator is not February's five trading days"
    );
    let rebalance = momentum::rebalance_at(&both, &config, 2).expect("rebalance forms");
    assert_eq!(
        rebalance.eligible,
        vec![0],
        "a name below the price floor or under the coverage bar was admitted"
    );

    // The floor is the only thing keeping CHEAP out: at exactly $5 it is in,
    // which pins the comparison as `< floor` rather than `<= floor`.
    let floor_ok = eligibility_panel("5", 3);
    assert_eq!(
        momentum::rebalance_at(&floor_ok, &config, 2)
            .expect("rebalance forms")
            .eligible,
        vec![0, 1],
        "$5.00 is not below a $5 floor and must be admitted"
    );

    // Coverage is the only thing keeping SPARSE out: at four of five days it is
    // in, at three it is not.
    let coverage_ok = eligibility_panel("4.99", 4);
    assert_eq!(
        momentum::rebalance_at(&coverage_ok, &config, 2)
            .expect("rebalance forms")
            .eligible,
        vec![0, 2],
        "four of five trading days clears an 80% bar and must be admitted"
    );
}
