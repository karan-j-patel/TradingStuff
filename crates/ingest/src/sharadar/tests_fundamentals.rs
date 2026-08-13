//! A1 through A6: the fundamentals decoder, on the JSON shape the host ships.
//!
//! **Every fixture is fabricated**, in the envelope shape the Phase A probe
//! observed. The shape is documentation; the rows are invented.
//!
//! # Why this file exists
//!
//! A blind fan-out ran a control probe: `decode_fundamental` was made to refuse
//! every row, and the whole suite stayed green. The decoder was reachable by
//! nothing. Everything below is written against a named mutation that survived
//! that fan-out, and the module doc of each test says which.
//!
//! Written as JSON rather than as constructed `FundamentalRow` values on
//! purpose. The hazards are about how a vendor cell crosses into a typed field,
//! and a fixture that skipped the JSON would skip the hazard.

use serde_json::{Value, json};

use super::columns::native;
use super::native::{ARQ, FundamentalRow, NativeRow, decode_fundamental, fundamentals_params};
use crate::provider::SourceError;

/// A well-formed ARQ row. Every test below starts here and breaks one thing.
///
/// The two dates are deliberately far apart and in the real order: a filing
/// published 2024-05-14 describing the quarter that ended 2024-03-31. A fixture
/// that gave them the same value would make the date-column swap invisible,
/// which is A1's whole subject.
fn base() -> Value {
    json!({
        "ticker": "AAAA",
        native::FILING_DATE: "2024-05-14",
        "reportperiod": "2024-03-31",
        "calendardate": "2024-12-31",
        "dimension": "ARQ",
        "lastupdated": "2026-08-01",
        "equity": 1234.5,
        "equityusd": 5000000000i64,
        "netinc": 10,
        "netinccmnusd": 11,
        "revenue": 12,
        "revenueusd": 13,
        "assets": 14,
        "sharesbas": 15,
        "shareswa": 16
    })
}

/// Decode one row, or report why not.
fn decode(row: Value) -> Result<FundamentalRow, SourceError> {
    let map = match row {
        Value::Object(map) => map,
        other => panic!("a row fixture must be an object, got {other}"),
    };
    decode_fundamental(&NativeRow::for_test(&map))
}

/// The same row with one column replaced.
fn with(column: &str, value: Value) -> Value {
    let Value::Object(mut map) = base() else {
        unreachable!("the base fixture is an object")
    };
    map.insert(column.to_string(), value);
    Value::Object(map)
}

/// The same row with one column removed.
fn without(column: &str) -> Value {
    let Value::Object(mut map) = base() else {
        unreachable!("the base fixture is an object")
    };
    map.remove(column);
    Value::Object(map)
}

/// A1. `as_reported` is the filing-date column and `period_end` is
/// `reportperiod`, and they are not interchangeable.
///
/// # The worst slip of the round
///
/// Swapping the two columns is lookahead by construction: the run would treat a
/// period end as a publication date and see every filing weeks or months before
/// anyone could have. It is the same failure the vendor's restated dimensions
/// have, arrived at from the other direction.
///
/// The fixture's dates differ and are in the real order, so a swap is visible.
/// Asserted in both directions, because an assertion on one field alone passes
/// against a decoder that wrote the same column into both.
#[test]
fn a1_the_two_dates_come_from_their_own_columns() {
    let row = decode(base()).expect("the base fixture decodes");

    assert_eq!(
        row.as_reported,
        jiff::civil::date(2024, 5, 14),
        "as_reported is not the filing-date column, so the run would see a \
         filing before it was published"
    );
    assert_eq!(
        row.period_end,
        jiff::civil::date(2024, 3, 31),
        "period_end is not the reportperiod column"
    );
    assert!(
        row.as_reported > row.period_end,
        "the fixture's dates must differ and be in the real order, or a swap \
         of the two columns is invisible here"
    );

    // Moving one column moves one field. A decoder reading the same cell twice
    // passes every assertion above and fails this.
    let moved = decode(with("reportperiod", json!("2024-02-29"))).expect("decodes");
    assert_eq!(moved.as_reported, row.as_reported);
    assert_eq!(moved.period_end, jiff::civil::date(2024, 2, 29));
}

/// A2. A JSON null becomes an absent map entry, never a zero.
///
/// For `equityusd` the two states are "this company reported no book value" and
/// "this company's book value is zero". The value rail excludes the second
/// before ranking and never sees the first, so a decoder that turned null into
/// zero would put unvaluable names into the portfolio at the most attractive
/// book-to-market there is.
#[test]
fn a2_a_null_column_is_absent_rather_than_zero() {
    let row = decode(with("equityusd", Value::Null)).expect("a null column decodes");

    assert_eq!(
        row.fields.get("equityusd"),
        None,
        "a null column became a map entry, so a company that reported nothing \
         is indistinguishable from one that reported zero"
    );
    // The other columns still arrive, so the null did not simply abort the row.
    assert_eq!(
        row.fields.get("revenue"),
        Some(&rust_decimal::Decimal::from(12)),
        "a null in one column dropped the others"
    );

    // A reported zero is a value and survives as one.
    let zero = decode(with("equityusd", json!(0))).expect("a zero decodes");
    assert_eq!(
        zero.fields.get("equityusd"),
        Some(&rust_decimal::Decimal::ZERO),
        "a reported zero came back absent, which is the same confusion the \
         other way round"
    );
}

