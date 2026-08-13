//! The corporate actions dataset: one `ActionRecord` per row.
//!
//! Every action gets its own payload column rather than sharing one value
//! column across kinds. A split ratio, a dividend amount, and a spin-off
//! factor are different quantities that happen to be numbers, and a schema
//! that says so is what a SQL reader leans on. One shared column would need
//! every query to know the kind before it could interpret the value.

use std::path::Path;
use std::sync::Arc;

use ::parquet::file::reader::ChunkReader;
use arrow::array::{ArrayRef, Date32Array, Decimal128Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use rust_decimal::Decimal;

use super::CurateError;
use super::codec::{
    DECIMAL_PRECISION, DECIMAL_SCALE, date_column, date_to_days, decimal_column, decimal_to_i128,
    decode_identity, optional_decimal, optional_str, required_date, required_str, string_column,
};
use super::enums::{dividend_kind_from, dividend_kind_str};
use crate::actions::{ActionRecord, CorporateAction, validate_action};

pub(crate) const DATASET: &str = "actions";

pub(crate) const SPLIT: &str = "split";
pub(crate) const DIVIDEND: &str = "dividend";
pub(crate) const SPIN_OFF: &str = "spin_off";

/// The column layout, which is also what the reader checks a file against.
pub(crate) fn schema() -> Arc<Schema> {
    let decimal = DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE);
    Arc::new(Schema::new(vec![
        Field::new("ticker", DataType::Utf8, false),
        Field::new("permanent_id_kind", DataType::Utf8, true),
        Field::new("permanent_id", DataType::Utf8, true),
        Field::new("effective", DataType::Date32, false),
        Field::new("action_kind", DataType::Utf8, false),
        Field::new("ratio", decimal.clone(), true),
        Field::new("amount", decimal.clone(), true),
        Field::new("dividend_kind", DataType::Utf8, true),
        Field::new("factor", decimal, true),
        Field::new("source", DataType::Utf8, false),
    ]))
}

/// The kind label and the dividend kind, which together complete the row key.
///
/// A regular cash dividend and a special dividend share an ex-date and are two
/// real records, so the dividend kind belongs in the key rather than counting
/// as evidence of a duplicate.
pub(crate) fn key_parts(action: &CorporateAction) -> (&'static str, Option<&'static str>) {
    match action {
        CorporateAction::Split { .. } => (SPLIT, None),
        CorporateAction::Dividend { kind, .. } => (DIVIDEND, Some(dividend_kind_str(*kind))),
        CorporateAction::SpinOff { .. } => (SPIN_OFF, None),
    }
}

/// One action spread across its kind label and three payload columns.
///
/// Building all four from the one enum is what makes an invalid combination
/// unrepresentable on the write side, which is why only the reader checks the
/// invariant.
type ActionColumns = (
    &'static str,
    Option<i128>,
    Option<i128>,
    Option<&'static str>,
    Option<i128>,
);

fn encode_action(action: &CorporateAction) -> Result<ActionColumns, CurateError> {
    Ok(match action {
        CorporateAction::Split { ratio } => (
            SPLIT,
            Some(decimal_to_i128(DATASET, "ratio", *ratio)?),
            None,
            None,
            None,
        ),
        CorporateAction::Dividend { amount, kind } => (
            DIVIDEND,
            None,
            Some(decimal_to_i128(DATASET, "amount", *amount)?),
            Some(dividend_kind_str(*kind)),
            None,
        ),
        CorporateAction::SpinOff { factor } => (
            SPIN_OFF,
            None,
            None,
            None,
            Some(decimal_to_i128(DATASET, "factor", *factor)?),
        ),
    })
}

