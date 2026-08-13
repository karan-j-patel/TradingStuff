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
use crate::adjusted::AdjustedBar;
use crate::schema::{AssetKey, CloseKind, PermanentId, SessionScope};

/// The refusals and the reader's strictness checks. Split out only for size;
/// they share the fixtures above.
mod corporate_actions;
mod marketcap;
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

/// `close_unadjusted` is deliberately not equal to `close`: four times it, as
/// after a 4-for-1 split. A helper that made every price column the same value
/// would let the writer or reader swap two of them and every round-trip
/// assertion would still pass.
fn bar(asset: AssetKey, date: Date, price: Decimal) -> AdjustedBar {
    AdjustedBar {
        asset,
        date,
        open: price,
        high: price,
        low: price,
        close: price,
        volume: dec("1000"),
        close_unadjusted: price * dec("4"),
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
        // Absent by default. The one test that cares supplies its own value,
        // so every other delisting fixture demonstrably exercises the null
        // column rather than a number that happened to be lying around.
        final_market_cap: None,
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
    assert_eq!(write_prices(input, &path, "synthetic").expect("write"), 3);

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
    assert_eq!(
        write_delistings(input, &path, "synthetic").expect("write"),
        3
    );

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

/// The final market cap column keeps absent and zero apart across a round trip.
///
/// The same hazard the decoder carries, one layer down. A vendor exit row ships
/// JSON null on a fund wind-up and a real zero on a bankruptcy liquidation, and
/// a storage layer that collapsed the two would undo the decoder's care on the
/// way to disk. The three rows are written in one batch so the null and the zero
/// share a column chunk, which is where a collapse would happen.
#[test]
fn t2_the_final_market_cap_keeps_a_zero_apart_from_an_absent_figure() {
    let path = scratch("t2-finalcap").join("delistings.parquet");

    let mut absent = delisting(
        sharadar("NONE", 111),
        day(2021, 3, 1),
        TerminalValue::Unknown,
    );
    absent.final_market_cap = None;
    let mut zero = delisting(
        sharadar("ZERO", 222),
        day(2021, 3, 2),
        TerminalValue::Unknown,
    );
    zero.final_market_cap = Some(dec("0"));
    let mut real = delisting(
        sharadar("REAL", 333),
        day(2021, 3, 3),
        TerminalValue::Unknown,
    );
    real.final_market_cap = Some(dec("2047.5"));

    assert_eq!(
        write_delistings(
            vec![zero.clone(), real.clone(), absent.clone()],
            &path,
            "synthetic"
        )
        .expect("write"),
        3
    );
    let read = read_delistings(&path).expect("read");

    // Sorted by permanent id as a string: "111", "222", "333".
    assert_eq!(
        read[0].final_market_cap, None,
        "an absent figure gained one"
    );
    assert_eq!(
        read[1].final_market_cap,
        Some(dec("0")),
        "a real final market capitalisation of zero read back as absent"
    );
    assert_ne!(read[1].final_market_cap, read[0].final_market_cap);
    assert_eq!(read[2].final_market_cap, Some(dec("2047.5")));
    assert_eq!(read, vec![absent, zero, real]);
}

/// A written delistings file declares its own units and source.
///
/// The refusal half, that a file without the declaration is unreadable, lives
/// in `strictness` beside the raw writer that can produce one.
#[test]
fn t2_a_delistings_file_declares_its_units_and_source() {
    let path = scratch("t2-provenance").join("delistings.parquet");
    let row = delisting(sharadar("GONE", 1), day(2021, 3, 1), TerminalValue::Unknown);
    write_delistings(vec![row], &path, "synthetic").expect("write");

    let declared = delistings_provenance(&path).expect("a written file declares itself");
    assert_eq!(
        declared.units, UNITS,
        "the final market cap column is in vendor millions and the file must say so"
    );
    assert_eq!(declared.source, "synthetic");
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

    let input: Vec<AdjustedBar> = values
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
        write_prices(input.clone(), &path, "synthetic").expect("write"),
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

    let error = write_prices(vec![too_precise], &path, "synthetic")
        .expect_err("a scale-12 value must refuse");
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

    write_prices(vec![padded], &path, "synthetic").expect("padded zeros are exactly representable");
    let read = read_prices(&path).expect("read");
    assert_eq!(read[0].close, dec("10.5"));
}

// --- P5: the synthetic twin of the measured probe ---------------------------
//
// The live probe on 2026-08-09 measured two arithmetic regimes in the vendor's
// split adjustment. Its verbatim output is recorded at
// `ContinuationDocs/2026-08-09-probe-output.md`, which is gitignored, and no
// vendor row is committed anywhere: Sharadar's licence forbids redistributing
// rows and CLAUDE.md applies that to fixtures as strictly as to a data
// directory.
//
// So the facts are preserved as a twin. Every number below is fabricated and
// engineered to reproduce the measured behaviour exactly. The free window rolls
// forward and the 2022 splits leave it around mid-2027; these tests do not.

/// A bar carrying one candidate reconstruction of a traded open.
///
/// `high` and `low` are deliberately wide so validation passes and the only
/// thing under test is whether the codec will store the value.
fn reconstruction_bar(
    reconstructed_open: Decimal,
    close: Decimal,
    unadjusted: Decimal,
) -> AdjustedBar {
    AdjustedBar {
        asset: sharadar("TWIN", 1),
        date: day(2022, 8, 22),
        open: reconstructed_open,
        high: dec("1000000"),
        low: dec("0.01"),
        close,
        volume: dec("1000"),
        close_unadjusted: unadjusted,
        session: SessionScope::RegularHours,
        close_kind: CloseKind::ClosingAuction,
    }
}

/// The 20-for-1 regime: the factor is exactly 0.05, so reconstruction is clean.
///
/// Twin of the measured GOOG and AMZN windows, where `close x 20` reproduced
/// `closeunadj` with a residual of exactly 0 and every reconstruction needed no
/// more than two decimal places.
#[test]
fn p5_a_twenty_for_one_twin_reconstructs_exactly_and_stores() {
    let close = dec("100.00");
    let unadjusted = dec("2000.00");
    let adjusted_open = dec("105.00");

    // The vendor's own adjustment is exact in this regime.
    assert_eq!(close * dec("20"), unadjusted, "the twin must be exact");

    // open / (close / closeunadj), as one division so nothing rounds twice.
    let reconstructed = (adjusted_open * unadjusted) / close;
    assert_eq!(reconstructed.normalize(), dec("2100"));
    assert!(
        reconstructed.normalize().scale() <= 9,
        "a clean regime must land inside the codec's scale"
    );

    let path = scratch("p5-clean").join("prices.parquet");
    write_prices(
        vec![reconstruction_bar(reconstructed, close, unadjusted)],
        &path,
        "synthetic",
    )
    .expect("an exactly representable reconstruction stores");
}

/// The 3-for-1 regime: the vendor already rounded, so nothing reconstructs.
///
/// Twin of the measured TSLA window. Two facts are reproduced. The adjusted
/// close misses the traded close by a residual in the 0.001 class, and the
/// implied factor therefore differs from a true third, so dividing by it gives
/// a repeating decimal far past the scale the curated codec stores.
#[test]
fn p5_a_three_for_one_twin_is_refused_rather_than_rounded() {
    let unadjusted = dec("600.01");
    // 600.01 / 3 is 200.00333..., which the vendor ships rounded to 3 places.
    let close = dec("200.003");
    let adjusted_open = dec("210.005");

    // Fact one: the vendor's adjustment is not exact here.
    let residual = unadjusted - close * dec("3");
    assert_eq!(residual, dec("0.001"), "the twin must carry the residual");

    // Fact two: the reconstruction is a repeating decimal.
    let reconstructed = (adjusted_open * unadjusted) / close;
    assert!(
        reconstructed.normalize().scale() > 9,
        "the twin must need more scale than the codec stores, got {}",
        reconstructed.normalize()
    );

    // And the codec refuses it rather than rounding it into range. Rounding
    // would produce a traded price nobody traded at, which is the whole reason
    // this dataset stores what the vendor shipped instead.
    let path = scratch("p5-repeating").join("prices.parquet");
    let error = write_prices(
        vec![reconstruction_bar(reconstructed, close, unadjusted)],
        &path,
        "synthetic",
    )
    .expect_err("a repeating reconstruction must not be stored");

    assert!(
        matches!(error, CurateError::InexactDecimal { field: "open", .. }),
        "got {error:?}"
    );
}

/// The control: no split in the window, so the two closes agree.
#[test]
fn p5_a_no_split_twin_has_one_close() {
    let price = dec("250.00");
    let bar = reconstruction_bar(price, price, price);
    assert_eq!(bar.close, bar.close_unadjusted);

    let path = scratch("p5-control").join("prices.parquet");
    write_prices(vec![bar], &path, "synthetic").expect("a control row stores");
    let read = read_prices(&path).expect("read");
    assert_eq!(read[0].close, read[0].close_unadjusted);
}
