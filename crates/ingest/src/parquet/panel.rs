//! The characteristic panel: one row per (security, month-end), wide.
//!
//! This is the boundary `CLAUDE.md` names. Rust writes it and Python reads it,
//! and neither calls the other. It is the sixth curated dataset and the first
//! one this codebase produces rather than fetches, which changes what its
//! provenance has to say.
//!
//! # Why the provenance is seven values rather than a units label
//!
//! The other six datasets carry a vendor's rows, so the question a reader has
//! is what units the vendor shipped and who the vendor was. This file carries
//! numbers that came out of the engine, so the question is instead which inputs
//! and which configuration produced them. That is answered by the hash of the
//! exporting configuration together with the digest of every file it read, and
//! those seven values name the run exactly rather than describing it.
//!
//! A source label would be strictly weaker here. "Sharadar" does not say which
//! prices, and the six digests do. So [`read_panel`] refuses a file missing any
//! of the seven rather than checking [`super::prices::SOURCE_KEY`], which is why
//! this dataset does not go through [`super::units_and_source`]: that helper
//! checks exactly two keys and would pass a file carrying none of these.
//!
//! # Why every characteristic column is nullable and none is imputed
//!
//! A characteristic that cannot be computed is absent, never zero and never a
//! median. Imputation is a modelling choice and belongs in the fit script where
//! it is visible, not baked into the artifact where every later reader inherits
//! it without being told. Zero is an ordinary value for a momentum signal, for a
//! share change and for a dividend yield, so substituting it would rank a name
//! with no data in the middle of the field.
//!
//! # The one transformation this writer performs, stated because it is one
//!
//! Characteristics are ratios of `Decimal`s and carry up to the type's full 28
//! decimal places. The curated column scale is fixed at
//! [`super::codec::DECIMAL_SCALE`], so each value is rounded to it here. That is
//! a storage decision rather than a modelling one, and it is done at the writer
//! so that [`crate::parquet::codec::decimal_to_i128`] keeps refusing anything
//! unexpected and the in-memory values the engine produced stay exact for the
//! equality tests that pin them against the strategies.
//!
//! Nine decimal places on a volatility of about 0.02 is eight significant
//! figures, and the consumer rank-transforms these columns cross-sectionally, so
//! a tie at eight significant figures is a genuine near-tie rather than an
//! artifact of the scale.

use std::path::Path;
use std::sync::Arc;

use ::parquet::file::metadata::KeyValue;
use ::parquet::file::reader::ChunkReader;
use arrow::array::{ArrayRef, BooleanArray, Date32Array, Decimal128Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use jiff::civil::Date;
use rust_decimal::Decimal;

use super::CurateError;
use super::codec::{
    DECIMAL_PRECISION, DECIMAL_SCALE, bool_column, date_column, date_to_days, decimal_column,
    decimal_to_i128, decode_identity, optional_bool, optional_decimal, optional_str, required_bool,
    required_date, required_str, string_column,
};
use crate::schema::AssetKey;

pub(crate) const DATASET: &str = "panel";

/// The metadata key carrying the hash of the configuration that exported the
/// file.
pub const CONFIG_HASH_KEY: &str = "config_hash";

/// The metadata keys carrying the digest of every file the export read.
///
/// Six: the universe list, the prices file, and the four attached datasets.
///
/// Prices is here even though every run has one and it is never optional. A
/// digest that lives only inside `config_hash` cannot be compared against
/// anything, because a run ranking on a different strategy has a different
/// config hash by construction, so the two can never be equal even when they
/// read identical bytes. The binding a predictions file needs is digest by
/// digest, and that requires each digest to be its own key.
///
/// One list rather than five constants, so the writer, the reader and the
/// provenance struct cannot disagree about which digests a file must carry.
pub const DIGEST_KEYS: &[&str] = &[
    "universe_sha256",
    "prices_sha256",
    "actions_sha256",
    "delistings_sha256",
    "marketcap_sha256",
    "filings_sha256",
];

/// What a panel file says about itself: the configuration and the inputs.
///
/// Every field is required and non-empty. There is no partial state: an export
/// runs against all five inputs or it does not run, so a file naming four of
/// them was written by something other than this codebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelProvenance {
    /// The exporting [`crate`]-external configuration's canonical hash.
    pub config_hash: String,
    pub universe_sha256: String,
    pub prices_sha256: String,
    pub actions_sha256: String,
    pub delistings_sha256: String,
    pub marketcap_sha256: String,
    pub filings_sha256: String,
}