/// Rebuild a `CorporateAction`, refusing any combination the writer could not
/// have produced.
///
/// The refusals matter more than the successes. A split read as a dividend, or
/// a dividend whose kind went missing, would each put a number into the
/// adjustment pipeline under the wrong meaning.
fn decode_action(
    row: usize,
    kind: &str,
    ratio: Option<Decimal>,
    amount: Option<Decimal>,
    dividend_kind: Option<&str>,
    factor: Option<Decimal>,
) -> Result<CorporateAction, CurateError> {
    let invariant = |detail: String| CurateError::ActionInvariant {
        dataset: DATASET,
        row,
        kind: kind.to_owned(),
        detail,
    };

    match kind {
        SPLIT => match (ratio, amount, dividend_kind, factor) {
            (Some(ratio), None, None, None) => Ok(CorporateAction::Split { ratio }),
            (None, ..) => Err(invariant("carries no ratio".into())),
            _ => Err(invariant(
                "carries a payload belonging to another kind".into(),
            )),
        },
        DIVIDEND => match (ratio, amount, dividend_kind, factor) {
            (None, Some(amount), Some(name), None) => Ok(CorporateAction::Dividend {
                amount,
                kind: dividend_kind_from(DATASET, name)?,
            }),
            (_, None, ..) => Err(invariant("carries no amount".into())),
            (_, _, None, _) => Err(invariant(
                "carries no dividend kind, so nothing says whether it was regular, special, or paid in stock".into(),
            )),
            _ => Err(invariant(
                "carries a payload belonging to another kind".into(),
            )),
        },
        SPIN_OFF => match (ratio, amount, dividend_kind, factor) {
            (None, None, None, Some(factor)) => Ok(CorporateAction::SpinOff { factor }),
            (.., None) => Err(invariant("carries no factor".into())),
            _ => Err(invariant(
                "carries a payload belonging to another kind".into(),
            )),
        },
        other => Err(CurateError::UnknownEnum {
            dataset: DATASET,
            field: "action_kind",
            value: other.to_owned(),
        }),
    }
}

/// Write actions to `path`, returning how many rows landed.
///
/// `source` names the data vendor and is recorded in the file's metadata, the
/// same way [`super::write_prices`] records it and for the same reason: a
/// curated file whose vendor is unknown is one nobody can reconcile. There is
/// deliberately no basis key here. A basis describes how a price series was
/// adjusted, and these rows are events rather than a series.
pub fn write_actions(
    rows: Vec<ActionRecord>,
    path: &Path,
    source: &str,
) -> Result<usize, CurateError> {
    // The writer is the guard. This function is publicly exported, so a caller
    // that never goes through `cli curate` still cannot put a record the domain
    // rejects into a curated file.
    let total = rows.len();
    let rejected: Vec<(&ActionRecord, _)> = rows
        .iter()
        .filter_map(|row| validate_action(row).err().map(|reason| (row, reason)))
        .collect();
    if !rejected.is_empty() {
        return Err(super::invalid_records(DATASET, total, &rejected, |row| {
            format!("{} {}", row.asset.ticker, row.effective)
        }));
    }

    let rows = super::prepare(DATASET, rows, |row| {
        let (kind, subkind) = key_parts(&row.action);
        super::RowKey {
            asset: &row.asset,
            date: row.effective,
            kind: Some(kind),
            subkind,
        }
    })?;
    let count = rows.len();

    let mut ticker = Vec::with_capacity(count);
    let mut id_kind = Vec::with_capacity(count);
    let mut permanent_id = Vec::with_capacity(count);
    let mut effective = Vec::with_capacity(count);
    let mut action_kind = Vec::with_capacity(count);
    let mut ratio = Vec::with_capacity(count);
    let mut amount = Vec::with_capacity(count);
    let mut dividend_kind = Vec::with_capacity(count);
    let mut factor = Vec::with_capacity(count);
    let mut row_source = Vec::with_capacity(count);

    for (identity, row) in &rows {
        let (kind, row_ratio, row_amount, row_dividend_kind, row_factor) =
            encode_action(&row.action)?;
        ticker.push(identity.ticker.as_str());
        id_kind.push(identity.kind);
        permanent_id.push(identity.id.as_deref());
        effective.push(date_to_days(row.effective));
        action_kind.push(kind);
        ratio.push(row_ratio);
        amount.push(row_amount);
        dividend_kind.push(row_dividend_kind);
        factor.push(row_factor);
        row_source.push(row.source.as_str());
    }

    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(StringArray::from(ticker)) as ArrayRef,
            Arc::new(StringArray::from(id_kind)),
            Arc::new(StringArray::from(permanent_id)),
            Arc::new(Date32Array::from(effective)),
            Arc::new(StringArray::from(action_kind)),
            decimals(ratio)?,
            decimals(amount)?,
            Arc::new(StringArray::from(dividend_kind)),
            decimals(factor)?,
            Arc::new(StringArray::from(row_source)),
        ],
    )?;

    super::write_atomically(path, |file| {
        let mut writer = ::parquet::arrow::ArrowWriter::try_new(
            file,
            schema(),
            Some(super::writer_properties(&[(super::SOURCE_KEY, source)])),
        )?;
        writer.write(&batch)?;
        writer.close()?;
        Ok(())
    })?;

    Ok(count)
}

