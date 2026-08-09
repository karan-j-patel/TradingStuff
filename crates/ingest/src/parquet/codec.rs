//! How each Rust value becomes a column value, and how it comes back.
//!
//! Every conversion here is total and checked in both directions. Nothing
//! rounds, nothing coerces an unrecognised label to a default, and no `f64`
//! appears in either direction. A curated file that cannot be read back into
//! the exact value that was written is a file that quietly changed the data.

use arrow::array::{Array, Date32Array, Decimal128Array, StringArray};
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use jiff::civil::Date;
use rust_decimal::Decimal;

use super::error::CurateError;
use crate::schema::{AssetKey, PermanentId};

/// Every Decimal column on disk uses one precision and one scale.
///
/// A single fixed scale means a column's values are directly comparable as
/// integers and that no reader has to ask what scale a particular row used.
/// Scale 9 covers sub-penny prices and fractional share quantities with room to
/// spare, and precision 38 is the widest Decimal128 can hold.
pub(crate) const DECIMAL_PRECISION: u8 = 38;
pub(crate) const DECIMAL_SCALE: i8 = 9;

/// Date32 counts days from this date, so both conversions are anchored here.
const EPOCH: Date = Date::constant(1970, 1, 1);

// --- decimals ---------------------------------------------------------------

/// Convert to the integer stored in a `Decimal128` column, or refuse.
///
/// `Decimal` is a mantissa plus a scale, so 1.5 and 1.500 are the same number
/// held two ways. `normalize` strips the trailing zeros first, which is what
/// makes the question "does this need more than 9 decimal places" a question
/// about the value rather than about how it happens to be written.
pub(crate) fn decimal_to_i128(
    dataset: &'static str,
    field: &'static str,
    value: Decimal,
) -> Result<i128, CurateError> {
    let trimmed = value.normalize();
    let scale = trimmed.scale();
    if scale > DECIMAL_SCALE as u32 {
        return Err(CurateError::InexactDecimal {
            dataset,
            field,
            value: value.to_string(),
        });
    }

    // Shift the mantissa up to the fixed scale. This is exact by construction:
    // the value needed no more than 9 places, so padding it to 9 adds zeros.
    let factor = 10i128.pow(DECIMAL_SCALE as u32 - scale);
    trimmed
        .mantissa()
        .checked_mul(factor)
        .ok_or_else(|| CurateError::DecimalOutOfRange {
            dataset,
            field,
            value: value.to_string(),
        })
}

/// Rebuild a `Decimal` from the stored integer.
pub(crate) fn i128_to_decimal(
    dataset: &'static str,
    field: &'static str,
    raw: i128,
) -> Result<Decimal, CurateError> {
    Decimal::try_from_i128_with_scale(raw, DECIMAL_SCALE as u32).map_err(|_| {
        CurateError::DecimalOutOfRange {
            dataset,
            field,
            value: raw.to_string(),
        }
    })
}

// --- dates ------------------------------------------------------------------

/// Civil dates carry no time, so the difference from the epoch is whole days.
pub(crate) fn date_to_days(date: Date) -> i32 {
    (date.duration_since(EPOCH).as_hours() / 24) as i32
}

pub(crate) fn days_to_date(
    dataset: &'static str,
    field: &'static str,
    days: i32,
) -> Result<Date, CurateError> {
    EPOCH
        .checked_add(jiff::SignedDuration::from_hours(days as i64 * 24))
        .map_err(|_| CurateError::ColumnType {
            dataset,
            field,
            expected: "a representable calendar date".into(),
            found: format!("{days} days from the epoch"),
        })
}

// --- identity ---------------------------------------------------------------

pub(crate) const KIND_SHARADAR: &str = "sharadar";
pub(crate) const KIND_SEC_CIK: &str = "sec_cik";
pub(crate) const KIND_ALPACA: &str = "alpaca";

/// An `AssetKey` spread across the three identity columns every curated
/// dataset shares.
///
/// Identity is kept in three columns rather than flattened into one string
/// because `AssetKey` treats an identified key and an unidentified key with the
/// same ticker as different securities. Flattening loses that distinction on
/// disk, and it is the distinction that stops a reassigned ticker welding two
/// companies' histories together.
pub(crate) struct Identity {
    pub kind: Option<&'static str>,
    pub id: Option<String>,
    pub ticker: String,
}

pub(crate) fn encode_identity(asset: &AssetKey) -> Identity {
    // The two columns are produced together from one enum, so the writer cannot
    // emit half an identifier. That invariant is unrepresentable here, which is
    // why only the reader has to check it.
    let (kind, id) = match &asset.permanent {
        None => (None, None),
        Some(PermanentId::Sharadar(n)) => (Some(KIND_SHARADAR), Some(n.to_string())),
        Some(PermanentId::SecCik(n)) => (Some(KIND_SEC_CIK), Some(n.to_string())),
        Some(PermanentId::Alpaca(s)) => (Some(KIND_ALPACA), Some(s.clone())),
    };
    Identity {
        kind,
        id,
        ticker: asset.ticker.clone(),
    }
}

