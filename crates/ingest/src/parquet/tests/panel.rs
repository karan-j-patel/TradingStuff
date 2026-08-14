//! X-R8, and the panel dataset's round trip.
//!
//! X-R8 is the claim that a panel file cannot be read without saying which
//! configuration and which five files produced it. The engine half of the
//! export, and the claim that the digests written are the ones the run
//! recorded, live in `crates/engine` and `crates/cli` respectively.
//!
//! Every value here is synthetic.

use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray, Date32Array, Decimal128Array, StringArray};
use arrow::record_batch::RecordBatch;

use super::super::*;
use super::{day, dec, scratch, sharadar};
use crate::parquet::panel::{CONFIG_HASH_KEY, DIGEST_KEYS};

/// A provenance block with a distinct value per key.
///
/// Distinct rather than repeated, because six keys carrying the same string
/// would let the writer put the filings digest under the actions key and every
/// round-trip assertion would still pass.
fn provenance() -> PanelProvenance {
    PanelProvenance {
        config_hash: "c".repeat(64),
        universe_sha256: "1".repeat(64),
        prices_sha256: "7".repeat(64),
        actions_sha256: "2".repeat(64),
        delistings_sha256: "3".repeat(64),
        marketcap_sha256: "4".repeat(64),
        filings_sha256: "5".repeat(64),
    }
}

/// One row with a distinct value in every characteristic column.
///
/// The distinctness is the point. Eight decimal columns of the same type sit
/// side by side, so a writer and a reader that agreed on the wrong order would
/// round-trip perfectly if every value were the same number.
fn full_row() -> PanelRow {
    PanelRow {
        asset: sharadar("AAA", 1),
        month_end: day(2020, 1, 31),
        momentum_12_1: Some(dec("0.11")),
        vol_daily_12m: Some(dec("0.22")),
        vol_monthly_36m: Some(dec("0.33")),
        log_marketcap: Some(dec("0.44")),
        book_to_market: Some(dec("0.55")),
        dividend_yield_12m: Some(dec("0.66")),
        share_change_24m: Some(dec("0.77")),
        median_dollar_volume_12m: Some(dec("0.88")),
        eligible: true,
        label_return_1m: Some(dec("0.99")),
        label_delisted_in_window: Some(false),
    }
}

/// The same key at a later month with nothing computable, which is the shape
/// most of a real panel's early rows take.
fn null_row() -> PanelRow {
    PanelRow {
        asset: sharadar("BBB", 2),
        month_end: day(2020, 1, 31),
        momentum_12_1: None,
        vol_daily_12m: None,
        vol_monthly_36m: None,
        log_marketcap: None,
        book_to_market: None,
        dividend_yield_12m: None,
        share_change_24m: None,
        median_dollar_volume_12m: None,
        eligible: false,
        label_return_1m: None,
        label_delisted_in_window: None,
    }
}

/// Every column survives the round trip, and a null stays a null.
///
/// The null half is not decoration. A reader that read a null decimal as zero
/// would pass every assertion about the populated row and would hand the fit
/// script a momentum signal of exactly zero for every name that had none, which
/// is the middle of the cross-section rather than the edge.
#[test]
fn a_panel_round_trips_every_column_including_its_nulls() {
    let path = scratch("panel-roundtrip").join("panel.parquet");
    let written = write_panel(vec![full_row(), null_row()], &path, &provenance())
        .expect("the fixture writes");
    assert_eq!(written, 2);

    let read = read_panel(&path).expect("the fixture reads back");
    assert_eq!(read, vec![full_row(), null_row()]);

    // And from bytes, which is the route a caller that hashed the file takes.
    let bytes = bytes::Bytes::from(std::fs::read(&path).expect("reading the file"));
    assert_eq!(
        read_panel_from_bytes(bytes).expect("the same file reads from bytes"),
        read,
        "the two entry points disagree, so one of them validates less than the other"
    );
}

