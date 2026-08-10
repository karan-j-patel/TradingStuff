//! Offline tests for the Sharadar connector.
//!
//! Nothing here opens a socket. Every response is a scripted fixture served
//! through the [`Http`] seam, which is why pagination and retry can be
//! exercised at all.
//!
//! **Every fixture is fabricated.** Sharadar's licence forbids redistributing
//! rows, and that applies to a test file as much as to a data directory. The
//! tickers are invented, the prices are invented, and the permatickers are
//! invented. What is copied from the vendor is the *shape* of the envelope,
//! which is documentation rather than data.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::str::FromStr;

use jiff::civil::Date;
use rust_decimal::Decimal;

use super::client::{SharadarClient, scrub_url};
use super::http::{Http, HttpFailure, HttpReply};
use crate::provider::SourceError;
use crate::schema::PermanentId;

/// Not a credential. A string chosen to be conspicuous if it ever leaks into
/// an assertion's failure output.
const TEST_KEY: &str = "synthetic-not-a-real-credential-a1b2c3";

const BASE: &str = "https://fixture.invalid/api/v3/datatables/SHARADAR";

// --- the scripted transport -------------------------------------------------

/// Serves a queue of prepared replies and records the URLs it was asked for.
#[derive(Default)]
struct Scripted {
    replies: RefCell<VecDeque<Result<HttpReply, HttpFailure>>>,
    seen: RefCell<Vec<String>>,
}

impl Scripted {
    fn new(replies: Vec<Result<HttpReply, HttpFailure>>) -> Rc<Self> {
        Rc::new(Scripted {
            replies: RefCell::new(replies.into_iter().collect()),
            seen: RefCell::new(Vec::new()),
        })
    }

    /// One 200 with this body.
    fn ok(body: String) -> Rc<Self> {
        Scripted::new(vec![Ok(HttpReply { status: 200, body })])
    }

    fn calls(&self) -> usize {
        self.seen.borrow().len()
    }

    fn url(&self, nth: usize) -> String {
        self.seen.borrow()[nth].clone()
    }
}

impl Http for Rc<Scripted> {
    fn get(&self, url: &str) -> Result<HttpReply, HttpFailure> {
        self.seen.borrow_mut().push(url.to_string());
        self.replies
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| panic!("the transport was called more times than the test scripted"))
    }
}

fn client(transport: &Rc<Scripted>) -> SharadarClient {
    SharadarClient::with_transport(Box::new(Rc::clone(transport)), TEST_KEY, BASE)
}

// --- fixture construction ---------------------------------------------------

