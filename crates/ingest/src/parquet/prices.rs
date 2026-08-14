//! The prices dataset: one [`AdjustedBar`] per row.
//!
//! # What the numbers in this file are
//!
//! `open`, `high`, `low` and `close` are on the vendor's split-and-stock-
//! dividend-adjusted basis, stored exactly as shipped, rounding included.
//! `close_unadjusted` is the as-traded close and is exact. That is not a
//! preference, it is what the vendor has: the probe on 2026-08-09 measured a
//! 3-for-1 split where the adjusted close missed the traded close by `0.001`
//! and the implied factor differed on every row, so the traded open, high and
//! low are not recoverable and are not stored.
//!
//! Adjustment for cash dividends and spinoffs is still computed in
//! `crate::adjust` from explicit corporate action records. What is stored here
//! is the vendor's split handling, which cannot be undone, labelled so nobody
//! has to guess.
//!
//! # The label is in the file, not only in this comment
//!
//! Parquet key-value metadata carries the basis string, and [`read_prices`]
//! refuses a file that lacks it or disagrees with it. A comment binds this
//! crate's readers; the metadata binds a DuckDB session and a pandas call too,
//! which are the readers most likely to assume a column named `close` is the
//! price something traded at.

use bytes::Bytes;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Date32Array, Decimal128Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;

use super::CurateError;
use super::codec::{
    DECIMAL_PRECISION, DECIMAL_SCALE, date_column, date_to_days, decimal_column, decimal_to_i128,
    decode_identity, optional_str, required_date, required_decimal, required_str, string_column,
};
use super::enums::{close_kind_from, close_kind_str, session_from, session_str};
use crate::adjusted::{AdjustedBar, validate_adjusted};

pub(crate) const DATASET: &str = "prices";

/// The metadata key naming what basis the price columns are on.
pub const BASIS_KEY: &str = "price_basis";

/// The only basis this dataset holds, and the only one its reader accepts.
///
/// A second basis would need its own value here and a reader that branches on
/// it. Until one exists, a file claiming anything else is a file this code did
/// not write and does not understand.
pub const BASIS: &str = "split_and_stock_dividend_adjusted";

/// The metadata key naming the data vendor the rows came from.
pub const SOURCE_KEY: &str = "source";

/// The column layout, which is also what the reader checks a file against.
pub(crate) fn schema() -> Arc<Schema> {
    let decimal = DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE);
    Arc::new(Schema::new(vec![
        Field::new("ticker", DataType::Utf8, false),
        Field::new("permanent_id_kind", DataType::Utf8, true),
        Field::new("permanent_id", DataType::Utf8, true),
        Field::new("date", DataType::Date32, false),
        // Adjusted, per BASIS. The names are the vendor's and are kept so a
        // reader recognises them; the basis metadata is what says what they mean.
        Field::new("open", decimal.clone(), false),
        Field::new("high", decimal.clone(), false),
        Field::new("low", decimal.clone(), false),
        Field::new("close", decimal.clone(), false),
        Field::new("volume", decimal.clone(), false),
        // As traded, exact. Not null: a row without it has no as-traded price
        // at all, which is a row this dataset has no use for.
        Field::new("close_unadjusted", decimal, false),
        Field::new("session", DataType::Utf8, false),
        Field::new("close_kind", DataType::Utf8, false),
    ]))
}

/// What a prices file says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Always [`BASIS`] for a file this reader accepts.
    pub basis: String,
    /// The data vendor the rows came from.
    pub source: String,
}

