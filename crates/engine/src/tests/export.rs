//! X-R1 through X-R5 and X-R7, the engine half of the characteristic panel.
//!
//! X-R6 is the CLI's, because that is where the trial log is opened. X-R8 is
//! ingest's, because that is where the file's metadata is written and read.
//!
//! # What X-R4 is for, and why it is the anchor the rest are built on
//!
//! The export calls the strategies' own functions, so it looks impossible for
//! it to disagree with them. What it can get wrong is the ARGUMENTS: which
//! month-end a window starts at, which one it ends at, and how old a filing may
//! be. Every one of those produces a plausible number in the right units, and
//! nothing about the file would look wrong.
//!
//! So the equality is against `momentum::rebalance_at` and its siblings rather
//! than against a literal. Each of them derives its own windows from its own
//! configuration, so the two agree only when the export spans what the strategy
//! spans. A lookback moved by one month fails it; a value staleness moved fails
//! it; an index arithmetic slip fails it.
//!
//! The four columns no rebalance exposes as a per-name value are pinned by hand
//! instead, and each fixture is built so that the wrong window gives a
//! different number rather than a slightly different one.
//!
//! Every fixture here is synthetic. Vendor licences forbid redistributing rows.

use std::collections::BTreeMap;

use ingest::PanelRow;
use ingest::actions::DelistingReason;
use ingest::marketcap::MarketCapRecord;
use ingest::provider::FundamentalRecord;
use ingest::schema::AssetKey;
use jiff::civil::{Date, date};
use rust_decimal::{Decimal, MathematicalOps};

use super::value::FILINGS_SHA256;
use super::value::{cap, filing};
use super::{
    ACTIONS_SHA256, DELISTINGS_SHA256, MARKETCAP_SHA256, asset, bar, cash_dividend, dec, delisting,
};
use crate::config::{BacktestConfig, DELISTING_CONVENTION};
use crate::error::EngineError;
use crate::export;
use crate::momentum;
use crate::panel::Panel;

/// The month-end the hand-computed pins are taken at.
///
/// Index 37 of forty, which leaves the lead-in of thirty-six behind it and one
/// month-end in front for the forward label. Every window the export writes is
/// entirely inside the fixture at this index, and the fixture puts a
/// discriminating value just outside each one.
const PIN: usize = 37;

/// Forty month-ends, the 28th of each month from 2018-01 to 2021-04.
///
/// Forty because the deepest window is thirty-six months and it has to sit
/// inside the panel with a month-end in front of it for the label and one
/// behind it for the volatility window's opening price.
///
/// The 28th of every month rather than a real last-trading-day calendar,
/// because the month-end derivation is exercised by the panel itself and a
/// fixture that had to encode market holidays would make every index in the
/// comments below unreadable.
fn month_ends() -> Vec<Date> {
    (0..40i16)
        .map(|step| {
            date(
                2018 + step / 12,
                i8::try_from(step % 12).expect("a month number fits") + 1,
                28,
            )
        })
        .collect()
}

/// Everything the fixture knows about one security.
///
/// Built as maps from month index rather than as parallel vectors, so a
/// security that trades on only some of the month-ends is expressed by leaving
/// the others out rather than by a vector of options nobody can read.
struct Security {
    asset: AssetKey,
    /// Adjusted close at each month index the name has a bar on. A month index
    /// absent here is a month the name did not trade at all.
    closes: BTreeMap<usize, Decimal>,
    /// Unadjusted close where it differs from the adjusted one. This is the
    /// column the price floor reads.
    unadjusted: BTreeMap<usize, Decimal>,
    /// Market cap in the vendor's millions, per month index.
    caps: BTreeMap<usize, Decimal>,
    filings: Vec<FundamentalRecord>,
    /// `(month index, amount per share)`, each going ex on that month-end.
    dividends: Vec<(usize, Decimal)>,
}

/// A security trading at one flat price on every month-end, with a flat market
/// cap and nothing else.
///
/// The base every fixture security is a modification of, so each one below
/// states only what makes it different and a reader can see the difference
/// without diffing two price paths.
fn flat(ticker: &str, permaticker: u64, close: &str, marketcap: &str) -> Security {
    let asset = asset(ticker, permaticker);
    Security {
        closes: (0..40).map(|index| (index, dec(close))).collect(),
        unadjusted: BTreeMap::new(),
        caps: (0..40).map(|index| (index, dec(marketcap))).collect(),
        filings: Vec::new(),
        dividends: Vec::new(),
        asset,
    }
}

