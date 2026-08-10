//! The second door: `api.sharadar.com`, the vendor's own API.
//!
//! # The envelope, verified live before this was written
//!
//! Two requests with the real key on 2026-08-09, `tickers` and `stocks` at
//! `limit=1`, both came back as:
//!
//! ```json
//! {"count":1,"data":[{"ticker":"ZTS","date":"2026-08-07","open":77.92, ...}]}
//! ```
//!
//! **Rows are keyed objects, not positional arrays.** That is the shape the
//! research reported for `fundamentals` and it holds for these two as well.
//!
//! The consequence worth stating plainly: the column-misalignment bug that the
//! datatables path needs [`super::decode::RowReader`] to prevent is
//! *unreachable* here. There is no column list to zip against and no order to
//! get wrong, so a reordered response decodes identically by construction
//! rather than by care. The F1 mutation from round 1 has no analogue on this
//! path. What does carry over is every question about how a single value is
//! spelled, so value parsing is shared rather than rewritten.
//!
//! `count` is the number of rows in *this* response, not a grand total. It
//! cannot be used to plan pagination.
//!
//! # What this host gets wrong quietly
//!
//! A wrong-case table name returns `{"count":0,"data":[]}` rather than an
//! error, so a typo reads as "this security has no data". Table names are
//! single-sourced in [`super::columns::native_tables`] and a fetch that filters
//! on something known-live and gets nothing back is an error here, not an empty
//! success.
//!
//! # Offset pagination, and the integrity problem it creates
//!
//! This host pages with `limit` and `skip` rather than a cursor. Offset paging
//! over a table that is being written can drop or duplicate rows across a page
//! boundary, which cursor paging cannot: if a row is inserted before the offset
//! between two requests, everything shifts by one and the row at the boundary
//! is served twice or not at all.
//!
//! The mitigation is to make the boundary checkable. Every page after the first
//! is requested with a one-row overlap, so the first row it returns must be the
//! last row already held, compared field by field as a whole row. If it is not,
//! rows moved underneath the walk and the fetch errors rather than returning a
//! series with a silent hole in it.
//!
//! Whole row rather than sort key, because ties are not exotic here: any query
//! spanning several tickers or several fundamentals dimensions has them by
//! construction, and two rows sharing a sort key are indistinguishable to a key
//! comparison exactly when the walk most needs telling them apart.

use jiff::civil::Date;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::client::{SharadarClient, check_query_safe};
use super::columns::native;
use super::columns::native_tables;
use super::decode::{date_of, decimal_of, text_of, token_of};
use super::sep::SepRow;
use super::tickers::TickerRow;
use super::{MAX_PAGES, malformed, refused};
use crate::provider::SourceError;
use crate::schema::{AssetKey, PermanentId};

/// The fields the ticker lookup asks for.
const TICKER_FIELDS: &str =
    "table,permaticker,ticker,name,exchange,category,isdelisted,firstpricedate,lastpricedate";

/// The fields the price probe asks for.
const PRICE_FIELDS: &str = "ticker,date,open,high,low,close,closeunadj,lastupdated";

// --- the envelope -----------------------------------------------------------

#[derive(Deserialize)]
struct Envelope {
    /// Rows in this response. Not a total, so it is read for nothing except
    /// cross-checking the array it describes.
    #[serde(default)]
    count: Option<usize>,
    #[serde(default)]
    data: Vec<Map<String, Value>>,
}

/// One row, read by key.
pub(crate) struct NativeRow<'a> {
    fields: &'a Map<String, Value>,
}

impl NativeRow<'_> {
    fn cell(&self, column: &str) -> Result<&Value, SourceError> {
        self.fields.get(column).ok_or_else(|| {
            malformed(format!(
                "the response has no field named {column:?}; it carried {}",
                self.field_names()
            ))
        })
    }

    fn field_names(&self) -> String {
        self.fields
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn is_null(&self, column: &str) -> Result<bool, SourceError> {
        Ok(self.cell(column)?.is_null())
    }

    pub(crate) fn text(&self, column: &str) -> Result<&str, SourceError> {
        text_of(column, self.cell(column)?)
    }

    pub(crate) fn token(&self, column: &str) -> Result<&str, SourceError> {
        token_of(column, self.cell(column)?)
    }

    pub(crate) fn decimal(&self, column: &str) -> Result<rust_decimal::Decimal, SourceError> {
        decimal_of(column, self.cell(column)?)
    }

    pub(crate) fn date(&self, column: &str) -> Result<Date, SourceError> {
        date_of(column, self.cell(column)?)
    }

    pub(crate) fn optional_text(&self, column: &str) -> Result<Option<&str>, SourceError> {
        if self.is_null(column)? {
            return Ok(None);
        }
        self.text(column).map(Some)
    }

    pub(crate) fn optional_date(&self, column: &str) -> Result<Option<Date>, SourceError> {
        if self.is_null(column)? {
            return Ok(None);
        }
        self.date(column).map(Some)
    }
}