/// Write bars to `path`, returning how many rows landed.
///
/// The whole batch is encoded into memory before the destination is touched, so
/// a value that cannot be represented fails with the previous file still in
/// place. A writer that opened the file first and validated second would report
/// the same error and still have destroyed the data.
/// `source` names the data vendor and is recorded in the file's metadata. It
/// is per write rather than per row: every row in one curated file comes from
/// one fetch, and a per-row column would invite mixing vendors in a file whose
/// numbers are only comparable because they do not.
pub fn write_prices(
    bars: Vec<AdjustedBar>,
    path: &Path,
    source: &str,
) -> Result<usize, CurateError> {
    // Validation is the writer's job, not the caller's. This function is
    // publicly exported, so a caller that never goes through `cli curate` still
    // cannot put a record the domain rejects into a curated file.
    let total = bars.len();
    let report = validate_adjusted(bars);
    if !report.rejected.is_empty() {
        return Err(super::invalid_records(
            DATASET,
            total,
            &report.rejected,
            |bar| format!("{} {}", bar.asset.ticker, bar.date),
        ));
    }

    let rows = super::prepare(DATASET, report.accepted, |bar| {
        super::RowKey::simple(&bar.asset, bar.date)
    })?;
    let count = rows.len();

    let mut ticker = Vec::with_capacity(count);
    let mut kind = Vec::with_capacity(count);
    let mut permanent_id = Vec::with_capacity(count);
    let mut date = Vec::with_capacity(count);
    let mut open = Vec::with_capacity(count);
    let mut high = Vec::with_capacity(count);
    let mut low = Vec::with_capacity(count);
    let mut close = Vec::with_capacity(count);
    let mut volume = Vec::with_capacity(count);
    let mut close_unadjusted = Vec::with_capacity(count);
    let mut session = Vec::with_capacity(count);
    let mut close_kind = Vec::with_capacity(count);

    for (identity, bar) in &rows {
        ticker.push(identity.ticker.as_str());
        kind.push(identity.kind);
        permanent_id.push(identity.id.as_deref());
        date.push(date_to_days(bar.date));
        open.push(decimal_to_i128(DATASET, "open", bar.open)?);
        high.push(decimal_to_i128(DATASET, "high", bar.high)?);
        low.push(decimal_to_i128(DATASET, "low", bar.low)?);
        close.push(decimal_to_i128(DATASET, "close", bar.close)?);
        volume.push(decimal_to_i128(DATASET, "volume", bar.volume)?);
        close_unadjusted.push(decimal_to_i128(
            DATASET,
            "close_unadjusted",
            bar.close_unadjusted,
        )?);
        session.push(session_str(bar.session));
        close_kind.push(close_kind_str(bar.close_kind));
    }

    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(StringArray::from(ticker)) as ArrayRef,
            Arc::new(StringArray::from(kind)),
            Arc::new(StringArray::from(permanent_id)),
            Arc::new(Date32Array::from(date)),
            decimals(open)?,
            decimals(high)?,
            decimals(low)?,
            decimals(close)?,
            decimals(volume)?,
            decimals(close_unadjusted)?,
            Arc::new(StringArray::from(session)),
            Arc::new(StringArray::from(close_kind)),
        ],
    )?;

    super::write_atomically(path, |file| {
        let mut writer = ::parquet::arrow::ArrowWriter::try_new(
            file,
            schema(),
            Some(super::writer_properties(&[
                (BASIS_KEY, BASIS),
                (SOURCE_KEY, source),
            ])),
        )?;
        writer.write(&batch)?;
        writer.close()?;
        Ok(())
    })?;

    Ok(count)
}

/// Wrap the raw integers in an array that declares the fixed precision and
/// scale, so a reader never has to guess where the decimal point sits.
fn decimals(values: Vec<i128>) -> Result<ArrayRef, CurateError> {
    Ok(Arc::new(
        Decimal128Array::from(values).with_precision_and_scale(DECIMAL_PRECISION, DECIMAL_SCALE)?,
    ))
}

/// Read every bar back, in the order the file holds them.
///
/// The basis metadata is checked before a single row is decoded. A file that
/// does not say what its prices are on is a file whose numbers cannot be used,
/// and reading it anyway would be the exact mistake [`AdjustedBar`] exists to
/// make impossible in memory.
pub fn read_prices(path: &Path) -> Result<Vec<AdjustedBar>, CurateError> {
    decode(super::open(path)?)
}

/// The same read, from bytes already in memory.
///
/// Added when the panel export began recording the prices digest in its
/// provenance. See the `read_from_bytes` section of [`super`]: hashing one read
/// of a path and parsing a second does not support the claim that the recorded
/// digest describes the rows that were used.
pub fn read_prices_from_bytes(bytes: Bytes) -> Result<Vec<AdjustedBar>, CurateError> {
    decode(bytes)
}