fn decimals(values: Vec<Option<i128>>) -> Result<ArrayRef, CurateError> {
    Ok(Arc::new(
        Decimal128Array::from(values).with_precision_and_scale(DECIMAL_PRECISION, DECIMAL_SCALE)?,
    ))
}

/// Read every action back, in the order the file holds them.
pub fn read_actions(path: &Path) -> Result<Vec<ActionRecord>, CurateError> {
    decode(super::open(path)?)
}

/// The same read, from bytes already in memory.
///
/// Exists so a caller can hash a file and parse it from one `read`. See
/// the `read_*_from_bytes` section of [`super`] for why a second read of the
/// same path is a correctness problem rather than a performance one.
pub fn read_actions_from_bytes(bytes: Bytes) -> Result<Vec<ActionRecord>, CurateError> {
    decode(bytes)
}

/// The body both entry points share, so neither can validate less than the
/// other.
fn decode<R: ChunkReader + 'static>(source: R) -> Result<Vec<ActionRecord>, CurateError> {
    let reader = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(source)?
        .build()?;

    let mut records = Vec::new();
    let mut offset = 0usize;

    for batch in reader {
        let batch = batch?;
        let ticker = string_column(&batch, DATASET, "ticker")?;
        let id_kind = string_column(&batch, DATASET, "permanent_id_kind")?;
        let permanent_id = string_column(&batch, DATASET, "permanent_id")?;
        let effective = date_column(&batch, DATASET, "effective")?;
        let action_kind = string_column(&batch, DATASET, "action_kind")?;
        let ratio = decimal_column(&batch, DATASET, "ratio")?;
        let amount = decimal_column(&batch, DATASET, "amount")?;
        let dividend_kind = string_column(&batch, DATASET, "dividend_kind")?;
        let factor = decimal_column(&batch, DATASET, "factor")?;
        let source = string_column(&batch, DATASET, "source")?;

        for row in 0..batch.num_rows() {
            let absolute = offset + row;
            records.push(ActionRecord {
                asset: decode_identity(
                    DATASET,
                    absolute,
                    required_str(ticker, DATASET, "ticker", row)?,
                    optional_str(id_kind, row),
                    optional_str(permanent_id, row),
                )?,
                effective: required_date(effective, DATASET, "effective", row)?,
                action: decode_action(
                    absolute,
                    required_str(action_kind, DATASET, "action_kind", row)?,
                    optional_decimal(ratio, DATASET, "ratio", row)?,
                    optional_decimal(amount, DATASET, "amount", row)?,
                    optional_str(dividend_kind, row),
                    optional_decimal(factor, DATASET, "factor", row)?,
                )?,
                source: required_str(source, DATASET, "source", row)?.to_owned(),
            });
        }
        offset += batch.num_rows();
    }

    Ok(records)
}