impl PanelProvenance {
    /// The six digests in [`DIGEST_KEYS`] order.
    fn digests(&self) -> [&str; 6] {
        [
            &self.universe_sha256,
            &self.prices_sha256,
            &self.actions_sha256,
            &self.delistings_sha256,
            &self.marketcap_sha256,
            &self.filings_sha256,
        ]
    }
}

/// One security's characteristics at one month-end, plus the forward label.
///
/// The key is `(asset, month_end)`. A second row for one key is a hard error at
/// the writer, never a deduplication, because the two rows describe the same
/// name on the same day and keeping either one picks an answer nobody chose.
///
/// The window each characteristic spans is fixed by the exporting configuration
/// and is named in the column, so a consumer reading `vol_monthly_36m` does not
/// have to be told out of band how many months it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelRow {
    pub asset: AssetKey,
    /// The last trading day of the calendar month, which is not the calendar
    /// month end. See `engine::Panel::month_ends`.
    pub month_end: Date,

    /// Price return over the twelve months ending one month before `month_end`.
    pub momentum_12_1: Option<Decimal>,
    /// Sample standard deviation of daily price returns over the same window.
    pub vol_daily_12m: Option<Decimal>,
    /// Sample standard deviation of monthly TOTAL returns over the thirty-six
    /// month-ends ending at `month_end` inclusive. A different measure from
    /// `vol_daily_12m`, not a longer one: this one includes cash dividends.
    pub vol_monthly_36m: Option<Decimal>,
    /// Natural log of the market capitalisation at `month_end`, in the vendor's
    /// own units.
    ///
    /// The log is what makes the units harmless. A change of units multiplies
    /// every market cap by one constant, so it adds one constant to every log,
    /// and the cross-sectional rank transform the consumer applies erases an
    /// additive constant exactly. Exporting the level instead would leave the
    /// units mattering for no gain.
    pub log_marketcap: Option<Decimal>,
    /// Book equity over market equity at `month_end`.
    pub book_to_market: Option<Decimal>,
    /// Trailing twelve months of cash dividends per share over the close.
    pub dividend_yield_12m: Option<Decimal>,
    /// Change in shares outstanding: the level at `month_end` over the mean of
    /// the twenty-four month-ends before it. Positive is issuance.
    pub share_change_24m: Option<Decimal>,
    /// Median daily dollar volume over the twelve months ending one month
    /// before `month_end`.
    pub median_dollar_volume_12m: Option<Decimal>,

    /// Whether the name cleared the three rules that do not depend on a
    /// ranking: it traded at `month_end`, above the price floor, with enough
    /// bars across the formation window.
    ///
    /// Not nullable. Every row here belongs to a name with a bar at
    /// `month_end`, so the three rules always have an answer.
    pub eligible: bool,

    /// Total return over `(month_end, next month_end]`, dividends included and
    /// the delisting convention applied where the name stops trading.
    ///
    /// Null at the final month-end of the panel, where there is no next one.
    pub label_return_1m: Option<Decimal>,
    /// Whether the name stopped trading inside the label's window.
    ///
    /// Null exactly when [`PanelRow::label_return_1m`] is null. A `false` there
    /// would assert that the name kept trading through a window that was never
    /// measured, which is the imputation this dataset refuses everywhere else.
    pub label_delisted_in_window: Option<bool>,
}

