//! The market cap dataset: round trip, units label, and the refusals.
//!
//! Every value here is synthetic. The vendor's own figures live only in the
//! gitignored probe output, because the licence covers rows wherever they are
//! written down. What is reproduced is the *shape* the probe measured: a figure
//! in millions to one decimal place.

use super::*;
use crate::marketcap::MarketCapRecord;
use crate::parquet::{UNITS, UNITS_KEY, marketcap_provenance, read_marketcap, write_marketcap};

fn cap(asset: AssetKey, date: Date, marketcap: Decimal) -> MarketCapRecord {
    MarketCapRecord {
        asset,
        date,
        marketcap,
        source: "synthetic".into(),
    }
}

// --- M1: round trip ---------------------------------------------------------

/// Rows survive a round trip, and identity keeps an identified key apart from
/// an unidentified one sharing its ticker.
///
/// Same property `t1` pins for prices, and it matters here for the same reason:
/// a codec that flattened identity to the ticker would weld two companies'
/// size histories together the first time a ticker was reassigned.
#[test]
fn m1_marketcap_rows_round_trip_and_keep_identities_apart() {
    let path = scratch("m1").join("marketcap.parquet");

    let identified = cap(
        sharadar("TWIN", 199_059),
        day(2024, 12, 30),
        dec("1234567.8"),
    );
    let anonymous = cap(
        AssetKey::ticker_only("TWIN"),
        day(2024, 12, 31),
        dec("2345678.9"),
    );

    // Not in the order the writer will choose, so the sort is exercised rather
    // than accidentally satisfied by the input order.
    let input = vec![anonymous.clone(), identified.clone()];
    assert_eq!(
        write_marketcap(input, &path, "synthetic").expect("write"),
        2
    );

    let read = read_marketcap(&path).expect("read");
    assert_eq!(read.len(), 2, "every row must come back");
    assert_ne!(
        read[0].asset, read[1].asset,
        "an identified key and an unidentified key sharing a ticker must not equate"
    );
    assert!(read[0].asset.is_stable(), "the permanent id was dropped");
    assert!(!read[1].asset.is_stable(), "a permanent id was invented");
    assert_eq!(read, vec![identified, anonymous]);
}

// --- M2: store as shipped ---------------------------------------------------

/// The figure lands byte for byte, with no unit conversion anywhere on the way.
///
/// This is the test the whole dataset exists to satisfy. The figure is in
/// millions, and the single most plausible "helpful" change anybody could make
/// to this codec is to multiply by a million on the way in so the column reads
/// in dollars. The assertions below are on the digits rather than only on
/// numeric equality, because a rescaled value is still a perfectly good number
/// and only its digits say it is the wrong one.
#[test]
fn m2_the_shipped_figure_is_stored_without_rescaling() {
    let path = scratch("m2").join("marketcap.parquet");

    // One decimal place, as the vendor ships. Written out as text so the
    // literal in this file is exactly the value under test.
    let shipped = dec("1234567.8");
    write_marketcap(
        vec![cap(sharadar("EXACT", 1), day(2024, 12, 31), shipped)],
        &path,
        "synthetic",
    )
    .expect("write");

    let read = read_marketcap(&path).expect("read");
    assert_eq!(read[0].marketcap, shipped);
    assert_eq!(
        read[0].marketcap.normalize().to_string(),
        "1234567.8",
        "the stored digits are not the shipped digits, so something rescaled the figure"
    );
    // A conversion to dollars would land here, and it is worth naming so a
    // failure says what happened rather than only that two numbers differ.
    assert_ne!(
        read[0].marketcap,
        shipped * dec("1000000"),
        "the figure was converted to dollars on the way in"
    );
}