/// The seven securities and everything attached to them.
///
/// ```text
///   name     what it is there for
///   AAA      the hand-computed pins: a flat path with one value just outside
///            each window, so a window off by one changes the answer
///   DIV      the two payout columns, whose windows the dividends straddle
///   GAP      stops trading inside the panel, and is delisted for performance
///   HALF     halves INTO the pin month, so a label including that month reads
///            -0.5 instead of 0
///   NOFILE   no filing anywhere, so its book-to-market is null with a row
///   PENNY    below the price floor at the pin month only
///   STALE    one filing that ages past the staleness bound between two
///            exported month-ends
/// ```
fn securities() -> Vec<Security> {
    // AAA. Flat at 100, with three values placed to sit just outside a window:
    //   index 0  = 200, one month-end before the thirty-six month volatility
    //              window opens at index 1. Reading one month further back
    //              turns a volatility of exactly zero into a large one.
    //   index 38 = 130, the month AFTER the pin. It is the label's closing
    //              price and must not be inside the volatility window.
    //   index 39 = 500, two months after the pin. Nothing at the pin may see it.
    let mut aaa = flat("AAA", 1, "100", "20");
    aaa.closes.insert(0, dec("200"));
    aaa.closes.insert(38, dec("130"));
    aaa.closes.insert(39, dec("500"));
    // Book 10,000,000 over a market cap of 20 million dollars is 0.5. Published
    // 2020-06-15, which is 258 days before the pin and well inside the bound.
    aaa.filings.push(filing(
        &aaa.asset,
        date(2020, 6, 15),
        date(2020, 3, 31),
        "10000000",
    ));

    // DIV. Flat at 100 so nothing but the payouts moves. The market cap steps
    // up at index 12, which is one month-end before the twenty-four month share
    // window opens at index 13, and again at the pin itself.
    let mut div = flat("DIV", 2, "100", "100");
    div.caps.insert(12, dec("110"));
    div.caps.insert(PIN, dec("110"));
    div.filings.push(filing(
        &div.asset,
        date(2020, 6, 15),
        date(2020, 3, 31),
        "55000000",
    ));
    // The trailing dividend window is (m25, m37]. The first of these goes ex
    // exactly on its opening month-end and belongs to the previous holder; the
    // last goes ex exactly on the pin and belongs to this one. The 4.0 falls
    // after the pin and belongs to the forward label instead.
    div.dividends = vec![
        (25, dec("1.0")),
        (26, dec("0.5")),
        (31, dec("2.0")),
        (PIN, dec("3.0")),
        (38, dec("4.0")),
    ];

    // GAP. Trades through the pin and then stops. Its last bar is the pin's own
    // month-end and the delistings file explains the exit.
    let mut gap = flat("GAP", 4, "100", "20");
    gap.closes.retain(|index, _| *index <= PIN);
    gap.filings.push(filing(
        &gap.asset,
        date(2020, 6, 15),
        date(2020, 3, 31),
        "10000000",
    ));

    // HALF. Two hundred at the month-end before the pin and a hundred from the
    // pin onwards, so the month INTO the pin is a halving. The label at the pin
    // is zero and a label that folded the pin's own month in would be -0.5.
    let mut half = flat("HALF", 7, "100", "20");
    half.closes.insert(36, dec("200"));
    half.filings.push(filing(
        &half.asset,
        date(2020, 6, 15),
        date(2020, 3, 31),
        "10000000",
    ));

    // NOFILE. Everything except a filing.
    let nofile = flat("NOFILE", 3, "100", "20");

    // PENNY. A three dollar share at the pin month only, which is below the
    // five dollar floor. The adjusted close stays at a hundred, so nothing but
    // the eligibility flag can notice.
    let mut penny = flat("PENNY", 5, "100", "20");
    penny.unadjusted.insert(PIN, dec("3"));
    penny.filings.push(filing(
        &penny.asset,
        date(2020, 6, 15),
        date(2020, 3, 31),
        "10000000",
    ));

    // STALE. One filing published 2019-08-01. It is 546 days old at the
    // month-end before the pin, inside the 548 day bound, and 577 days old at
    // the pin, outside it. So its book-to-market exists at one exported
    // month-end and not at the next.
    let mut stale = flat("STALE", 6, "100", "20");
    stale.filings.push(filing(
        &stale.asset,
        date(2019, 8, 1),
        date(2019, 6, 30),
        "10000000",
    ));

    vec![aaa, div, gap, half, nofile, penny, stale]
}

