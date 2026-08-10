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
use crate::provider::SourceError;
use crate::schema::PermanentId;

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
        r#"{{"ticker":"ZZTOP","date":"{date}","open":10.25,"high":10.9,"low":9.9,"close":{close},"closeunadj":{close_unadjusted},"lastupdated":"2022-09-01"}}"#
    )
}

/// The same row with its keys in a different order and one number quoted.
fn price_row_reordered(date: &str, close: &str, close_unadjusted: &str) -> String {
    format!(
        r#"{{"lastupdated":"2022-09-01","closeunadj":{close_unadjusted},"close":{close},"low":9.9,"high":10.9,"open":"10.25","date":"{date}","ticker":"ZZTOP"}}"#
    )
}

fn ticker_row(table: &str, permaticker: &str, ticker: &str) -> String {
    format!(
        r#"{{"table":"{table}","permaticker":"{permaticker}","ticker":"{ticker}","name":"Fabricated Holdings Inc","exchange":"NASDAQ","category":"Domestic Common Stock","isdelisted":"N","firstpricedate":"2015-01-02","lastpricedate":"2026-08-07"}}"#
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
        &[r#"{"count":7,"data":[{"ticker":"ZZTOP","date":"2022-08-22","open":1,"high":1,"low":1,"close":1,"closeunadj":1,"lastupdated":"2022-09-01"}]}"#.to_string()],
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
