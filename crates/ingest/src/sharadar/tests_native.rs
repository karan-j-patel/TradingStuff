//! Offline tests for the native `api.sharadar.com` door.
//!
//! **Every fixture is fabricated**, in the envelope shape observed live on
//! 2026-08-09. The shape is documentation; the rows are invented.
//!
//! The fake transport here is keyed by table name on purpose. This host answers
//! a wrong-case table with `{"count":0,"data":[]}` and HTTP 200 rather than an
//! error, so a fake that ignored the requested table would make a miscased
//! constant undetectable offline, which is the one hazard these tests exist to
//! catch.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::str::FromStr;

use jiff::civil::Date;
use rust_decimal::Decimal;

use super::client::SharadarClient;
use super::columns::{datatables, native, native_tables};
use super::http::{Http, HttpFailure, HttpReply};
use crate::provider::{AdjustedPriceSource, SourceError};
use crate::schema::{AssetKey, PermanentId};

const TEST_KEY: &str = "synthetic-not-a-real-credential-a1b2c3";
const BASE: &str = "https://fixture.invalid/v1.0/data";

/// What this host sends for a table it does not recognise.
const EMPTY: &str = r#"{"count":0,"data":[]}"#;

// The names as they appear on the wire, written out rather than taken from
// `super::columns`. These fixtures exist to pin those constants, so writing
// them in terms of the constants would make the pinning circular: miscasing a
// constant would move the request and the fixture together and every test
// would still pass. That is not hypothetical, it is what the first attempt at
// the F5 mutation did.
const WIRE_STOCKS: &str = "stocks";
const WIRE_TICKERS: &str = "tickers";
const WIRE_FUNDAMENTALS: &str = "fundamentals";
const WIRE_ACTIONS: &str = "actions";
const WIRE_DAILY: &str = "daily";

// --- a transport that answers by table --------------------------------------

#[derive(Default)]
struct Fake {
    bodies: RefCell<HashMap<String, VecDeque<String>>>,
    failures: RefCell<VecDeque<String>>,
    seen: RefCell<Vec<String>>,
}

impl Fake {
    fn serving(table: &str, pages: &[String]) -> Rc<Self> {
        let mut bodies = HashMap::new();
        bodies.insert(table.to_string(), pages.iter().cloned().collect());
        Rc::new(Fake {
            bodies: RefCell::new(bodies),
            ..Default::default()
        })
    }

    fn failing(message: &str) -> Rc<Self> {
        Rc::new(Fake {
            failures: RefCell::new(std::iter::repeat_n(message.to_string(), 8).collect()),
            ..Default::default()
        })
    }

    fn calls(&self) -> usize {
        self.seen.borrow().len()
    }
}

