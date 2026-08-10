//! SEP rows, verbatim, for the split-adjustment probe.
//!
//! # This is not the price path
//!
//! [`SepRow`] is a diagnostic record, not a [`crate::schema::PriceBar`]. It
//! carries the vendor's adjusted `close` and unadjusted `closeunadj` side by
//! side and makes no decision about which is which, because that decision is
//! exactly what the probe exists to inform.
//!
//! SEP's `close` is "adjusted for stock splits and stock dividends. Not
//! adjusted for cash dividends or spinoffs", and `closeunadj` is "not adjusted
//! for stock splits, stock dividends, cash dividends or spinoffs". There is
//! also a `closeadj` which adjusts for all four; it is not fetched here because
//! the probe is about splits.

use jiff::civil::Date;
use rust_decimal::Decimal;

use super::client::{SharadarClient, check_query_safe};
use crate::provider::SourceError;

/// The columns the probe reads.
const COLUMNS: &str = "ticker,date,open,high,low,close,volume,closeunadj,lastupdated";

/// One SEP row as shipped, with no adjustment applied or undone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SepRow {
    pub ticker: String,
    pub date: Date,
    /// Split-adjusted, as the vendor ships them.
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    /// The official exchange close with no adjustment of any kind.
    pub close_unadjusted: Decimal,
    pub last_updated: Option<Date>,
}

impl SharadarClient {
    /// Fetch SEP rows for one ticker over an inclusive date window.
    ///
    /// Diagnostic only. Nothing here writes to disk and nothing maps these rows
    /// into the vendor-neutral schema.
    pub fn sep_window(
        &self,
        ticker: &str,
        from: Date,
        to: Date,
    ) -> Result<Vec<SepRow>, SourceError> {
        check_query_safe("ticker", ticker)?;

        let params = [
            ("ticker", ticker.to_string()),
            // `.gte` and `.lte` are the vendor's inclusive range operators, and
            // `date` is one of SEP's documented filter columns. Filtering on a
            // column the table does not list as a filter is ignored server-side
            // rather than refused.
            ("date.gte", from.to_string()),
            ("date.lte", to.to_string()),
            ("qopts.columns", COLUMNS.to_string()),
        ];

        self.fetch_rows("SEP", &params, |row| {
            Ok(SepRow {
                ticker: row.text("ticker")?.to_string(),
                date: row.date("date")?,
                open: row.decimal("open")?,
                high: row.decimal("high")?,
                low: row.decimal("low")?,
                close: row.decimal("close")?,
                volume: row.decimal("volume")?,
                close_unadjusted: row.decimal("closeunadj")?,
                last_updated: row.optional_date("lastupdated")?,
            })
        })
    }
}
