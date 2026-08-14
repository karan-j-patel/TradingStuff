//! The filings dataset: one [`FundamentalRecord`] per row, wide.
//!
//! # Why wide rather than long
//!
//! A long form, one row per (filing, field, value), would let new fields arrive
//! without a schema change. It would also make the reader's job unbounded: the
//! consumer reads three columns of a known list, and a fixed schema is what the
//! writer-is-the-guard pattern can actually validate. A long table validates
//! nothing about which fields exist.
//!
//! The columns are fixed at the probe-verified list. Every one of them is
//! nullable, because the vendor expresses "no figure" as a JSON null on a row it
//! still ships, and null is therefore a real state rather than corruption.
//!
//! # The two dates, and which one anything may join on
//!
//! `as_reported` is the day the filing became public. `period_end` is the fiscal
//! period it describes and precedes `as_reported` by weeks or months. Every
//! research join uses `as_reported`; joining on `period_end` is the classic
//! lookahead bug and it is exactly what the vendor's MR dimensions do. See
//! [`FundamentalRecord`] for the full statement.
//!
//! `calendardate` is fetched and deliberately NOT stored. The probe found it is
//! the calendar year end regardless of fiscal end, so it is a third date that
//! looks joinable and is not. A column nobody may use is a trap with a schema
//! entry.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use ::parquet::file::reader::ChunkReader;
use arrow::array::{ArrayRef, Date32Array, Decimal128Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;

use super::CurateError;
use super::codec::{
    DECIMAL_PRECISION, DECIMAL_SCALE, Identity, date_column, date_to_days, decimal_column,
    decimal_to_i128, decode_identity, describe, encode_identity, optional_decimal, optional_str,
    required_date, required_str, string_column,
};
use super::enums::{basis_from, basis_str, scope_from, scope_str};
use super::prices::SOURCE_KEY;
use crate::provider::{FundamentalRecord, ReportBasis, ReportScope};
use crate::schema::AssetKey;
use jiff::civil::Date;

pub(crate) const DATASET: &str = "filings";

/// The metadata key naming what units the money columns are in.
pub const UNITS_KEY: &str = "equity_units";

/// Whole US dollars, exactly as the vendor ships them.
///
/// Pinned against EDGAR to the digit during the Phase A probe for `equityusd`.
/// The `as_shipped` half says the number was written the way the vendor sent it,
/// so nothing downstream has to wonder whether somebody already converted.
///
/// Note what this does NOT claim: the bare columns (`equity`, `netinc`,
/// `revenue`) are in the filer's REPORTING currency, and only the `*usd`
/// columns are comparable across a mixed-currency universe. The label covers
/// the units of the dollar columns; the currency of the bare ones is a property
/// of the filer.
pub const UNITS: &str = "usd_as_shipped";

/// The value columns, in schema order.
///
/// One list rather than two, so the writer and the reader cannot disagree about
/// which fields exist. Adding a column here adds it to both halves and to the
/// round-trip test at once.
///
/// `equityusd` is the only one any strategy reads today. The rest are fetched
/// and stored because one quota walk serves the value rail and the later quality
/// and profitability characteristics, and a second walk to collect columns the
/// first request could have carried is a cost with nothing bought.
pub(crate) const VALUE_COLUMNS: &[&str] = &[
    "equity",
    "equityusd",
    "netinc",
    "netinccmnusd",
    "revenue",
    "revenueusd",
    "assets",
    "sharesbas",
    "shareswa",
];

/// What a filings file says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilingsProvenance {
    /// Always [`UNITS`] for a file this reader accepts.
    pub units: String,
    /// The data vendor the rows came from.
    pub source: String,
}

/// The column layout, which is also what the reader checks a file against.
pub(crate) fn schema() -> Arc<Schema> {
    let money = DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE);
    let mut fields = vec![
        Field::new("ticker", DataType::Utf8, false),
        Field::new("permanent_id_kind", DataType::Utf8, true),
        Field::new("permanent_id", DataType::Utf8, true),
        // The fiscal period. Never a join key. See the module documentation.
        Field::new("period_end", DataType::Date32, false),
        // The day the filing became public. This is the join key.
        Field::new("as_reported", DataType::Date32, false),
        Field::new("basis", DataType::Utf8, false),
        Field::new("scope", DataType::Utf8, false),
        // Null throughout for this vendor, which ships no accession number. The
        // column exists so a second provider that does ship one fits without a
        // schema break, and so the six-part filing identity of `CLAUDE.md` rule
        // 4 is representable on disk rather than only in memory.
        Field::new("filing_id", DataType::Utf8, true),
    ];
    fields.extend(
        VALUE_COLUMNS
            .iter()
            .map(|name| Field::new(*name, money.clone(), true)),
    );
    Arc::new(Schema::new(fields))
}