/// The whole fixture as a panel, with all four datasets attached.
fn fixture() -> Panel {
    let m = month_ends();
    let securities = securities();

    let mut bars = Vec::new();
    let mut caps: Vec<MarketCapRecord> = Vec::new();
    let mut filings: Vec<FundamentalRecord> = Vec::new();
    let mut dividends = Vec::new();

    for security in &securities {
        for (index, close) in &security.closes {
            let unadjusted = security.unadjusted.get(index).copied().unwrap_or(*close);
            bars.push(bar(&security.asset, m[*index], *close, unadjusted));
        }
        for (index, marketcap) in &security.caps {
            caps.push(cap(
                &security.asset,
                m[*index],
                &marketcap.normalize().to_string(),
            ));
        }
        filings.extend(security.filings.iter().cloned());
        for (index, amount) in &security.dividends {
            dividends.push(cash_dividend(&security.asset, m[*index], *amount));
        }
    }

    // The one delisting: GAP, performance-related, dated the month-end after
    // its last bar. That is what turns its forward label from a flat mark into
    // the published convention's haircut.
    let gap = asset("GAP", 4);
    let delistings = vec![delisting(gap, m[38], DelistingReason::Bankruptcy)];

    Panel::from_bars(bars)
        .expect("the fixture panel builds")
        .with_dividends(&dividends, ACTIONS_SHA256)
        .expect("fixture dividends attach")
        .with_delistings(&delistings, DELISTINGS_SHA256)
        .expect("fixture delistings attach")
        .with_marketcaps(&caps, MARKETCAP_SHA256)
        .expect("fixture market caps attach")
        .with_filings(&filings, FILINGS_SHA256)
        .expect("fixture filings attach")
}

/// The export configuration, wired to the digests the fixture attaches under.
fn export_config() -> BacktestConfig {
    BacktestConfig {
        actions_sha256: Some(ACTIONS_SHA256.to_string()),
        delisting_convention: Some(DELISTING_CONVENTION.to_string()),
        delistings_sha256: Some(DELISTINGS_SHA256.to_string()),
        marketcap_sha256: Some(MARKETCAP_SHA256.to_string()),
        filings_sha256: Some(FILINGS_SHA256.to_string()),
        ..BacktestConfig::panel_export("0".repeat(64))
    }
}

fn exported() -> Vec<PanelRow> {
    export::characteristics(&fixture(), &export_config()).expect("the fixture exports")
}

/// The one row for a ticker at a month index, or a failure naming what was
/// asked for.
fn row<'a>(rows: &'a [PanelRow], ticker: &str, index: usize) -> &'a PanelRow {
    let month_end = month_ends()[index];
    let found: Vec<&PanelRow> = rows
        .iter()
        .filter(|row| row.asset.ticker == ticker && row.month_end == month_end)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one {ticker} row at {month_end}, found {}",
        found.len()
    );
    found[0]
}

/// Whether a ticker has a row at a month index at all.
fn has_row(rows: &[PanelRow], ticker: &str, index: usize) -> bool {
    let month_end = month_ends()[index];
    rows.iter()
        .any(|row| row.asset.ticker == ticker && row.month_end == month_end)
}

/// The month indices the export writes rows for: the lead-in to the end.
fn exported_indices() -> std::ops::Range<usize> {
    export_config().required_lead_in()..month_ends().len()
}

/// One export column, the program that owns it, and how to read the column off
/// a row.
///
/// A named type rather than an inline tuple so the three anchors read as one
/// list of three things rather than as a signature.
type Anchor = (
    &'static str,
    BacktestConfig,
    fn(&PanelRow) -> Option<Decimal>,
);

