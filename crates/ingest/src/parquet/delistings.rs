//! The delistings dataset: one `Delisting` per row.

use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Date32Array, Decimal128Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use rust_decimal::Decimal;

use super::CurateError;
use super::codec::{
    DECIMAL_PRECISION, DECIMAL_SCALE, date_column, date_to_days, decimal_column, decimal_to_i128,
    decode_identity, optional_decimal, optional_str, required_date, required_str, string_column,
};
use super::enums::{
    convention_from, convention_str, listing_from, listing_str, reason_from, reason_str,
};
use super::marketcap::{UNITS, UNITS_KEY};
use super::prices::SOURCE_KEY;
use crate::actions::{Delisting, TerminalValue, validate_delisting};

pub(crate) const DATASET: &str = "delistings";

const OBSERVED: &str = "observed";
const IMPUTED: &str = "imputed";
const UNKNOWN: &str = "unknown";

/// What a delistings file says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelistingProvenance {
    /// Units of `final_market_cap`. Always [`UNITS`] for a file this reader
    /// accepts.
    pub units: String,
    /// The data vendor the rows came from.
    pub source: String,
}

/// The column layout, which is also what the reader checks a file against.
///
/// `TerminalValue` takes three columns rather than one nullable number.
/// Observed and imputed values are both numbers, and most delisting values are
/// imputed rather than observed, so a shape that cannot tell them apart turns a
/// published convention into a measurement on every read.
pub(crate) fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ticker", DataType::Utf8, false),
        Field::new("permanent_id_kind", DataType::Utf8, true),
        Field::new("permanent_id", DataType::Utf8, true),
        Field::new("date", DataType::Date32, false),
        Field::new("reason", DataType::Utf8, false),
        Field::new("listing", DataType::Utf8, false),
        Field::new("terminal_kind", DataType::Utf8, false),
        Field::new(
            "terminal_value",
            DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE),
            true,
        ),
        Field::new("imputation_convention", DataType::Utf8, true),
        // Nullable, and null means the source row carried no figure rather
        // than a figure of zero. Zero is a real final market capitalisation on
        // a bankruptcy liquidation, so the two states cannot share an encoding.
        // The units live in the file's key-value metadata, as they do on the
        // market cap dataset, because the two candidate readings of this
        // column differ by a factor of a million.
        Field::new(
            "final_market_cap",
            DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE),
            true,
        ),
        Field::new("source", DataType::Utf8, false),
    ]))
}

/// Write delistings to `path`, returning how many rows landed.
pub fn write_delistings(
    rows: Vec<Delisting>,
    path: &Path,
    source: &str,
) -> Result<usize, CurateError> {
    // Same reasoning as `write_prices`. The writer is the guard, because it is
    // the one thing every caller has to go through.
    let total = rows.len();
    let rejected: Vec<(&Delisting, _)> = rows
        .iter()
        .filter_map(|row| validate_delisting(row).err().map(|reason| (row, reason)))
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
    let mut kind = Vec::with_capacity(count);
    let mut permanent_id = Vec::with_capacity(count);
    let mut date = Vec::with_capacity(count);
    let mut reason = Vec::with_capacity(count);
    let mut listing = Vec::with_capacity(count);
    let mut terminal_kind = Vec::with_capacity(count);
    let mut terminal_value = Vec::with_capacity(count);
    let mut convention = Vec::with_capacity(count);
    let mut final_market_cap = Vec::with_capacity(count);
    let mut row_source = Vec::with_capacity(count);

    for (identity, row) in &rows {
        let (state, value, assumed) = encode_terminal(&row.terminal)?;
        ticker.push(identity.ticker.as_str());
        kind.push(identity.kind);
        permanent_id.push(identity.id.as_deref());
        date.push(date_to_days(row.date));
        reason.push(reason_str(row.reason));
        listing.push(listing_str(row.listing));
        terminal_kind.push(state);
        terminal_value.push(value);
        convention.push(assumed);
        // `transpose` turns the `Option` inside out: a row with no figure
        // stays `None`, and a row with one carries the encoding error out. A
        // figure of zero encodes as `Some(0)` and is written as a value, not
        // as a null.
        final_market_cap.push(
            row.final_market_cap
                .map(|value| decimal_to_i128(DATASET, "final_market_cap", value))
                .transpose()?,
        );
        row_source.push(row.source.as_str());
    }

    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(StringArray::from(ticker)) as ArrayRef,
            Arc::new(StringArray::from(kind)),
            Arc::new(StringArray::from(permanent_id)),
            Arc::new(Date32Array::from(date)),
            Arc::new(StringArray::from(reason)),
            Arc::new(StringArray::from(listing)),
            Arc::new(StringArray::from(terminal_kind)),
            Arc::new(
                Decimal128Array::from(terminal_value)
                    .with_precision_and_scale(DECIMAL_PRECISION, DECIMAL_SCALE)?,
            ),
            Arc::new(StringArray::from(convention)),
            Arc::new(
                Decimal128Array::from(final_market_cap)
                    .with_precision_and_scale(DECIMAL_PRECISION, DECIMAL_SCALE)?,
            ),
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

/// Spread one `TerminalValue` across its three columns.
///
/// Building all three from the one enum is what makes an invalid combination
/// unrepresentable on the write side, which is why only the reader checks the
/// invariant.
type TerminalColumns = (&'static str, Option<i128>, Option<&'static str>);

fn encode_terminal(terminal: &TerminalValue) -> Result<TerminalColumns, CurateError> {
    Ok(match terminal {
        TerminalValue::Observed(value) => (
            OBSERVED,
            Some(decimal_to_i128(DATASET, "terminal_value", *value)?),
            None,
        ),
        TerminalValue::Imputed { value, convention } => (
            IMPUTED,
            Some(decimal_to_i128(DATASET, "terminal_value", *value)?),
            Some(convention_str(*convention)),
        ),
        TerminalValue::Unknown => (UNKNOWN, None, None),
    })
}

/// Rebuild a `TerminalValue`, refusing any combination the writer could not
/// have produced.
///
/// The refusals matter more than the successes. An observed state with no value
/// read as `Unknown`, or an imputed state read as observed, would each turn a
/// gap or an assumption into a measurement without anything saying so.
fn decode_terminal(
    row: usize,
    state: &str,
    value: Option<Decimal>,
    convention: Option<&str>,
) -> Result<TerminalValue, CurateError> {
    let invariant = |detail: String| CurateError::TerminalInvariant {
        dataset: DATASET,
        row,
        kind: state.to_owned(),
        detail,
    };

    match state {
        OBSERVED => match (value, convention) {
            (Some(value), None) => Ok(TerminalValue::Observed(value)),
            (None, _) => Err(invariant(
                "carries no value, and an observed value is one that was seen".into(),
            )),
            (Some(_), Some(name)) => Err(invariant(format!(
                "carries imputation convention {name}, which belongs only to an imputed value"
            ))),
        },
        IMPUTED => match (value, convention) {
            (Some(value), Some(name)) => Ok(TerminalValue::Imputed {
                value,
                convention: convention_from(DATASET, name)?,
            }),
            (None, _) => Err(invariant("carries no value".into())),
            (Some(_), None) => Err(invariant(
                "carries no convention, so nothing records what the number assumed".into(),
            )),
        },
        UNKNOWN => match (value, convention) {
            (None, None) => Ok(TerminalValue::Unknown),
            _ => Err(invariant(
                "carries a value or a convention, and unknown means neither".into(),
            )),
        },
        other => Err(CurateError::UnknownEnum {
            dataset: DATASET,
            field: "terminal_kind",
            value: other.to_owned(),
        }),
    }
}

/// What a curated delistings file declares about itself.
pub fn delistings_provenance(path: &Path) -> Result<DelistingProvenance, CurateError> {
    let file = super::open(path)?;
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)?;
    let (units, source) = super::units_and_source(
        DATASET,
        UNITS,
        UNITS_KEY,
        builder.metadata().file_metadata().key_value_metadata(),
    )?;
    Ok(DelistingProvenance { units, source })
}

