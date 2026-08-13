//! E8. Cash dividends reaching the return, and the boundaries around them.
//!
//! These are mandatory-test paths under `CLAUDE.md`: this is cost and return
//! application, and a dividend silently missing from one of the two return
//! loops is exactly the class of bug that leaves every test green and every
//! number wrong. Each expected figure below is derived in its comment, and the
//! derivation is the test. A number updated to match what the code produced
//! would turn this file into a transcript of a past run.
//!
//! # The two loops
//!
//! `portfolio::advance` carries the momentum strategy and every random draw.
//! `baseline::buy_and_hold` has its own inline copy of the same arithmetic. The
//! duplication is deliberate and predates this file, so the dividend tests come
//! in pairs: one through each loop, on the same fixture, with the shared helper
//! underneath both.

use super::{asset, bar, dec, month_ends, panel_of, test_config, two_asset_panel};
use super::{cash_dividend, with_cash_dividends};
use crate::config::BacktestConfig;
use crate::panel::Panel;
use crate::portfolio::{self, Weights};
use crate::run;
use jiff::civil::date;

/// E8. One cash dividend, through the strategy loop, to the penny.
///
/// The dividend goes ex on 2020-04-15, inside the first holding month, and
/// belongs to AAA which is the name held over that month.
///
/// ```text
///   m2 = 2020-03-31   m3 = 2020-04-30
///   AAA close 20 at m2, 22 at m3, and 0.44 of cash going ex on 04-15
///
///   hold m2->m3:  (22 + 0.44) / 20 - 1 = 22.44 / 20 - 1        =  0.122
///     traded 1.0 into the position, cost 0.0010
///     net = (1 - 0.0010)(1.122) - 1 = 1.120878 - 1             =  0.120878
///
///   the remaining two months carry no dividend and are unchanged from E2:
///     m3->m4:  22 -> 11, no trading                            = -0.5
///     m4->m5:  BBB 20 -> 10, rotation costs 0.0020             = -0.501
///              then the final liquidation lands on it          = -0.501499
/// ```
///
/// Without the dividend the first month is 0.0989, which is what E2 asserts on
/// the same fixture. The two tests differ by one 0.44 distribution and nothing
/// else.
#[test]
fn e8_a_cash_dividend_reaches_the_holding_return_to_the_penny() {
    let aaa = asset("AAA", 1);
    let panel = with_cash_dividends(two_asset_panel(), &[(aaa, date(2020, 4, 15), dec("0.44"))]);
    let config = with_actions(test_config(2));

    let report = run::backtest(&panel, &config).expect("the fixture backtests");

    assert_eq!(
        report.strategy.gross_monthly,
        vec![dec("0.122"), dec("-0.5"), dec("-0.5")],
        "the dividend is missing from the gross monthly return"
    );
    assert_eq!(
        report.strategy.net_monthly,
        vec![dec("0.120878"), dec("-0.5"), dec("-0.501499")]
    );
}

/// E8b. The same dividend, through the buy-and-hold loop.
///
/// This is the test that dies if dividends are wired into `advance` alone.
/// Buy-and-hold does not call `advance`; it has its own copy of the return
/// arithmetic, and the two have to be changed together.
///
/// ```text
///   entry at m2, equal weight over the eligible universe {AAA, BBB}
///   traded 1.0, cost 0.0010
///
///   m2->m3:  AAA (22 + 0.44) / 20 - 1                          =  0.122
///            BBB       20 / 10 - 1                             =  1.0
///            gross = 0.5(0.122) + 0.5(1.0) = 0.061 + 0.5       =  0.561
///            net = (1 - 0.0010)(1.561) - 1 = 1.559439 - 1      =  0.559439
/// ```
///
/// Without the dividend the month is 0.54845, from a gross of 0.55.
#[test]
fn e8b_buy_and_hold_receives_the_identical_dividend() {
    let aaa = asset("AAA", 1);
    let panel = with_cash_dividends(two_asset_panel(), &[(aaa, date(2020, 4, 15), dec("0.44"))]);
    let config = with_actions(test_config(2));

    let report = run::backtest(&panel, &config).expect("the fixture backtests");

    assert_eq!(
        report.buy_and_hold.net_monthly[0],
        dec("0.559439"),
        "the buy-and-hold baseline did not collect the dividend the strategy did"
    );
}

