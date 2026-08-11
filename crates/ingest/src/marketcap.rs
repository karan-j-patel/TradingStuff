//! Market capitalisation, one figure per security per day.
//!
//! *Market capitalisation* is share price times shares outstanding, the plain
//! measure of how big a company is. Three planned consumers need it and none of
//! them is built yet: value weighting a portfolio by size, deriving a share
//! count as `marketcap / close_unadjusted` so the change in shares over a year
//! gives buybacks and issuance, and ranking a universe by size.
//!
//! # The figure is stored as the vendor ships it
//!
//! This vendor ships market cap in **millions of US dollars**, to one decimal
//! place. That was measured rather than read off documentation, by
//! `cli ingest probe-daily` on 2026-08-11: for three large securities on one
//! date, the shipped figure divided by that day's unadjusted close gives a
//! share count matching each company's own filings, while reading the figure as
//! whole dollars implies share counts a million times too small. The raw rows
//! are in the probe's gitignored output rather than quoted here, since the
//! vendor's licence covers rows wherever they are written down.
//!
//! Nothing here or in the codec rescales that number. The units travel as a
//! label in the curated file's metadata, and a consumer that wants dollars
//! multiplies at the point of use. Rescaling on the way in would mean the
//! stored figure is no longer the figure anyone can check against the vendor,
//! and a later reader has no way to tell a converted number from a shipped one.
//!
//! # Why a null is never a row here
//!
//! The vendor's table has no null market cap in it. Measured across 1500 rows
//! sampled from three different decades, every row carried a strictly positive
//! figure, and a company in bankruptcy proceedings trading at pennies still
//! shipped a positive one. What the table does instead is omit the row
//! entirely: a security served with prices can have no market cap row at all.
//!
//! So absence and corruption are different things here, and the code keeps them
//! apart. A missing row is normal and silent. A row that arrives carrying a
//! null, a zero, or a negative is refused loudly, because if it were dropped
//! instead it would be indistinguishable from the absence that happens all the
//! time.

use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::schema::AssetKey;

/// One security's market capitalisation on one date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketCapRecord {
    pub asset: AssetKey,
    pub date: Date,
    /// As shipped, in the units the curated file's metadata names.
    ///
    /// Deliberately not called `marketcap_usd`. The number is in millions and a
    /// name claiming dollars would be wrong in a way no compiler could catch.
    pub marketcap: Decimal,
    /// Which provider reported this.
    pub source: String,
}

/// Why a market cap record was rejected.
///
/// Arithmetic and type-level facts rather than risk thresholds. A company with
/// a market cap of zero or less does not exist, which is a statement about
/// arithmetic rather than a conservative choice. Nothing here belongs in
/// `DECISION_CRITERIA.md`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MarketCapReject {
    #[error("ticker is empty")]
    EmptyTicker,

    #[error("source is empty, and a market cap with no provenance cannot be reconciled")]
    EmptySource,

    /// Refused rather than skipped, and the distinction is the whole point.
    /// This table expresses "no market cap" by having no row, so a zero or a
    /// negative that arrives anyway is a corrupt row rather than an absent one.
    /// Dropping it quietly would disguise it as the absence that is normal here.
    #[error(
        "marketcap must be strictly positive, got {value}. This table omits the row when it has \
         no figure, so a non-positive value is a corrupt row rather than a missing one"
    )]
    NonPositive { value: Decimal },
}

/// Check one record against every rule, returning the first violation.
pub fn validate_marketcap(record: &MarketCapRecord) -> Result<(), MarketCapReject> {
    if record.asset.ticker.trim().is_empty() {
        return Err(MarketCapReject::EmptyTicker);
    }
    if record.source.trim().is_empty() {
        return Err(MarketCapReject::EmptySource);
    }
    if record.marketcap <= Decimal::ZERO {
        return Err(MarketCapReject::NonPositive {
            value: record.marketcap,
        });
    }
    Ok(())
}