/// The column layout, which is also what the reader checks a file against.
pub(crate) fn schema() -> Arc<Schema> {
    let characteristic = DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE);
    let nullable = |name: &str| Field::new(name, characteristic.clone(), true);
    Arc::new(Schema::new(vec![
        Field::new("ticker", DataType::Utf8, false),
        Field::new("permanent_id_kind", DataType::Utf8, true),
        Field::new("permanent_id", DataType::Utf8, true),
        Field::new("month_end", DataType::Date32, false),
        nullable("momentum_12_1"),
        nullable("vol_daily_12m"),
        nullable("vol_monthly_36m"),
        nullable("log_marketcap"),
        nullable("book_to_market"),
        nullable("dividend_yield_12m"),
        nullable("share_change_24m"),
        nullable("median_dollar_volume_12m"),
        Field::new("eligible", DataType::Boolean, false),
        nullable("label_return_1m"),
        Field::new("label_delisted_in_window", DataType::Boolean, true),
    ]))
}

/// Write rows to `path`, returning how many landed.
///
/// The duplicate rule is `(asset, month_end)` and it is [`super::prepare`]'s,
/// which also fixes the row order. Writing a second check beside it would be a
/// second implementation of one guarantee and the two could drift.
pub fn write_panel(
    rows: Vec<PanelRow>,
    path: &Path,
    provenance: &PanelProvenance,
) -> Result<usize, CurateError> {
    // Refused rather than written. A file whose provenance cannot be read back
    // is one nobody can reproduce, and the reader refuses it anyway, so writing
    // it would only move the failure to whoever picks the file up next.
    check_provenance(DATASET, provenance)?;

    let rows = super::prepare(DATASET, rows, |row| {
        super::RowKey::simple(&row.asset, row.month_end)
    })?;
    let count = rows.len();

    let mut ticker = Vec::with_capacity(count);
    let mut id_kind = Vec::with_capacity(count);
    let mut permanent_id = Vec::with_capacity(count);
    let mut month_end = Vec::with_capacity(count);
    let mut eligible = Vec::with_capacity(count);
    let mut label = Vec::with_capacity(count);
    let mut delisted = Vec::with_capacity(count);
    // Eight parallel columns, in schema order. A vector of vectors rather than
    // eight named locals so that the writer walks the same list twice, once to
    // fill and once to encode, and cannot fill one and forget it.
    let mut characteristics: Vec<Vec<Option<i128>>> = (0..CHARACTERISTIC_COUNT)
        .map(|_| Vec::with_capacity(count))
        .collect();

    for (identity, row) in &rows {
        ticker.push(identity.ticker.as_str());
        id_kind.push(identity.kind);
        permanent_id.push(identity.id.as_deref());
        month_end.push(date_to_days(row.month_end));
        eligible.push(row.eligible);
        label.push(
            row.label_return_1m
                .map(|value| stored("label_return_1m", value))
                .transpose()?,
        );
        delisted.push(row.label_delisted_in_window);
        for (slot, (name, value)) in characteristics.iter_mut().zip(characteristic_values(row)) {
            slot.push(value.map(|value| stored(name, value)).transpose()?);
        }
    }

    // Schema order exactly: the four identity and key columns, the eight
    // characteristics, then the eligibility flag and the two label columns
    // around it. `RecordBatch::try_new` checks the count and the types, which
    // is what caught this list being one column short while it was written.
    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(ticker)),
        Arc::new(StringArray::from(id_kind)),
        Arc::new(StringArray::from(permanent_id)),
        Arc::new(Date32Array::from(month_end)),
    ];
    for slot in characteristics {
        columns.push(decimals(slot)?);
    }
    columns.push(Arc::new(BooleanArray::from(eligible)));
    columns.push(decimals(label)?);
    columns.push(Arc::new(BooleanArray::from(delisted)));

    let batch = RecordBatch::try_new(schema(), columns)?;

    let metadata = provenance_metadata(provenance);
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
pub fn read_panel(path: &Path) -> Result<Vec<PanelRow>, CurateError> {
    decode(super::open(path)?)
}

/// The same read, from bytes already in memory.
///
/// Present from this dataset's first commit rather than added later, on the
/// rule the `read_*_from_bytes` section of [`super`] states. A caller recording
/// this file's digest and then parsing a second read of the path is claiming
/// the numbers came from bytes it never checked.
pub fn read_panel_from_bytes(bytes: Bytes) -> Result<Vec<PanelRow>, CurateError> {
    decode(bytes)
}

