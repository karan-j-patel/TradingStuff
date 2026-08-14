//! The predictions dataset: one predicted next-month return per (security,
//! month-end).
//!
//! The seventh curated dataset and the second this codebase produces rather
//! than fetches. Python writes it, Rust reads it, and neither calls the other.
//!
//! # Why the provenance is the panel's whole block plus the panel's digest
//!
//! A prediction is only meaningful against the characteristics it was fitted
//! on, so the fit passes the panel file's six provenance keys through unchanged
//! and adds the digest of the panel file itself. Those six are exactly
//! [`PanelProvenance`], so this dataset reuses the panel's own struct and its
//! own validation rather than keeping a second list that could drift.
//!
//! The two halves answer different questions and both are required. The six say
//! which CONFIGURATION produced the characteristics and which five files it
//! read. `panel_sha256` says which panel FILE the fit actually opened, which no
//! configuration hash can settle: an export rerun byte-for-byte identically has
//! the same config hash, and a panel regenerated from changed data does not.
//!
//! [`read_predictions`] refuses a file missing any of the seven. A prediction
//! column whose provenance is unknown is a column nobody can reproduce, and it
//! would be ranked on all the same.
//!
//! The fit also writes `fit_script` and `fit_spec`. Neither is read here:
//! nothing in the engine consumes them, and the trial log pins this file by
//! digest, so they are preserved in the file for a human without this reader
//! having to have an opinion about their shape.
//!
//! # Rounding
//!
//! Same as the panel and for the same reason: a fitted prediction is a real
//! number and the curated column scale is fixed, so the value is rounded to it
//! at this writer. See [`super::panel`] for the full statement.

use std::path::Path;
use std::sync::Arc;

use ::parquet::file::metadata::KeyValue;
use ::parquet::file::reader::ChunkReader;
use arrow::array::{ArrayRef, Date32Array, Decimal128Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use jiff::civil::Date;
use rust_decimal::Decimal;

use super::CurateError;
use super::codec::{
    DECIMAL_PRECISION, DECIMAL_SCALE, date_column, date_to_days, decimal_column, decimal_to_i128,
    decode_identity, optional_str, required_date, required_decimal, required_str, string_column,
};
use super::panel::{self, PanelProvenance};
use crate::schema::AssetKey;

pub(crate) const DATASET: &str = "predictions";

/// The metadata key carrying the digest of the panel file the fit read.
pub const PANEL_SHA256_KEY: &str = "panel_sha256";

/// What a predictions file says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictionsProvenance {
    /// The panel file's own seven keys, passed through by the fit unchanged.
    pub panel: PanelProvenance,
    /// SHA-256 of the panel file the fit read.
    pub panel_sha256: String,
}

/// One model's predicted return for one security at one month-end.
///
/// The key is `(asset, month_end)`. A second row for one key is a hard error at
/// the writer: two predictions for one name on one day is a fit that ran twice,
/// and keeping either picks a number nobody chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictionRow {
    pub asset: AssetKey,
    /// The formation month-end the prediction is made AT, matching the panel's
    /// own key. The return it predicts is the one over `(month_end, next]`,
    /// which is the panel's `label_return_1m` column.
    pub month_end: Date,
    /// The predicted next-month total return. Not nullable: a row exists
    /// because the model produced a number, and a name it produced nothing for
    /// simply has no row.
    pub predicted_return_1m: Decimal,
}

pub(crate) fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ticker", DataType::Utf8, false),
        Field::new("permanent_id_kind", DataType::Utf8, true),
        Field::new("permanent_id", DataType::Utf8, true),
        Field::new("month_end", DataType::Date32, false),
        Field::new(
            "predicted_return_1m",
            DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE),
            false,
        ),
    ]))
}

/// Write rows to `path`, returning how many landed.
///
/// The duplicate rule is `(asset, month_end)` and it is [`super::prepare`]'s,
/// which also fixes the row order.
pub fn write_predictions(
    rows: Vec<PredictionRow>,
    path: &Path,
    provenance: &PredictionsProvenance,
) -> Result<usize, CurateError> {
    check_provenance(provenance)?;

    let rows = super::prepare(DATASET, rows, |row| {
        super::RowKey::simple(&row.asset, row.month_end)
    })?;
    let count = rows.len();

    let mut ticker = Vec::with_capacity(count);
    let mut id_kind = Vec::with_capacity(count);
    let mut permanent_id = Vec::with_capacity(count);
    let mut month_end = Vec::with_capacity(count);
    let mut predicted = Vec::with_capacity(count);

    for (identity, row) in &rows {
        ticker.push(identity.ticker.as_str());
        id_kind.push(identity.kind);
        permanent_id.push(identity.id.as_deref());
        month_end.push(date_to_days(row.month_end));
        predicted.push(decimal_to_i128(
            DATASET,
            "predicted_return_1m",
            row.predicted_return_1m.round_dp(DECIMAL_SCALE as u32),
        )?);
    }

    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(StringArray::from(ticker)) as ArrayRef,
            Arc::new(StringArray::from(id_kind)),
            Arc::new(StringArray::from(permanent_id)),
            Arc::new(Date32Array::from(month_end)),
            Arc::new(
                Decimal128Array::from(predicted)
                    .with_precision_and_scale(DECIMAL_PRECISION, DECIMAL_SCALE)?,
            ),
        ],
    )?;

    let mut metadata = panel::provenance_metadata(&provenance.panel);
    metadata.push((PANEL_SHA256_KEY, provenance.panel_sha256.as_str()));

    super::write_atomically(path, |file| {
        let mut writer = ::parquet::arrow::ArrowWriter::try_new(
            file,
            schema(),
            Some(super::writer_properties(&metadata)),
        )?;
        writer.write(&batch)?;
        writer.close()?;
        Ok(())
    })?;

    Ok(count)
}

