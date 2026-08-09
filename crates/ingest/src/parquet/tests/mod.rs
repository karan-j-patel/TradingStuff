//! Round trip and strictness tests for the curated codec.
//!
//! Every value here is synthetic. Vendor licences forbid redistributing rows,
//! and that applies to a test fixture as much as to a data directory.
//!
//! The tests are written against the properties that would be expensive to lose
//! rather than against the happy path. Each one names, in its own doc comment,
//! the change it is meant to catch.

use jiff::civil::Date;
use rust_decimal::Decimal;

use super::*;
use crate::actions::{Convention, Delisting, DelistingReason, Listing, TerminalValue};
use crate::schema::{AssetKey, CloseKind, PermanentId, PriceBar, SessionScope};

/// The refusals and the reader's strictness checks. Split out only for size;
/// they share the fixtures above.
mod ordering;
mod strictness;
mod validation;

// --- fixtures ---------------------------------------------------------------

/// A directory of this test's own, emptied first so a previous run cannot make
/// a later one pass.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("curated-tests-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

fn day(year: i16, month: i8, d: i8) -> Date {
    Date::new(year, month, d).expect("valid test date")
}

fn dec(text: &str) -> Decimal {
    // `from_str_exact` refuses to round, so a test literal is exactly the value
    // written here rather than the nearest one the parser could reach.
    Decimal::from_str_exact(text).expect("test literal is an exact decimal")
}

fn sharadar(ticker: &str, id: u64) -> AssetKey {
    AssetKey {
        ticker: ticker.into(),
        permanent: Some(PermanentId::Sharadar(id)),
    }
}

fn bar(asset: AssetKey, date: Date, price: Decimal) -> PriceBar {
    PriceBar {
        asset,
        date,
        open: price,
        high: price,
        low: price,
        close: price,
        volume: dec("1000"),
        session: SessionScope::RegularHours,
        close_kind: CloseKind::ClosingAuction,
    }
}

fn delisting(asset: AssetKey, date: Date, terminal: TerminalValue) -> Delisting {
    Delisting {
        asset,
        date,
        reason: DelistingReason::Bankruptcy,
        listing: Listing::Nasdaq,
        terminal,
        source: "synthetic".into(),
    }
}

// --- T1: prices round trip --------------------------------------------------

/// Identity survives a round trip, including the distinction between a key that
/// carries a permanent identifier and one that does not.
///
/// The pair of `TWIN` rows is the point. `AssetKey` deliberately treats an
/// identified key and an unidentified key with the same ticker as different
/// securities, because equating them assumes the ticker was never reassigned.
/// A codec that flattened identity to the ticker would make them equal again
/// and the assertion below is what notices.
#[test]
fn t1_prices_round_trip_and_keep_identified_and_unidentified_keys_apart() {
    let path = scratch("t1").join("prices.parquet");

    let identified = bar(sharadar("TWIN", 199_059), day(2020, 1, 2), dec("10.25"));
    let anonymous = bar(AssetKey::ticker_only("TWIN"), day(2020, 1, 3), dec("11.50"));
    let other_namespace = bar(
        AssetKey {
            ticker: "AAAA".into(),
            permanent: Some(PermanentId::SecCik(320_193)),
        },
        day(2020, 1, 2),
        dec("7.00"),
    );

    // Deliberately not in the order the writer will choose, so the sort is
    // exercised rather than accidentally satisfied by the input order.
    let input = vec![
        anonymous.clone(),
        identified.clone(),
        other_namespace.clone(),
    ];
    assert_eq!(write_prices(input, &path).expect("write"), 3);

    let read = read_prices(&path).expect("read");
    assert_eq!(read.len(), 3, "every row must come back");

    // The two TWIN rows. Under a codec that flattened identity these compare
    // equal, which is the failure this test exists for.
    assert_ne!(
        read[1].asset, read[2].asset,
        "an identified key and an unidentified key sharing a ticker must not equate after read-back"
    );
    assert!(read[1].asset.is_stable(), "the permanent id was dropped");
    assert!(!read[2].asset.is_stable(), "a permanent id was invented");

    // `sec_cik` sorts before `sharadar`, and both sort before the null kind.
    let expected = vec![other_namespace, identified, anonymous];
    assert_eq!(read, expected, "rows must round trip exactly and in order");
}

// --- T2: delistings round trip ----------------------------------------------