pub(crate) fn decode_identity(
    dataset: &'static str,
    row: usize,
    ticker: &str,
    kind: Option<&str>,
    id: Option<&str>,
) -> Result<AssetKey, CurateError> {
    let permanent = match (kind, id) {
        (None, None) => None,
        (Some(kind), Some(id)) => Some(match kind {
            KIND_SHARADAR => PermanentId::Sharadar(parse_u64(dataset, kind, id)?),
            KIND_SEC_CIK => PermanentId::SecCik(parse_u64(dataset, kind, id)?),
            KIND_ALPACA => PermanentId::Alpaca(id.to_owned()),
            other => {
                return Err(CurateError::UnknownEnum {
                    dataset,
                    field: "permanent_id_kind",
                    value: other.to_owned(),
                });
            }
        }),
        // Half an identifier is corrupt rather than weak, so it errors instead
        // of degrading to a ticker-only key.
        (kind, id) => {
            return Err(CurateError::HalfNullIdentity {
                dataset,
                row,
                kind: kind.map(str::to_owned),
                id: id.map(str::to_owned),
            });
        }
    };
    Ok(AssetKey {
        ticker: ticker.to_owned(),
        permanent,
    })
}

fn parse_u64(dataset: &'static str, kind: &str, value: &str) -> Result<u64, CurateError> {
    value
        .parse()
        .map_err(|_| CurateError::MalformedPermanentId {
            dataset,
            field: "permanent_id",
            kind: kind.to_owned(),
            value: value.to_owned(),
        })
}

/// Name a key in an error message without pretending a ticker is an identity.
pub(crate) fn describe(asset: &AssetKey) -> String {
    match &asset.permanent {
        None => format!("{} (no permanent id)", asset.ticker),
        Some(PermanentId::Sharadar(n)) => format!("{} (sharadar {n})", asset.ticker),
        Some(PermanentId::SecCik(n)) => format!("{} (sec_cik {n})", asset.ticker),
        Some(PermanentId::Alpaca(s)) => format!("{} (alpaca {s})", asset.ticker),
    }
}

// --- column access ----------------------------------------------------------
//
// Columns are fetched by name and their type is checked, rather than trusted by
// position. A file whose columns moved would otherwise read as valid data with
// every field in the wrong place.

pub(crate) fn string_column<'a>(
    batch: &'a RecordBatch,
    dataset: &'static str,
    field: &'static str,
) -> Result<&'a StringArray, CurateError> {
    column(batch, dataset, field, &DataType::Utf8)
}

pub(crate) fn date_column<'a>(
    batch: &'a RecordBatch,
    dataset: &'static str,
    field: &'static str,
) -> Result<&'a Date32Array, CurateError> {
    column(batch, dataset, field, &DataType::Date32)
}

pub(crate) fn decimal_column<'a>(
    batch: &'a RecordBatch,
    dataset: &'static str,
    field: &'static str,
) -> Result<&'a Decimal128Array, CurateError> {
    // The scale is checked, not assumed. Reading a scale-2 column as scale 9
    // would silently divide every value by ten million.
    column(
        batch,
        dataset,
        field,
        &DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE),
    )
}

/// Fetch one column by name, verify its type, and downcast it.
///
/// The `A: 'static` bound is what `downcast_ref` needs to compare type
/// identities at runtime. It says the concrete array type borrows nothing, not
/// that the reference does.
fn column<'a, A: Array + 'static>(
    batch: &'a RecordBatch,
    dataset: &'static str,
    field: &'static str,
    expected: &DataType,
) -> Result<&'a A, CurateError> {
    let array = batch
        .column_by_name(field)
        .ok_or(CurateError::MissingColumn { dataset, field })?;

    if array.data_type() != expected {
        return Err(CurateError::ColumnType {
            dataset,
            field,
            expected: expected.to_string(),
            found: array.data_type().to_string(),
        });
    }

    array
        .as_any()
        .downcast_ref::<A>()
        .ok_or_else(|| CurateError::ColumnType {
            dataset,
            field,
            expected: expected.to_string(),
            found: array.data_type().to_string(),
        })
}

/// Read a column that must have a value, erroring rather than substituting one.
///
/// Nullability is enforced here on the values instead of being left to the
/// schema flags, because a file written by anything other than this module can
/// declare a column nullable and then leave a hole in it.
pub(crate) fn required_str<'a>(
    array: &'a StringArray,
    dataset: &'static str,
    field: &'static str,
    row: usize,
) -> Result<&'a str, CurateError> {
    if array.is_null(row) {
        return Err(CurateError::NullInRequiredColumn {
            dataset,
            field,
            row,
        });
    }
    Ok(array.value(row))
}

pub(crate) fn optional_str(array: &StringArray, row: usize) -> Option<&str> {
    if array.is_null(row) {
        None
    } else {
        Some(array.value(row))
    }
}

pub(crate) fn required_decimal(
    array: &Decimal128Array,
    dataset: &'static str,
    field: &'static str,
    row: usize,
) -> Result<Decimal, CurateError> {
    if array.is_null(row) {
        return Err(CurateError::NullInRequiredColumn {
            dataset,
            field,
            row,
        });
    }
    i128_to_decimal(dataset, field, array.value(row))
}

/// Read a column that is allowed to be absent, distinguishing absent from zero.
pub(crate) fn optional_decimal(
    array: &Decimal128Array,
    dataset: &'static str,
    field: &'static str,
    row: usize,
) -> Result<Option<Decimal>, CurateError> {
    if array.is_null(row) {
        return Ok(None);
    }
    i128_to_decimal(dataset, field, array.value(row)).map(Some)
}

pub(crate) fn required_date(
    array: &Date32Array,
    dataset: &'static str,
    field: &'static str,
    row: usize,
) -> Result<Date, CurateError> {
    if array.is_null(row) {
        return Err(CurateError::NullInRequiredColumn {
            dataset,
            field,
            row,
        });
    }
    days_to_date(dataset, field, array.value(row))
}
