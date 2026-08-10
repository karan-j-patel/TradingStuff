//! TICKERS: resolving a ticker string to a permanent identity.
//!
//! # The trap this table exists to avoid
//!
//! Sharadar recycles tickers. When a company is delisted and its ticker is
//! later reassigned, the vendor gives the ticker to the currently active
//! company and appends a number to the delisted one. So a ticker is a lookup
//! convenience and never a join key: joining on it across time welds two
//! unrelated companies' histories together, and the result looks like data.
//!
//! `permaticker` is the vendor's own "unique and unchanging identifier", and it
//! is what [`crate::schema::PermanentId::Sharadar`] carries.

use jiff::civil::Date;

use super::client::{SharadarClient, check_query_safe};
use super::malformed;
use crate::provider::SourceError;
use crate::schema::{AssetKey, PermanentId};

/// The columns this fetch asks for, and the only ones it reads.
///
/// Requested explicitly rather than taking the whole row, because the table has
/// roughly thirty columns and most of them are current-state metadata with no
/// as-of date. `sector`, `industry` and `scalemarketcap` in particular describe
/// a company as it is now, so using them to build a historical universe is a
/// lookahead leak. Not fetching them is the cheapest way to not misuse them.
const COLUMNS: &str =
    "table,permaticker,ticker,name,exchange,category,isdelisted,firstpricedate,lastpricedate";

/// One security in the Sharadar security master.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickerRow {
    /// Ticker plus the permanent identity, which is the whole point of the
    /// lookup.
    pub asset: AssetKey,
    /// Issuer name. Descriptive, so a null becomes `None` rather than an error.
    pub name: Option<String>,
    pub exchange: Option<String>,
    /// "Domestic", "Canadian" or "ADR", per the vendor's SEC filing category.
    pub category: Option<String>,
    /// Whether the security has been delisted.
    ///
    /// Required rather than optional, and strict about `Y`/`N`. Survivorship
    /// bias is the failure this field guards against, and a null quietly
    /// becoming `false` would resurrect every delisted company in the universe.
    pub is_delisted: bool,
    /// First and last dates a price exists for. The only fields on this row
    /// that carry any temporal bound at all.
    pub first_price_date: Option<Date>,
    pub last_price_date: Option<Date>,
}

impl SharadarClient {
    /// Resolve tickers to permanent identities.
    ///
    /// An empty request sends nothing. A bare `ticker=` filter would fetch the
    /// entire security master, which is roughly 22,000 rows nobody asked for.
    pub fn fetch_tickers(&self, tickers: &[&str]) -> Result<Vec<TickerRow>, SourceError> {
        if tickers.is_empty() {
            return Ok(Vec::new());
        }

        for ticker in tickers {
            check_query_safe("ticker", ticker)?;
        }

        let params = [
            ("ticker", tickers.join(",")),
            ("qopts.columns", COLUMNS.to_string()),
        ];

        let rows = self.fetch_rows("TICKERS", &params, decode_row)?;
        Ok(rows.into_iter().flatten().collect())
    }
}

/// Decode one TICKERS row, or `None` if it belongs to a different table.
fn decode_row(row: &super::decode::RowReader<'_>) -> Result<Option<TickerRow>, SourceError> {
    // TICKERS keys on (table, permaticker, ticker), so one security appears
    // once per table it belongs to: a stock in SEP, its fundamentals in SF1,
    // and so on. Keeping every row would multiply each security by the number
    // of tables it is in, and a load keyed on permaticker alone would instead
    // collapse them and lose whichever arrived first.
    if row.text("table")? != super::columns::datatables::EQUITY_TABLE_TAG {
        return Ok(None);
    }

    let permaticker = row.token("permaticker")?;
    let permanent = permaticker.parse::<u64>().map_err(|error| {
        malformed(format!(
            "the permaticker {permaticker:?} is not a number: {error}"
        ))
    })?;

    let is_delisted = match row.text("isdelisted")? {
        "Y" => true,
        "N" => false,
        other => {
            return Err(malformed(format!(
                "isdelisted holds {other:?}, and only \"Y\" or \"N\" say whether a \
                 security was delisted"
            )));
        }
    };

    Ok(Some(TickerRow {
        asset: AssetKey {
            ticker: row.text("ticker")?.to_string(),
            permanent: Some(PermanentId::Sharadar(permanent)),
        },
        name: row.optional_text("name")?.map(str::to_string),
        exchange: row.optional_text("exchange")?.map(str::to_string),
        category: row.optional_text("category")?.map(str::to_string),
        is_delisted,
        first_price_date: row.optional_date("firstpricedate")?,
        last_price_date: row.optional_date("lastpricedate")?,
    }))
}