/// A figure needing more than nine decimal places is refused, not rounded.
///
/// The vendor ships one decimal place, so this cannot arise from the fetch. It
/// can arise from a caller doing arithmetic first, which is exactly the caller
/// this dataset should refuse to store for.
#[test]
fn m2_a_figure_beyond_scale_nine_is_refused_and_named() {
    let directory = scratch("m2-scale");
    let path = directory.join("marketcap.parquet");

    let error = write_marketcap(
        vec![cap(
            sharadar("TINY", 2),
            day(2024, 12, 31),
            dec("1.0000000000001"),
        )],
        &path,
        "synthetic",
    )
    .expect_err("a scale-13 figure must refuse");

    assert!(
        matches!(
            error,
            CurateError::InexactDecimal {
                field: "marketcap",
                ..
            }
        ),
        "got {error:?}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

// --- M3: the units label ----------------------------------------------------

/// The units reach the file's own metadata, where a reader outside this crate
/// can see them.
#[test]
fn m3_the_units_label_reaches_the_file_metadata() {
    let path = scratch("m3").join("marketcap.parquet");
    write_marketcap(
        vec![cap(sharadar("AAA", 1), day(2024, 12, 31), dec("1234567.8"))],
        &path,
        "synthetic-vendor",
    )
    .expect("write");

    let provenance = marketcap_provenance(&path).expect("the file declares itself");
    assert_eq!(provenance.units, UNITS);
    assert_eq!(provenance.units, "usd_millions_as_shipped");
    assert_eq!(provenance.source, "synthetic-vendor");
}

/// A file carrying no units label is refused before a single row is decoded.
///
/// Without the label the column is unreadable in the only sense that matters: a
/// reader can get values out and cannot know whether they are dollars or
/// millions. The two answers differ by a factor of a million, which is not a
/// discrepancy anybody would spot in a portfolio weight.
#[test]
fn m3_a_file_without_the_units_label_is_refused() {
    let path = scratch("m3-missing").join("marketcap.parquet");
    write_with_metadata(&path, &[(SOURCE_KEY, "synthetic")]);

    let error = read_marketcap(&path).expect_err("a file with no units label must not read");
    assert!(
        matches!(error, CurateError::MissingMetadata { key: UNITS_KEY, .. }),
        "got {error:?}"
    );
}

/// A file whose source label is blank is refused, while the VALUE stays open.
///
/// The reader deliberately does not pin the source to one vendor, because the
/// platform is provider-abstracted and another vendor's legitimately curated
/// file must stay readable. What it refuses is attribution that says nothing:
/// a whitespace source passes a presence check while carrying no provenance,
/// which is the gap a presence-only rule leaves open.
#[test]
fn m3_a_blank_source_is_refused_and_a_foreign_source_is_not() {
    let blank = scratch("m3-blank").join("marketcap.parquet");
    write_with_metadata(&blank, &[(UNITS_KEY, UNITS), (SOURCE_KEY, "  ")]);
    let error = read_marketcap(&blank).expect_err("blank attribution must not read");
    assert!(
        matches!(
            error,
            CurateError::UnexpectedMetadata {
                key: SOURCE_KEY,
                ..
            }
        ),
        "got {error:?}"
    );

    let foreign = scratch("m3-foreign").join("marketcap.parquet");
    write_with_metadata(
        &foreign,
        &[(UNITS_KEY, UNITS), (SOURCE_KEY, "another-vendor")],
    );
    let provenance = marketcap_provenance(&foreign).expect("a named foreign source reads");
    assert_eq!(provenance.source, "another-vendor");
}

/// A file claiming units this reader does not understand is refused.
///
/// The realistic case is a second units convention arriving later, whether from
/// another vendor or from a change at this one. Reading such a file with this
/// decoder would put figures a million out into whatever consumed them.
#[test]
fn m3_a_file_claiming_unknown_units_is_refused() {
    let path = scratch("m3-wrong").join("marketcap.parquet");
    write_with_metadata(
        &path,
        &[(UNITS_KEY, "usd_whole_dollars"), (SOURCE_KEY, "synthetic")],
    );

    let error = read_marketcap(&path).expect_err("unknown units must not read");
    assert!(
        matches!(
            error,
            CurateError::UnexpectedMetadata {
                key: UNITS_KEY,
                expected: UNITS,
                ..
            }
        ),
        "got {error:?}"
    );
}

/// A file with a units label and no source is refused too.
///
/// Both keys or neither. A figure whose vendor is unknown cannot be reconciled
/// against anything later, which is the same rule the prices reader applies.
#[test]
fn m3_a_file_without_a_source_is_refused() {
    let path = scratch("m3-nosource").join("marketcap.parquet");
    write_with_metadata(&path, &[(UNITS_KEY, UNITS)]);

    let error = read_marketcap(&path).expect_err("a file with no source must not read");
    assert!(
        matches!(
            error,
            CurateError::MissingMetadata {
                key: SOURCE_KEY,
                ..
            }
        ),
        "got {error:?}"
    );
}

/// Write a well-formed marketcap file carrying exactly the metadata given.
///
/// The writer always writes both keys, which is the point of it, so a file
/// missing one has to be built here. It goes through the same schema and the
/// same arrow writer, so what is under test is the reader's metadata check and
/// nothing else.
fn write_with_metadata(path: &Path, metadata: &[(&str, &str)]) {
    use arrow::array::{ArrayRef, Date32Array, Decimal128Array, StringArray};
    use arrow::record_batch::RecordBatch;
    use std::sync::Arc;

    let schema = crate::parquet::marketcap::schema();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["AAA"])) as ArrayRef,
            Arc::new(StringArray::from(vec![None::<&str>])),
            Arc::new(StringArray::from(vec![None::<&str>])),
            Arc::new(Date32Array::from(vec![20_088])),
            Arc::new(
                Decimal128Array::from(vec![1_234_567_800_000_000i128])
                    .with_precision_and_scale(38, 9)
                    .expect("a valid decimal column"),
            ),
            Arc::new(StringArray::from(vec!["synthetic"])),
        ],
    )
    .expect("a valid batch");

    crate::parquet::write_atomically(path, |file| {
        let mut writer = ::parquet::arrow::ArrowWriter::try_new(
            file,
            schema,
            Some(crate::parquet::writer_properties(metadata)),
        )?;
        writer.write(&batch)?;
        writer.close()?;
        Ok(())
    })
    .expect("the fixture writes");
}