/// Build a datatable envelope from rows of (column name, raw JSON value) pairs.
///
/// The column list and each row are emitted from the same ordered pairs, so
/// reordering the pairs reorders both together. That is what makes "the same
/// data with the columns shuffled" a change of order and not a change of data,
/// which is the distinction S1 turns on.
fn envelope(rows: &[Vec<(&str, &str)>], cursor: Option<&str>) -> String {
    let columns = rows
        .first()
        .expect("a fixture needs at least one row to name its columns")
        .iter()
        .map(|(name, _)| format!(r#"{{"name":"{name}","type":"String"}}"#))
        .collect::<Vec<_>>()
        .join(",");

    let data = rows
        .iter()
        .map(|row| {
            let cells = row
                .iter()
                .map(|(_, value)| *value)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{cells}]")
        })
        .collect::<Vec<_>>()
        .join(",");

    let next = match cursor {
        Some(id) => format!(r#""{id}""#),
        None => "null".to_string(),
    };

    format!(
        r#"{{"datatable":{{"columns":[{columns}],"data":[{data}]}},"meta":{{"next_cursor_id":{next}}}}}"#
    )
}

/// A fabricated TICKERS row, in the column order the client asks for.
fn ticker_row(table: &str, permaticker: &str, ticker: &str) -> Vec<(&'static str, String)> {
    vec![
        ("table", format!(r#""{table}""#)),
        ("permaticker", permaticker.to_string()),
        ("ticker", format!(r#""{ticker}""#)),
        ("name", r#""Fabricated Holdings Inc""#.to_string()),
        ("exchange", r#""NASDAQ""#.to_string()),
        ("category", r#""Domestic""#.to_string()),
        ("isdelisted", r#""N""#.to_string()),
        ("firstpricedate", r#""2015-01-02""#.to_string()),
        ("lastpricedate", r#""2026-08-07""#.to_string()),
    ]
}

/// Borrow an owned fixture row as the `&str` pairs `envelope` takes.
fn as_pairs<'a>(row: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    row.iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
}

/// A fabricated SEP row. `close` and `closeunadj` are supplied so a test can
/// choose the exact numeric token it wants to put through the decoder.
fn sep_row(close: &str, close_unadjusted: &str) -> Vec<(&'static str, String)> {
    vec![
        ("ticker", r#""ZZTOP""#.to_string()),
        ("date", r#""2020-01-02""#.to_string()),
        ("open", r#""10.10""#.to_string()),
        ("high", r#""10.90""#.to_string()),
        ("low", r#""9.90""#.to_string()),
        ("close", close.to_string()),
        ("volume", r#""1000""#.to_string()),
        ("closeunadj", close_unadjusted.to_string()),
        ("lastupdated", r#""2020-01-03""#.to_string()),
    ]
}

fn date(text: &str) -> Date {
    Date::from_str(text).expect("valid fixture date")
}

fn decimal(text: &str) -> Decimal {
    Decimal::from_str_exact(text).expect("valid fixture decimal")
}

fn only_sep_row(transport: &Rc<Scripted>) -> Result<super::SepRow, SourceError> {
    let rows = client(transport).sep_window("ZZTOP", date("2020-01-01"), date("2020-01-31"))?;
    Ok(rows.into_iter().next().expect("fixture has one row"))
}

// --- S1: columns bind by name ----------------------------------------------

#[test]
fn s1_a_row_decodes_by_column_name() {
    let row = ticker_row("SEP", "199059", "ZZTOP");
    let transport = Scripted::ok(envelope(&[as_pairs(&row)], None));

    let rows = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect("fixture decodes");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].asset.ticker, "ZZTOP");
    assert_eq!(
        rows[0].asset.permanent,
        Some(PermanentId::Sharadar(199_059))
    );
    assert_eq!(rows[0].name.as_deref(), Some("Fabricated Holdings Inc"));
    assert_eq!(rows[0].exchange.as_deref(), Some("NASDAQ"));
    assert!(!rows[0].is_delisted);
    assert_eq!(rows[0].first_price_date, Some(date("2015-01-02")));
    assert_eq!(rows[0].last_price_date, Some(date("2026-08-07")));
}

#[test]
fn s1_shuffling_the_columns_decodes_to_identical_values() {
    // The vendor does not promise an order, and a client that assumes one reads
    // every value out of its neighbour's column. Same data, reversed order.
    let row = ticker_row("SEP", "199059", "ZZTOP");
    let straight = Scripted::ok(envelope(&[as_pairs(&row)], None));

    let mut reversed_row = row.clone();
    reversed_row.reverse();
    let shuffled = Scripted::ok(envelope(&[as_pairs(&reversed_row)], None));

    let from_straight = client(&straight)
        .fetch_tickers(&["ZZTOP"])
        .expect("straight order decodes");
    let from_shuffled = client(&shuffled)
        .fetch_tickers(&["ZZTOP"])
        .expect("shuffled order decodes");

    assert_eq!(
        from_straight, from_shuffled,
        "column order changed the decoded values, so decoding is positional"
    );

    // Asserted separately because `AssetKey`'s equality deliberately ignores
    // the ticker whenever a permanent id is present, which is what lets a
    // rename keep one company's history together. The consequence here is that
    // the comparison above would not notice a ticker read out of the wrong
    // column, so the one field that equality is blind to is checked by hand.
    let tickers: Vec<&str> = from_straight
        .iter()
        .chain(from_shuffled.iter())
        .map(|row| row.asset.ticker.as_str())
        .collect();
    assert_eq!(tickers, ["ZZTOP", "ZZTOP"], "the ticker moved column");
}

#[test]
fn s1_a_missing_column_errors_naming_it() {
    let row: Vec<(&'static str, String)> = ticker_row("SEP", "199059", "ZZTOP")
        .into_iter()
        .filter(|(name, _)| *name != "permaticker")
        .collect();
    let transport = Scripted::ok(envelope(&[as_pairs(&row)], None));

    let error = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect_err("a missing column must not decode");

    let rendered = error.to_string();
    assert!(
        matches!(error, SourceError::Malformed { .. }),
        "expected Malformed, got {rendered}"
    );
    assert!(
        rendered.contains("permaticker"),
        "the error must name the column it could not find, got {rendered}"
    );
}

// --- S2: pagination ---------------------------------------------------------

#[test]
fn s2_every_row_of_a_two_page_response_arrives_exactly_once() {
    let first = ticker_row("SEP", "199059", "ZZTOP");
    let second = ticker_row("SEP", "199060", "QQQX");
    let transport = Scripted::new(vec![
        Ok(HttpReply {
            status: 200,
            body: envelope(&[as_pairs(&first)], Some("cursor-page-2")),
        }),
        Ok(HttpReply {
            status: 200,
            body: envelope(&[as_pairs(&second)], None),
        }),
    ]);

    let rows = client(&transport)
        .fetch_tickers(&["ZZTOP", "QQQX"])
        .expect("both pages decode");

    // The rows come first, because they are the property. The call count below
    // is the mechanism, and asserting the mechanism first would report a
    // dropped page as "a request was not made" rather than as missing data.
    let tickers: Vec<&str> = rows.iter().map(|row| row.asset.ticker.as_str()).collect();
    assert_eq!(tickers, ["ZZTOP", "QQQX"], "rows dropped or duplicated");
    assert_eq!(transport.calls(), 2, "the second page was not requested");

    assert!(
        !transport.url(0).contains("cursor_id"),
        "the first request must not carry a cursor"
    );
    assert!(
        transport.url(1).contains("qopts.cursor_id=cursor-page-2"),
        "the second request must carry the cursor the first page returned, got {}",
        transport.url(1)
    );
}

// --- S3: Decimal fidelity ---------------------------------------------------

#[test]
fn s3_eighteen_significant_digits_survive_a_bare_json_number() {
    // Bare, not quoted: the form that would go through f64 without
    // serde_json's arbitrary_precision, losing the last digits silently.
    let row = sep_row("123456789.012345678", "123456789.012345678");
    let transport = Scripted::ok(envelope(&[as_pairs(&row)], None));

    let decoded = only_sep_row(&transport).expect("fixture decodes");

    assert_eq!(
        decoded.close,
        decimal("123456789.012345678"),
        "an 18 significant digit price did not survive decoding"
    );
    assert_eq!(decoded.close.to_string(), "123456789.012345678");
}

#[test]
fn s3_a_quoted_number_parses_identically_to_a_bare_one() {
    // The vendor mixes the two forms within a single row: large integers arrive
    // quoted while ratios arrive bare.
    let bare = Scripted::ok(envelope(
        &[as_pairs(&sep_row("123456789.012345678", "1.5"))],
        None,
    ));
    let quoted = Scripted::ok(envelope(
        &[as_pairs(&sep_row(r#""123456789.012345678""#, r#""1.5""#))],
        None,
    ));

    assert_eq!(
        only_sep_row(&bare).expect("bare decodes").close,
        only_sep_row(&quoted).expect("quoted decodes").close,
    );
}

#[test]
fn s3_a_garbage_numeric_token_is_malformed_rather_than_zero() {
    let row = sep_row(r#""not-a-number""#, "1.5");
    let transport = Scripted::ok(envelope(&[as_pairs(&row)], None));

    let error = only_sep_row(&transport).expect_err("garbage must not decode");
    let rendered = error.to_string();
    assert!(
        matches!(error, SourceError::Malformed { .. }),
        "expected Malformed, got {rendered}"
    );
    assert!(
        rendered.contains("close"),
        "the error must name the column, got {rendered}"
    );
}

#[test]
fn s3_a_token_too_precise_to_hold_is_refused_rather_than_rounded() {
    // Decimal holds 28 to 29 significant digits. A token past that must be an
    // error, because a rounded price is a wrong price that looks right.
    let row = sep_row("1.00000000000000000000000000000001", "1.5");
    let transport = Scripted::ok(envelope(&[as_pairs(&row)], None));

    let error = only_sep_row(&transport).expect_err("an unrepresentable token must not decode");
    assert!(matches!(error, SourceError::Malformed { .. }));
}

#[test]
fn s3_a_scientific_token_parses_exactly_or_is_refused() {
    // `from_str_exact` does not accept an exponent, so there is a fallback for
    // it. The fallback must not become a way for a value to round: it succeeds
    // only where the result is exact, and errors otherwise.
    let representable = Scripted::ok(envelope(&[as_pairs(&sep_row("1.5e-5", "1.5"))], None));
    assert_eq!(
        only_sep_row(&representable)
            .expect("an exactly representable exponent decodes")
            .close,
        decimal("0.000015"),
    );

    // Past Decimal's scale. The danger is that this becomes zero, which would
    // be a price of nothing rather than an error.
    let too_small = Scripted::ok(envelope(&[as_pairs(&sep_row("1e-40", "1.5"))], None));
    assert!(matches!(
        only_sep_row(&too_small).expect_err("an out-of-range exponent must not decode"),
        SourceError::Malformed { .. }
    ));
}

#[test]
fn s3_a_null_price_is_malformed_rather_than_zero() {
    let row = sep_row("null", "1.5");
    let transport = Scripted::ok(envelope(&[as_pairs(&row)], None));

    assert!(matches!(
        only_sep_row(&transport).expect_err("null must not decode"),
        SourceError::Malformed { .. }
    ));
}

// --- S4: the key cannot be printed -----------------------------------------

#[test]
fn s4_debug_output_omits_the_key() {
    let transport = Scripted::new(Vec::new());
    let rendered = format!("{:?}", client(&transport));

    assert!(
        !rendered.contains(TEST_KEY),
        "Debug printed the API key: {rendered}"
    );
    assert!(
        rendered.contains("SharadarClient"),
        "Debug should still identify the type, got {rendered}"
    );
}

#[test]
fn s4_a_transport_error_omits_the_key_and_keeps_the_path() {
    // Four consecutive failures, so the client exhausts its attempts and
    // surfaces an error carrying the URL it was fetching.
    let transport = Scripted::new(
        (0..4)
            .map(|_| Err(HttpFailure("connection refused".to_string())))
            .collect(),
    );

    let error = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect_err("an unreachable host must error");
    let rendered = format!("{error}: {}", source_chain(&error));

    assert!(
        !rendered.contains(TEST_KEY),
        "the rendered error carried the API key: {rendered}"
    );
    assert!(
        rendered.contains("TICKERS"),
        "the error should still say what was being fetched, got {rendered}"
    );
}

#[test]
fn s4_a_key_echoed_by_the_vendor_is_redacted() {
    // A 400 whose body quotes the request back. This is the case URL scrubbing
    // alone does not cover, because the key is in the response rather than in
    // the URL the client built.
    let transport = Scripted::new(vec![Ok(HttpReply {
        status: 400,
        body: format!(r#"{{"error":"bad filter in ?api_key={TEST_KEY}&ticker=ZZTOP"}}"#),
    })]);

    let error = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect_err("a 400 must error");
    let rendered = format!("{error}: {}", source_chain(&error));

    assert!(
        !rendered.contains(TEST_KEY),
        "the vendor echoed the key and it reached the error: {rendered}"
    );
}

#[test]
fn s4_scrubbing_drops_the_whole_query_string() {
    let scrubbed = scrub_url(&format!(
        "https://data.nasdaq.com/api/v3/datatables/SHARADAR/SEP.json?api_key={TEST_KEY}&ticker=ZZTOP"
    ));

    assert!(!scrubbed.contains(TEST_KEY));
    assert!(scrubbed.contains("/SHARADAR/SEP.json"), "got {scrubbed}");
}

/// Render an error together with everything it wraps, since the key could be
/// hiding one level down rather than in the top message.
fn source_chain(error: &SourceError) -> String {
    let mut rendered = String::new();
    let mut current = std::error::Error::source(error);
    while let Some(cause) = current {
        rendered.push_str(&cause.to_string());
        rendered.push_str(" | ");
        current = cause.source();
    }
    rendered
}

// --- S5: tickers ------------------------------------------------------------

#[test]
fn s5_a_permaticker_becomes_a_sharadar_identity_in_either_json_form() {
    for token in ["199059", r#""199059""#] {
        let row = ticker_row("SEP", token, "ZZTOP");
        let transport = Scripted::ok(envelope(&[as_pairs(&row)], None));

        let rows = client(&transport)
            .fetch_tickers(&["ZZTOP"])
            .unwrap_or_else(|error| panic!("{token} should decode, got {error}"));

        assert_eq!(
            rows[0].asset.permanent,
            Some(PermanentId::Sharadar(199_059)),
            "{token} did not become a Sharadar identity"
        );
    }
}

#[test]
fn s5_a_non_numeric_permaticker_errors_naming_the_value() {
    let row = ticker_row("SEP", r#""not-a-number""#, "ZZTOP");
    let transport = Scripted::ok(envelope(&[as_pairs(&row)], None));

    let error = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect_err("a non-numeric permaticker must not decode");
    let rendered = error.to_string();

    assert!(matches!(error, SourceError::Malformed { .. }));
    assert!(
        rendered.contains("not-a-number"),
        "the error must name the value it rejected, got {rendered}"
    );
}

#[test]
fn s5_rows_belonging_to_another_table_are_dropped() {
    // TICKERS keys on (table, permaticker, ticker), so one security appears
    // once per table it belongs to. Keeping them all would multiply the row.
    let sep = ticker_row("SEP", "199059", "ZZTOP");
    let sf1 = ticker_row("SF1", "199059", "ZZTOP");
    let sfp = ticker_row("SFP", "199059", "ZZTOP");
    let transport = Scripted::ok(envelope(
        &[as_pairs(&sf1), as_pairs(&sep), as_pairs(&sfp)],
        None,
    ));

    let rows = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect("fixture decodes");

    assert_eq!(rows.len(), 1, "one security must not become three rows");
    assert_eq!(
        rows[0].asset.permanent,
        Some(PermanentId::Sharadar(199_059))
    );
}

#[test]
fn s5_an_empty_request_asks_the_vendor_nothing() {
    // An empty ticker filter would fetch the entire table.
    let transport = Scripted::new(Vec::new());
    let rows = client(&transport).fetch_tickers(&[]).expect("no request");

    assert!(rows.is_empty());
    assert_eq!(transport.calls(), 0, "an empty request reached the vendor");
}

#[test]
fn s5_a_ticker_that_would_alter_the_query_is_refused() {
    let transport = Scripted::new(Vec::new());
    let error = client(&transport)
        .fetch_tickers(&["ZZ&api_key=stolen"])
        .expect_err("a ticker carrying a query separator must be refused");

    assert!(matches!(error, SourceError::Refused { .. }));
    assert_eq!(transport.calls(), 0, "the malformed request was still sent");
}

#[test]
fn s5_a_delisted_flag_is_read_strictly() {
    let mut row = ticker_row("SEP", "199059", "ZZTOP");
    for (name, value) in row.iter_mut() {
        if *name == "isdelisted" {
            *value = r#""Y""#.to_string();
        }
    }
    let transport = Scripted::ok(envelope(&[as_pairs(&row)], None));
    let rows = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect("decodes");
    assert!(rows[0].is_delisted);

    for (name, value) in row.iter_mut() {
        if *name == "isdelisted" {
            *value = r#""maybe""#.to_string();
        }
    }
    let transport = Scripted::ok(envelope(&[as_pairs(&row)], None));
    assert!(
        matches!(
            client(&transport).fetch_tickers(&["ZZTOP"]),
            Err(SourceError::Malformed { .. })
        ),
        "an unrecognised delisting flag must not become false"
    );
}

// --- S6: retry --------------------------------------------------------------

#[test]
fn s6_a_429_then_a_success_succeeds() {
    let row = ticker_row("SEP", "199059", "ZZTOP");
    let transport = Scripted::new(vec![
        Ok(HttpReply {
            status: 429,
            body: "rate limited".to_string(),
        }),
        Ok(HttpReply {
            status: 200,
            body: envelope(&[as_pairs(&row)], None),
        }),
    ]);

    let rows = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect("a retried request should succeed");

    assert_eq!(rows.len(), 1);
    assert_eq!(transport.calls(), 2);
}

#[test]
fn s6_four_consecutive_429s_surface_transport() {
    let transport = Scripted::new(
        (0..4)
            .map(|_| {
                Ok(HttpReply {
                    status: 429,
                    body: "rate limited".to_string(),
                })
            })
            .collect(),
    );

    let error = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect_err("exhausted retries must error");

    assert!(matches!(error, SourceError::Transport { .. }));
    assert_eq!(
        transport.calls(),
        2,
        "one attempt, one wait, one more, then fail"
    );
}

#[test]
fn s6_a_500_is_retried() {
    let row = ticker_row("SEP", "199059", "ZZTOP");
    let transport = Scripted::new(vec![
        Ok(HttpReply {
            status: 503,
            body: "unavailable".to_string(),
        }),
        Ok(HttpReply {
            status: 200,
            body: envelope(&[as_pairs(&row)], None),
        }),
    ]);

    assert_eq!(
        client(&transport)
            .fetch_tickers(&["ZZTOP"])
            .expect("a 503 should be retried")
            .len(),
        1
    );
    assert_eq!(transport.calls(), 2);
}

#[test]
fn s6_a_400_is_not_retried() {
    let transport = Scripted::new(vec![Ok(HttpReply {
        status: 400,
        body: "bad filter".to_string(),
    })]);

    let error = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect_err("a 400 must error");

    assert!(matches!(error, SourceError::Transport { .. }));
    assert_eq!(
        transport.calls(),
        1,
        "a request the client got wrong was repeated"
    );
}

#[test]
fn s6_a_401_is_unauthorized_and_is_not_retried() {
    let transport = Scripted::new(vec![Ok(HttpReply {
        status: 401,
        body: "unauthorized".to_string(),
    })]);

    let error = client(&transport)
        .fetch_tickers(&["ZZTOP"])
        .expect_err("a 401 must error");

    assert!(matches!(error, SourceError::Unauthorized { .. }));
    assert_eq!(transport.calls(), 1);
}