// --- the paginated fetch ----------------------------------------------------

impl SharadarClient {
    /// Walk one native query, checking the integrity of every page boundary.
    ///
    /// `sort` is required rather than optional. It is both the ordering the
    /// vendor applies and the key this function compares across boundaries, and
    /// without a stated order offset paging has no defined meaning at all.
    ///
    /// The overlap row is compared whole, every field, not by its sort token.
    /// Tied sort keys are certain as soon as a query spans several tickers or
    /// several fundamentals dimensions, and under a tie a token comparison
    /// accepts a different row as if it were the same one. A check that stops
    /// protecting exactly when the data gets harder is worse than none, because
    /// it still reads like protection.
    ///
    /// ponytail: what remains uncovered is a compensating change entirely
    /// between two boundaries, one row inserted and another removed inside the
    /// same page. No boundary check can see that, and nothing else here does
    /// either: the count check in [`SharadarClient::native_page`] only proves
    /// the envelope's own count matches its rows, which catches a malformed
    /// envelope, not a balanced replacement. Upgrade path if that ever matters
    /// is a full re-read and diff, which costs a second pass.
    pub(crate) fn fetch_native<T>(
        &self,
        table: &str,
        params: &[(&str, String)],
        sort: &str,
        decode: impl Fn(&NativeRow<'_>) -> Result<T, SourceError>,
    ) -> Result<Vec<T>, SourceError> {
        let mut collected = Vec::new();
        let mut skip = 0usize;
        let mut boundary: Option<Map<String, Value>> = None;

        for _ in 0..MAX_PAGES {
            let page = self.native_page(table, params, sort, skip)?;
            let returned = page.len();
            let mut rows = page.iter();

            if let Some(expected) = &boundary {
                // The page was requested with a one-row overlap, so the first
                // row back must be the row already held: the whole row, not
                // merely something that sorts to the same place.
                let Some(first) = rows.next() else {
                    return Err(malformed(format!(
                        "the page of {table} after {sort}={:?} came back empty, \
                         so rows were removed underneath the walk",
                        sort_token(table, sort, expected)?
                    )));
                };
                if first != expected {
                    return Err(malformed(format!(
                        "the {table} page boundary does not line up: expected the row at \
                         {sort}={:?} to repeat as the first row of the next page, got a \
                         different row at {sort}={:?}. Rows shifted underneath the walk, so \
                         this result would silently duplicate or drop data",
                        sort_token(table, sort, expected)?,
                        sort_token(table, sort, first)?
                    )));
                }
            }

            let mut last_row = boundary.clone();
            for row in rows {
                // The sort field is still required on every row, so a response
                // that cannot be ordered is refused rather than walked blind.
                sort_token(table, sort, row)?;
                last_row = Some(row.clone());
                collected.push(decode(&NativeRow { fields: row })?);
            }

            // A short page is the last page. This host gives no total, so the
            // only end-of-data signal is getting back fewer rows than asked for.
            if returned < self.page_size() {
                return Ok(collected);
            }

            boundary = last_row;
            // The absolute index of the last row just consumed, so the next
            // request re-serves it as its first row.
            skip += returned - 1;
        }

        Err(refused(format!(
            "the {table} query was still paginating after {MAX_PAGES} pages"
        )))
    }

    fn native_page(
        &self,
        table: &str,
        params: &[(&str, String)],
        sort: &str,
        skip: usize,
    ) -> Result<Vec<Map<String, Value>>, SourceError> {
        let mut all: Vec<(&str, String)> = params.to_vec();
        all.push(("sort", sort.to_string()));
        all.push(("limit", self.page_size().to_string()));
        if skip > 0 {
            all.push(("skip", skip.to_string()));
        }

        let body = self.get_native(table, &all)?;
        let envelope: Envelope = serde_json::from_str(&body)
            .map_err(|error| malformed(format!("the {table} response was not JSON: {error}")))?;

        // `count` is redundant with the array length, which makes it a free
        // cross-check rather than something to trust on its own.
        if let Some(count) = envelope.count
            && count != envelope.data.len()
        {
            return Err(malformed(format!(
                "the {table} response says count={count} but carries {} rows",
                envelope.data.len()
            )));
        }

        Ok(envelope.data)
    }
}

/// The sort field of one row, required so an unorderable response is refused.
fn sort_token<'a>(
    table: &str,
    sort: &str,
    row: &'a Map<String, Value>,
) -> Result<&'a str, SourceError> {
    let value = row
        .get(sort)
        .ok_or_else(|| malformed(format!("the {table} response has no sort field {sort:?}")))?;
    token_of(sort, value)
}