/// Read every row back, in the order the file holds them.
pub fn read_predictions(path: &Path) -> Result<Vec<PredictionRow>, CurateError> {
    decode(super::open(path)?)
}

/// The same read, from bytes already in memory.
///
/// Present from this dataset's first commit, on the rule the `read_*_from_bytes`
/// section of [`super`] states: the digest a trial records and the rows the
/// backtest ranks on have to describe the same bytes.
pub fn read_predictions_from_bytes(bytes: Bytes) -> Result<Vec<PredictionRow>, CurateError> {
    decode(bytes)
}

/// What a curated predictions file declares about itself.
pub fn predictions_provenance(path: &Path) -> Result<PredictionsProvenance, CurateError> {
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        super::open(path)?,
    )?;
    provenance_of(builder.metadata().file_metadata().key_value_metadata())
}

/// Refuse provenance that is missing or blank, at the writer and the reader
/// alike, so a file cannot exist with less than a reader demands.
///
/// The seven panel keys go through the panel's own check, which is what keeps one
/// definition of "a complete provenance block" for both datasets.
fn check_provenance(provenance: &PredictionsProvenance) -> Result<(), CurateError> {
    panel::check_provenance(DATASET, &provenance.panel)?;
    if provenance.panel_sha256.trim().is_empty() {
        return Err(CurateError::UnexpectedMetadata {
            dataset: DATASET,
            key: PANEL_SHA256_KEY,
            expected: "a non-empty hex digest",
            found: provenance.panel_sha256.clone(),
        });
    }
    Ok(())
}

fn provenance_of(metadata: Option<&Vec<KeyValue>>) -> Result<PredictionsProvenance, CurateError> {
    let provenance = PredictionsProvenance {
        panel: panel::provenance_of(DATASET, metadata)?,
        panel_sha256: metadata
            .into_iter()
            .flatten()
            .find(|entry| entry.key == PANEL_SHA256_KEY)
            .and_then(|entry| entry.value.as_deref())
            .map(str::to_owned)
            .ok_or(CurateError::MissingMetadata {
                dataset: DATASET,
                key: PANEL_SHA256_KEY,
            })?,
    };
    // Present and blank passes the lookup and carries no provenance, which is
    // the gap a presence-only rule leaves open.
    check_provenance(&provenance)?;
    Ok(provenance)
}

/// The same provenance read, from bytes already in memory.
///
/// The caller that attaches predictions has the bytes and their digest in hand
/// already; re-opening the path would be the second read the read-once rule
/// exists to prevent.
pub fn predictions_provenance_from_bytes(
    bytes: Bytes,
) -> Result<PredictionsProvenance, CurateError> {
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes)?;
    provenance_of(builder.metadata().file_metadata().key_value_metadata())
}

/// The body both entry points share, so neither validates less than the other.
fn decode<R: ChunkReader + 'static>(source: R) -> Result<Vec<PredictionRow>, CurateError> {
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(source)?;
    provenance_of(builder.metadata().file_metadata().key_value_metadata())?;
    let reader = builder.build()?;

    let mut rows = Vec::new();
    let mut offset = 0usize;

    for batch in reader {
        let batch = batch?;
        let ticker = string_column(&batch, DATASET, "ticker")?;
        let id_kind = string_column(&batch, DATASET, "permanent_id_kind")?;
        let permanent_id = string_column(&batch, DATASET, "permanent_id")?;
        let month_end = date_column(&batch, DATASET, "month_end")?;
        let predicted = decimal_column(&batch, DATASET, "predicted_return_1m")?;

        for row in 0..batch.num_rows() {
            rows.push(PredictionRow {
                asset: decode_identity(
                    DATASET,
                    offset + row,
                    required_str(ticker, DATASET, "ticker", row)?,
                    optional_str(id_kind, row),
                    optional_str(permanent_id, row),
                )?,
                month_end: required_date(month_end, DATASET, "month_end", row)?,
                predicted_return_1m: required_decimal(
                    predicted,
                    DATASET,
                    "predicted_return_1m",
                    row,
                )?,
            });
        }
        offset += batch.num_rows();
    }

    Ok(rows)
}