/// All three `TerminalValue` states survive, and an imputed value does not read
/// back as an observed one.
///
/// The observed and imputed rows carry the same number on purpose. If the codec
/// stored only the number, the two would be indistinguishable on read and a
/// published convention would have quietly become a measurement.
#[test]
fn t2_delistings_round_trip_and_keep_imputed_apart_from_observed() {
    let path = scratch("t2").join("delistings.parquet");
    let same_value = dec("-0.55");

    let observed = delisting(
        sharadar("AAA", 111),
        day(2021, 3, 1),
        TerminalValue::Observed(same_value),
    );
    let imputed = delisting(
        sharadar("BBB", 222),
        day(2021, 3, 2),
        TerminalValue::Imputed {
            value: same_value,
            convention: Convention::ShumwayWarther1999Nasdaq,
        },
    );
    let unknown = delisting(
        sharadar("CCC", 333),
        day(2021, 3, 3),
        TerminalValue::Unknown,
    );

    let input = vec![unknown.clone(), imputed.clone(), observed.clone()];
    assert_eq!(write_delistings(input, &path).expect("write"), 3);

    let read = read_delistings(&path).expect("read");
    assert_eq!(read.len(), 3);

    // Sorted by permanent id as a string: "111", "222", "333".
    let (back_observed, back_imputed, back_unknown) = (&read[0], &read[1], &read[2]);

    assert_ne!(
        back_observed.terminal, back_imputed.terminal,
        "an imputed value read back as observed, so a convention became a fact"
    );
    assert!(back_observed.terminal.is_observed());
    assert!(
        !back_imputed.terminal.is_observed(),
        "the imputed row lost its imputed state"
    );
    assert!(
        matches!(
            back_imputed.terminal,
            TerminalValue::Imputed {
                convention: Convention::ShumwayWarther1999Nasdaq,
                ..
            }
        ),
        "the convention did not survive, got {:?}",
        back_imputed.terminal
    );
    assert_eq!(back_unknown.terminal, TerminalValue::Unknown);
    assert_eq!(
        back_observed.terminal.value(),
        back_imputed.terminal.value(),
        "the two rows were supposed to carry the same number"
    );

    let expected = vec![observed, imputed, unknown];
    assert_eq!(read, expected);
}

// --- T3: decimal exactness --------------------------------------------------

/// Values at every scale from 0 to 9 come back bit-for-bit, including one that
/// `f64` cannot hold.
///
/// `123456789.123456789` has 18 significant digits. An `f64` carries about 15
/// to 17, so a codec that routed the value through one returns
/// `123456789.12345679` and this test says so. That is the mutation it is here
/// to catch, and it is a realistic one because `as f64` compiles anywhere a
/// number is expected.
#[test]
fn t3_decimals_round_trip_exactly_at_every_scale_up_to_nine() {
    let path = scratch("t3").join("prices.parquet");

    let values = [
        "7",
        "7.1",
        "7.12",
        "7.123",
        "7.1234",
        "7.12345",
        "7.123456",
        "7.1234567",
        "7.12345678",
        "7.123456789",
        // 18 significant digits, the value f64 cannot represent.
        "123456789.123456789",
    ];

    let input: Vec<PriceBar> = values
        .iter()
        .enumerate()
        .map(|(index, text)| {
            bar(
                sharadar("EXACT", 1),
                day(2020, 1, 1 + index as i8),
                dec(text),
            )
        })
        .collect();

    assert_eq!(
        write_prices(input.clone(), &path).expect("write"),
        values.len()
    );
    let read = read_prices(&path).expect("read");

    // Per-value first, so a failure names the value that did not survive rather
    // than printing a diff of every row.
    for (want, got) in input.iter().zip(&read) {
        assert_eq!(
            want.close, got.close,
            "close for {} did not survive the round trip",
            want.date
        );
        // Numeric equality alone would accept a value that arrived with extra
        // precision from somewhere. Comparing normalised text says the digits
        // are the same digits.
        assert_eq!(
            want.close.normalize().to_string(),
            got.close.normalize().to_string(),
            "close for {} changed its digits",
            want.date
        );
    }

    assert_eq!(read, input);
}

/// A value needing more than 9 decimal places is a write error naming the
/// dataset, the field, and the value, rather than a rounded number.
#[test]
fn t3_a_scale_twelve_value_is_refused_and_named() {
    let directory = scratch("t3-scale12");
    let path = directory.join("prices.parquet");

    // The bar has to be domain-valid, or the writer's validation refuses it
    // first and this test silently stops exercising the decimal codec at all.
    let mut too_precise = bar(sharadar("TINY", 2), day(2020, 1, 2), dec("10.00"));
    too_precise.high = dec("11.00");
    too_precise.low = dec("9.00");
    too_precise.close = dec("10.000000000001");

    let error = write_prices(vec![too_precise], &path).expect_err("a scale-12 value must refuse");
    assert!(
        matches!(error, CurateError::InexactDecimal { .. }),
        "must fail on the decimal codec rather than on validation: {error:?}"
    );
    let message = error.to_string();

    assert!(
        message.contains("prices"),
        "must name the dataset: {message}"
    );
    assert!(message.contains("close"), "must name the field: {message}");
    assert!(
        message.contains("10.000000000001"),
        "must name the value: {message}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// Trailing zeros are not extra precision. A value written with 12 decimal
/// places that are all zero past the ninth is exactly representable and must be
/// accepted, or the check is rejecting on how a number was typed rather than on
/// what it is.
#[test]
fn t3_trailing_zeros_beyond_scale_nine_are_not_a_refusal() {
    let path = scratch("t3-zeros").join("prices.parquet");
    let mut padded = bar(sharadar("PAD", 3), day(2020, 1, 2), dec("10.00"));
    padded.high = dec("11.00");
    padded.low = dec("9.00");
    padded.close = dec("10.500000000000");

    write_prices(vec![padded], &path).expect("padded zeros are exactly representable");
    let read = read_prices(&path).expect("read");
    assert_eq!(read[0].close, dec("10.5"));
}