/// A characteristic carrying more decimal places than the column holds is
/// rounded to the column scale rather than refused.
///
/// This dataset is the one exception to the codec's "nothing here rounds" rule
/// and it is deliberate: these are ratios of `Decimal`s and carry the type's
/// full twenty-eight places, so refusing them would mean no panel could ever be
/// written. The rounding happens at this writer, so the engine's in-memory
/// values stay exact for the equality tests that pin them, and nothing else in
/// the codebase gains a rounding path.
#[test]
fn a_characteristic_longer_than_the_column_scale_is_rounded_to_it() {
    let path = scratch("panel-rounding").join("panel.parquet");
    // One third, to the type's full precision. Nine places of it is
    // 0.333333333, and the tenth place is a 3, so the rounding is downwards and
    // the expected value is exact rather than a boundary case.
    let third = dec("1")
        .checked_div(dec("3"))
        .expect("one third is representable");
    assert!(third.scale() > 9, "the fixture value is not long enough");

    let row = PanelRow {
        momentum_12_1: Some(third),
        ..full_row()
    };
    write_panel(vec![row], &path, &provenance()).expect("a long value writes");

    let read = read_panel(&path).expect("it reads back");
    assert_eq!(read[0].momentum_12_1, Some(dec("0.333333333")));
    // Every other column is untouched by the rounding of one of them.
    assert_eq!(read[0].vol_daily_12m, Some(dec("0.22")));
}

/// X-R8. A file with no configuration hash is refused before a row is decoded.
///
/// Without it nobody can say which windows, which staleness bound or which
/// universe produced the numbers, and every one of those changes what the
/// columns mean while leaving them looking identical.
#[test]
fn x_r8_a_file_without_the_configuration_hash_is_refused() {
    let path = scratch("x-r8-no-config").join("panel.parquet");
    let mut metadata = full_metadata();
    metadata.retain(|(key, _)| *key != CONFIG_HASH_KEY);
    write_with_metadata(&path, &metadata);

    let error = read_panel(&path).expect_err("a file with no configuration hash must not read");
    assert!(
        matches!(
            error,
            CurateError::MissingMetadata {
                key: CONFIG_HASH_KEY,
                ..
            }
        ),
        "got {error:?}"
    );
}

/// X-R8. A file missing any one of the five dataset digests is refused, and the
/// error names which one.
///
/// Each key is dropped in turn rather than all five at once. A reader that
/// checked only the first would pass four of these five, and a panel whose
/// filings digest is absent is one whose book-to-market column cannot be traced
/// to a file at all.
#[test]
fn x_r8_a_file_missing_any_dataset_digest_is_refused_and_named() {
    for missing in DIGEST_KEYS {
        let path = scratch(&format!("x-r8-no-{missing}")).join("panel.parquet");
        let mut metadata = full_metadata();
        metadata.retain(|(key, _)| key != missing);
        write_with_metadata(&path, &metadata);

        match read_panel(&path).expect_err("a file missing a digest must not read") {
            CurateError::MissingMetadata { key, .. } => assert_eq!(
                key, *missing,
                "the refusal names {key} while {missing} is the key that is absent"
            ),
            other => panic!("got {other:?} for a missing {missing}"),
        }
    }
}

/// X-R8. A key that is present and blank is refused too.
///
/// The gap a presence-only rule leaves open. A whitespace digest passes a
/// lookup and carries no provenance, so the file reads as though its inputs
/// were named when nothing was named.
#[test]
fn x_r8_a_blank_digest_is_refused() {
    let path = scratch("x-r8-blank").join("panel.parquet");
    let mut metadata = full_metadata();
    for entry in metadata.iter_mut() {
        if entry.0 == DIGEST_KEYS[5] {
            entry.1 = "   ".to_string();
        }
    }
    write_with_metadata(&path, &metadata);

    let error = read_panel(&path).expect_err("a blank digest must not read");
    assert!(
        matches!(
            error,
            CurateError::UnexpectedMetadata { key, .. } if key == DIGEST_KEYS[5]
        ),
        "got {error:?}"
    );
}