/// E8c. The ex-date window is open at `from` and closed at `to`.
///
/// Three dividends with distinct amounts, so every off-by-one produces a
/// different sum and none of them can pass by luck.
///
/// ```text
///   1 going ex exactly on `from`   excluded, the close of `from` already gave it up
///   2 going ex inside the window   included
///   4 going ex exactly on `to`     included, its drop is inside the return measured
///
///   expected 2 + 4 = 6
///     including `from` as well would give 7
///     excluding `to`            would give 2
///     excluding both            would give 2, and the interior-only case is
///                               separated from it by the `to` assertion below
/// ```
#[test]
fn e8c_the_ex_date_boundary_is_open_at_from_and_closed_at_to() {
    let m = month_ends();
    let (from, to) = (m[2], m[3]);
    let one = asset("ONE", 1);
    let points: Vec<_> = m.iter().map(|day| (*day, dec("10"))).collect();

    let panel = with_cash_dividends(
        panel_of(&[(one.clone(), points)]),
        &[
            (one.clone(), from, dec("1")),
            (one.clone(), date(2020, 4, 15), dec("2")),
            (one, to, dec("4")),
        ],
    );
    let series = &panel.securities()[0];

    assert_eq!(
        series.dividend_cash_in(from, to).expect("sums"),
        dec("6"),
        "the boundary is wrong in one direction or the other"
    );

    // The two ends, each isolated, so a single assertion cannot be satisfied by
    // two compensating mistakes.
    assert_eq!(
        series
            .dividend_cash_in(from, date(2020, 4, 15))
            .expect("sums"),
        dec("2"),
        "a dividend going ex on `from` was collected by the wrong holder"
    );
    assert_eq!(
        series
            .dividend_cash_in(date(2020, 4, 15), to)
            .expect("sums"),
        dec("4"),
        "a dividend going ex exactly on `to` was not collected"
    );
}

/// E8d. The shipped amount is added as it arrives, not scaled onto the close.
///
/// Branch B, settled by probe on 2026-08-10. The vendor restates amounts onto
/// the adjusted-close basis, so the figure against a pre-split date is already
/// split-adjusted.
///
/// The fixture makes the two candidate formulas differ by a factor of two: the
/// bar's close is 20 against an unadjusted close of 40, which is what a
/// security looks like after a 2-for-1 split. So
///
/// ```text
///   amount as it arrives                     0.6
///   amount scaled by close / close_unadjusted  0.6 * (20 / 40) = 0.3
/// ```
///
/// The factor is asserted too. Without it a later edit could quietly set the
/// unadjusted close equal to the close, and this test would pass under both
/// formulas while looking exactly as it does now.
#[test]
fn e8d_the_amount_is_added_unscaled() {
    let m = month_ends();
    let split = asset("SPLIT", 1);
    let bars = m
        .iter()
        .map(|day| bar(&split, *day, dec("20"), dec("40")))
        .collect();
    let panel = with_cash_dividends(
        Panel::from_bars(bars).expect("fixture panel builds"),
        &[(split, date(2020, 4, 15), dec("0.6"))],
    );
    let series = &panel.securities()[0];

    assert_eq!(
        series.close_on(m[3]).expect("bar"),
        dec("20"),
        "the fixture stopped discriminating between the two formulas"
    );
    assert_eq!(
        series.close_unadjusted_on(m[3]).expect("bar"),
        dec("40"),
        "the fixture stopped discriminating between the two formulas"
    );

    assert_eq!(
        series.dividend_cash_in(m[2], m[3]).expect("sums"),
        dec("0.6"),
        "the amount was scaled onto the close basis, which double-adjusts a \
         figure the vendor has already adjusted"
    );
}

