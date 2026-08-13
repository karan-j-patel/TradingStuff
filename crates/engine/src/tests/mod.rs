//! Fixtures shared by the engine tests, and the tests themselves.
//!
//! Every fixture is synthetic. Vendor licences forbid redistributing rows, and
//! that applies to test files as much as to a data directory.
//!
//! # Why the numbers are what they are
//!
//! Prices are chosen so that every return, weight, and cost is a terminating
//! decimal. A fixture whose arithmetic does not terminate forces the expected
//! values to be produced by running the code, which turns a hand check into a
//! record of whatever the code did on the day it was written.
//!
//! The lead-in is shortened to two months in most fixtures. The accounting
//! under test does not depend on the lookback length, and a twelve-month
//! lead-in would mean fourteen months of hand-computed prices per security to
//! test three months of holding.

mod accounting;
mod baselines;
mod conservative;
mod delistings;
mod dividends;
mod eligibility;
mod hashing;
mod lowvol;
mod screens;
mod turnover;
mod value;
mod weighting;
mod wiring;

use ingest::actions::{
    ActionRecord, CorporateAction, Delisting, DelistingReason, DividendKind, Listing, TerminalValue,
};
use ingest::adjusted::AdjustedBar;
use ingest::schema::{AssetKey, CloseKind, PermanentId, SessionScope};
use jiff::civil::{Date, date};
use rust_decimal::Decimal;

use crate::config::BacktestConfig;
use crate::panel::Panel;

/// Placeholder digests the fixture panels attach under.
///
/// Not real SHA-256 values. The wiring guard compares the configuration's
/// recorded digest against the panel's for equality, so what a fixture needs is
/// for the two to agree, and named constants are what stop them drifting apart
/// silently. `wired()` in the conservative tests records exactly these.
pub const ACTIONS_SHA256: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const DELISTINGS_SHA256: &str =
    "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
pub const MARKETCAP_SHA256: &str =
    "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm";

/// A stable-identity asset key. The permaticker is what the panel sorts on, so
/// the numbers here fix the security order the tests assert against.
pub fn asset(ticker: &str, permaticker: u64) -> AssetKey {
    AssetKey {
        ticker: ticker.to_string(),
        permanent: Some(PermanentId::Sharadar(permaticker)),
    }
}

/// Exact-decimal literal. `from_str_exact` refuses to round, so a literal with
/// more precision than `Decimal` can hold fails the test rather than quietly
/// becoming a different number.
pub fn dec(value: &str) -> Decimal {
    Decimal::from_str_exact(value).expect("test literal is a valid Decimal")
}

/// One bar. Open, high, and low are set to the close because nothing under test
/// reads them, and giving them invented spreads would suggest they mattered.
pub fn bar(asset: &AssetKey, day: Date, close: Decimal, unadjusted: Decimal) -> AdjustedBar {
    AdjustedBar {
        asset: asset.clone(),
        date: day,
        open: close,
        high: close,
        low: close,
        close,
        volume: Decimal::from(1000u64),
        close_unadjusted: unadjusted,
        session: SessionScope::RegularHours,
        close_kind: CloseKind::ClosingAuction,
    }
}

/// Six month-end dates, the last trading day of each month in the first half of
/// 2020. Real calendar dates, so the month-end derivation is exercised rather
/// than bypassed by numbers that are all the 31st.
pub fn month_ends() -> [Date; 6] {
    [
        date(2020, 1, 31),
        date(2020, 2, 28),
        date(2020, 3, 31),
        date(2020, 4, 30),
        date(2020, 5, 29),
        date(2020, 6, 30),
    ]
}

/// Eighteen trading days, three per month, whose last day in each month is
/// exactly the corresponding entry of [`month_ends`].
///
/// Volatility is estimated from daily returns, so a fixture built from
/// month-ends alone would give the estimator one observation per window and no
/// dispersion to measure. Three days a month is the smallest count that leaves
/// a formation window of one month with three daily returns, which is two more
/// than the `n - 1` denominator needs.
pub fn trading_days() -> [Date; 18] {
    [
        date(2020, 1, 29),
        date(2020, 1, 30),
        date(2020, 1, 31),
        date(2020, 2, 26),
        date(2020, 2, 27),
        date(2020, 2, 28),
        date(2020, 3, 27),
        date(2020, 3, 30),
        date(2020, 3, 31),
        date(2020, 4, 28),
        date(2020, 4, 29),
        date(2020, 4, 30),
        date(2020, 5, 27),
        date(2020, 5, 28),
        date(2020, 5, 29),
        date(2020, 6, 26),
        date(2020, 6, 29),
        date(2020, 6, 30),
    ]
}

/// A configuration with a shortened lead-in, and every other constant as the
/// real run uses it.
pub fn test_config(lookback: usize) -> BacktestConfig {
    BacktestConfig {
        signal_lookback_months: lookback,
        ..BacktestConfig::momentum_v0("0".repeat(64))
    }
}

