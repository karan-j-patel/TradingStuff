//! E2, E3, and E4. Positions, costs, returns, and what a delisting does.
//!
//! These are mandatory-test paths under `CLAUDE.md`: portfolio accounting and
//! cost application. Every expected number below is derived in a comment, and
//! the derivation is the test. If a number here is ever updated to match what
//! the code produced, the test has been deleted and replaced with a transcript.

use super::{asset, dec, month_ends, panel_of, test_config, two_asset_panel};
use crate::baseline;
use crate::momentum;
use crate::run;
use jiff::civil::date;

/// E2. Every position, cost, and return on the two-asset fixture, to the penny.
///
/// Costs are ten basis points per side on traded notional, so an initial
/// purchase moves 1.0 of notional and costs 0.0010, a complete rotation moves
/// 2.0 and costs 0.0020, and a liquidation moves 1.0 and costs 0.0010.
///
/// ```text
///   rebalance m2:  hold {} -> {AAA:1}          traded 1.0   cost 0.0010
///     hold m2->m3: AAA 22/20 - 1 = 0.1
///     net = (1 - 0.0010)(1 + 0.1) - 1 = 0.999 * 1.1 - 1        =  0.0989
///
///   rebalance m3:  hold {AAA:1} -> {AAA:1}     traded 0.0   cost 0
///     hold m3->m4: AAA 11/22 - 1 = -0.5
///     net = (1 - 0)(1 - 0.5) - 1                               = -0.5
///
///   rebalance m4:  hold {AAA:1} -> {BBB:1}     traded 2.0   cost 0.0020
///     hold m4->m5: BBB 10/20 - 1 = -0.5
///     net = (1 - 0.0020)(1 - 0.5) - 1 = 0.998 * 0.5 - 1        = -0.501
///
///   rebalance m5:  hold {BBB:1} -> {}          traded 1.0   cost 0.0010
///     the exit lands on the month just finished:
///     net = (1 - 0.501)(1 - 0.0010) - 1 = 0.499 * 0.999 - 1    = -0.501499
/// ```
#[test]
fn e2_accounting_is_exact_to_the_penny() {
    let panel = two_asset_panel();
    let config = test_config(2);
    let report = run::backtest(&panel, &config).expect("the fixture backtests");

    assert_eq!(report.first_rebalance, date(2020, 3, 31));
    assert_eq!(report.last_rebalance, date(2020, 6, 30));
    assert_eq!(report.months, 3);

    // Positions. AAA is index 0 and BBB is index 1, because the panel sorts on
    // the permaticker and AAA's is the lower one.
    let rebalances: Vec<_> = (2..6)
        .map(|index| momentum::rebalance_at(&panel, &config, index).expect("rebalance"))
        .collect();
    assert_eq!(
        rebalances[0].chosen,
        vec![0],
        "m2 is a tie, broken on identity"
    );
    assert_eq!(rebalances[1].chosen, vec![0]);
    assert_eq!(rebalances[2].chosen, vec![1]);

    assert_eq!(
        report.strategy.net_monthly,
        vec![dec("0.0989"), dec("-0.5"), dec("-0.501499")]
    );
    assert_eq!(
        report.strategy.gross_monthly,
        vec![dec("0.1"), dec("-0.5"), dec("-0.5")]
    );

    // 1.0989 * 0.5 * 0.498501 - 1 = 0.27390137445 - 1
    assert_eq!(report.total_net_return, dec("-0.72609862555"));

    // Peak 1.0989 at the end of month one, trough 0.27390137445 at the end.
    // (1.0989 - 0.27390137445) / 1.0989 = 0.82499862555 / 1.0989
    assert_eq!(report.max_drawdown, dec("0.7507495"));

    // Traded notional 1 + 0 + 2 + 1 = 4 over four rebalances, halved because
    // turnover is quoted one-way.
    assert_eq!(report.mean_one_way_turnover, dec("0.5"));

    // Nothing delisted in this fixture.
    assert_eq!(report.strategy.delisting_exits, 0);
}