/// X-R4. Every characteristic the export writes equals the number the strategy
/// that owns it computed at the same formation.
///
/// Three columns, three programs, every exported month-end, every name the
/// program ranked. The comparison is against `rebalance_at`, which derives its
/// windows from its own configuration, so the two agree only when the export
/// spans what the strategy spans. Nothing here is a literal, deliberately: a
/// literal would pin what the export does today and say nothing about whether
/// the strategies still do the same thing.
///
/// The eligibility flag is checked by containment rather than by equality,
/// because a program's `eligible` set is this flag's three rules AND a
/// computable signal, so it is a subset by construction and equality would be
/// a property of the fixture rather than of the code.
#[test]
fn x_r4_every_characteristic_matches_the_rebalance_that_owns_it() {
    let panel = fixture();
    let rows = export::characteristics(&panel, &export_config()).expect("the fixture exports");
    let universe = "0".repeat(64);

    let programs: [Anchor; 3] = [
        (
            "momentum_12_1",
            BacktestConfig::momentum_v0(&universe),
            |row| row.momentum_12_1,
        ),
        (
            "vol_daily_12m",
            BacktestConfig::lowvol_v0(&universe),
            |row| row.vol_daily_12m,
        ),
        (
            "book_to_market",
            BacktestConfig::value_v0(&universe),
            |row| row.book_to_market,
        ),
    ];

    let mut compared = 0usize;
    for index in exported_indices() {
        for (column, config, read) in &programs {
            let rebalance = momentum::rebalance_at(&panel, config, index)
                .unwrap_or_else(|error| panic!("{column} rebalances at {index}: {error}"));

            for (position, signal) in &rebalance.signals {
                let ticker = &panel.securities()[*position].asset.ticker;
                assert_eq!(
                    read(row(&rows, ticker, index)),
                    Some(*signal),
                    "{column} for {ticker} at month index {index} is not the value the \
                     strategy computed at that formation, so the export has drifted from \
                     what the strategies trade on"
                );
                compared += 1;
            }

            // Every name the strategy was willing to rank cleared the three
            // rules the flag reports, so the flag has to be true for it.
            for position in &rebalance.eligible {
                let ticker = &panel.securities()[*position].asset.ticker;
                assert!(
                    row(&rows, ticker, index).eligible,
                    "{ticker} at month index {index} was in the {column} program's eligible \
                     set and the export marks it ineligible"
                );
            }

            // The other direction, and it is not decoration. The loop above
            // walks the names the strategy ranked, so an export that computed a
            // characteristic the strategy declined to compute would pass it
            // untouched. A staleness bound loosened here, or a `None` given a
            // default, is exactly that shape: more values, none of them wrong
            // where the strategy has one to compare against.
            //
            // Restricted to eligible rows because the export writes a row for
            // every name with a bar while a program ranks only the tradable
            // ones, so an ineligible name legitimately carries a value nothing
            // ranked.
            let ranked: std::collections::BTreeSet<&str> = rebalance
                .signals
                .iter()
                .map(|(position, _)| panel.securities()[*position].asset.ticker.as_str())
                .collect();
            let valued: std::collections::BTreeSet<&str> = rows
                .iter()
                .filter(|row| {
                    row.month_end == month_ends()[index] && row.eligible && read(row).is_some()
                })
                .map(|row| row.asset.ticker.as_str())
                .collect();
            assert_eq!(
                valued, ranked,
                "the {column} column and the {column} program disagree about which eligible \
                 names have a value at month index {index}, so the export is computing a \
                 characteristic on a name the strategy declined to"
            );
        }
    }

    // The fixture has to actually exercise the comparison. A panel that
    // produced no signals at all would pass every assertion above by having
    // nothing to check, which is the way this test would rot silently.
    assert!(
        compared >= 60,
        "the fixture compared only {compared} values, so it is not exercising the \
         cross-section this test claims to cover"
    );
}

/// X-R4, the four columns no rebalance exposes as a per-name value.
///
/// The conservative formula's two payout legs and its monthly volatility reach
/// its `signals` only as an averaged rank, so there is no number to compare
/// against. They are pinned by hand instead, and each fixture puts a
/// discriminating value just outside the window so a window off by one gives a
/// different answer rather than a slightly different one.
///
/// `log_marketcap` is pinned by its inverse. Asserting it equals the log of the
/// market cap would be the same call twice; asserting that raising e to it
/// returns the market cap in the vendor's own units is a different statement,
/// and it is the one that fails if anything multiplies by a million on the way.
#[test]
fn x_r4_the_payout_volatility_and_size_columns_are_pinned_by_value() {
    let rows = exported();

    let aaa = row(&rows, "AAA", PIN);
    // The thirty-six month window opens at index 1. AAA is flat at 100 from
    // there to the pin, so every monthly total return in it is exactly zero.
    // Index 0 carries 200 and index 38 carries 130: reading either would make
    // this a large number instead.
    assert_eq!(
        aaa.vol_monthly_36m,
        Some(Decimal::ZERO),
        "the monthly volatility window reached outside the thirty-six month-ends it names"
    );
    // No dividends at all is a yield of zero rather than a missing value. A
    // name that paid nothing did pay nothing.
    assert_eq!(aaa.dividend_yield_12m, Some(Decimal::ZERO));
    // Market cap and close are both flat, so the share count is flat.
    assert_eq!(aaa.share_change_24m, Some(Decimal::ZERO));
    // e raised to the log is the market cap in the vendor's millions, twenty,
    // and not twenty million. Rounded because the log and the exponential are
    // each iterative to the type's full precision and do not compose exactly.
    let recovered = aaa
        .log_marketcap
        .expect("AAA has a market cap at the pin")
        .checked_exp()
        .expect("the log of twenty exponentiates")
        .round_dp(6);
    assert_eq!(
        recovered,
        dec("20"),
        "the log market cap does not exponentiate back to the vendor's own figure, so \
         either the month is wrong or something converted the units"
    );

    let div = row(&rows, "DIV", PIN);
    // The window is (m25, m37]. Of the four dividends at or before the pin,
    // the 1.0 goes ex exactly on the opening month-end and belongs to the
    // previous holder; 0.5, 2.0 and 3.0 are inside. Over a close of 100 that
    // is 5.5 / 100. A window closed at its start would read 0.065; an eleven
    // month window would read 0.05; a thirteen month one would read 0.065.
    assert_eq!(
        div.dividend_yield_12m,
        Some(dec("0.055")),
        "the trailing dividend window is not the twelve months open at its start"
    );
    // Shares are market cap over close. DIV is 110/100 at the pin and 100/100
    // at each of the twenty-four month-ends before it, so the ratio is
    // 1.1 / 1.0 - 1. Including the pin in its own average, or reaching one
    // month-end further back to index 12 where the cap also steps up, both give
    // 0.0956... instead.
    assert_eq!(
        div.share_change_24m,
        Some(dec("0.1")),
        "the share-change window is not the twenty-four month-ends before the formation"
    );
    // DIV's price never moves, so its daily price volatility is exactly zero
    // while its monthly TOTAL return volatility is not: the dividends are in
    // one measure and not the other. The two columns cannot be the same
    // function under two names.
    assert_eq!(div.vol_daily_12m, Some(Decimal::ZERO));
    assert!(
        div.vol_monthly_36m
            .is_some_and(|value| value > Decimal::ZERO),
        "the monthly volatility of a flat price with four dividends in the window is \
         zero, so it is measuring price returns rather than total returns, got {:?}",
        div.vol_monthly_36m
    );
}