/// What a curated panel file declares about itself.
pub fn panel_provenance(path: &Path) -> Result<PanelProvenance, CurateError> {
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        super::open(path)?,
    )?;
    provenance_of(
        DATASET,
        builder.metadata().file_metadata().key_value_metadata(),
    )
}

/// How many characteristic columns sit between the key and the flags.
const CHARACTERISTIC_COUNT: usize = 8;

/// Each characteristic column's name and this row's value for it, in schema
/// order.
///
/// One function rather than the list written out at the writer and again at the
/// reader. The names travel with the values, so a column renamed in the schema
/// and not here fails at `RecordBatch::try_new` rather than writing a value
/// under the wrong heading.
fn characteristic_values(
    row: &PanelRow,
) -> [(&'static str, Option<Decimal>); CHARACTERISTIC_COUNT] {
    [
        ("momentum_12_1", row.momentum_12_1),
        ("vol_daily_12m", row.vol_daily_12m),
        ("vol_monthly_36m", row.vol_monthly_36m),
        ("log_marketcap", row.log_marketcap),
        ("book_to_market", row.book_to_market),
        ("dividend_yield_12m", row.dividend_yield_12m),
        ("share_change_24m", row.share_change_24m),
        ("median_dollar_volume_12m", row.median_dollar_volume_12m),
    ]
}

/// Wrap the raw integers in an array declaring the fixed precision and scale,
/// so a reader never has to guess where the decimal point sits.
fn decimals(values: Vec<Option<i128>>) -> Result<ArrayRef, CurateError> {
    Ok(Arc::new(
        Decimal128Array::from(values).with_precision_and_scale(DECIMAL_PRECISION, DECIMAL_SCALE)?,
    ))
}

/// One characteristic, rounded to the column scale and encoded.
///
/// The rounding is this dataset's and this dataset's only. See the module
/// documentation for why it happens here rather than in the codec, which still
/// refuses a value it was not asked to round.
fn stored(field: &'static str, value: Decimal) -> Result<i128, CurateError> {
    decimal_to_i128(DATASET, field, value.round_dp(DECIMAL_SCALE as u32))
}

/// The six key-value pairs a panel file carries.
pub(crate) fn provenance_metadata(provenance: &PanelProvenance) -> Vec<(&str, &str)> {
    let mut metadata = vec![(CONFIG_HASH_KEY, provenance.config_hash.as_str())];
    metadata.extend(DIGEST_KEYS.iter().copied().zip(provenance.digests()));
    metadata
}

/// Refuse provenance that is missing or blank anywhere.
///
/// Applied by the writer and, through [`provenance_of`], by the reader, so a
/// file cannot exist with less than a reader demands.
pub(crate) fn check_provenance(
    dataset: &'static str,
    provenance: &PanelProvenance,
) -> Result<(), CurateError> {
    // Both halves of the chain yield `&'static str`, which is what
    // `CurateError`'s key field takes, so the key travels into the error
    // without being looked up again.
    let named = std::iter::once((CONFIG_HASH_KEY, provenance.config_hash.as_str()))
        .chain(DIGEST_KEYS.iter().copied().zip(provenance.digests()));
    for (key, value) in named {
        if value.trim().is_empty() {
            return Err(CurateError::UnexpectedMetadata {
                dataset,
                key,
                expected: "a non-empty hex digest",
                found: value.to_owned(),
            });
        }
    }
    Ok(())
}

/// Read the seven keys, refusing a file that declares less than all of them.
///
/// # Why this does not go through `units_and_source`
///
/// That helper checks a units key and a source key, which is two keys, and a
/// panel file carries neither. A reader that called it would pass a file with
/// no configuration hash and no digests at all, which is exactly the file this
/// check exists to refuse.
pub(crate) fn provenance_of(
    dataset: &'static str,
    metadata: Option<&Vec<KeyValue>>,
) -> Result<PanelProvenance, CurateError> {
    let value = |key: &'static str| -> Result<String, CurateError> {
        metadata
            .into_iter()
            .flatten()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.value.as_deref())
            .map(str::to_owned)
            .ok_or(CurateError::MissingMetadata { dataset, key })
    };

    let provenance = PanelProvenance {
        config_hash: value(CONFIG_HASH_KEY)?,
        universe_sha256: value(DIGEST_KEYS[0])?,
        prices_sha256: value(DIGEST_KEYS[1])?,
        actions_sha256: value(DIGEST_KEYS[2])?,
        delistings_sha256: value(DIGEST_KEYS[3])?,
        marketcap_sha256: value(DIGEST_KEYS[4])?,
        filings_sha256: value(DIGEST_KEYS[5])?,
    };
    // A key present and blank passes the lookup above and carries no
    // provenance, which is the gap a presence-only rule leaves open.
    check_provenance(dataset, &provenance)?;
    Ok(provenance)
}