/// Refuse an empty result where the filter named something that should exist.
///
/// The silent-empty hazard this exists for: a wrong-case table name is answered
/// with zero rows and HTTP 200, which is indistinguishable from a security
/// having no data unless somebody insists on the difference.
fn require_rows<T>(table: &str, filter: &str, rows: Vec<T>) -> Result<Vec<T>, SourceError> {
    if rows.is_empty() {
        return Err(malformed(format!(
            "the {table} table returned no rows for {filter}, which should exist. \
             This host answers a wrong-case table name with an empty result rather \
             than an error, so an empty answer is treated as a fault"
        )));
    }
    Ok(rows)
}

// --- the fetches ------------------------------------------------------------

impl SharadarClient {
    /// Resolve tickers to permanent identities, native door.
    pub fn native_tickers(&self, tickers: &[&str]) -> Result<Vec<TickerRow>, SourceError> {
        if tickers.is_empty() {
            return Ok(Vec::new());
        }
        for ticker in tickers {
            check_query_safe("ticker", ticker)?;
        }

        let joined = tickers.join(",");
        let params = [
            ("ticker", joined.clone()),
            ("fields", TICKER_FIELDS.to_string()),
        ];

        // Sorted by table, which is unique within one ticker's rows and is the
        // field the boundary check compares.
        let rows = self.fetch_native(native_tables::TICKERS, &params, "table", decode_ticker)?;
        let kept: Vec<TickerRow> = rows.into_iter().flatten().collect();
        require_rows(native_tables::TICKERS, &format!("ticker={joined}"), kept)
    }

    /// Fetch price rows for one ticker over an inclusive date window.
    ///
    /// Diagnostic only, as on the other door. Nothing here maps a vendor row
    /// into the platform's own price type.
    pub fn native_price_window(
        &self,
        ticker: &str,
        from: Date,
        to: Date,
    ) -> Result<Vec<SepRow>, SourceError> {
        check_query_safe("ticker", ticker)?;

        let params = [
            ("ticker", ticker.to_string()),
            // `from` and `to` replace the datatables `.gte`/`.lte` operators.
            ("from", from.to_string()),
            ("to", to.to_string()),
            ("fields", PRICE_FIELDS.to_string()),
        ];

        let rows = self.fetch_native(native_tables::STOCKS, &params, "date", decode_price)?;
        require_rows(
            native_tables::STOCKS,
            &format!("ticker={ticker} between {from} and {to}"),
            rows,
        )
    }

    /// The earliest date this key is served for one ticker.
    ///
    /// The free tier is a rolling window, and `tickers.firstpricedate` reports
    /// the full history rather than the part the key can see, so the two
    /// disagree and only this one is actionable. Measured rather than assumed,
    /// which is what [`crate::provider::PriceSource::earliest_available`]
    /// exists to carry.
    pub fn native_earliest_date(&self, ticker: &str) -> Result<Option<Date>, SourceError> {
        check_query_safe("ticker", ticker)?;
        let params = [
            ("ticker", ticker.to_string()),
            ("fields", "ticker,date,close".to_string()),
        ];
        let rows = self.fetch_native(native_tables::STOCKS, &params, "date", decode_price_date)?;
        Ok(rows.into_iter().next())
    }

    /// Filing dates from the fundamentals table.
    ///
    /// Exists to exercise the one column whose name genuinely differs between
    /// the two hosts. See [`native::FILING_DATE`].
    pub fn native_filing_dates(&self, ticker: &str) -> Result<Vec<Date>, SourceError> {
        check_query_safe("ticker", ticker)?;
        let params = [
            ("ticker", ticker.to_string()),
            (
                "fields",
                format!("ticker,{},reportperiod", native::FILING_DATE),
            ),
        ];
        self.fetch_native(
            native_tables::FUNDAMENTALS,
            &params,
            native::FILING_DATE,
            |row| row.date(native::FILING_DATE),
        )
    }
}

// --- row decoding -----------------------------------------------------------

/// Decode one native TICKERS row, or `None` if it describes another table.
fn decode_ticker(row: &NativeRow<'_>) -> Result<Option<TickerRow>, SourceError> {
    // One security appears once per table it belongs to. On this host the tag
    // for equity price coverage is `stocks`, where the datatables host says
    // `SEP` for the same thing.
    if row.text("table")? != native::EQUITY_TABLE_TAG {
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

fn decode_price(row: &NativeRow<'_>) -> Result<SepRow, SourceError> {
    Ok(SepRow {
        ticker: row.text("ticker")?.to_string(),
        date: row.date("date")?,
        open: row.decimal("open")?,
        high: row.decimal("high")?,
        low: row.decimal("low")?,
        close: row.decimal("close")?,
        close_unadjusted: row.decimal("closeunadj")?,
        last_updated: row.optional_date("lastupdated")?,
    })
}

fn decode_price_date(row: &NativeRow<'_>) -> Result<Date, SourceError> {
    row.date("date")
}
