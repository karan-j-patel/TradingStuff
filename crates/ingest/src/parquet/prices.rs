//! The prices dataset: one `PriceBar` per row.
//!
//! There is deliberately no adjusted column. Adjustment is computed in
//! `crate::adjust` from raw prices plus explicit corporate action records, and
//! storing a vendor's version of it here would put a number nobody can
//! reproduce into the curated layer.

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
use crate::schema::{PriceBar, validate_batch};

pub(crate) const DATASET: &str = "prices";

/// The column layout, which is also what the reader checks a file against.
pub(crate) fn schema() -> Arc<Schema> {
    let decimal = DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE);
    Arc::new(Schema::new(vec![
        Field::new("ticker", DataType::Utf8, false),
        Field::new("permanent_id_kind", DataType::Utf8, true),
        Field::new("permanent_id", DataType::Utf8, true),
        Field::new("date", DataType::Date32, false),
        Field::new("open", decimal.clone(), false),
        Field::new("high", decimal.clone(), false),
        Field::new("low", decimal.clone(), false),
        Field::new("close", decimal.clone(), false),
        Field::new("volume", decimal, false),
        Field::new("session", DataType::Utf8, false),
        Field::new("close_kind", DataType::Utf8, false),
    ]))
}

/// Write bars to `path`, returning how many rows landed.
///
/// The whole batch is encoded into memory before the destination is touched, so
/// a value that cannot be represented fails with the previous file still in
/// place. A writer that opened the file first and validated second would report
/// the same error and still have destroyed the data.
pub fn write_prices(bars: Vec<PriceBar>, path: &Path) -> Result<usize, CurateError> {
    // Validation is the writer's job, not the caller's. This function is
    // publicly exported, so a caller that never goes through `cli curate` still
    // cannot put a record the domain rejects into a curated file.
    let total = bars.len();
    let report = validate_batch(bars);
    if !report.rejected.is_empty() {
        return Err(super::invalid_records(
            DATASET,
            total,
            &report.rejected,
            |bar| format!("{} {}", bar.asset.ticker, bar.date),
        ));
    }

    let rows = super::prepare(DATASET, report.accepted, |bar| (&bar.asset, bar.date))?;
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
            Arc::new(StringArray::from(session)),
            Arc::new(StringArray::from(close_kind)),
        ],
    )?;

    super::write_atomically(path, |file| {
        let mut writer = ::parquet::arrow::ArrowWriter::try_new(
            file,
            schema(),
            Some(super::writer_properties()),
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
pub fn read_prices(path: &Path) -> Result<Vec<PriceBar>, CurateError> {
    let file = super::open(path)?;
    let reader =
        ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;

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
        let session = string_column(&batch, DATASET, "session")?;
        let close_kind = string_column(&batch, DATASET, "close_kind")?;

        for row in 0..batch.num_rows() {
            bars.push(PriceBar {
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