/// E8e. A name that stops trading still collects the cash it went ex for.
///
/// The delisting fallback marks the holding at its last available close. That
/// path is separate from the live one, so it needs its own assertion.
///
/// ```text
///   GONE trades 2020-03-31 at 20 and 2020-04-15 at 18, then stops
///   0.5 of cash goes ex on 2020-04-10, before the last bar
///
///   held 03-31 -> 04-30, with no bar on 04-30:
///     marked at the last close on or before 04-30, which is 18
///     (18 + 0.5) / 20 - 1 = 18.5 / 20 - 1                      = -0.075
/// ```
///
/// Without the dividend the month is -0.1.
#[test]
fn e8e_a_delisted_name_still_collects_its_final_dividend() {
    let gone = asset("GONE", 1);
    let bars = vec![
        bar(&gone, date(2020, 3, 31), dec("20"), dec("20")),
        bar(&gone, date(2020, 4, 15), dec("18"), dec("18")),
    ];
    let panel = with_cash_dividends(
        Panel::from_bars(bars).expect("fixture panel builds"),
        &[(gone, date(2020, 4, 10), dec("0.5"))],
    );

    let held: Weights = [(0usize, dec("1"))].into_iter().collect();
    let advance = portfolio::advance(&panel, &held, date(2020, 3, 31), date(2020, 4, 30))
        .expect("the holding advances");

    assert_eq!(
        advance.exits,
        vec![0],
        "the fixture stopped exercising the delisting fallback"
    );
    assert_eq!(advance.gross_return, dec("-0.075"));
}

/// E8f. The engine refuses a run whose labelling would be wrong.
///
/// The configuration records whether an actions file was supplied and the panel
/// records whether dividends were attached. If those disagree, the run either
/// reports dividend-inclusive returns it never computed or computes them under
/// a hash that does not say so. Both are silently mislabelled numbers, so both
/// are errors at the boundary rather than results.
#[test]
fn e8f_the_engine_refuses_mismatched_dividend_wiring() {
    let config_says_yes = with_actions(test_config(2));
    let config_says_no = test_config(2);

    let bare = two_asset_panel();
    let attached = with_cash_dividends(two_asset_panel(), &[]);
    assert!(
        attached.dividends_attached(),
        "an attachment that matched nothing still read an actions file"
    );

    let claimed = run::backtest(&bare, &config_says_yes);
    assert!(
        matches!(
            claimed,
            Err(crate::error::EngineError::DividendWiringMismatch { .. })
        ),
        "a configuration naming an actions file ran against a panel with none, got {claimed:?}"
    );

    let unclaimed = run::backtest(&attached, &config_says_no);
    assert!(
        matches!(
            unclaimed,
            Err(crate::error::EngineError::DividendWiringMismatch { .. })
        ),
        "a panel carrying dividends ran under a configuration that does not record them, \
         got {unclaimed:?}"
    );
}

/// A record naming a security the panel does not hold is counted, not dropped.
///
/// An actions file covers the vendor's whole master while a universe file is a
/// sample of it, so overhang is expected rather than exceptional. What is not
/// acceptable is nobody being able to tell how much of the file applied.
#[test]
fn unmatched_and_non_cash_records_are_counted_rather_than_dropped() {
    let aaa = asset("AAA", 1);
    let absent = asset("NOPE", 99);
    let records = vec![
        cash_dividend(&aaa, date(2020, 4, 15), dec("0.44")),
        cash_dividend(&absent, date(2020, 4, 15), dec("1")),
        ingest::actions::ActionRecord {
            asset: aaa,
            effective: date(2020, 4, 15),
            action: ingest::actions::CorporateAction::Split { ratio: dec("2") },
            source: "synthetic".to_string(),
        },
    ];

    let panel = two_asset_panel()
        .with_dividends(&records, super::ACTIONS_SHA256)
        .expect("attaches");

    assert_eq!(panel.unmatched_dividends(), 1);
    assert_eq!(panel.non_cash_actions(), 1);
    assert_eq!(
        panel.securities()[0]
            .dividend_cash_in(date(2020, 3, 31), date(2020, 4, 30))
            .expect("sums"),
        dec("0.44"),
        "the matched record did not reach the security it names"
    );
}