/// The body both entry points share, so neither can validate less than the
/// other. The provenance check runs before a row is decoded on both.
fn decode<R: ChunkReader + 'static>(source: R) -> Result<Vec<PanelRow>, CurateError> {
    let builder = ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(source)?;
    provenance_of(
        DATASET,
        builder.metadata().file_metadata().key_value_metadata(),
    )?;
    let reader = builder.build()?;

    let mut rows = Vec::new();
    let mut offset = 0usize;

    for batch in reader {
        let batch = batch?;
        let ticker = string_column(&batch, DATASET, "ticker")?;
        let id_kind = string_column(&batch, DATASET, "permanent_id_kind")?;
        let permanent_id = string_column(&batch, DATASET, "permanent_id")?;
        let month_end = date_column(&batch, DATASET, "month_end")?;
        let momentum = decimal_column(&batch, DATASET, "momentum_12_1")?;
        let vol_daily = decimal_column(&batch, DATASET, "vol_daily_12m")?;
        let vol_monthly = decimal_column(&batch, DATASET, "vol_monthly_36m")?;
        let log_marketcap = decimal_column(&batch, DATASET, "log_marketcap")?;
        let book_to_market = decimal_column(&batch, DATASET, "book_to_market")?;
        let dividend_yield = decimal_column(&batch, DATASET, "dividend_yield_12m")?;
        let share_change = decimal_column(&batch, DATASET, "share_change_24m")?;
        let dollar_volume = decimal_column(&batch, DATASET, "median_dollar_volume_12m")?;
        let eligible = bool_column(&batch, DATASET, "eligible")?;
        let label = decimal_column(&batch, DATASET, "label_return_1m")?;
        let delisted = bool_column(&batch, DATASET, "label_delisted_in_window")?;

        for row in 0..batch.num_rows() {
            rows.push(PanelRow {
                asset: decode_identity(
                    DATASET,
                    offset + row,
                    required_str(ticker, DATASET, "ticker", row)?,
                    optional_str(id_kind, row),
                    optional_str(permanent_id, row),
                )?,
                month_end: required_date(month_end, DATASET, "month_end", row)?,
                momentum_12_1: optional_decimal(momentum, DATASET, "momentum_12_1", row)?,
                vol_daily_12m: optional_decimal(vol_daily, DATASET, "vol_daily_12m", row)?,
                vol_monthly_36m: optional_decimal(vol_monthly, DATASET, "vol_monthly_36m", row)?,
                log_marketcap: optional_decimal(log_marketcap, DATASET, "log_marketcap", row)?,
                book_to_market: optional_decimal(book_to_market, DATASET, "book_to_market", row)?,
                dividend_yield_12m: optional_decimal(
                    dividend_yield,
                    DATASET,
                    "dividend_yield_12m",
                    row,
                )?,
                share_change_24m: optional_decimal(share_change, DATASET, "share_change_24m", row)?,
                median_dollar_volume_12m: optional_decimal(
                    dollar_volume,
                    DATASET,
                    "median_dollar_volume_12m",
                    row,
                )?,
                eligible: required_bool(eligible, DATASET, "eligible", row)?,
                label_return_1m: optional_decimal(label, DATASET, "label_return_1m", row)?,
                label_delisted_in_window: optional_bool(delisted, row),
            });
        }
        offset += batch.num_rows();
    }

    Ok(rows)
}