/// E3. Zero turnover charges zero, a full rotation charges exactly two sides,
/// and the net-versus-gross gap is the hand-computed drag.
#[test]
fn e3_costs_are_charged_per_side_on_traded_notional() {
    let panel = two_asset_panel();
    let config = test_config(2);
    let report = run::backtest(&panel, &config).expect("the fixture backtests");
    let series = &report.strategy;

    assert_eq!(
        series.traded,
        vec![dec("1"), dec("0"), dec("2"), dec("1")],
        "initial purchase, no change, full rotation, liquidation"
    );

    // Zero turnover charges zero, so the net month equals the gross month
    // exactly rather than approximately.
    assert_eq!(series.traded[1], dec("0"));
    assert_eq!(series.net_monthly[1], series.gross_monthly[1]);

    // A full rotation moves 2.0 of notional, which at ten basis points a side
    // is 0.0020 rather than 0.0010.
    assert_eq!(series.traded[2], dec("2"));

    // Month three pays the rotation and then the liquidation:
    //   gross                              = -0.5
    //   after rotation  (1-0.0020)(0.5)-1  = -0.501
    //   after exit      (0.499)(0.9990)-1  = -0.501499
    //   drag = -0.5 - (-0.501499)          =  0.001499
    let drag = series.gross_monthly[2] - series.net_monthly[2];
    assert_eq!(drag, dec("0.001499"));

    // The first month pays one side only, on 1.0 of notional:
    //   1.1 - 1.0989 = 0.0011, which is 0.0010 * 1.1
    assert_eq!(
        series.gross_monthly[0] - series.net_monthly[0],
        dec("0.0011")
    );
}

/// E4. A name whose bars stop mid-hold is marked at its last close, is sold at
/// the next rebalance, and increments the caveat counter.
///
/// ```text
///          m0     m1     m2     m3   mid-May    m4     m5
///   AAA    10     20     25     50      25       -      -
///   BBB    10     10     10     10       -      10     10
/// ```
///
/// AAA's last bar is 2020-05-15, which is inside the month whose last trading
/// day is m4. It is therefore not a month-end, and the panel's month-ends stay
/// the same six dates.
///
/// ```text
///   rebalance m2:  AAA 20/10-1 = 1.0 beats BBB 0    hold {AAA:1}
///                  traded 1.0, cost 0.0010
///     hold m2->m3: AAA 50/25 - 1 = 1.0
///     net = (1 - 0.0010)(2.0) - 1                              =  0.998
///
///   rebalance m3:  AAA 25/20-1 = 0.25 beats BBB 0   hold {AAA:1}
///                  traded 0.0, cost 0
///     hold m3->m4: AAA has no bar on m4, so it is marked at its last close,
///                  the 2020-05-15 print of 25:  25/50 - 1 = -0.5
///     net = -0.5                                               = -0.5
///
///   rebalance m4:  AAA has no bar on m4 and is ineligible, so BBB is held
///                  traded 2.0 (sell AAA, buy BBB), cost 0.0020
///     hold m4->m5: BBB 10/10 - 1 = 0
///     net = (1 - 0.0020)(1.0) - 1                              = -0.002
///
///   rebalance m5:  liquidation, traded 1.0, cost 0.0010
///     net = (1 - 0.002)(1 - 0.0010) - 1 = 0.998 * 0.999 - 1    = -0.002998
/// ```
#[test]
fn e4_a_delisted_name_exits_at_its_last_close_and_is_counted() {
    let m = month_ends();
    let aaa = asset("AAA", 1);
    let bbb = asset("BBB", 2);
    let panel = panel_of(&[
        (
            aaa,
            vec![
                (m[0], dec("10")),
                (m[1], dec("20")),
                (m[2], dec("25")),
                (m[3], dec("50")),
                (date(2020, 5, 15), dec("25")),
            ],
        ),
        (
            bbb,
            m.iter().map(|day| (*day, dec("10"))).collect::<Vec<_>>(),
        ),
    ]);
    let config = test_config(2);

    assert_eq!(
        panel.month_ends(),
        m,
        "the mid-month bar must not become a month-end"
    );

    let report = run::backtest(&panel, &config).expect("the fixture backtests");

    // Three separate things happen to a delisting, and a review of this code
    // read them as one because the composite assertion below was all that held
    // them. They are asserted apart first, then together.

    // 1. Exit VALUE. The position is marked at the last close, 25, against an
    //    entry of 50, so the month is -0.5. Marking it at the entry price would
    //    make it 0, and marking it at anything else would make it neither.
    assert_eq!(
        report.strategy.gross_monthly[1],
        dec("-0.5"),
        "the exit was not valued at the last close of 25 against an entry of 50"
    );

    // 2. Exit COST. The delisted name keeps its weight until a rebalance can
    //    sell it, so the next rebalance moves 2.0 of notional: 1.0 selling the
    //    dead position and 1.0 buying BBB. A portfolio that dropped the
    //    delisted name from its holdings instead would move only 1.0 here, and
    //    the forced sale would never be charged.
    assert_eq!(
        report.strategy.traded[2],
        dec("2"),
        "the forced sale of the delisted name is missing from traded notional, \
         so its 10 bps exit charge was never applied"
    );

    // 3. The caveat COUNTER, which is what the printed report tells a reader.
    assert_eq!(
        report.strategy.delisting_exits, 1,
        "the caveat counter did not see the exit"
    );

    // The three composed. Month three carries the rotation and the liquidation:
    //   (1 - 0.0020)(1 + 0) - 1 = -0.002, then x (1 - 0.0010) = -0.002998
    assert_eq!(
        report.strategy.net_monthly,
        vec![dec("0.998"), dec("-0.5"), dec("-0.002998")]
    );
}