/// X-R1. The label at m reads nothing after m+1's month-end close.
///
/// AAA closes at 130 the month after the pin and at 500 the month after that.
/// The label is 0.30. A window running to m+2 would read 4.00, which is not a
/// number anyone would mistake for a monthly return, and that is the point:
/// the fixture makes the wrong window loud.
#[test]
fn x_r1_the_label_reads_nothing_past_the_next_month_end() {
    let rows = exported();
    let aaa = row(&rows, "AAA", PIN);

    assert_eq!(
        aaa.label_return_1m,
        Some(dec("0.3")),
        "the forward label is not the return to the next month-end alone"
    );
    assert_eq!(aaa.label_delisted_in_window, Some(false));

    // And the final month-end has no label at all, because there is no next
    // month-end to close it. A zero there would be a month of flat performance
    // that never happened.
    let last = month_ends().len() - 1;
    let final_row = row(&rows, "AAA", last);
    assert_eq!(final_row.label_return_1m, None);
    assert_eq!(
        final_row.label_delisted_in_window, None,
        "a label with no window still claimed the name kept trading through it"
    );
}

/// X-R2. The label at m excludes month m's own return.
///
/// HALF is priced at 200 at the month-end before the pin and at 100 from the
/// pin onwards, so the month INTO the pin halved it. The denominator is the
/// close at the pin, which makes the label zero. A label whose window opened a
/// month earlier would read -0.5.
#[test]
fn x_r2_the_label_excludes_the_month_it_is_written_against() {
    let rows = exported();
    let half = row(&rows, "HALF", PIN);

    assert_eq!(
        half.label_return_1m,
        Some(Decimal::ZERO),
        "the forward label folded the formation month's own return into itself, which is \
         the return of a position nobody could have held"
    );
}

/// X-R3. A dividend going ex exactly at m belongs to the previous holder, and
/// one at m+1 is collected.
///
/// DIV pays 3.0 on the pin's own month-end and 4.0 on the next. Its price never
/// moves, so the label is exactly the collected cash over the close: 4 / 100.
/// A window closed at its start would read 0.07 and a label computed on price
/// alone would read 0.
///
/// The same 3.0 IS inside the trailing dividend yield at the pin, which the
/// test above pins. One dividend, two windows, opposite answers: an off-by-one
/// in either direction breaks one of the two.
#[test]
fn x_r3_the_labels_dividend_window_is_open_at_its_start_and_closed_at_its_end() {
    let rows = exported();
    let div = row(&rows, "DIV", PIN);

    assert_eq!(
        div.label_return_1m,
        Some(dec("0.04")),
        "the forward label's dividend window is not (m, m+1]"
    );
}

/// The label applies the delisting convention rather than marking a flat exit.
///
/// GAP's last bar is the pin's own month-end and the delistings file dates its
/// exit at the month-end after. The label marks the last close at the published
/// convention, which is -30 percent, and says the name stopped trading inside
/// the window. Carrying the last close unchanged would read zero, which is the
/// upward bias `run::CAVEATS` names.
#[test]
fn the_label_marks_a_delisted_name_at_the_published_convention() {
    let rows = exported();
    let gap = row(&rows, "GAP", PIN);

    assert_eq!(
        gap.label_return_1m,
        Some(dec("-0.30")),
        "a name delisted for performance inside the label window was not marked at the \
         convention the configuration records"
    );
    assert_eq!(gap.label_delisted_in_window, Some(true));
}