/// Write records to `path`, returning how many rows landed.
///
/// # The duplicate rule, and why it is `prepare`'s
///
/// `(asset, basis, scope, period_end)` must be unique. A second row is a hard
/// error, never a dedup: the probe verified AR carries exactly one row per
/// period on every subject it checked, so a duplicate means the fetch or the
/// vendor broke, and silently keeping either row would pick a filing nobody
/// chose.
///
/// That key is carried by [`super::RowKey`] directly. Its two discriminators are
/// `Option<&'static str>` and [`basis_str`] and [`scope_str`] return exactly
/// that, so the shared [`super::prepare`] both enforces the rule and fixes the
/// row order. Writing a second duplicate check beside it would be a second
/// implementation of a guarantee that already has one, and the two could drift.
pub fn write_filings(
    rows: Vec<FundamentalRecord>,
    path: &Path,
    source: &str,
) -> Result<usize, CurateError> {
    let rows = prepare_filings(rows)?;
    let count = rows.len();

    let mut ticker = Vec::with_capacity(count);
    let mut id_kind = Vec::with_capacity(count);
    let mut permanent_id = Vec::with_capacity(count);
    let mut period_end = Vec::with_capacity(count);
    let mut as_reported = Vec::with_capacity(count);
    let mut basis = Vec::with_capacity(count);
    let mut scope = Vec::with_capacity(count);
    let mut filing_id = Vec::with_capacity(count);
    let mut values: Vec<Vec<Option<i128>>> = VALUE_COLUMNS
        .iter()
        .map(|_| Vec::with_capacity(count))
        .collect();

    for (identity, row) in &rows {
        ticker.push(identity.ticker.as_str());
        id_kind.push(identity.kind);
        permanent_id.push(identity.id.as_deref());
        period_end.push(date_to_days(row.period_end));
        as_reported.push(date_to_days(row.as_reported));
        basis.push(basis_str(row.basis));
        scope.push(scope_str(row.scope));
        filing_id.push(row.filing_id.as_deref());
        for (column, slot) in VALUE_COLUMNS.iter().zip(values.iter_mut()) {
            // Absent stays absent. Substituting zero would make a company that
            // reported nothing indistinguishable from one that reported zero,
            // and for `equityusd` those are a name with no data and a name with
            // no book value.
            slot.push(
                row.fields
                    .get(*column)
                    .map(|value| decimal_to_i128(DATASET, column, *value))
                    .transpose()?,
            );
        }
    }

    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(ticker)),
        Arc::new(StringArray::from(id_kind)),
        Arc::new(StringArray::from(permanent_id)),
        Arc::new(Date32Array::from(period_end)),
        Arc::new(Date32Array::from(as_reported)),
        Arc::new(StringArray::from(basis)),
        Arc::new(StringArray::from(scope)),
        Arc::new(StringArray::from(filing_id)),
    ];
    for slot in values {
        columns.push(Arc::new(
            Decimal128Array::from(slot)
                .with_precision_and_scale(DECIMAL_PRECISION, DECIMAL_SCALE)?,
        ));
    }

    let batch = RecordBatch::try_new(schema(), columns)?;

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

/// Reject duplicates on the five-part key, then put the rows in a
/// deterministic order.
///
/// # Why this is not `super::prepare`
///
/// The shared helper keys on `(asset, date, kind, subkind)`, one date. This
/// dataset needs two: a company can file twice for one period, and the second
/// filing is a new public event rather than a duplicate row.
///
/// Measured, not theorised. SOLR1 (sharadar 101544) carries two as-reported
/// rows for period 1996-12-31, filed 1997-04-15 and 1997-04-30 with identical
/// values, and two for 1997-09-30, filed 1997-11-12 and 1998-01-28 with
/// DIFFERENT equity and net income. The second pair is a genuine amendment. Both
/// rows of both pairs are true point-in-time facts: on 1997-11-12 the world knew
/// the first set of figures, and only from 1998-01-28 did it know the second.
/// Collapsing them would either invent a restatement nobody could see yet or
/// discard the correction entirely.
///
/// The probe's "as-reported keeps the first filing, amendments never appear" was
/// verified on a 2017 filer and does not hold for 1990s names.
///
/// So the duplicate rule is `(asset, basis, scope, period_end, as_reported)`.
/// Two filings for one period on different dates are two rows; the same period
/// filed twice on the same date is still a hard error, because that is the
/// vendor sending one event twice and keeping either copy picks a filing nobody
/// chose.
///
/// The ordering came with it. `super::sort_key` has no `as_reported` term, so
/// two rows differing only in filing date would hold their input order and the
/// file's bytes would depend on the order the fetch happened to collect them.
fn prepare_filings(
    rows: Vec<FundamentalRecord>,
) -> Result<Vec<(Identity, FundamentalRecord)>, CurateError> {
    if rows.is_empty() {
        return Err(CurateError::EmptyDataset { dataset: DATASET });
    }

    let mut seen: HashSet<(AssetKey, ReportBasis, ReportScope, Date, Date)> =
        HashSet::with_capacity(rows.len());
    for row in &rows {
        let key = (
            row.asset.clone(),
            row.basis,
            row.scope,
            row.period_end,
            row.as_reported,
        );
        if !seen.insert(key) {
            return Err(CurateError::DuplicateRow {
                dataset: DATASET,
                asset: describe(&row.asset),
                date: row.period_end,
                qualifier: format!(
                    " ({} {} filed {})",
                    basis_str(row.basis),
                    scope_str(row.scope),
                    row.as_reported
                ),
            });
        }
    }

    let mut decorated: Vec<(Identity, FundamentalRecord)> = rows
        .into_iter()
        .map(|row| (encode_identity(&row.asset), row))
        .collect();

    // Identity first with nulls last, matching `super::sort_key`, then the two
    // dates. Borrowed throughout, so ordering allocates nothing.
    decorated.sort_by(|left, right| {
        let key = |identity: &'_ Identity, row: &'_ FundamentalRecord| {
            (
                identity.kind.is_none(),
                identity.kind,
                identity.id.clone(),
                identity.ticker.clone(),
                row.period_end,
                row.as_reported,
                basis_str(row.basis),
                scope_str(row.scope),
            )
        };
        key(&left.0, &left.1).cmp(&key(&right.0, &right.1))
    });

    Ok(decorated)
}