/// A3. The dimension guard is an exact match on ARQ.
///
/// A prefix or `starts_with` test would admit `ARY` and `ART`, which are
/// as-reported but annual and trailing-twelve-month. Those are not wrong in the
/// lookahead sense, and admitting them is still wrong: they would land in the
/// same table under the quarterly scope this decoder hardcodes, so a year's
/// revenue would be stored as a quarter's.
///
/// The restated three are the lookahead case and are refused by the same
/// comparison.
#[test]
fn a3_the_dimension_guard_is_an_exact_match() {
    assert!(decode(base()).is_ok(), "the ARQ fixture must decode");

    // The two that a prefix match on "AR" would let through.
    for admitted_by_a_prefix in ["ARY", "ART"] {
        let error = decode(with("dimension", json!(admitted_by_a_prefix)))
            .expect_err(&format!("{admitted_by_a_prefix} must be refused"));
        assert!(
            error.to_string().contains(admitted_by_a_prefix),
            "the refusal does not name the dimension it refused"
        );
    }

    // The restated three, which are the lookahead case.
    for restated in ["MRQ", "MRY", "MRT"] {
        assert!(
            decode(with("dimension", json!(restated))).is_err(),
            "{restated} was accepted, and a restated row dates its values by a \
             period end while they were corrected later"
        );
    }

    assert_eq!(ARQ, "ARQ");
}

/// A4. A response missing a column is an error, not a silent success.
///
/// `ticker` is read and discarded by the decoder, which only makes sense if
/// reading it is what turns a dropped column into a failure. A decoder that
/// stopped reading it would accept a response shaped like nothing this vendor
/// sends.
///
/// The other required columns are checked too: a missing date is the same class
/// of fault and there is no reason for one to be guarded and not the others.
#[test]
fn a4_a_missing_column_is_refused() {
    // The control: with every column present the row decodes, so the refusals
    // below are about the missing column and not about the fixture.
    assert!(decode(base()).is_ok());

    for column in ["ticker", native::FILING_DATE, "reportperiod", "dimension"] {
        let error = decode(without(column))
            .err()
            .unwrap_or_else(|| panic!("a response with no {column} column was accepted"));
        assert!(
            error.to_string().contains(column),
            "the refusal does not name the missing column {column}, got {error}"
        );
    }
}

/// A5. There is no fallback from `equityusd` to the bare `equity` column.
///
/// The bare columns are in the filer's REPORTING currency and only the `*usd`
/// ones are comparable across a mixed-currency universe. A fallback would put a
/// yen-denominated book value into a dollar-denominated ratio and produce a
/// book-to-market off by roughly a factor of a hundred and fifty, silently, on
/// exactly the foreign filers a value screen finds cheapest.
#[test]
fn a5_there_is_no_fallback_from_equityusd_to_the_reporting_currency_column() {
    let row = decode(with("equityusd", Value::Null)).expect("decodes");

    assert_eq!(
        row.fields.get("equityusd"),
        None,
        "a row with no equityusd carries one anyway, so the reporting-currency \
         column was substituted for it"
    );
    // The bare column is still stored under its own name, which is what makes
    // the absence above a real absence rather than a dropped column.
    assert_eq!(
        row.fields.get("equity"),
        Some(&rust_decimal::Decimal::from_str_exact("1234.5").expect("literal")),
        "the reporting-currency column is missing, so this test cannot tell a \
         fallback from a column that was never read"
    );
}

/// A6. The outgoing request names the ARQ dimension.
///
/// The decoder refuses a restated row if one arrives, which is the second line
/// of defence. This is the first: a request that stopped naming the dimension
/// would ask for all six and be answered with all six, and the walk would spend
/// its quota fetching rows the decoder then throws away.
#[test]
fn a6_the_request_names_the_arq_dimension() {
    let params = fundamentals_params("AAAA");

    let dimension = params
        .iter()
        .find(|(key, _)| *key == "dimension")
        .map(|(_, value)| value.as_str());
    assert_eq!(
        dimension,
        Some("ARQ"),
        "the request does not filter on the ARQ dimension, so the vendor would \
         answer it with the restated rows too"
    );

    assert_eq!(
        params
            .iter()
            .find(|(key, _)| *key == "ticker")
            .map(|(_, value)| value.as_str()),
        Some("AAAA")
    );

    // Every column the decoder reads has to be asked for, or the response
    // arrives without it and A4's refusal fires on every single row.
    let fields = params
        .iter()
        .find(|(key, _)| *key == "fields")
        .map(|(_, value)| value.as_str())
        .expect("the request names its fields");
    for column in [
        "ticker",
        native::FILING_DATE,
        "reportperiod",
        "dimension",
        "equityusd",
    ] {
        assert!(
            fields.split(',').any(|named| named == column),
            "the request does not ask for {column}, which the decoder reads. \
             Got {fields}"
        );
    }
}