/// A dividend that matches a held series on ticker text alone is not attached.
///
/// Tickers are reused and renamed; the permanent identifier is the identity,
/// which is why [`identity`] prefers it and falls back to ticker only when a
/// provider supplies none. The record here shares the panel's `AAA` ticker but
/// carries a permanent id the panel does not hold, so it must land in the
/// unmatched census, not in `AAA`'s returns. This is the one fixture shape
/// that separates identity keying from ticker keying: every other dividend
/// fixture is consistent in both, so both keyings pass them.
#[test]
fn a_dividend_matching_on_ticker_alone_is_not_attached() {
    let stranger_with_familiar_ticker = asset("AAA", 42);
    let records = vec![cash_dividend(
        &stranger_with_familiar_ticker,
        date(2020, 4, 15),
        dec("0.44"),
    )];

    let panel = two_asset_panel()
        .with_dividends(&records, super::ACTIONS_SHA256)
        .expect("attaches");

    assert_eq!(panel.unmatched_dividends(), 1);
    assert_eq!(
        panel.securities()[0]
            .dividend_cash_in(date(2020, 3, 31), date(2020, 4, 30))
            .expect("sums"),
        dec("0"),
        "a ticker-text match must not move cash onto a different security"
    );
}

/// A non-positive amount is a vendor-data fault, not a zero-value dividend.
#[test]
fn a_non_positive_dividend_amount_is_refused() {
    let aaa = asset("AAA", 1);
    let records = vec![cash_dividend(&aaa, date(2020, 4, 15), dec("0"))];

    assert!(matches!(
        two_asset_panel().with_dividends(&records, super::ACTIONS_SHA256),
        Err(crate::error::EngineError::NonPositiveDividend { .. })
    ));
}

/// E8h, engine half. The caveat block follows the dividend flag.
///
/// The CLI half, that the printer selects on the flag rather than ignoring it,
/// lives beside the printer in `crates/cli/src/backtest/tests.rs`.
///
/// The DOWN bias exists only while cash dividends are missing. Publishing it
/// beside dividend-inclusive returns would describe a bias that is not there,
/// and worse, the closing line would keep calling the net direction unknown
/// when it is known and it is UP.
#[test]
fn e8h_the_caveat_block_follows_the_dividend_flag() {
    // The delisting axis is held off in both, so this test still measures only
    // what the dividend flag changes. E11h is the other axis.
    let without = run::caveats(false, false);
    let with = run::caveats(true, false);

    assert!(
        without.contains("DOWN") && without.contains("UP"),
        "the no-dividend block must still name both biases"
    );
    assert!(
        without.contains("UNKNOWN"),
        "without dividends the net direction genuinely is unknown"
    );

    assert!(
        !with.contains("DOWN"),
        "the dividend-inclusive block still claims a downward bias that has been removed"
    );
    assert!(
        with.contains("UP"),
        "the delisting bias survives this round and must still be stated"
    );
    assert!(
        !with.contains("UNKNOWN"),
        "with one bias removed the remaining direction is known, and saying otherwise \
         understates what the reader can conclude"
    );
    assert!(
        with.contains("must not be described as one"),
        "both blocks must refuse the word conservative"
    );

    // The report carries the flag the printer selects on, and it comes from the
    // panel rather than from anything a caller could set independently.
    let panel = with_cash_dividends(two_asset_panel(), &[]);
    let report = run::backtest(&panel, &with_actions(test_config(2))).expect("backtests");
    assert!(report.dividends_applied);

    let bare = run::backtest(&two_asset_panel(), &test_config(2)).expect("backtests");
    assert!(!bare.dividends_applied);
}

/// A configuration naming an actions file, for the fixtures that need the two
/// halves of the wiring to agree.
fn with_actions(config: BacktestConfig) -> BacktestConfig {
    BacktestConfig {
        actions_sha256: Some(super::ACTIONS_SHA256.to_string()),
        ..config
    }
}