// --- M4: the writer refuses rather than skips -------------------------------

/// A non-positive figure fails the whole write, and the error names the row.
///
/// Refused rather than skipped, and that is the ruling this test exists to
/// hold. This table expresses "no figure" by having no row, measured, so a
/// dropped bad row would be indistinguishable from the absence that happens
/// routinely. The write fails instead, and the message names what was refused
/// so the operator can see which name and date to look at.
#[test]
fn m4_a_zero_figure_is_a_real_sub_quantum_value_and_round_trips() {
    // Learned in production on the first full-universe walk, 2026-08-12:
    // 4387 of 2186126 rows carried marketcap 0, all on distressed names
    // (DMCSQ and kin, 2001-2002). The vendor quantises to one decimal of a
    // million, so a sub-$50k market cap legitimately rounds to 0.0 on a row
    // the vendor deliberately shipped. Zero is a value below the quantum,
    // not corruption; negative remains corruption.
    let path = scratch("m4-zero").join("marketcap.parquet");
    let rows = vec![
        cap(sharadar("GOOD", 1), day(2024, 12, 30), dec("1234567.8")),
        cap(sharadar("DUST", 2), day(2024, 12, 31), dec("0")),
    ];
    let written = write_marketcap(rows, &path, "synthetic").expect("a zero figure writes");
    assert_eq!(written, 2);

    let back = read_marketcap(&path).expect("reads back");
    assert_eq!(back[1].marketcap, dec("0"));
}

#[test]
fn m4_a_negative_figure_fails_the_write_and_names_it() {
    let directory = scratch("m4");
    let path = directory.join("marketcap.parquet");

    for bad in ["-0.1", "-1234.5"] {
        let rows = vec![
            cap(sharadar("GOOD", 1), day(2024, 12, 30), dec("1234567.8")),
            cap(sharadar("BAD", 2), day(2024, 12, 31), dec(bad)),
        ];

        let error = match write_marketcap(rows, &path, "synthetic") {
            Err(error) => error,
            Ok(written) => panic!("a {bad} figure was written anyway, {written} row(s) landed"),
        };

        assert!(
            matches!(
                error,
                CurateError::InvalidRecords {
                    rejected: 1,
                    total: 2,
                    ..
                }
            ),
            "one row of two must be named as rejected, got {error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("BAD") && message.contains("2024-12-31"),
            "the refusal must name the row it refused: {message}"
        );
        assert!(
            message.contains(bad),
            "the refusal must quote the offending figure: {message}"
        );
        assert!(!path.exists(), "a refused write must leave no file behind");
    }
}

/// An empty ticker or an empty source fails the write.
///
/// A row with no provenance cannot be reconciled, and a row with no ticker
/// cannot be joined to anything. Same rule the other datasets apply.
#[test]
fn m4_an_empty_ticker_or_source_fails_the_write() {
    let directory = scratch("m4-identity");
    let path = directory.join("marketcap.parquet");

    let mut blank_ticker = cap(
        AssetKey::ticker_only("   "),
        day(2024, 12, 31),
        dec("1234567.8"),
    );
    blank_ticker.source = "synthetic".into();
    let error = write_marketcap(vec![blank_ticker], &path, "synthetic")
        .expect_err("a blank ticker must not be written");
    assert!(
        matches!(error, CurateError::InvalidRecords { .. }),
        "got {error:?}"
    );

    let mut blank_source = cap(sharadar("AAA", 1), day(2024, 12, 31), dec("1234567.8"));
    blank_source.source = "  ".into();
    let error = write_marketcap(vec![blank_source], &path, "synthetic")
        .expect_err("a blank source must not be written");
    assert!(
        matches!(error, CurateError::InvalidRecords { .. }),
        "got {error:?}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// Two rows for one security on one date are a duplicate, not a silent merge.
#[test]
fn m4_a_duplicate_security_and_date_is_refused() {
    let path = scratch("m4-dup").join("marketcap.parquet");
    let rows = vec![
        cap(sharadar("AAA", 1), day(2024, 12, 31), dec("1234567.8")),
        cap(sharadar("AAA", 1), day(2024, 12, 31), dec("7654321.0")),
    ];

    let error = write_marketcap(rows, &path, "synthetic").expect_err("a duplicate must refuse");
    assert!(
        matches!(error, CurateError::DuplicateRow { .. }),
        "got {error:?}"
    );
}