/// X-R7. A characteristic that cannot be computed is null on a row that exists,
/// and a name with no bar at the month-end has no row at all.
///
/// The two halves are the same rule from opposite sides. NOFILE is present and
/// unvalued; GAP is absent rather than present and stale. Substituting a zero
/// for the first would rank a name with no book value in the middle of the
/// field, and emitting a row for the second would put a name nobody could have
/// bought into the cross-section.
#[test]
fn x_r7_an_uncomputable_characteristic_is_null_and_an_absent_name_has_no_row() {
    let rows = exported();

    let nofile = row(&rows, "NOFILE", PIN);
    assert_eq!(
        nofile.book_to_market, None,
        "a name with no filing anywhere was given a book-to-market, which is an \
         imputation the export is not allowed to make"
    );
    // The row exists and its other columns are real, which is what makes the
    // null a null rather than a dropped name.
    assert!(nofile.momentum_12_1.is_some());
    assert!(nofile.log_marketcap.is_some());
    assert!(nofile.eligible);

    // GAP's last bar is the pin's month-end. It has a row there and none after.
    assert!(has_row(&rows, "GAP", PIN));
    for index in (PIN + 1)..month_ends().len() {
        assert!(
            !has_row(&rows, "GAP", index),
            "a name with no bar at month index {index} still got a row, so the panel \
             carries a name nobody could have bought that day"
        );
    }

    // The staleness bound is a null the same way. STALE's one filing is 546
    // days old at the month-end before the pin and 577 at the pin, against a
    // bound of 548, so its book-to-market exists at one and not at the other.
    assert!(
        row(&rows, "STALE", PIN - 1).book_to_market.is_some(),
        "the filing is inside the staleness bound at this month-end and was dropped"
    );
    assert_eq!(
        row(&rows, "STALE", PIN).book_to_market,
        None,
        "a filing past the staleness bound was still valued, so the bound the \
         configuration records is not the one being applied"
    );

    // And the eligibility flag is a measurement rather than a constant. PENNY
    // trades at three dollars at the pin and at a hundred either side.
    assert!(!row(&rows, "PENNY", PIN).eligible);
    assert!(row(&rows, "PENNY", PIN - 1).eligible);
    assert!(row(&rows, "PENNY", PIN + 1).eligible);
}

/// X-R5, first half. An export with a dataset the panel never got refuses.
///
/// The configuration and the panel AGREE in this state, both saying no filings,
/// so `check_wiring` passes it: it is a perfectly legitimate momentum backtest.
/// What it is not is an export, because the book-to-market column would be null
/// from the first row to the last and nothing downstream could tell that apart
/// from a universe where no company had ever filed.
#[test]
fn x_r5_an_export_missing_a_dataset_refuses_rather_than_writing_a_null_column() {
    let panel = fixture();
    let m = month_ends();
    let securities = securities();

    // The same panel with three of the four attachments, built from the same
    // bars so nothing but the missing dataset differs.
    let mut bars = Vec::new();
    let mut caps = Vec::new();
    let mut dividends = Vec::new();
    for security in &securities {
        for (index, close) in &security.closes {
            let unadjusted = security.unadjusted.get(index).copied().unwrap_or(*close);
            bars.push(bar(&security.asset, m[*index], *close, unadjusted));
        }
        for (index, marketcap) in &security.caps {
            caps.push(cap(
                &security.asset,
                m[*index],
                &marketcap.normalize().to_string(),
            ));
        }
        for (index, amount) in &security.dividends {
            dividends.push(cash_dividend(&security.asset, m[*index], *amount));
        }
    }
    let gap = asset("GAP", 4);
    let unfiled = Panel::from_bars(bars)
        .expect("the fixture panel builds")
        .with_dividends(&dividends, ACTIONS_SHA256)
        .expect("fixture dividends attach")
        .with_delistings(
            &[delisting(gap, m[38], DelistingReason::Bankruptcy)],
            DELISTINGS_SHA256,
        )
        .expect("fixture delistings attach")
        .with_marketcaps(&caps, MARKETCAP_SHA256)
        .expect("fixture market caps attach");

    let config = BacktestConfig {
        filings_sha256: None,
        ..export_config()
    };
    // The state really does get past the wiring guard, which is the whole
    // reason the export needs a check of its own. Asserted rather than assumed.
    assert!(
        crate::run::check_wiring(&unfiled, &config).is_ok(),
        "the fixture no longer reaches the guard under test"
    );

    let error = export::characteristics(&unfiled, &config)
        .expect_err("an export with no filings attached must refuse");
    assert!(
        matches!(
            error,
            EngineError::ExportMissingInputs { filings: false, .. }
        ),
        "got {error:?}"
    );

    // And the fully wired panel still exports, so the guard is refusing the
    // missing dataset rather than refusing everything.
    assert!(export::characteristics(&panel, &export_config()).is_ok());
}