/// The table is the last path segment before the query string.
fn table_of(url: &str) -> String {
    url.split('?')
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

impl Http for Rc<Fake> {
    fn get(&self, url: &str) -> Result<HttpReply, HttpFailure> {
        self.seen.borrow_mut().push(url.to_string());

        if let Some(message) = self.failures.borrow_mut().pop_front() {
            return Err(HttpFailure(message));
        }

        // A table with no queued body gets the vendor's empty answer, which is
        // exactly what a miscased name would receive.
        let body = self
            .bodies
            .borrow_mut()
            .get_mut(&table_of(url))
            .and_then(|queue| queue.pop_front())
            .unwrap_or_else(|| EMPTY.to_string());

        Ok(HttpReply { status: 200, body })
    }
}

fn client(fake: &Rc<Fake>) -> SharadarClient {
    SharadarClient::with_transport(Box::new(Rc::clone(fake)), TEST_KEY, BASE)
}

// --- fixture construction ---------------------------------------------------

fn envelope(rows: &[String]) -> String {
    format!(r#"{{"count":{},"data":[{}]}}"#, rows.len(), rows.join(","))
}

/// One fabricated price row, numbers bare as the host actually sends them.
fn price_row(date: &str, close: &str, close_unadjusted: &str) -> String {
    format!(
        r#"{{"ticker":"ZZTOP","date":"{date}","open":10.25,"high":10.9,"low":9.9,"close":{close},"volume":1000,"closeunadj":{close_unadjusted},"lastupdated":"2022-09-01"}}"#
    )
}

/// The same row with its keys in a different order and one number quoted.
fn price_row_reordered(date: &str, close: &str, close_unadjusted: &str) -> String {
    format!(
        r#"{{"lastupdated":"2022-09-01","closeunadj":{close_unadjusted},"volume":1000,"close":{close},"low":9.9,"high":10.9,"open":"10.25","date":"{date}","ticker":"ZZTOP"}}"#
    )
}

fn ticker_row(table: &str, permaticker: &str, ticker: &str) -> String {
    format!(
        r#"{{"table":"{table}","permaticker":"{permaticker}","ticker":"{ticker}","name":"Fabricated Holdings Inc","exchange":"NASDAQ","category":"Domestic Common Stock","isdelisted":"N","firstpricedate":"2015-01-02","lastpricedate":"2026-08-07"}}"#
    )
}

/// A ticker row with the two fields the universe filter reads left free.
///
/// `delisted` is the wire spelling, `"Y"` or `"N"`, because that is what the
/// decoder is strict about and a bool here would hide the strictness.
fn master_row(permaticker: &str, ticker: &str, delisted: &str, first_price: &str) -> String {
    format!(
        r#"{{"table":"{WIRE_STOCKS}","permaticker":"{permaticker}","ticker":"{ticker}","name":"Fabricated Holdings Inc","exchange":"NASDAQ","category":"Domestic Common Stock","isdelisted":"{delisted}","firstpricedate":"{first_price}","lastpricedate":"2026-08-07"}}"#
    )
}

fn date(text: &str) -> Date {
    Date::from_str(text).expect("valid fixture date")
}

fn decimal(text: &str) -> Decimal {
    Decimal::from_str_exact(text).expect("valid fixture decimal")
}

fn window(client: &SharadarClient) -> Result<Vec<super::SepRow>, SourceError> {
    client.native_price_window("ZZTOP", date("2022-08-22"), date("2022-08-30"))
}

// --- N1: the envelope decodes ----------------------------------------------

#[test]
fn n1_a_keyed_row_decodes() {
    let fake = Fake::serving(
        WIRE_STOCKS,
        &[envelope(&[price_row("2022-08-22", "289.913", "869.74")])],
    );

    let rows = window(&client(&fake)).expect("the observed envelope decodes");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ticker, "ZZTOP");
    assert_eq!(rows[0].date, date("2022-08-22"));
    assert_eq!(rows[0].close, decimal("289.913"));
    assert_eq!(rows[0].close_unadjusted, decimal("869.74"));
    assert_eq!(rows[0].last_updated, Some(date("2022-09-01")));
}

#[test]
fn n1_key_order_and_number_spelling_do_not_change_the_values() {
    // On this host a row is a keyed object, so there is no column list to zip
    // against and the positional-misalignment bug of the datatables path is
    // unreachable. This pins that, and pins that a quoted number and a bare one
    // decode alike, which is the part that is genuinely at risk.
    let straight = Fake::serving(
        WIRE_STOCKS,
        &[envelope(&[price_row("2022-08-22", "289.913", "869.74")])],
    );
    let shuffled = Fake::serving(
        WIRE_STOCKS,
        &[envelope(&[price_row_reordered(
            "2022-08-22",
            "289.913",
            "869.74",
        )])],
    );

    let from_straight = window(&client(&straight)).expect("straight decodes");
    let from_shuffled = window(&client(&shuffled)).expect("reordered decodes");

    assert_eq!(from_straight, from_shuffled);
    assert_eq!(from_straight[0].open, decimal("10.25"));
}

#[test]
fn n1_a_count_that_disagrees_with_the_rows_is_malformed() {
    let fake = Fake::serving(
        WIRE_STOCKS,
        &[r#"{"count":7,"data":[{"ticker":"ZZTOP","date":"2022-08-22","open":1,"high":1,"low":1,"close":1,"volume":1,"closeunadj":1,"lastupdated":"2022-09-01"}]}"#.to_string()],
    );

    assert!(matches!(
        window(&client(&fake)).expect_err("a disagreeing count must not decode"),
        SourceError::Malformed { .. }
    ));
}

// --- N2: offset pagination and its boundaries ------------------------------

/// Page size three, so a boundary is reachable with four rows.
fn paging_client(fake: &Rc<Fake>) -> SharadarClient {
    client(fake).with_page_size(3)
}

#[test]
fn n2_a_clean_two_page_walk_yields_every_row_once() {
    let page_one = envelope(&[
        price_row("2022-08-22", "1", "1"),
        price_row("2022-08-23", "2", "2"),
        price_row("2022-08-24", "3", "3"),
    ]);
    // Requested with a one-row overlap, so it repeats 08-24 and then ends.
    let page_two = envelope(&[
        price_row("2022-08-24", "3", "3"),
        price_row("2022-08-25", "4", "4"),
    ]);
    let fake = Fake::serving(WIRE_STOCKS, &[page_one, page_two]);

    let rows = window(&paging_client(&fake)).expect("a clean walk succeeds");

    let dates: Vec<String> = rows.iter().map(|row| row.date.to_string()).collect();
    assert_eq!(
        dates,
        ["2022-08-22", "2022-08-23", "2022-08-24", "2022-08-25"],
        "the overlap row was dropped or duplicated"
    );
    assert_eq!(fake.calls(), 2);
    assert!(
        fake.seen.borrow()[1].contains("skip=2"),
        "the second page must be requested with a one-row overlap, got {}",
        fake.seen.borrow()[1]
    );
}

#[test]
fn n2_a_duplicated_boundary_row_errors_naming_the_key() {
    // Rows shifted forward: the page repeats 08-23 as well as 08-24, so the
    // walk would emit 08-23 twice.
    let page_one = envelope(&[
        price_row("2022-08-22", "1", "1"),
        price_row("2022-08-23", "2", "2"),
        price_row("2022-08-24", "3", "3"),
    ]);
    let page_two = envelope(&[
        price_row("2022-08-23", "2", "2"),
        price_row("2022-08-24", "3", "3"),
    ]);
    let fake = Fake::serving(WIRE_STOCKS, &[page_one, page_two]);

    let error = window(&paging_client(&fake)).expect_err("a shifted boundary must error");
    let rendered = error.to_string();

    assert!(matches!(error, SourceError::Malformed { .. }));
    assert!(
        rendered.contains("2022-08-24") && rendered.contains("2022-08-23"),
        "the error must name the key it expected and the key it got: {rendered}"
    );
}

#[test]
fn n2_a_gap_at_the_boundary_errors_naming_the_key() {
    // Rows removed: the next page starts past the overlap row, so 08-25 would
    // vanish from the result without a word.
    let page_one = envelope(&[
        price_row("2022-08-22", "1", "1"),
        price_row("2022-08-23", "2", "2"),
        price_row("2022-08-24", "3", "3"),
    ]);
    let page_two = envelope(&[
        price_row("2022-08-26", "5", "5"),
        price_row("2022-08-29", "6", "6"),
    ]);
    let fake = Fake::serving(WIRE_STOCKS, &[page_one, page_two]);

    let error = window(&paging_client(&fake)).expect_err("a gap must error");
    let rendered = error.to_string();

    assert!(matches!(error, SourceError::Malformed { .. }));
    assert!(
        rendered.contains("2022-08-24") && rendered.contains("2022-08-26"),
        "the error must name both keys: {rendered}"
    );
}

#[test]
fn n2_an_empty_page_after_a_full_one_errors() {
    let page_one = envelope(&[
        price_row("2022-08-22", "1", "1"),
        price_row("2022-08-23", "2", "2"),
        price_row("2022-08-24", "3", "3"),
    ]);
    let fake = Fake::serving(WIRE_STOCKS, &[page_one, EMPTY.to_string()]);

    let error = window(&paging_client(&fake)).expect_err("a vanished overlap row must error");
    assert!(error.to_string().contains("came back empty"), "got {error}");
}

/// Three rows sharing one sort token, so the boundary lands inside a tie.
fn tied_page_one() -> String {
    envelope(&[
        price_row("2022-08-22", "1", "1"),
        price_row("2022-08-22", "2", "2"),
        price_row("2022-08-22", "3", "3"),
    ])
}

#[test]
fn n2_a_tied_sort_token_on_a_different_row_still_errors() {
    // The overlap row carries the same `date` as the row actually held, but is
    // a different row. Comparing sort tokens alone accepts this and silently
    // re-emits close=2 while losing close=3.
    let page_two = envelope(&[
        price_row("2022-08-22", "2", "2"),
        price_row("2022-08-23", "4", "4"),
    ]);
    let fake = Fake::serving(WIRE_STOCKS, &[tied_page_one(), page_two]);

    let error = window(&paging_client(&fake))
        .expect_err("a shifted row under a tied sort token must error");

    assert!(matches!(error, SourceError::Malformed { .. }));
    assert!(
        error.to_string().contains("2022-08-22"),
        "the error must name the boundary: {error}"
    );
}

#[test]
fn n2_a_tie_with_a_genuinely_identical_overlap_row_succeeds() {
    // The other half, so the check above cannot be satisfied by refusing every
    // tie. Same tokens, and the overlap row really is the row already held.
    let page_two = envelope(&[
        price_row("2022-08-22", "3", "3"),
        price_row("2022-08-23", "4", "4"),
    ]);
    let fake = Fake::serving(WIRE_STOCKS, &[tied_page_one(), page_two]);

    let rows = window(&paging_client(&fake)).expect("a legitimate tie must walk cleanly");

    let closes: Vec<String> = rows.iter().map(|row| row.close.to_string()).collect();
    assert_eq!(
        closes,
        ["1", "2", "3", "4"],
        "every row exactly once across a tied boundary"
    );
}

// --- N3: table names and the silent-empty guard ----------------------------

/// The table-name constants match the wire, compared against literals.
///
/// The behavioural tests below catch a miscased constant by getting an empty
/// answer, which is what this host really does. This one says so directly, so
/// the reason is one line rather than a deduction from a decode failure.
#[test]
fn n3_native_table_names_are_lowercase_as_the_wire_spells_them() {
    assert_eq!(native_tables::STOCKS, WIRE_STOCKS);
    assert_eq!(native_tables::TICKERS, WIRE_TICKERS);
    assert_eq!(native_tables::FUNDAMENTALS, WIRE_FUNDAMENTALS);
}

#[test]
fn n3_zero_rows_for_a_known_live_ticker_is_an_error() {
    // No body queued for `tickers`, so the fake answers as this host does for a
    // table name it does not recognise: HTTP 200 and no rows.
    let fake = Fake::serving(
        "something-else",
        &[envelope(&[ticker_row("stocks", "1", "X")])],
    );

    let error = client(&fake)
        .native_tickers(&["ZZTOP"])
        .expect_err("an empty answer for a named ticker must not read as success");
    let rendered = error.to_string();

    assert!(matches!(error, SourceError::Malformed { .. }));
    assert!(
        rendered.contains(WIRE_TICKERS) && rendered.contains("ZZTOP"),
        "the error must name the table and the filter: {rendered}"
    );
}

#[test]
fn n3_a_populated_ticker_lookup_still_succeeds() {
    // The guard must not be satisfiable by refusing everything.
    let fake = Fake::serving(
        WIRE_TICKERS,
        &[envelope(&[
            ticker_row("fundamentals", "194897", "ZZTOP"),
            ticker_row(WIRE_STOCKS, "194897", "ZZTOP"),
        ])],
    );

    let rows = client(&fake).native_tickers(&["ZZTOP"]).expect("decodes");

    assert_eq!(rows.len(), 1, "only the equity row is kept");
    assert_eq!(
        rows[0].asset.permanent,
        Some(PermanentId::Sharadar(194_897))
    );
    assert!(!rows[0].is_delisted);
}

// --- N4: per-host column names ---------------------------------------------

#[test]
fn n4_the_filing_date_column_is_named_per_host() {
    assert_eq!(datatables::FILING_DATE, "datekey");
    assert_eq!(native::FILING_DATE, "date");
    assert_ne!(
        datatables::FILING_DATE,
        native::FILING_DATE,
        "the two hosts spell this field differently and the constants must say so"
    );
}

/// The constants above are only worth having if the fetch actually uses them.
///
/// Split from the assertion on the values themselves so that pointing the
/// native path at the wrong spelling fails on a decode, which is the thing that
/// would break in production, rather than only on a string comparison.
#[test]
fn n4_the_native_path_reads_its_own_filing_date_column() {
    let fake = Fake::serving(
        WIRE_FUNDAMENTALS,
        &[envelope(&[
            r#"{"ticker":"ZZTOP","date":"2022-05-02","reportperiod":"2022-03-31"}"#.to_string(),
        ])],
    );

    let dates = client(&fake)
        .native_filing_dates("ZZTOP")
        .expect("the native filing date column decodes");
    assert_eq!(dates, [date("2022-05-02")]);
}

#[test]
fn n4_the_equity_table_tag_is_named_per_host() {
    assert_eq!(datatables::EQUITY_TABLE_TAG, "SEP");
    assert_eq!(native::EQUITY_TABLE_TAG, "stocks");
    assert_ne!(datatables::EQUITY_TABLE_TAG, native::EQUITY_TABLE_TAG);
}

// --- N5: the credential defences hold on this door too ---------------------

#[test]
fn n5_a_native_transport_error_carries_no_key() {
    let fake = Fake::failing("connection refused");

    let error = window(&client(&fake)).expect_err("an unreachable host must error");
    let mut rendered = error.to_string();
    let mut cause = std::error::Error::source(&error);
    while let Some(next) = cause {
        rendered.push_str(&next.to_string());
        cause = next.source();
    }

    assert!(
        !rendered.contains(TEST_KEY),
        "the native path leaked the key: {rendered}"
    );
    assert!(
        rendered.contains(WIRE_STOCKS),
        "the error should still say what was being fetched: {rendered}"
    );
}

#[test]
fn n5_debug_on_a_native_client_carries_no_key() {
    let fake = Fake::serving(WIRE_STOCKS, &[]);
    let rendered = format!("{:?}", client(&fake));

    assert!(
        !rendered.contains(TEST_KEY),
        "Debug leaked the key: {rendered}"
    );
}

// --- P4: the measured window is enforced ------------------------------------

/// A client whose window measurement and price window are both scripted.
///
/// The first `stocks` body answers the `limit=1` boundary measurement; the rest
/// answer the fetch. That ordering is the real request order, so a test that
/// got it wrong would fail rather than quietly pass.
fn windowed(pages: &[String]) -> Rc<Fake> {
    let mut bodies = vec![envelope(&[price_row("2021-08-10", "1", "1")])];
    bodies.extend_from_slice(pages);
    Fake::serving(WIRE_STOCKS, &bodies)
}

fn range(from: &str, to: &str) -> crate::provider::DateRange {
    crate::provider::DateRange::new(date(from), date(to)).expect("valid range")
}

#[test]
fn p4_the_measured_boundary_is_reported() {
    let fake = windowed(&[]);
    let measured = client(&fake)
        .earliest_available()
        .expect("the boundary is measurable");

    assert_eq!(measured, Some(date("2021-08-10")));
    assert!(
        fake.seen.borrow()[0].contains("limit=1"),
        "the measurement must be one row, not a walk: {}",
        fake.seen.borrow()[0]
    );
}

#[test]
fn p4_a_range_reaching_before_the_boundary_errors_naming_it() {
    // A page is queued deliberately, so that code which clipped the range to
    // the boundary instead of refusing would succeed and return a short
    // series. The failure this test must produce is "it worked", not "it ran
    // out of fixture".
    let fake = windowed(&[envelope(&[price_row("2022-08-22", "10.25", "41.00")])]);
    let error = client(&fake)
        .fetch_adjusted(
            &[AssetKey::ticker_only("ZZTOP")],
            range("2019-01-02", "2022-01-02"),
        )
        .expect_err("a range before the measured window must not be served");

    let rendered = error.to_string();
    assert!(matches!(error, SourceError::Refused { .. }));
    assert!(
        rendered.contains("2021-08-10"),
        "the error must name the boundary: {rendered}"
    );
    assert_eq!(
        fake.calls(),
        1,
        "the refusal must cost the measurement and nothing more"
    );
}

#[test]
fn p4_a_range_inside_the_boundary_proceeds() {
    let fake = windowed(&[envelope(&[price_row("2022-08-22", "289.913", "869.74")])]);

    let bars = client(&fake)
        .fetch_adjusted(
            &[AssetKey::ticker_only("ZZTOP")],
            range("2022-08-22", "2022-08-30"),
        )
        .expect("a range inside the window is served");

    assert_eq!(bars.len(), 1);
    assert_eq!(bars[0].close, decimal("289.913"));
    assert_eq!(bars[0].close_unadjusted, decimal("869.74"));
}

/// The boundary costs one request per client, not one per fetch.
#[test]
fn p4_the_boundary_is_measured_once() {
    let fake = windowed(&[
        envelope(&[price_row("2022-08-22", "1", "1")]),
        envelope(&[price_row("2022-08-23", "2", "2")]),
    ]);
    let source = client(&fake);

    for _ in 0..2 {
        source
            .fetch_adjusted(
                &[AssetKey::ticker_only("ZZTOP")],
                range("2022-08-22", "2022-08-30"),
            )
            .expect("served");
    }

    assert_eq!(
        fake.calls(),
        3,
        "one measurement plus two fetches, not two measurements"
    );
}

// --- P6: vendor JSON to validated Parquet, end to end -----------------------

/// The whole door in one test: scripted vendor response, through the source,
/// through validation, into a curated file, and back out.
///
/// The CLI layer above this is argument plumbing; what it calls is exactly this
/// sequence, and the live run reported alongside exercises the plumbing.
#[test]
fn p6_a_fetch_becomes_a_curated_file() {
    // Closes inside the fixture's own high-low band, because these rows go
    // through validation rather than only through the decoder.
    let fake = windowed(&[envelope(&[
        price_row("2022-08-22", "10.25", "41.00"),
        price_row("2022-08-23", "10.50", "42.00"),
    ])]);

    let bars = client(&fake)
        .fetch_adjusted(
            &[AssetKey::ticker_only("ZZTOP")],
            range("2022-08-22", "2022-08-30"),
        )
        .expect("fetched");

    let report = crate::adjusted::validate_adjusted(bars);
    assert!(report.rejected.is_empty(), "vendor rows must validate");

    let dir = std::env::temp_dir().join("p6-fetch-to-parquet");
    let _ = std::fs::remove_dir_all(&dir);
    let path = crate::parquet::prices_path(&dir);
    let written = crate::parquet::write_prices(report.accepted, &path, "synthetic").expect("write");
    assert_eq!(written, 2);

    let read = crate::parquet::read_prices(&path).expect("read");
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].close_unadjusted, decimal("41.00"));
    assert_eq!(read[0].asset.ticker, "ZZTOP");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- P7: retry conduct, this door ------------------------------------------

#[test]
fn p7_the_native_door_makes_one_attempt_one_wait_one_more() {
    let fake = Rc::new(Fake {
        failures: RefCell::new(std::iter::repeat_n("connection refused".to_string(), 8).collect()),
        ..Default::default()
    });

    client(&fake)
        .native_tickers(&["ZZTOP"])
        .expect_err("an unreachable host must error");

    assert_eq!(
        fake.calls(),
        2,
        "one long wait and one retry, never a burst of fast ones"
    );
}

// --- N8: the whole security master, for building a universe ----------------
//
// The universe these rows feed is the reason rule 4 is satisfiable at all. A
// hand-picked ticker list is survivorship-biased by construction, because the
// hand doing the picking already knows which companies still exist. So the
// master is fetched whole and sampled deterministically, and the tests below
// are about what the fetch must not quietly drop on the way.

#[test]
fn n8_the_master_fetch_keeps_delisted_securities() {
    // The entire point of pulling the master rather than naming tickers. A
    // universe built only from survivors reports the returns of companies
    // selected for having survived, which is a different and much better
    // number than the one being asked for.
    let fake = Fake::serving(
        WIRE_TICKERS,
        &[envelope(&[
            master_row("100", "ALIVE", "N", "2015-01-02"),
            master_row("101", "GONE", "Y", "2015-01-02"),
        ])],
    );

    let rows = client(&fake)
        .native_ticker_master()
        .expect("the master fetch succeeds");

    let delisted: Vec<&str> = rows
        .iter()
        .filter(|row| row.is_delisted)
        .map(|row| row.asset.ticker.as_str())
        .collect();
    assert_eq!(
        delisted,
        ["GONE"],
        "a delisted security was dropped, which is survivorship bias entering at the source"
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn n8_the_master_fetch_keeps_only_equity_coverage_rows() {
    // TICKERS keys on (table, permaticker, ticker), so one security appears
    // once per table it belongs to. Keeping the others would multiply the
    // universe by the number of tables each name is in.
    let fake = Fake::serving(
        WIRE_TICKERS,
        &[envelope(&[
            ticker_row("fundamentals", "100", "DUPE"),
            ticker_row(WIRE_STOCKS, "100", "DUPE"),
            ticker_row("sf3", "100", "DUPE"),
        ])],
    );

    let rows = client(&fake)
        .native_ticker_master()
        .expect("the master fetch succeeds");

    assert_eq!(rows.len(), 1, "one security must appear once");
    assert_eq!(rows[0].asset.permanent, Some(PermanentId::Sharadar(100)));
}

#[test]
fn n8_the_master_fetch_walks_every_page() {
    // Page size three with a one-row overlap, the same shape the price walk
    // uses. A master fetch that stopped at the first page would build a
    // universe out of whichever names sort first, which is not a sample.
    let page_one = envelope(&[
        master_row("100", "AAA", "N", "2015-01-02"),
        master_row("101", "BBB", "N", "2015-01-02"),
        master_row("102", "CCC", "N", "2015-01-02"),
    ]);
    let page_two = envelope(&[
        master_row("102", "CCC", "N", "2015-01-02"),
        master_row("103", "DDD", "N", "2015-01-02"),
    ]);
    let fake = Fake::serving(WIRE_TICKERS, &[page_one, page_two]);

    let rows = client(&fake)
        .with_page_size(3)
        .native_ticker_master()
        .expect("a clean walk succeeds");

    let tickers: Vec<&str> = rows.iter().map(|row| row.asset.ticker.as_str()).collect();
    assert_eq!(tickers, ["AAA", "BBB", "CCC", "DDD"]);
    assert_eq!(fake.calls(), 2);
}

#[test]
fn n8_an_empty_master_is_an_error_rather_than_an_empty_universe() {
    // This host answers a wrong-case table name with HTTP 200 and no rows, so
    // an empty master is indistinguishable from a typo unless somebody insists
    // on the difference. An empty universe would otherwise reach the engine as
    // a backtest over nothing.
    let fake = Fake::serving("something-else", &[envelope(&[])]);

    let error = client(&fake)
        .native_ticker_master()
        .expect_err("an empty master must not read as success");

    assert!(matches!(error, SourceError::Malformed { .. }));
    assert!(
        error.to_string().contains(WIRE_TICKERS),
        "the error must name the table: {error}"
    );
}

// --- N4: the corporate actions fetch ----------------------------------------

/// One fabricated actions row, in the shape the probe measured on 2026-08-10.
///
/// Seven fields, `value` bare as the host sends it, and `contraticker` and
/// `contraname` carrying the string `"N/A"` rather than JSON null, because that
/// is what this table really does.
fn action_row(date: &str, action: &str, value: &str) -> String {
    format!(
        r#"{{"date":"{date}","action":"{action}","ticker":"ZZTOP","name":"Fabricated Holdings Inc","value":{value},"contraticker":"N/A","contraname":"N/A"}}"#
    )
}

fn dividends(client: &SharadarClient) -> Result<Vec<crate::actions::ActionRecord>, SourceError> {
    client.fetch_cash_dividends(
        &AssetKey {
            ticker: "ZZTOP".to_string(),
            permanent: Some(PermanentId::Sharadar(199_059)),
        },
        crate::provider::DateRange::new(date("2023-01-01"), date("2023-12-31")).expect("range"),
    )
}

/// N4. A vendor row becomes a cash dividend carrying the caller's identity.
#[test]
fn n4_an_actions_row_decodes_into_a_cash_dividend() {
    let fake = Fake::serving(
        WIRE_ACTIONS,
        &[envelope(&[action_row("2023-03-16", "dividend", "0.19")])],
    );

    let records = dividends(&client(&fake)).expect("the fetch succeeds");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].effective, date("2023-03-16"));
    assert_eq!(
        records[0].action,
        crate::actions::CorporateAction::Dividend {
            amount: decimal("0.19"),
            kind: crate::actions::DividendKind::Cash,
        }
    );
    assert_eq!(
        records[0].asset.permanent,
        Some(PermanentId::Sharadar(199_059)),
        "the caller's permanent identity must survive, or a curated action \
         cannot be matched to its own price series after a ticker change"
    );
    assert_eq!(records[0].source, super::PROVIDER);

    // The kind filter is sent, so the vendor does the work rather than this
    // process downloading every action ever recorded and discarding most of it.
    assert!(
        fake.seen.borrow()[0].contains("action=dividend"),
        "got {}",
        fake.seen.borrow()[0]
    );
}

/// N4. A kind that is not exactly `dividend` is a hard error.
///
/// This is the `spinoffdividend` trap, observed live on 2026-08-10: that kind
/// carries the per-share value of shares in a spun-off company rather than
/// cash, at a magnitude two orders above a quarterly dividend. Booking it as a
/// distribution invents money, and one such row moves a monthly return by
/// percent. The row below is fabricated in its image.
///
/// A row like this can only arrive if the server-side filter stopped being
/// honoured, which is precisely the change no request-side code would notice.
#[test]
fn n4_a_kind_that_is_not_exactly_dividend_is_refused() {
    let fake = Fake::serving(
        WIRE_ACTIONS,
        &[envelope(&[
            action_row("2023-03-16", "dividend", "0.19"),
            action_row("2023-03-31", "spinoffdividend", "21.5"),
        ])],
    );

    let error = dividends(&client(&fake)).expect_err("an unfiltered kind must not be mapped");
    let rendered = error.to_string();

    assert!(matches!(error, SourceError::Malformed { .. }));
    assert!(
        rendered.contains("spinoffdividend"),
        "the error must name the kind it refused: {rendered}"
    );
}

/// N4. A null amount is refused rather than read as zero.
///
/// Measured 2026-08-10: `value` is null on `listed`, `relation` and both
/// ticker-change kinds. A null reaching the panel as zero would be a dividend
/// of nothing, which passes every positivity check by being absent.
#[test]
fn n4_a_null_amount_is_refused() {
    let fake = Fake::serving(
        WIRE_ACTIONS,
        &[envelope(&[action_row("2023-03-16", "dividend", "null")])],
    );

    let error = dividends(&client(&fake)).expect_err("a null amount must not decode");
    assert!(matches!(error, SourceError::Malformed { .. }));
    assert!(error.to_string().contains("value"), "got {error}");
}

/// N4. A security that paid nothing in the window is not an error.
///
/// The opposite of the price fetch, where an empty answer means the request was
/// wrong. Most securities pay no dividends, so refusing an empty answer here
/// would refuse the common case.
#[test]
fn n4_no_dividends_in_the_window_is_an_empty_success() {
    let fake = Fake::serving(WIRE_ACTIONS, &[EMPTY.to_string()]);
    assert!(
        dividends(&client(&fake))
            .expect("an empty answer is fine")
            .is_empty()
    );
}

/// N4. The page-boundary check covers the actions table too.
///
/// The walk is the same `fetch_native`, so this is the shared machinery rather
/// than a second implementation. What it proves is that the new table goes
/// through it, with a sort key that exists, rather than around it.
#[test]
fn n4_the_actions_walk_checks_its_page_boundary() {
    let page_one = envelope(&[
        action_row("2023-03-16", "dividend", "1"),
        action_row("2023-06-15", "dividend", "2"),
        action_row("2023-09-14", "dividend", "3"),
    ]);
    // Shifted underneath the walk: the overlap row is a different row from the
    // one held, though it sorts to the same place.
    let page_two = envelope(&[
        action_row("2023-09-14", "dividend", "9"),
        action_row("2023-12-14", "dividend", "4"),
    ]);
    let fake = Fake::serving(WIRE_ACTIONS, &[page_one, page_two]);

    let error = dividends(&paging_client(&fake)).expect_err("a shifted boundary must error");
    assert!(matches!(error, SourceError::Malformed { .. }));
    assert!(
        error.to_string().contains("2023-09-14"),
        "the error must name the boundary: {error}"
    );
}

/// N4. A clean walk across a page boundary keeps every row exactly once.
#[test]
fn n4_a_clean_actions_walk_keeps_every_row() {
    let page_one = envelope(&[
        action_row("2023-03-16", "dividend", "1"),
        action_row("2023-06-15", "dividend", "2"),
        action_row("2023-09-14", "dividend", "3"),
    ]);
    let page_two = envelope(&[
        action_row("2023-09-14", "dividend", "3"),
        action_row("2023-12-14", "dividend", "4"),
    ]);
    let fake = Fake::serving(WIRE_ACTIONS, &[page_one, page_two]);

    let records = dividends(&paging_client(&fake)).expect("a clean walk succeeds");

    let dates: Vec<String> = records
        .iter()
        .map(|record| record.effective.to_string())
        .collect();
    assert_eq!(
        dates,
        ["2023-03-16", "2023-06-15", "2023-09-14", "2023-12-14"],
        "the overlap row was dropped or duplicated"
    );
}

/// N4. The actions table name matches the wire, compared against a literal.
///
/// The actions fetch accepts an empty answer, so the wrong-case trap cannot be
/// caught behaviourally the way `n3_zero_rows_for_a_known_live_ticker_is_an_error`
/// catches it for prices. A miscased constant here would report that nobody in
/// the universe has ever paid a dividend, and every downstream number would
/// look plausible. This assertion is the only thing standing in front of that.
#[test]
fn n4_the_actions_table_name_matches_the_wire() {
    assert_eq!(native_tables::ACTIONS, WIRE_ACTIONS);
}

// --- N6: the daily metrics table and market capitalisation -----------------
//
// Every row below is fabricated. The vendor's own figures live only in the
// gitignored probe output, because the licence covers rows wherever they are
// written. What is copied here is the *shape* the probe measured: keyed
// objects, a bare number to one decimal place, and the three fields the fetch
// asks for.

/// One fabricated daily row, in the shape the probe measured.
fn daily_row(date: &str, marketcap: &str) -> String {
    format!(r#"{{"ticker":"ZZTOP","date":"{date}","marketcap":{marketcap}}}"#)
}

fn marketcaps(
    client: &SharadarClient,
) -> Result<Vec<crate::marketcap::MarketCapRecord>, SourceError> {
    client.fetch_marketcaps(
        &AssetKey {
            ticker: "ZZTOP".into(),
            permanent: Some(PermanentId::Sharadar(194_897)),
        },
        range("2024-12-30", "2024-12-31"),
    )
}

/// N6. A vendor daily row becomes a curated record with its figure untouched.
///
/// The figure is asserted as text as well as as a number. Numeric equality
/// alone would accept a value that arrived with different digits and compared
/// equal after some rescaling cancelled out, and the digits are the whole point
/// of a store-as-shipped dataset.
#[test]
fn n6_a_daily_row_decodes_into_a_market_cap_record() {
    let fake = Fake::serving(
        WIRE_DAILY,
        &[envelope(&[daily_row("2024-12-31", "1234567.8")])],
    );

    let records = marketcaps(&client(&fake)).expect("the measured envelope decodes");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].date, date("2024-12-31"));
    assert_eq!(records[0].marketcap, decimal("1234567.8"));
    assert_eq!(
        records[0].marketcap.normalize().to_string(),
        "1234567.8",
        "the shipped digits changed on the way in"
    );
    // The caller's key, not one rebuilt from the ticker string.
    assert_eq!(
        records[0].asset.permanent,
        Some(PermanentId::Sharadar(194_897))
    );
    assert_eq!(records[0].source, super::PROVIDER);
}