/// E4b. The buy-and-hold baseline pays to be rid of a delisted name, exactly as
/// the strategy pays to sell one at the next rebalance.
///
/// Both names move together, so the weights stay at one half and every figure
/// terminates. AAA's last print is mid-May, inside the month whose last trading
/// day is m4, so it is not a month-end.
///
/// ```text
///          m2     m3   mid-May    m4     m5
///   AAA    10     20      10       -      -
///   BBB    10     20       -      10     10
///
///   entry at m2, equal weight, traded 1.0, cost 0.0010
///     m2->m3: both 20/10 - 1 = 1.0, gross = 1.0
///             net = (1 - 0.0010)(2.0) - 1                        =  0.998
///             weights stay 0.5 and 0.5, nothing has delisted yet
///
///     m3->m4: AAA has no bar on m4 and is marked at its LAST CLOSE, 10,
///             against the 20 it was bought at, so its return is -0.5.
///             BBB halves too, so the growth divides the weights exactly.
///             gross = 0.5(-0.5) + 0.5(-0.5)                      = -0.5
///             AAA's grown half = 0.5(0.5)/0.5 = 0.5, sold at that mark:
///               exit charge = 0.0010 * 0.5                       =  0.0005
///             net = (1 - 0.0005)(0.5) - 1                        = -0.50025
///
///     m4->m5: BBB flat, gross = 0. Half the portfolio is now cash, which
///             costs nothing to hold and nothing to close.
///             the final exit sells the live half only: 0.0010 * 0.5
///             net = (1 + 0)(1 - 0.0005) - 1                      = -0.0005
///
///   total = 1.998 * 0.49975 * 0.9995 - 1                    = -0.00199875025
///
/// # Why AAA's last print is not its entry price
///
/// It was 20, equal to the m3 close AAA is bought at, and that made this test
/// blind. A blind falsification round marked the delisted holding at `open`
/// instead of at `last_close_on_or_before` and nothing failed, because the two
/// were the same `Decimal`. 10 is load-bearing: it is neither the entry price
/// nor anything else in the fixture, so the middle month separates a mark at
/// the last close (-0.50025) from a mark at the entry (-0.2505).
/// ```
#[test]
fn e4b_buy_and_hold_pays_to_be_rid_of_a_delisted_name() {
    let m = month_ends();
    let panel = panel_of(&[
        (
            asset("AAA", 1),
            vec![
                (m[0], dec("10")),
                (m[1], dec("10")),
                (m[2], dec("10")),
                (m[3], dec("20")),
                (date(2020, 5, 15), dec("10")),
            ],
        ),
        (
            asset("BBB", 2),
            vec![
                (m[0], dec("10")),
                (m[1], dec("10")),
                (m[2], dec("10")),
                (m[3], dec("20")),
                (m[4], dec("10")),
                (m[5], dec("10")),
            ],
        ),
    ]);
    let config = test_config(2);

    let rebalances: Vec<_> = (2..6)
        .map(|index| momentum::rebalance_at(&panel, &config, index).expect("rebalance"))
        .collect();
    assert_eq!(
        rebalances[0].eligible,
        vec![0, 1],
        "buy-and-hold must enter holding both names"
    );

    // The fixture's discriminating property. AAA is bought at 20 on m3 and
    // last prints at 10, so a mark taken from the wrong one of those two
    // prices changes the answer. An edit that makes them equal again fails
    // here rather than silently blinding the assertions below.
    let aaa = &panel.securities()[0];
    assert_eq!(aaa.close_on(m[3]).expect("bar"), dec("20"));
    assert_eq!(
        aaa.last_close_on_or_before(m[4]).expect("bar").1,
        dec("10"),
        "the entry price and the last close must differ, or this test cannot \
         tell a mark at the last close from a mark at the entry"
    );

    let held = baseline::buy_and_hold(&panel, &config, &rebalances, config.weighting)
        .expect("baseline runs");

    // The middle month is the whole point. Without the exit charge it is 0,
    // because a delisting moves value to cash and nothing else happens.
    assert_eq!(
        held.net_monthly[1],
        dec("-0.50025"),
        "the delisted half left the portfolio at a price that is neither its \
         last close nor a mark that paid the 10 bps to be sold"
    );
    assert_eq!(
        held.net_monthly,
        vec![dec("0.998"), dec("-0.50025"), dec("-0.0005")]
    );
    assert_eq!(held.total_net_return, dec("-0.00199875025"));
}