/// X-R8. The writer refuses to produce a file the reader would refuse.
///
/// Both ends hold the same line, so a blank digest fails where it can be traced
/// to the run that made it rather than later, in front of whoever picked the
/// file up.
#[test]
fn x_r8_the_writer_refuses_provenance_the_reader_would_refuse() {
    let path = scratch("x-r8-writer").join("panel.parquet");
    let error = write_panel(
        vec![full_row()],
        &path,
        &PanelProvenance {
            filings_sha256: String::new(),
            ..provenance()
        },
    )
    .expect_err("blank provenance must not be written");
    assert!(
        matches!(error, CurateError::UnexpectedMetadata { .. }),
        "got {error:?}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// What a good file declares, read back whole.
#[test]
fn the_provenance_reads_back_key_by_key() {
    let path = scratch("panel-provenance").join("panel.parquet");
    write_panel(vec![full_row()], &path, &provenance()).expect("the fixture writes");

    assert_eq!(
        panel_provenance(&path).expect("the file declares itself"),
        provenance()
    );
}

/// Two rows for one security on one month-end are an error naming the key.
///
/// Never a deduplication. The two rows describe the same name on the same day,
/// so keeping either one picks an answer nobody chose, and the export produces
/// one row per pair by construction, which makes a duplicate a bug rather than
/// a data shape.
#[test]
fn a_duplicate_security_and_month_end_is_refused_and_named() {
    let path = scratch("panel-duplicate").join("panel.parquet");
    let second = PanelRow {
        // The same security by `AssetKey`'s own equality, under a different
        // ticker, which is what a rename looks like in a batch.
        asset: sharadar("RENAMED", 1),
        ..full_row()
    };

    let error = write_panel(vec![full_row(), second], &path, &provenance())
        .expect_err("a duplicate key must be refused");
    let message = error.to_string();
    assert!(
        matches!(error, CurateError::DuplicateRow { .. }),
        "got {error:?}"
    );
    assert!(message.contains("2020-01-31"), "got {message}");
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// An empty panel is refused rather than written over a good one.
#[test]
fn an_empty_panel_is_refused() {
    let path = scratch("panel-empty").join("panel.parquet");
    let error =
        write_panel(Vec::new(), &path, &provenance()).expect_err("an empty batch must refuse");
    assert!(
        matches!(error, CurateError::EmptyDataset { dataset: "panel" }),
        "got {error:?}"
    );
    assert!(!path.exists());
}

// --- fixtures ---------------------------------------------------------------

/// The six keys a good file carries.
fn full_metadata() -> Vec<(&'static str, String)> {
    let provenance = provenance();
    let mut metadata = vec![(CONFIG_HASH_KEY, provenance.config_hash.clone())];
    metadata.push((DIGEST_KEYS[0], provenance.universe_sha256));
    metadata.push((DIGEST_KEYS[1], provenance.prices_sha256));
    metadata.push((DIGEST_KEYS[2], provenance.actions_sha256));
    metadata.push((DIGEST_KEYS[3], provenance.delistings_sha256));
    metadata.push((DIGEST_KEYS[4], provenance.marketcap_sha256));
    metadata.push((DIGEST_KEYS[5], provenance.filings_sha256));
    metadata
}

/// Write a well-formed panel file carrying exactly the metadata given.
///
/// The writer always writes all six keys, which is the point of it, so a file
/// missing one has to be built here. It goes through the same schema and the
/// same arrow writer, so what is under test is the reader's provenance check
/// and nothing else.
fn write_with_metadata(path: &Path, metadata: &[(&'static str, String)]) {
    let schema = crate::parquet::panel::schema();
    let decimal = |value: Option<i128>| -> ArrayRef {
        Arc::new(
            Decimal128Array::from(vec![value])
                .with_precision_and_scale(38, 9)
                .expect("a valid decimal column"),
        )
    };
    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(vec!["AAA"])),
        Arc::new(StringArray::from(vec![Some("sharadar")])),
        Arc::new(StringArray::from(vec![Some("1")])),
        Arc::new(Date32Array::from(vec![18_292])),
    ];
    for _ in 0..8 {
        columns.push(decimal(Some(0)));
    }
    columns.push(Arc::new(BooleanArray::from(vec![true])));
    columns.push(decimal(None));
    columns.push(Arc::new(BooleanArray::from(vec![None::<bool>])));

    let batch = RecordBatch::try_new(schema.clone(), columns).expect("a valid batch");
    let borrowed: Vec<(&str, &str)> = metadata
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();

    crate::parquet::write_atomically(path, |file| {
        let mut writer = ::parquet::arrow::ArrowWriter::try_new(
            file,
            schema,
            Some(crate::parquet::writer_properties(&borrowed)),
        )?;
        writer.write(&batch)?;
        writer.close()?;
        Ok(())
    })
    .expect("the fixture writes");
}