/// X-R5, second half. An export whose recorded digest is not the digest of the
/// data it read refuses.
///
/// This is the failure the export inherits from the backtest path and the
/// reason it goes through `check_wiring` rather than only through its own
/// guard. The digests travel into the written file's provenance metadata, so an
/// artifact produced in this state would name a dataset it never opened, and
/// the metadata is the only description of the input the file keeps.
#[test]
fn x_r5_an_export_whose_recorded_digest_is_not_the_panels_refuses() {
    let panel = fixture();
    let config = BacktestConfig {
        filings_sha256: Some("1".repeat(64)),
        ..export_config()
    };

    let error =
        export::characteristics(&panel, &config).expect_err("a digest disagreement must refuse");
    assert!(
        matches!(
            error,
            EngineError::DatasetDigestMismatch {
                dataset: "filings",
                ..
            }
        ),
        "got {error:?}"
    );
}

/// The export writes rows from the ELIGIBILITY lead-in onwards, not from the
/// deepest window.
///
/// The two differ by twenty-four month-ends on the shipped configuration, which
/// is two years of rows in which eight of the eleven columns are computable.
/// Starting at the deeper window would discard them, and on the real panel that
/// silently truncates the first training window of the fit that consumes this
/// file.
#[test]
fn the_export_starts_at_the_eligibility_lead_in_not_the_deepest_window() {
    let rows = exported();
    let m = month_ends();
    let config = export_config();
    assert_eq!(
        config.signal_lookback_months, 12,
        "the eligibility lead-in moved"
    );
    assert_eq!(
        config.required_lead_in(),
        36,
        "the deepest window moved, so this test no longer distinguishes the two"
    );

    let earliest = rows
        .iter()
        .map(|row| row.month_end)
        .min()
        .expect("the fixture exports rows");
    assert_eq!(
        earliest, m[config.signal_lookback_months],
        "the export starts at the deepest window rather than the eligibility one, \
         discarding rows whose characteristics are computable"
    );
    assert_eq!(
        rows.iter().map(|row| row.month_end).max(),
        Some(m[m.len() - 1])
    );
}

/// A row in the partial region carries the columns whose windows fit and nulls
/// the ones that do not.
///
/// # Why the boundaries rather than one row in the middle
///
/// A test asserting only that index 12 has a null volatility would pass against
/// an export that never computed the column at all. Each window is pinned at
/// the last index where it does NOT fit and the first index where it does, so
/// the assertion is about where the boundary is rather than about a null
/// existing somewhere.
///
/// The columns that are present matter as much as the ones that are absent.
/// This is the region the old uniform start threw away, and the reason to keep
/// it is that momentum, eligibility and the label are all real here.
#[test]
fn a_partial_row_carries_the_windows_that_fit_and_nulls_the_rest() {
    let rows = exported();
    let config = export_config();
    let volatility = config
        .volatility_lookback_months
        .expect("the export configuration sets it");
    let average = config
        .payout_share_average_months
        .expect("the export configuration sets it");

    // The first exported month-end. Momentum's window fits exactly here, which
    // is what makes this the first row that can exist at all.
    let first = row(&rows, "AAA", config.signal_lookback_months);
    assert!(
        first.momentum_12_1.is_some(),
        "the first exported row has no momentum, so the lead-in and the window \
         it is taken from disagree"
    );
    assert!(first.vol_daily_12m.is_some());
    assert!(first.median_dollar_volume_12m.is_some());
    assert!(first.dividend_yield_12m.is_some());
    assert!(first.log_marketcap.is_some());
    assert!(first.label_return_1m.is_some());
    assert!(first.eligible, "the eligibility flag is computable here");
    // Null here for a different reason from the two below, and the difference
    // is worth pinning now that the export reaches back this far. AAA's filing
    // was published 2020-06-15 and this row is dated January 2019, so there is
    // nothing to value it on YET. That is the point-in-time rule rather than a
    // window that does not fit, and extending the export backwards must not
    // have quietly made an unpublished filing visible.
    assert_eq!(
        first.book_to_market, None,
        "a filing published in June 2020 was visible at a January 2019 formation"
    );
    let published = 29;
    assert_eq!(
        row(&rows, "AAA", published - 1).book_to_market,
        None,
        "the filing became visible a month before it was published"
    );
    assert!(
        row(&rows, "AAA", published).book_to_market.is_some(),
        "the filing is published and unstale at this month-end and was not read"
    );
    assert_eq!(
        first.vol_monthly_36m, None,
        "a thirty-six month volatility was produced twelve month-ends into the panel, \
         so it is a partial window wearing a full window's name"
    );
    assert_eq!(first.share_change_24m, None);

    // Each window at the last index it does not fit, and the first it does.
    assert_eq!(row(&rows, "AAA", average - 1).share_change_24m, None);
    assert!(
        row(&rows, "AAA", average).share_change_24m.is_some(),
        "the share-change window does not fit at exactly its own length"
    );
    assert_eq!(row(&rows, "AAA", volatility - 1).vol_monthly_36m, None);
    assert!(
        row(&rows, "AAA", volatility).vol_monthly_36m.is_some(),
        "the volatility window does not fit at exactly its own length"
    );
}