/// Read every record back, in the order the file holds them.
pub fn read_filings(path: &Path) -> Result<Vec<FundamentalRecord>, CurateError> {
    decode(super::open(path)?)
}

/// The same read, from bytes already in memory.
///
/// Present from this dataset's first commit rather than added later. The
/// read-once rule exists so a recorded digest and the parsed records describe
/// the same bytes, and a dataset that shipped without it would be the one
/// attachment where that is not true.
pub fn read_filings_from_bytes(bytes: Bytes) -> Result<Vec<FundamentalRecord>, CurateError> {
    decode(bytes)
}

/// What a curated filings file declares about itself.
pub fn filings_provenance(path: &Path) -> Result<FilingsProvenance, CurateError> {
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        super::open(path)?,
    )?;
    provenance_of(builder.metadata().file_metadata().key_value_metadata())
}

fn provenance_of(
    metadata: Option<&Vec<::parquet::file::metadata::KeyValue>>,
) -> Result<FilingsProvenance, CurateError> {
    let (units, source) = super::units_and_source(DATASET, UNITS, UNITS_KEY, metadata)?;
    Ok(FilingsProvenance { units, source })
}

/// The body both entry points share, so neither can validate less than the
/// other. The units and source check runs before a row is decoded on both.
fn decode<R: ChunkReader + 'static>(source: R) -> Result<Vec<FundamentalRecord>, CurateError> {
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(source)?;
    let provenance = provenance_of(builder.metadata().file_metadata().key_value_metadata())?;
    let reader = builder.build()?;

    let mut records = Vec::new();
    let mut offset = 0usize;

    for batch in reader {
        let batch = batch?;
        let ticker = string_column(&batch, DATASET, "ticker")?;
        let id_kind = string_column(&batch, DATASET, "permanent_id_kind")?;
        let permanent_id = string_column(&batch, DATASET, "permanent_id")?;
        let period_end = date_column(&batch, DATASET, "period_end")?;
        let as_reported = date_column(&batch, DATASET, "as_reported")?;
        let basis = string_column(&batch, DATASET, "basis")?;
        let scope = string_column(&batch, DATASET, "scope")?;
        let filing_id = string_column(&batch, DATASET, "filing_id")?;
        let values: Vec<_> = VALUE_COLUMNS
            .iter()
            .map(|name| decimal_column(&batch, DATASET, name))
            .collect::<Result<_, _>>()?;

        for row in 0..batch.num_rows() {
            let mut fields = std::collections::BTreeMap::new();
            for (name, column) in VALUE_COLUMNS.iter().zip(&values) {
                // A null column contributes no entry, which is what makes the
                // map's absence mean the vendor's null rather than a zero.
                if let Some(value) = optional_decimal(column, DATASET, name, row)? {
                    fields.insert((*name).to_string(), value);
                }
            }
            records.push(FundamentalRecord {
                asset: decode_identity(
                    DATASET,
                    offset + row,
                    required_str(ticker, DATASET, "ticker", row)?,
                    optional_str(id_kind, row),
                    optional_str(permanent_id, row),
                )?,
                as_reported: required_date(as_reported, DATASET, "as_reported", row)?,
                period_end: required_date(period_end, DATASET, "period_end", row)?,
                // The file records when the filing was public, not when this
                // snapshot was pulled. A curated file is one snapshot, so the
                // read-back stamps every row with the day it was written, which
                // the provenance carries. Nothing on the backtest path reads it.
                observed_at: required_date(as_reported, DATASET, "as_reported", row)?,
                source: provenance.source.clone(),
                basis: basis_from(DATASET, required_str(basis, DATASET, "basis", row)?)?,
                scope: scope_from(DATASET, required_str(scope, DATASET, "scope", row)?)?,
                filing_id: optional_str(filing_id, row).map(str::to_owned),
                fields,
            });
        }
        offset += batch.num_rows();
    }

    Ok(records)
}