fn decode<R: ::parquet::file::reader::ChunkReader + 'static>(
    source: R,
) -> Result<Vec<AdjustedBar>, CurateError> {
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(source)?;

    provenance_of(builder.metadata().file_metadata().key_value_metadata())?;

    let reader = builder.build()?;

    let mut bars = Vec::new();
    // Row numbers in errors are counted across the whole file rather than
    // within a batch, because a reader chasing one down has a file and not a
    // batch in front of them.
    let mut offset = 0usize;

    for batch in reader {
        let batch = batch?;
        let ticker = string_column(&batch, DATASET, "ticker")?;
        let kind = string_column(&batch, DATASET, "permanent_id_kind")?;
        let permanent_id = string_column(&batch, DATASET, "permanent_id")?;
        let date = date_column(&batch, DATASET, "date")?;
        let open = decimal_column(&batch, DATASET, "open")?;
        let high = decimal_column(&batch, DATASET, "high")?;
        let low = decimal_column(&batch, DATASET, "low")?;
        let close = decimal_column(&batch, DATASET, "close")?;
        let volume = decimal_column(&batch, DATASET, "volume")?;
        let close_unadjusted = decimal_column(&batch, DATASET, "close_unadjusted")?;
        let session = string_column(&batch, DATASET, "session")?;
        let close_kind = string_column(&batch, DATASET, "close_kind")?;

        for row in 0..batch.num_rows() {
            bars.push(AdjustedBar {
                asset: decode_identity(
                    DATASET,
                    offset + row,
                    required_str(ticker, DATASET, "ticker", row)?,
                    optional_str(kind, row),
                    optional_str(permanent_id, row),
                )?,
                date: required_date(date, DATASET, "date", row)?,
                open: required_decimal(open, DATASET, "open", row)?,
                high: required_decimal(high, DATASET, "high", row)?,
                low: required_decimal(low, DATASET, "low", row)?,
                close: required_decimal(close, DATASET, "close", row)?,
                volume: required_decimal(volume, DATASET, "volume", row)?,
                close_unadjusted: required_decimal(
                    close_unadjusted,
                    DATASET,
                    "close_unadjusted",
                    row,
                )?,
                session: session_from(DATASET, required_str(session, DATASET, "session", row)?)?,
                close_kind: close_kind_from(
                    DATASET,
                    required_str(close_kind, DATASET, "close_kind", row)?,
                )?,
            });
        }
        offset += batch.num_rows();
    }

    Ok(bars)
}

/// What a curated prices file declares about itself.
///
/// Exposed so a caller can ask which vendor a file came from without decoding
/// every row, and so the basis and the source are always read together: either
/// alone is half an answer.
pub fn prices_provenance(path: &Path) -> Result<Provenance, CurateError> {
    let file = super::open(path)?;
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)?;
    provenance_of(builder.metadata().file_metadata().key_value_metadata())
}

/// Read both keys, refusing a file that does not declare what this reader needs.
fn provenance_of(
    metadata: Option<&Vec<::parquet::file::metadata::KeyValue>>,
) -> Result<Provenance, CurateError> {
    let value = |key: &str| {
        metadata
            .into_iter()
            .flatten()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.value.as_deref())
    };

    let basis = match value(BASIS_KEY) {
        Some(BASIS) => BASIS.to_owned(),
        Some(other) => {
            return Err(CurateError::UnexpectedMetadata {
                dataset: DATASET,
                key: BASIS_KEY,
                expected: BASIS,
                found: other.to_owned(),
            });
        }
        None => {
            return Err(CurateError::MissingMetadata {
                dataset: DATASET,
                key: BASIS_KEY,
            });
        }
    };

    let source = value(SOURCE_KEY)
        .ok_or(CurateError::MissingMetadata {
            dataset: DATASET,
            key: SOURCE_KEY,
        })?
        .to_owned();

    Ok(Provenance { basis, source })
}
