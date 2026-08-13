//! The market cap dataset: one [`MarketCapRecord`] per row.
//!
//! # The label is in the file, not only in this comment
//!
//! The stored figure is in millions of US dollars, exactly as the vendor ships
//! it. Nothing here rescales it, so the only thing standing between a reader
//! and a number a million times too small is the label, and the label lives in
//! the file's own key-value metadata rather than in this comment. A comment
//! binds this crate's readers. The metadata binds a DuckDB session and a pandas
//! call too, and those are the readers most likely to see a column called
//! `marketcap` and assume dollars.
//!
//! [`read_marketcap`] refuses a file that does not carry the label, and refuses
//! one carrying a label it does not understand, on the same rule
//! [`super::read_prices`] applies to the price basis.

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
use super::prices::SOURCE_KEY;
use crate::marketcap::{MarketCapRecord, validate_marketcap};

pub(crate) const DATASET: &str = "marketcap";

/// The metadata key naming what units the figure is in.
pub const UNITS_KEY: &str = "marketcap_units";

/// The only units this dataset holds, and the only ones its reader accepts.
///
/// Measured live on 2026-08-11 against three securities on one date, each
/// checked by dividing the shipped figure by that day's unadjusted close and
/// comparing the implied share count with the company's own filings. The
/// `as_shipped` half is not decoration: it says the number was written the way
/// the vendor sent it, so a reader wanting dollars multiplies rather than
/// wondering whether somebody already did.
pub const UNITS: &str = "usd_millions_as_shipped";

/// What a market cap file says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketCapProvenance {
    /// Always [`UNITS`] for a file this reader accepts.
    pub units: String,
    /// The data vendor the rows came from.
    pub source: String,
}

/// The column layout, which is also what the reader checks a file against.
pub(crate) fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ticker", DataType::Utf8, false),
        Field::new("permanent_id_kind", DataType::Utf8, true),
        Field::new("permanent_id", DataType::Utf8, true),
        Field::new("date", DataType::Date32, false),
        // Not null. This table expresses "no figure" by omitting the row, so a
        // null here would be a state the vendor never produces and this crate
        // has no meaning for.
        Field::new(
            "marketcap",
            DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE),
            false,
        ),
        Field::new("source", DataType::Utf8, false),
    ]))
}

/// Write records to `path`, returning how many rows landed.
///
/// The whole batch is validated and encoded before the destination is touched,
/// so a bad row fails with the previous file still in place.
pub fn write_marketcap(
    rows: Vec<MarketCapRecord>,
    path: &Path,
    source: &str,
) -> Result<usize, CurateError> {
    // The writer is the guard. This function is publicly exported, so a caller
    // that never goes through `cli curate` still cannot put a record the domain
    // rejects into a curated file.
    //
    // Every rejection fails the whole write. Nothing here skips a row: this
    // table omits the row when it has no figure, so a dropped row would be
    // indistinguishable from the absence that happens routinely, and the
    // dataset would quietly disagree with the vendor about which days exist.
    let total = rows.len();
    let rejected: Vec<(&MarketCapRecord, _)> = rows
        .iter()
        .filter_map(|row| validate_marketcap(row).err().map(|reason| (row, reason)))
        .collect();
    if !rejected.is_empty() {
        return Err(super::invalid_records(DATASET, total, &rejected, |row| {
            format!("{} {}", row.asset.ticker, row.date)
        }));
    }

    let rows = super::prepare(DATASET, rows, |row| {
        super::RowKey::simple(&row.asset, row.date)
    })?;
    let count = rows.len();

    let mut ticker = Vec::with_capacity(count);
    let mut id_kind = Vec::with_capacity(count);
    let mut permanent_id = Vec::with_capacity(count);
    let mut date = Vec::with_capacity(count);
    let mut marketcap = Vec::with_capacity(count);
    let mut row_source = Vec::with_capacity(count);

    for (identity, row) in &rows {
        ticker.push(identity.ticker.as_str());
        id_kind.push(identity.kind);
        permanent_id.push(identity.id.as_deref());
        date.push(date_to_days(row.date));
        // As shipped. The only transformation is padding the mantissa out to
        // the fixed scale, which adds zeros and changes no digit. Anything that
        // multiplied here would be converting units behind the label's back.
        marketcap.push(decimal_to_i128(DATASET, "marketcap", row.marketcap)?);
        row_source.push(row.source.as_str());
    }

    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(StringArray::from(ticker)) as ArrayRef,
            Arc::new(StringArray::from(id_kind)),
            Arc::new(StringArray::from(permanent_id)),
            Arc::new(Date32Array::from(date)),
            decimals(marketcap)?,
            Arc::new(StringArray::from(row_source)),
        ],
    )?;

    super::write_atomically(path, |file| {
        let mut writer = ::parquet::arrow::ArrowWriter::try_new(
            file,
            schema(),
            Some(super::writer_properties(&[
                (UNITS_KEY, UNITS),
                (SOURCE_KEY, source),
            ])),
        )?;
        writer.write(&batch)?;
        writer.close()?;
        Ok(())
    })?;

    Ok(count)
}

