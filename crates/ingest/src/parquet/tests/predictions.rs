//! The predictions dataset's round trip and its one provenance rule.
//!
//! The engine-side falsifications (ranking direction, the pre-window clamp,
//! missing-month ineligibility) live in `crates/engine`. What only exists here
//! is the claim that a predictions file cannot be read without saying which
//! panel configuration it was fitted against.
//!
//! Every value is synthetic.

use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Date32Array, Decimal128Array, StringArray};
use arrow::record_batch::RecordBatch;

use super::super::*;
use super::{day, dec, scratch, sharadar};
use crate::parquet::panel::{CONFIG_HASH_KEY, DIGEST_KEYS};
use crate::parquet::predictions::PANEL_SHA256_KEY;

/// The seven keys the fit passes through, each distinct so a writer or reader
/// putting one under another's heading is visible.
fn provenance() -> PredictionsProvenance {
    PredictionsProvenance {
        panel: PanelProvenance {
            config_hash: "c".repeat(64),
            universe_sha256: "1".repeat(64),
            prices_sha256: "7".repeat(64),
            actions_sha256: "2".repeat(64),
            delistings_sha256: "3".repeat(64),
            marketcap_sha256: "4".repeat(64),
            filings_sha256: "5".repeat(64),
        },
        panel_sha256: "6".repeat(64),
    }
}

/// The same seven as writable metadata pairs.
fn full_metadata() -> Vec<(&'static str, String)> {
    let p = provenance();
    let mut metadata = vec![(CONFIG_HASH_KEY, p.panel.config_hash)];
    metadata.push((DIGEST_KEYS[0], p.panel.universe_sha256));
    metadata.push((DIGEST_KEYS[1], p.panel.prices_sha256));
    metadata.push((DIGEST_KEYS[2], p.panel.actions_sha256));
    metadata.push((DIGEST_KEYS[3], p.panel.delistings_sha256));
    metadata.push((DIGEST_KEYS[4], p.panel.marketcap_sha256));
    metadata.push((DIGEST_KEYS[5], p.panel.filings_sha256));
    metadata.push((PANEL_SHA256_KEY, p.panel_sha256));
    metadata
}

fn row(ticker: &str, id: u64, value: &str) -> PredictionRow {
    PredictionRow {
        asset: sharadar(ticker, id),
        month_end: day(2020, 1, 31),
        predicted_return_1m: dec(value),
    }
}

/// Every column survives the round trip, including a negative forecast.
///
/// The negative is not decoration. A model that only ever predicted gains would
/// hide a sign error in the decimal codec, and the ranking is descending, so a
/// flipped sign would put the worst names at the top.
#[test]
fn predictions_round_trip_including_negative_forecasts() {
    let path = scratch("predictions-roundtrip").join("predictions.parquet");
    let rows = vec![row("AAA", 1, "-0.0425"), row("BBB", 2, "0.1337")];
    assert_eq!(
        write_predictions(rows.clone(), &path, &provenance()).expect("the fixture writes"),
        2
    );

    let read = read_predictions(&path).expect("the fixture reads back");
    assert_eq!(read, rows);

    let bytes = bytes::Bytes::from(std::fs::read(&path).expect("reading the file"));
    assert_eq!(
        read_predictions_from_bytes(bytes).expect("the same file reads from bytes"),
        read,
        "the two entry points disagree, so one of them validates less than the other"
    );
    assert_eq!(
        predictions_provenance(&path).expect("the file declares itself"),
        provenance()
    );
}

/// A forecast carrying more decimal places than the column holds is rounded to
/// the column scale rather than refused.
///
/// A fitted value is a real number and would otherwise never be writable. Same
/// rule as the panel's characteristics, and the same reason.
#[test]
fn a_forecast_longer_than_the_column_scale_is_rounded_to_it() {
    let path = scratch("predictions-rounding").join("predictions.parquet");
    let third = dec("1")
        .checked_div(dec("3"))
        .expect("one third is representable");
    assert!(third.scale() > 9, "the fixture value is not long enough");

    write_predictions(
        vec![PredictionRow {
            predicted_return_1m: third,
            ..row("AAA", 1, "0")
        }],
        &path,
        &provenance(),
    )
    .expect("a long value writes");

    assert_eq!(
        read_predictions(&path).expect("it reads back")[0].predicted_return_1m,
        dec("0.333333333")
    );
}

/// Any one of the seven keys missing is refused, and the error names it.
///
/// Each is dropped in turn rather than all seven at once. A reader that checked
/// only the first would pass six of these seven, and the fit passes the panel's
/// six through precisely so a prediction can be traced to the characteristics
/// behind it.
#[test]
fn a_file_missing_any_provenance_key_is_refused_and_named() {
    for missing in std::iter::once(CONFIG_HASH_KEY)
        .chain(DIGEST_KEYS.iter().copied())
        .chain(std::iter::once(PANEL_SHA256_KEY))
    {
        let path = scratch(&format!("predictions-no-{missing}")).join("predictions.parquet");
        let mut metadata = full_metadata();
        metadata.retain(|(key, _)| *key != missing);
        write_with_metadata(&path, &metadata);

        match read_predictions(&path).expect_err("a file missing a key must not read") {
            CurateError::MissingMetadata { key, .. } => assert_eq!(
                key, missing,
                "the refusal names {key} while {missing} is the key that is absent"
            ),
            other => panic!("got {other:?} for a missing {missing}"),
        }
    }
}