/// N6. The daily table name matches the wire, compared against a literal.
///
/// This fetch accepts an empty answer, measured, so the wrong-case trap cannot
/// be caught behaviourally the way `n3_zero_rows_for_a_known_live_ticker_is_an_error`
/// catches it for prices. It is worse here than for actions: `metrics` is a
/// *live* table on this host carrying plausible daily figures and no market cap
/// column at all, so a wrong name can return real rows rather than none. This
/// assertion is the only thing standing in front of that.
#[test]
fn n6_the_daily_table_name_matches_the_wire() {
    assert_eq!(native_tables::DAILY, WIRE_DAILY);
}

/// N6. The fetch asks the daily table for exactly the three fields it stores.
///
/// The host honours `fields`, measured, so sending the trimmed list is what
/// keeps seven valuation ratios this platform does not use off the wire. The
/// assertion is on the URL because that is the only place the request is
/// visible before it leaves.
#[test]
fn n6_the_fetch_asks_the_daily_table_for_only_what_it_stores() {
    let fake = Fake::serving(WIRE_DAILY, &[envelope(&[daily_row("2024-12-31", "1.5")])]);
    marketcaps(&client(&fake)).expect("decodes");

    let seen = fake.seen.borrow();
    let url = seen.first().expect("one request was made");
    assert_eq!(
        table_of(url),
        WIRE_DAILY,
        "the request went elsewhere: {url}"
    );
    assert!(
        url.contains("fields="),
        "no fields parameter was sent: {url}"
    );
    assert!(
        url.contains("marketcap"),
        "the stored field was not asked for: {url}"
    );
    assert!(
        !url.contains("evebitda"),
        "the fetch asked for a ratio it does not store: {url}"
    );
}