/// Read every record back, in the order the file holds them.
///
/// The units metadata is checked before a single row is decoded. A file that
/// does not say what units its figures are in is a file whose numbers cannot be
/// used, and the two candidate readings differ by a factor of a million.
pub fn read_marketcap(path: &Path) -> Result<Vec<MarketCapRecord>, CurateError> {
    let file = super::open(path)?;
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)?;

    provenance_of(builder.metadata().file_metadata().key_value_metadata())?;

    let reader = builder.build()?;

    let mut records = Vec::new();
    // Row numbers in errors are counted across the whole file rather than
    // within a batch, because a reader chasing one down has a file in front of
    // them and not a batch.
    let mut offset = 0usize;

    for batch in reader {
        let batch = batch?;
        let ticker = string_column(&batch, DATASET, "ticker")?;
        let id_kind = string_column(&batch, DATASET, "permanent_id_kind")?;
        let permanent_id = string_column(&batch, DATASET, "permanent_id")?;
        let date = date_column(&batch, DATASET, "date")?;
        let marketcap = decimal_column(&batch, DATASET, "marketcap")?;
        let source = string_column(&batch, DATASET, "source")?;

        for row in 0..batch.num_rows() {
            records.push(MarketCapRecord {
                asset: decode_identity(
                    DATASET,
                    offset + row,
                    required_str(ticker, DATASET, "ticker", row)?,
                    optional_str(id_kind, row),
                    optional_str(permanent_id, row),
                )?,
                date: required_date(date, DATASET, "date", row)?,
                marketcap: required_decimal(marketcap, DATASET, "marketcap", row)?,
                source: required_str(source, DATASET, "source", row)?.to_owned(),
            });
        }
        offset += batch.num_rows();
    }

    Ok(records)
}

/// What a curated market cap file declares about itself.
pub fn marketcap_provenance(path: &Path) -> Result<MarketCapProvenance, CurateError> {
    let file = super::open(path)?;
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)?;
    provenance_of(builder.metadata().file_metadata().key_value_metadata())
}

/// Read both keys, refusing a file that does not declare what this reader needs.
///
/// The checking is in [`super::units_and_source`], shared with the delistings
/// dataset, which stores a figure in the same units and carries the same
/// factor-of-a-million hazard. Value-open on the source and pinned on the
/// units, for the reasons recorded there.
fn provenance_of(
    metadata: Option<&Vec<::parquet::file::metadata::KeyValue>>,
) -> Result<MarketCapProvenance, CurateError> {
    let (units, source) = super::units_and_source(DATASET, UNITS, UNITS_KEY, metadata)?;
    Ok(MarketCapProvenance { units, source })
}

/// Wrap the raw integers in an array that declares the fixed precision and
/// scale, so a reader never has to guess where the decimal point sits.
fn decimals(values: Vec<i128>) -> Result<ArrayRef, CurateError> {
    Ok(Arc::new(
        Decimal128Array::from(values).with_precision_and_scale(DECIMAL_PRECISION, DECIMAL_SCALE)?,
    ))
}