/// The same configuration, ranking on volatility instead.
///
/// Built by overriding [`test_config`] rather than by shortening
/// [`BacktestConfig::lowvol_v0`], so that a test of the selection rule fails
/// only when the selection rule is wrong. Whether `lowvol_v0` itself carries
/// the right strategy is a separate question, and `e9d` is the test that asks
/// it.
pub fn lowvol_config(lookback: usize) -> BacktestConfig {
    BacktestConfig {
        strategy: crate::config::Strategy::LowVolatility,
        ..test_config(lookback)
    }
}

/// Build a panel from one price path per security over [`trading_days`].
///
/// Panics if a path is not eighteen long, because a short path would silently
/// shorten a formation window and change what the test measures.
pub fn daily_panel(series: &[(AssetKey, [&str; 18])]) -> Panel {
    let days = trading_days();
    let paths: Vec<(AssetKey, Vec<(Date, Decimal)>)> = series
        .iter()
        .map(|(key, closes)| {
            (
                key.clone(),
                days.iter()
                    .zip(closes)
                    .map(|(day, close)| (*day, dec(close)))
                    .collect(),
            )
        })
        .collect();
    panel_of(&paths)
}

/// Build a panel from `(asset, [(date, close)])` pairs, with the unadjusted
/// close equal to the adjusted one.
pub fn panel_of(series: &[(AssetKey, Vec<(Date, Decimal)>)]) -> Panel {
    let bars = series
        .iter()
        .flat_map(|(key, points)| {
            points
                .iter()
                .map(move |(day, close)| bar(key, *day, *close, *close))
        })
        .collect();
    Panel::from_bars(bars).expect("fixture panel builds")
}

/// One synthetic cash dividend record.
pub fn cash_dividend(asset: &AssetKey, ex_date: Date, amount: Decimal) -> ActionRecord {
    ActionRecord {
        asset: asset.clone(),
        effective: ex_date,
        action: CorporateAction::Dividend {
            amount,
            kind: DividendKind::Cash,
        },
        source: "synthetic".to_string(),
    }
}

/// Attach cash dividends to a panel a fixture already built.
///
/// A separate step rather than a wider `panel_of`, so every existing fixture
/// keeps its signature and a test that says nothing about dividends is
/// demonstrably running the path that has none.
pub fn with_cash_dividends(panel: Panel, dividends: &[(AssetKey, Date, Decimal)]) -> Panel {
    let records: Vec<ActionRecord> = dividends
        .iter()
        .map(|(asset, ex_date, amount)| cash_dividend(asset, *ex_date, *amount))
        .collect();
    panel
        .with_dividends(&records, ACTIONS_SHA256)
        .expect("fixture dividends attach")
}

/// One synthetic delisting record, in the shape curation produces.
///
/// `terminal` is [`TerminalValue::Unknown`] because this vendor publishes no
/// delisting return on any row, so the engine is the thing that imputes. A
/// fixture that arrived pre-imputed would be testing the fixture.
///
/// `listing` is [`Listing::Other`] for the same reason curation stores it: the
/// actions row carries no exchange, and the engine's convention does not
/// consult one.
pub fn delisting(asset: AssetKey, date: Date, reason: DelistingReason) -> Delisting {
    Delisting {
        asset,
        date,
        reason,
        listing: Listing::Other,
        terminal: TerminalValue::Unknown,
        final_market_cap: None,
        source: "synthetic".to_string(),
    }
}

/// Attach delistings to a panel a fixture already built.
///
/// A separate step rather than a wider `panel_of`, on the rule
/// [`with_cash_dividends`] follows. Every existing fixture keeps its signature,
/// so a test that says nothing about delistings is demonstrably running the
/// path that has none.
pub fn with_delistings(panel: Panel, delistings: &[Delisting]) -> Panel {
    panel
        .with_delistings(delistings, DELISTINGS_SHA256)
        .expect("fixture delistings attach")
}

/// Two securities over the six month-ends.
///
/// ```text
///          m0     m1     m2     m3     m4     m5
///   AAA    10     10     20     22     11     11
///   BBB    10     10     10     20     20     10
/// ```
///
/// With a two-month lookback and one month skipped, the signal at each
/// rebalance spans the two preceding month-ends:
///
/// ```text
///   m2:  AAA 10/10-1 = 0      BBB 10/10-1 = 0     tie, breaks on identity: AAA
///   m3:  AAA 20/10-1 = 1.0    BBB 10/10-1 = 0     AAA
///   m4:  AAA 22/20-1 = 0.1    BBB 20/10-1 = 1.0   BBB
/// ```
pub fn two_asset_panel() -> Panel {
    let m = month_ends();
    let aaa = asset("AAA", 1);
    let bbb = asset("BBB", 2);
    let prices_a = ["10", "10", "20", "22", "11", "11"];
    let prices_b = ["10", "10", "10", "20", "20", "10"];
    panel_of(&[
        (
            aaa,
            m.iter()
                .zip(prices_a)
                .map(|(day, price)| (*day, dec(price)))
                .collect(),
        ),
        (
            bbb,
            m.iter()
                .zip(prices_b)
                .map(|(day, price)| (*day, dec(price)))
                .collect(),
        ),
    ])
}