/// N6. A name the daily table has no rows for is an empty success.
///
/// Measured 2026-08-11: a security served with prices had no daily row in the
/// same window and none on any date, because this table's history starts later
/// than the price table's. `require_rows` here would turn every such name into
/// a failure, so the empty answer is a fact rather than a fault.
#[test]
fn n6_a_name_with_no_daily_rows_is_an_empty_success() {
    // No body queued for the daily table, so the fake answers exactly as this
    // host does: HTTP 200 and no rows.
    let fake = Fake::serving(
        "something-else",
        &[envelope(&[daily_row("2024-12-31", "1.5")])],
    );

    let records = marketcaps(&client(&fake)).expect("an empty answer is not an error here");
    assert!(records.is_empty());
}

/// N6. A null figure is refused rather than read as zero.
///
/// This table says "no figure" by omitting the row, so a null that does arrive
/// is a row that changed shape. Reading it as zero would claim a company is
/// worth nothing, and a zero market cap propagates into a weight of zero rather
/// than into an error anybody would see.
#[test]
fn n6_a_null_marketcap_is_malformed_rather_than_zero() {
    let fake = Fake::serving(
        WIRE_DAILY,
        &[envelope(&[
            r#"{"ticker":"ZZTOP","date":"2024-12-31","marketcap":null}"#.to_string(),
        ])],
    );

    let error = marketcaps(&client(&fake)).expect_err("a null figure must not decode");
    assert!(matches!(error, SourceError::Malformed { .. }));
    assert!(
        error.to_string().contains("marketcap"),
        "the error must name the field: {error}"
    );
}