/// A key present and blank is refused too, at the reader and at the writer.
///
/// The gap a presence-only rule leaves open: a whitespace hash passes a lookup
/// and carries no provenance, so the file reads as though its origin were
/// recorded when nothing was.
#[test]
fn a_blank_provenance_value_is_refused_at_both_ends() {
    for blanked in [
        CONFIG_HASH_KEY,
        DIGEST_KEYS[1],
        DIGEST_KEYS[5],
        PANEL_SHA256_KEY,
    ] {
        let path = scratch(&format!("predictions-blank-{blanked}")).join("predictions.parquet");
        let mut metadata = full_metadata();
        for entry in metadata.iter_mut() {
            if entry.0 == blanked {
                entry.1 = "   ".to_string();
            }
        }
        write_with_metadata(&path, &metadata);
        match read_predictions(&path).expect_err("a blank value must not read") {
            CurateError::UnexpectedMetadata { key, .. } => assert_eq!(key, blanked),
            other => panic!("got {other:?} for a blank {blanked}"),
        }
    }

    let written = scratch("predictions-blank-write").join("predictions.parquet");
    let error = write_predictions(
        vec![row("AAA", 1, "0.01")],
        &written,
        &PredictionsProvenance {
            panel_sha256: String::new(),
            ..provenance()
        },
    )
    .expect_err("blank provenance must not be written");
    assert!(
        matches!(
            error,
            CurateError::UnexpectedMetadata {
                key: PANEL_SHA256_KEY,
                ..
            }
        ),
        "got {error:?}"
    );
    assert!(
        !written.exists(),
        "a refused write must leave no file behind"
    );
}

/// Keys this reader does not know about are carried by the file and ignored.
///
/// The fit also writes `fit_script` and `fit_spec`. Nothing in the engine reads
/// them, and a reader that refused what it did not recognise would reject the
/// real file. Pinned so that stays deliberate.
#[test]
fn unknown_metadata_keys_are_ignored_rather_than_refused() {
    let path = scratch("predictions-extra-keys").join("predictions.parquet");
    let mut metadata = full_metadata();
    metadata.push(("fit_script", "ml/fit_ridge.py".to_string()));
    metadata.push(("fit_spec", r#"{"target":"label_return_1m"}"#.to_string()));
    write_with_metadata(&path, &metadata);

    assert_eq!(
        predictions_provenance(&path).expect("a file with extra keys still reads"),
        provenance()
    );
}

/// Two forecasts for one security on one month-end are an error naming the key.
///
/// Never a deduplication. The pair here shares a permanent id under two
/// tickers, which is the same security by `AssetKey`'s own equality and is what
/// a rename looks like in a batch.
#[test]
fn a_duplicate_security_and_month_end_is_refused_and_named() {
    let path = scratch("predictions-duplicate").join("predictions.parquet");
    let error = write_predictions(
        vec![row("AAA", 1, "0.01"), row("RENAMED", 1, "0.09")],
        &path,
        &provenance(),
    )
    .expect_err("a duplicate key must be refused");
    assert!(
        matches!(error, CurateError::DuplicateRow { .. }),
        "got {error:?}"
    );
    assert!(error.to_string().contains("2020-01-31"), "got {error}");
    assert!(!path.exists());
}

/// An empty batch is refused rather than written over a good file.
#[test]
fn an_empty_predictions_batch_is_refused() {
    let path = scratch("predictions-empty").join("predictions.parquet");
    let error = write_predictions(Vec::new(), &path, &provenance())
        .expect_err("an empty batch must refuse");
    assert!(
        matches!(
            error,
            CurateError::EmptyDataset {
                dataset: "predictions"
            }
        ),
        "got {error:?}"
    );
    assert!(!path.exists());
}

/// Write a well-formed predictions file carrying exactly the metadata given.
///
/// The writer always writes the key, which is the point of it, so a file
/// missing it has to be built here. Same schema, same arrow writer, so what is
/// under test is the reader's provenance check and nothing else.
fn write_with_metadata(path: &Path, metadata: &[(&'static str, String)]) {
    let schema = crate::parquet::predictions::schema();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["AAA"])) as ArrayRef,
            Arc::new(StringArray::from(vec![Some("sharadar")])),
            Arc::new(StringArray::from(vec![Some("1")])),
            Arc::new(Date32Array::from(vec![18_292])),
            Arc::new(
                Decimal128Array::from(vec![10_000_000i128])
                    .with_precision_and_scale(38, 9)
                    .expect("a valid decimal column"),
            ),
        ],
    )
    .expect("a valid batch");

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