/// Read every delisting back, in the order the file holds them.
///
/// The units metadata is checked before a single row is decoded, on the same
/// rule the market cap reader applies. `final_market_cap` is a vendor figure in
/// millions, and a file that does not say so is a file whose column cannot be
/// used by anything except this crate.
pub fn read_delistings(path: &Path) -> Result<Vec<Delisting>, CurateError> {
    let file = super::open(path)?;
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)?;
    super::units_and_source(
        DATASET,
        UNITS,
        UNITS_KEY,
        builder.metadata().file_metadata().key_value_metadata(),
    )?;
    let reader = builder.build()?;

    let mut delistings = Vec::new();
    let mut offset = 0usize;

    for batch in reader {
        let batch = batch?;
        let ticker = string_column(&batch, DATASET, "ticker")?;
        let kind = string_column(&batch, DATASET, "permanent_id_kind")?;
        let permanent_id = string_column(&batch, DATASET, "permanent_id")?;
        let date = date_column(&batch, DATASET, "date")?;
        let reason = string_column(&batch, DATASET, "reason")?;
        let listing = string_column(&batch, DATASET, "listing")?;
        let terminal_kind = string_column(&batch, DATASET, "terminal_kind")?;
        let terminal_value = decimal_column(&batch, DATASET, "terminal_value")?;
        let convention = string_column(&batch, DATASET, "imputation_convention")?;
        let final_market_cap = decimal_column(&batch, DATASET, "final_market_cap")?;
        let source = string_column(&batch, DATASET, "source")?;

        for row in 0..batch.num_rows() {
            let absolute = offset + row;
            delistings.push(Delisting {
                asset: decode_identity(
                    DATASET,
                    absolute,
                    required_str(ticker, DATASET, "ticker", row)?,
                    optional_str(kind, row),
                    optional_str(permanent_id, row),
                )?,
                date: required_date(date, DATASET, "date", row)?,
                reason: reason_from(DATASET, required_str(reason, DATASET, "reason", row)?)?,
                listing: listing_from(DATASET, required_str(listing, DATASET, "listing", row)?)?,
                terminal: decode_terminal(
                    absolute,
                    required_str(terminal_kind, DATASET, "terminal_kind", row)?,
                    optional_decimal(terminal_value, DATASET, "terminal_value", row)?,
                    optional_str(convention, row),
                )?,
                final_market_cap: optional_decimal(
                    final_market_cap,
                    DATASET,
                    "final_market_cap",
                    row,
                )?,
                source: required_str(source, DATASET, "source", row)?.to_owned(),
            });
        }
        offset += batch.num_rows();
    }

    Ok(delistings)
}