/// A configuration with a window unset is refused rather than defaulted.
///
/// Reachable only by a caller building its own configuration, since
/// `BacktestConfig::panel_export` sets all four. A default invented here would
/// write a column headed with one window length computed over another.
#[test]
fn a_missing_window_is_refused_rather_than_defaulted() {
    let panel = fixture();
    for (field, config) in [
        (
            "volatility_lookback_months",
            BacktestConfig {
                volatility_lookback_months: None,
                ..export_config()
            },
        ),
        (
            "payout_share_average_months",
            BacktestConfig {
                payout_share_average_months: None,
                ..export_config()
            },
        ),
        (
            "payout_dividend_trailing_months",
            BacktestConfig {
                payout_dividend_trailing_months: None,
                ..export_config()
            },
        ),
    ] {
        let error =
            export::characteristics(&panel, &config).expect_err("a missing window must refuse");
        assert!(
            matches!(error, EngineError::ExportWindowMissing { field: named } if named == field),
            "got {error:?} for {field}"
        );
    }

    let error = export::characteristics(
        &panel,
        &BacktestConfig {
            book_staleness_days: None,
            ..export_config()
        },
    )
    .expect_err("a missing staleness bound must refuse");
    assert!(
        matches!(error, EngineError::ValueStalenessMissing),
        "got {error:?}"
    );
}

/// A signal window that ends at or before it starts is refused at BOTH doors.
///
/// # What the unguarded code did, measured
///
/// A skip past the lead-in underflowed the month-end index and PANICKED, in
/// `momentum::rebalance_at` on the backtest path and in the export's own loop.
///
/// A skip inside the lead-in did not panic, and that is the worse case. The
/// window reversed, `momentum::signal` returned a backward return, and a whole
/// backtest ran to a Sharpe of -6.93 on the two-asset fixture with nothing
/// failing. Neither door had any validation of the relation at all.
///
/// Equality is refused alongside, because a skip equal to the lookback is a
/// window of zero length rather than a short one.
#[test]
fn a_signal_window_that_ends_before_it_starts_is_refused_at_both_doors() {
    let panel = fixture();

    for (lookback, skip) in [(12usize, 12usize), (12, 13), (1, 2)] {
        let config = BacktestConfig {
            signal_lookback_months: lookback,
            signal_skip_months: skip,
            ..export_config()
        };

        let error = export::characteristics(&panel, &config)
            .expect_err("an inverted signal window must be refused by the export");
        assert!(
            matches!(
                error,
                EngineError::SignalWindowInverted {
                    lookback_months,
                    skip_months,
                } if lookback_months == lookback && skip_months == skip
            ),
            "the export accepted lookback {lookback} skip {skip}, got {error:?}"
        );

        // The same configuration through the backtest's own door. One relation,
        // two entry points, and a guard on only one of them would leave the
        // other panicking on the identical input.
        let error = crate::run::schedule(&panel, &config)
            .expect_err("an inverted signal window must be refused by the schedule");
        assert!(
            matches!(error, EngineError::SignalWindowInverted { .. }),
            "the backtest path accepted lookback {lookback} skip {skip}, got {error:?}"
        );
    }

    // And the shipped relation is still accepted, so the guard is refusing the
    // inversion rather than refusing everything.
    let shipped = export_config();
    assert!(shipped.signal_skip_months < shipped.signal_lookback_months);
    assert!(export::characteristics(&panel, &shipped).is_ok());
    assert!(crate::run::schedule(&panel, &shipped).is_ok());
}

/// The export configuration is not reachable through the backtest door.
///
/// Everything in the registry records a trial when it runs. This configuration
/// ranks nothing and holds nothing, so a trial under it would be a hypothesis
/// nobody put forward, and `for_program` is what would let one happen.
#[test]
fn the_export_configuration_is_not_a_runnable_program() {
    let universe = "0".repeat(64);
    let export = BacktestConfig::panel_export(&universe);
    let export_hash = export.config_hash().expect("hashes");

    for (program, variant) in crate::config::RUNNABLE {
        let runnable = BacktestConfig::for_program(program, *variant, &universe)
            .expect("every registered pair resolves");
        assert_ne!(
            runnable.config_hash().expect("hashes").as_str(),
            export_hash.as_str(),
            "the export configuration is reachable as --program {program}, so exporting \
             the panel could record a trial"
        );
    }
}