/// N6. A row missing any of the three requested fields is malformed.
///
/// The request names exactly these three, so a row arriving without one is the
/// host disagreeing with what it was asked for. The ticker is the interesting
/// case: nothing downstream reads it, since identity comes from the caller's
/// key, so a decoder that never touched it would accept a response that had
/// silently stopped filtering by name.
#[test]
fn n6_a_row_missing_a_requested_field_is_malformed() {
    for row in [
        r#"{"date":"2024-12-31","marketcap":1234.5}"#,
        r#"{"ticker":"ZZTOP","marketcap":1234.5}"#,
        r#"{"ticker":"ZZTOP","date":"2024-12-31"}"#,
    ] {
        let fake = Fake::serving(WIRE_DAILY, &[envelope(&[row.to_string()])]);
        let error = match marketcaps(&client(&fake)) {
            Err(error) => error,
            Ok(decoded) => {
                panic!("a row missing a requested field decoded anyway: {row}, got {decoded:?}")
            }
        };
        assert!(
            matches!(error, SourceError::Malformed { .. }),
            "expected Malformed for {row}, got {error:?}"
        );
    }
}

/// N6. A name the vendor declines is an ordinary error, not a credential fault.
///
/// This is the seam the universe walk branches on: it records a decline against
/// the name and carries on, and stops only for `Unauthorized`, which is one
/// fault affecting every remaining name. The 401-to-`Unauthorized` mapping
/// itself is shared by every fetch on this client and is pinned by
/// `s6_a_401_is_unauthorized_and_is_not_retried`.
#[test]
fn n6_a_declined_name_is_not_an_unauthorized() {
    let fake = Fake::failing("connection refused");

    let error = marketcaps(&client(&fake)).expect_err("an unreachable host must error");
    assert!(
        !matches!(error, SourceError::Unauthorized { .. }),
        "a transport failure must not read as a credential fault, or the walk aborts: {error}"
    );
}
