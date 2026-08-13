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
//! A wrong table name fails two different ways, and neither names the fault.
//!
//! A **wrong-case** name for a table that exists returns `{"count":0,"data":[]}`
//! rather than an error, so a typo reads as "this security has no data". An
//! **unknown** name is answered with HTTP 401, measured 2026-08-10 by
//! `cli ingest probe-actions` asking for a table called `action`, which arrives
//! as `Unauthorized` and reads as a credential problem while the credential is
//! fine.
//!
//! Table names are therefore single-sourced in
//! [`super::columns::native_tables`], every one of them is pinned against a
//! separately written wire literal in the tests, and a fetch that filters on
//! something known-live and gets nothing back is an error here rather than an
//! empty success. The one fetch that must accept an empty answer is
//! [`SharadarClient::native_actions_window`], because most securities pay no
//! dividends, and it says there why the name pin carries the weight instead.
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
use crate::actions::{ActionRecord, CorporateAction, DividendKind};
use crate::adjusted::AdjustedBar;
use crate::marketcap::MarketCapRecord;
use crate::provider::{AdjustedPriceSource, DateRange, SourceError};
use crate::provider::{FundamentalRecord, ReportBasis, ReportScope};
use crate::schema::{AssetKey, CloseKind, PermanentId, SessionScope};
use rust_decimal::Decimal;

/// The fields the ticker lookup asks for.
const TICKER_FIELDS: &str =
    "table,permaticker,ticker,name,exchange,category,isdelisted,firstpricedate,lastpricedate";

/// The fields the price probe asks for.
const PRICE_FIELDS: &str = "ticker,date,open,high,low,close,volume,closeunadj,lastupdated";

/// The fields the market cap fetch asks for.
///
/// Trimmed to exactly what is stored, plus the ticker the decoder checks is
/// still there. Measured 2026-08-11: this host honours the parameter, returning
/// three keys against the ten the daily table otherwise sends. The seven left
/// behind are valuation ratios this platform computes itself from inputs it can
/// see, and a ratio taken on trust is a number nobody can reproduce.
const MARKETCAP_FIELDS: &str = "ticker,date,marketcap";

/// The fundamentals columns this platform fetches, pinned to the probe.
///
/// Every name here exists in the 112-key set the Phase A probe enumerated, and
/// nothing outside that set is requested. One walk serves the value rail and the
/// later quality and profitability characteristics, so the columns those will
/// need are collected now rather than spending a second quota window on them.
///
/// `calendardate` is fetched and deliberately not stored. Seeing it in a probe
/// output is useful; keeping it on disk would put a third date next to two real
/// ones, and the probe established it is the calendar year end regardless of
/// fiscal end.
const FUNDAMENTAL_FIELDS: &str = "ticker,date,reportperiod,calendardate,dimension,lastupdated,\
equity,equityusd,netinc,netinccmnusd,revenue,revenueusd,assets,sharesbas,shareswa";

/// The only dimension this platform requests.
///
/// As-reported quarterly. The MR dimensions date a row by its period end while
/// restating the values later, which is lookahead by construction, so they are
/// never requested and the panel refuses them if one arrives anyway.
pub const ARQ: &str = "ARQ";

/// The fields the actions fetch asks for.
///
/// `closeadj` is deliberately absent from every fetch in this file. It is the
/// vendor's dividend-adjusted close, and storing it beside cash dividends that
/// the engine adds to returns itself would be the same distribution counted
/// twice.
pub(super) const ACTION_FIELDS: &str = "ticker,date,action,value";

/// The one action kind this fetch asks for and the only one it accepts.
///
/// Matched exactly, never as a prefix or a substring. The vendor's vocabulary
/// contains `spinoffdividend`, observed live on 2026-08-10, whose value is the
/// per-share worth of shares in another company rather than cash anyone was
/// paid, at a magnitude two orders above the same name's cash dividends. A
/// loose match books it as a distribution and invents money.
const CASH_DIVIDEND: &str = "dividend";

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

impl<'a> NativeRow<'a> {
    /// Wrap a row built in a test, so a decoder can be exercised on the JSON
    /// shape the host ships without a network.
    ///
    /// Test-only, because the production path always builds these inside
    /// [`SharadarClient::fetch_native`] and a second construction site would be
    /// a way to skip the page-boundary check.
    #[cfg(test)]
    pub(crate) fn for_test(fields: &'a Map<String, Value>) -> Self {
        NativeRow { fields }
    }
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

    /// A number that the vendor is allowed to ship as JSON null.
    ///
    /// Null and zero are different answers and this keeps them apart. A missing
    /// *column* is still an error, because [`NativeRow::is_null`] goes through
    /// [`NativeRow::cell`], so the tolerance is for a null value and not for a
    /// response that dropped the field.
    pub(crate) fn optional_decimal(
        &self,
        column: &str,
    ) -> Result<Option<rust_decimal::Decimal>, SourceError> {
        if self.is_null(column)? {
            return Ok(None);
        }
        self.decimal(column).map(Some)
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
        // The tie group at the previous page's end: every trailing row of that
        // page sharing its last row's sort token. Empty before the first
        // request, and one row whenever the boundary token is unique, which is
        // the overwhelmingly common case and is byte-identical to the
        // single-row overlap this walk used before tie groups existed.
        let mut boundary: Vec<Map<String, Value>> = Vec::new();

        for _ in 0..MAX_PAGES {
            let page = self.native_page(table, params, sort, skip, self.page_size())?;
            let returned = page.len();

            if let Some(held) = boundary.last() {
                // The page was requested with an overlap covering the whole tie
                // group at the previous page's end, so that group must repeat,
                // in order, as this page's head. Comparing only the first row
                // pins the boundary row and says nothing about the order of the
                // tie around it, which is how a reordered tie group loses a row.
                if returned == 0 {
                    return Err(malformed(format!(
                        "the page of {table} after {sort}={:?} came back empty, \
                         so rows were removed underneath the walk",
                        sort_token(table, sort, held)?
                    )));
                }
                if returned < boundary.len() {
                    return Err(malformed(format!(
                        "the page of {table} after {sort}={:?} came back with {returned} \
                         row(s) where the overlap of the tie group alone needs {}, so rows \
                         were removed underneath the walk",
                        sort_token(table, sort, held)?,
                        boundary.len()
                    )));
                }
                for (expected, got) in boundary.iter().zip(&page) {
                    if expected != got {
                        return Err(malformed(format!(
                            "the {table} page boundary does not line up: expected the row at \
                             {sort}={:?} to repeat in the overlap of the next page, got a \
                             different row at {sort}={:?}. Rows shifted underneath the walk, \
                             so this result would silently duplicate or drop data",
                            sort_token(table, sort, expected)?,
                            sort_token(table, sort, got)?
                        )));
                    }
                }
            }

            for row in &page[boundary.len()..] {
                // The sort field is still required on every row, so a response
                // that cannot be ordered is refused rather than walked blind.
                sort_token(table, sort, row)?;
                collected.push(decode(&NativeRow { fields: row })?);
            }

            // A short page is the last page. This host gives no total, so the
            // only end-of-data signal is getting back fewer rows than asked for.
            if returned < self.page_size() {
                return Ok(collected);
            }

            let group = trailing_tie_group(table, sort, &page)?;
            // A tie group filling a whole page cannot be walked past: the next
            // request would have to overlap every row it just served, so `skip`
            // would not advance and the walk would spin until MAX_PAGES.
            // Refused by name rather than looping, because the alternative to a
            // loud failure here is a silently truncated dataset.
            if group.len() == returned {
                return Err(malformed(format!(
                    "every row of a full {table} page shares {sort}={:?}, so the tie group \
                     fills the page and the walk cannot overlap it to make progress. A \
                     larger page size is the fix; guessing which rows to skip would drop \
                     data without saying so",
                    sort_token(table, sort, &page[returned - 1])?
                )));
            }
            // Back up over the whole tie group, so the next page re-serves it
            // and the check above can compare it row by row.
            skip += returned - group.len();
            boundary = group;
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
        limit: usize,
    ) -> Result<Vec<Map<String, Value>>, SourceError> {
        let mut all: Vec<(&str, String)> = params.to_vec();
        all.push(("sort", sort.to_string()));
        all.push(("limit", limit.to_string()));
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
/// The trailing run of rows sharing the page's last sort token.
///
/// One row whenever that token is unique, which is what keeps the ordinary walk
/// exactly as it was. Longer only where the vendor's ordering is genuinely
/// ambiguous, which is the case a one-row overlap cannot pin.
fn trailing_tie_group(
    table: &str,
    sort: &str,
    page: &[Map<String, Value>],
) -> Result<Vec<Map<String, Value>>, SourceError> {
    let Some(last) = page.last() else {
        return Ok(Vec::new());
    };
    let token = sort_token(table, sort, last)?.to_owned();

    let mut group = Vec::new();
    for row in page.iter().rev() {
        if sort_token(table, sort, row)? != token {
            break;
        }
        group.push(row.clone());
    }
    group.reverse();
    Ok(group)
}

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

    /// Every security in the master with equity price coverage.
    ///
    /// # Why the whole table rather than a list of names
    ///
    /// A universe assembled from tickers somebody typed is survivorship biased
    /// by construction, because the person typing already knows which companies
    /// still exist. *Survivorship bias* is the error of measuring only the
    /// survivors and reading the result as though it described everyone.
    /// `CLAUDE.md` rule 4 forbids it, and the only way to satisfy the rule is to
    /// start from every name the vendor has, delisted ones included, and cut it
    /// down by a rule that does not know the answer.
    ///
    /// So nothing here filters on `isdelisted` or `lastpricedate`. Whoever
    /// samples this list decides what to keep, and that decision is recorded.
    ///
    /// # The `table` parameter, and why correctness does not depend on it
    ///
    /// One security appears once per vendor table it belongs to, so the master
    /// is several times longer than the number of securities in it. The
    /// parameter asks the host to send only the equity-coverage rows. Whether
    /// it honours that is not something this function needs to know:
    /// [`decode_ticker`] drops every other row anyway, so an ignored parameter
    /// costs extra pages and returns the same answer.
    pub fn native_ticker_master(&self) -> Result<Vec<TickerRow>, SourceError> {
        let params = [
            ("table", native::EQUITY_TABLE_TAG.to_string()),
            ("fields", TICKER_FIELDS.to_string()),
        ];

        // Sorted on `permaticker`, the vendor's unique and unchanging
        // identifier. Offset pagination has no defined meaning without a stated
        // order, and `fetch_native` compares whole rows across every boundary.
        let rows = self.fetch_native(
            native_tables::TICKERS,
            &params,
            "permaticker",
            decode_ticker,
        )?;

        require_rows(
            native_tables::TICKERS,
            &format!("table={}", native::EQUITY_TABLE_TAG),
            rows.into_iter().flatten().collect(),
        )
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
    /// One request, `limit=1` ascending, rather than a paginated walk. The
    /// answer is the first row, and fetching a whole history to look at its
    /// head would be thousands of round trips against a host that publishes no
    /// rate limits.
    ///
    /// `tickers.firstpricedate` reports a security's full history rather than
    /// the part the key in use can see, so the two disagree whenever the
    /// subscription is windowed and only this one is actionable.
    pub fn native_earliest_date(&self, ticker: &str) -> Result<Option<Date>, SourceError> {
        check_query_safe("ticker", ticker)?;
        let params = [
            ("ticker", ticker.to_string()),
            ("fields", "ticker,date,close".to_string()),
        ];
        let page = self.native_page(native_tables::STOCKS, &params, "date", 0, 1)?;
        page.first()
            .map(|row| NativeRow { fields: row }.date("date"))
            .transpose()
    }

    /// Fetch cash dividends for one ticker over an inclusive date window.
    ///
    /// # Why an empty answer is not an error here
    ///
    /// [`require_rows`] guards the price fetch, because a security with a
    /// listing has prices and an empty answer there means the request was
    /// wrong. Dividends are different: most securities pay none, and a name
    /// that paid nothing in the window is the common case rather than a fault.
    ///
    /// That leaves the wrong-case table trap uncovered by a runtime check, so
    /// it is covered by a test instead. `the_actions_table_name_matches_the_wire`
    /// pins the constant against the literal spelling, written out separately,
    /// so a miscased constant fails the suite rather than quietly reporting
    /// that nobody pays dividends.
    ///
    /// # Why the kind filter is sent and then checked again
    ///
    /// The filter was measured honoured on 2026-08-10, against a security whose
    /// window carries splits and an acquisition. Sending it saves the rows.
    /// Checking every row that comes back is what notices if it ever stops
    /// being honoured, on a host whose habit is to answer a request it does not
    /// understand with something that looks like data.
    pub(crate) fn native_actions_window(
        &self,
        ticker: &str,
        from: Date,
        to: Date,
    ) -> Result<Vec<ActionRow>, SourceError> {
        check_query_safe("ticker", ticker)?;

        let params = [
            ("ticker", ticker.to_string()),
            ("from", from.to_string()),
            ("to", to.to_string()),
            ("action", CASH_DIVIDEND.to_string()),
            ("fields", ACTION_FIELDS.to_string()),
        ];

        self.fetch_native(native_tables::ACTIONS, &params, "date", decode_action)
    }

    /// Cash dividends for one security, as domain records.
    ///
    /// The asset key is the caller's, not one rebuilt from the ticker string,
    /// so the vendor's permanent identifier survives into the record exactly as
    /// it does through the price fetch. A curated action keyed on a label
    /// companies change would fail to match its own price series the day the
    /// ticker moved.
    ///
    /// No window check against [`AdjustedPriceSource::earliest_available`].
    /// That measures the price table, and the two are served over different
    /// spans: the probe on 2026-08-10 was served Apple actions from 2019 by a
    /// key whose price window had been measured elsewhere. Refusing a range on
    /// the strength of the other table's boundary would decline data this key
    /// does serve.
    pub fn fetch_cash_dividends(
        &self,
        asset: &AssetKey,
        range: DateRange,
    ) -> Result<Vec<ActionRecord>, SourceError> {
        let rows = self.native_actions_window(&asset.ticker, range.start(), range.end())?;
        Ok(rows
            .into_iter()
            .map(|row| ActionRecord {
                asset: asset.clone(),
                // The vendor's date is the ex-date. Measured 2026-08-10: the
                // price and the dividend-adjusted series separate across
                // exactly the transition ending on this date, and nowhere else
                // in the surrounding week.
                effective: row.date,
                action: CorporateAction::Dividend {
                    amount: row.amount,
                    kind: DividendKind::Cash,
                },
                source: super::PROVIDER.to_string(),
            })
            .collect())
    }

    /// Fetch daily market capitalisation for one ticker over an inclusive
    /// window.
    ///
    /// # Why an empty answer is not an error here
    ///
    /// [`require_rows`] guards the price fetch, because a security with a
    /// listing has prices and an empty answer there means the request was
    /// wrong. This table is different, and the difference was measured rather
    /// than assumed. On 2026-08-11 `cli ingest probe-daily` asked the daily
    /// table for CUTC1 over the window ending on its last trading day and got
    /// `{"count":0,"data":[]}`, while the price table served seven rows for the
    /// same name and the same window. The daily table has no row for that name
    /// on any date. Its history starts around 1998-12-01, and a security
    /// delisted before that is served with prices and with no market cap at
    /// all.
    ///
    /// So an empty answer here is an ordinary fact about a security rather than
    /// a fault, and `require_rows` would turn every such name into a failure.
    /// Do not add it back.
    ///
    /// That leaves the wrong-case table trap uncovered by a runtime check, so
    /// it is covered by a test instead, exactly as the actions fetch is.
    /// `the_daily_table_name_matches_the_wire` pins the constant against the
    /// literal spelling written out separately, so a miscased constant fails
    /// the suite rather than quietly reporting that no security has a size.
    pub(crate) fn native_daily_marketcap_window(
        &self,
        ticker: &str,
        from: Date,
        to: Date,
    ) -> Result<Vec<DailyRow>, SourceError> {
        check_query_safe("ticker", ticker)?;

        let params = [
            ("ticker", ticker.to_string()),
            ("from", from.to_string()),
            ("to", to.to_string()),
            ("fields", MARKETCAP_FIELDS.to_string()),
        ];

        self.fetch_native(native_tables::DAILY, &params, "date", decode_marketcap)
    }

    /// Market capitalisation for one security, as domain records.
    ///
    /// The asset key is the caller's, not one rebuilt from the ticker string,
    /// so the vendor's permanent identifier survives into the record exactly as
    /// it does through the price and dividend fetches. A curated row keyed on a
    /// label companies change would fail to match its own price series the day
    /// the ticker moved.
    pub fn fetch_marketcaps(
        &self,
        asset: &AssetKey,
        range: DateRange,
    ) -> Result<Vec<MarketCapRecord>, SourceError> {
        let rows = self.native_daily_marketcap_window(&asset.ticker, range.start(), range.end())?;
        Ok(rows
            .into_iter()
            .map(|row| MarketCapRecord {
                asset: asset.clone(),
                date: row.date,
                // As shipped. Nothing on this path multiplies, and the units
                // travel as a label on the curated file.
                marketcap: row.marketcap,
                source: super::PROVIDER.to_string(),
            })
            .collect())
    }

    /// One native request, returned as the host sent it.
    ///
    /// For probes, which exist to find out how a table is shaped before any
    /// decoder can be written for it. Nothing in the production path reads an
    /// undecoded body: a fetch that ships rows into the platform goes through
    /// [`SharadarClient::fetch_native`] so it gets pagination and the
    /// page-boundary check with it.
    ///
    /// The body is redacted on the way out. A successful response has no reason
    /// to contain the key, and "no reason to" is not the standard this codebase
    /// prints credentials under.
    pub fn native_raw(
        &self,
        table: &str,
        params: &[(&str, String)],
    ) -> Result<String, SourceError> {
        Ok(self.redact(&self.get_native(table, params)?))
    }

    /// As-reported quarterly fundamentals for one security.
    ///
    /// # Why there is no `require_rows` here
    ///
    /// A name with no rows is an ordinary fact rather than a fault: plenty of
    /// securities in a price universe never filed with the SEC at all, and the
    /// probe found real holes in AR coverage besides. Treating an empty answer
    /// as a failure would turn every such name into a decline. The walk counts
    /// them separately instead.
    ///
    /// That leaves the wrong-case table trap uncovered at runtime, exactly as it
    /// is for the daily and actions fetches, and it is covered by a test pinning
    /// the constant against the literal spelling.
    ///
    /// The asset key is the caller's rather than one rebuilt from the ticker
    /// string. This table ships no permanent identifier at all and silently
    /// ignores a `permaticker` filter, so the identity has to come from the
    /// universe entry the fetch was made under. That is also why the walk
    /// applies a life-window filter afterwards: a recycled ticker returns
    /// another company's filings and nothing in the row says so.
    pub fn fetch_fundamentals_arq(
        &self,
        asset: &AssetKey,
    ) -> Result<Vec<FundamentalRecord>, SourceError> {
        check_query_safe("ticker", &asset.ticker)?;
        let params = fundamentals_params(&asset.ticker);
        let rows = self.fetch_native(
            native_tables::FUNDAMENTALS,
            &params,
            native::FILING_DATE,
            decode_fundamental,
        )?;
        Ok(rows
            .into_iter()
            .map(|row| row.into_record(asset.clone()))
            .collect())
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

/// One decoded vendor actions row.
///
/// Private to this module, like [`SepRow`] is to the price path. What leaves
/// the crate is the domain record, so no caller can end up holding a vendor
/// row's idea of what a field means.
pub(crate) struct ActionRow {
    pub(crate) date: Date,
    pub(crate) amount: Decimal,
}

fn decode_action(row: &NativeRow<'_>) -> Result<ActionRow, SourceError> {
    // Exact equality, and an error rather than a skip. Skipping would leave a
    // filter that has silently stopped working looking exactly like a quarter
    // in which nobody paid anything.
    let kind = row.text("action")?;
    if kind != CASH_DIVIDEND {
        return Err(malformed(format!(
            "the actions query filtered on {CASH_DIVIDEND:?} and got a row marked {kind:?}, \
             so the server-side filter was not honoured. Nothing here maps an unexpected kind \
             to cash: {CASH_DIVIDEND:?} and \"spinoffdividend\" are different events, and the \
             second carries the per-share value of shares in another company"
        )));
    }

    Ok(ActionRow {
        date: row.date("date")?,
        // Null on several of this table's kinds, measured 2026-08-10 on
        // `listed`, `relation` and both ticker-change rows. `decimal` refuses a
        // null rather than reading it as zero, which is what keeps a row
        // carrying no amount from becoming a dividend of nothing.
        amount: row.decimal("value")?,
    })
}

/// One decoded vendor daily row.
///
/// Private to this module, like [`SepRow`] and [`ActionRow`]. What leaves the
/// crate is the domain record, so no caller ends up holding a vendor row's idea
/// of what a field means, and in particular of what units it is in.
pub(crate) struct DailyRow {
    pub(crate) date: Date,
    pub(crate) marketcap: Decimal,
}

/// Decode one daily row, refusing anything that is not a usable figure.
///
/// All three fields are required and none of them is tolerated missing. The
/// request names exactly these three in its `fields` parameter, so a row
/// arriving without one is the host disagreeing with what it was asked for
/// rather than a security with a gap.
///
/// A null `marketcap` is an error rather than a skip or a zero. `decimal`
/// refuses a null, and that refusal is the point: this table says "no figure"
/// by omitting the row entirely, measured on 2026-08-11 across 1500 rows in
/// which no null, zero, or negative appeared. So a null that does arrive is a
/// row that changed shape, and reading it as zero would claim a company is
/// worth nothing.
fn decode_marketcap(row: &NativeRow<'_>) -> Result<DailyRow, SourceError> {
    // Read and discarded. The ticker is not stored from here, because the
    // caller's AssetKey carries identity, but asking for it is what makes a
    // response that dropped the column an error rather than a silent success.
    row.text("ticker")?;

    Ok(DailyRow {
        date: row.date("date")?,
        marketcap: row.decimal("marketcap")?,
    })
}

/// The query one fundamentals request sends.
///
/// A free function rather than three lines inline, so a test can read the
/// outgoing parameters without a transport. What it pins is the `dimension`
/// filter: the restated dimensions date a row by its period end while the
/// values were corrected later, and a request that stopped naming ARQ would ask
/// for all six and be answered with the lot.
pub(crate) fn fundamentals_params(ticker: &str) -> [(&'static str, String); 3] {
    [
        ("ticker", ticker.to_string()),
        ("dimension", ARQ.to_string()),
        ("fields", FUNDAMENTAL_FIELDS.to_string()),
    ]
}

/// One fundamentals row as the vendor sent it, before an identity is attached.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FundamentalRow {
    pub(crate) as_reported: Date,
    pub(crate) period_end: Date,
    pub(crate) dimension: String,
    pub(crate) fields: std::collections::BTreeMap<String, rust_decimal::Decimal>,
}

impl FundamentalRow {
    /// Attach the caller's identity, which the vendor row does not carry.
    pub(crate) fn into_record(self, asset: AssetKey) -> FundamentalRecord {
        // ARQ is the only dimension requested and the only one this maps. A row
        // carrying anything else is refused at decode, so the pair below is not
        // a default standing in for an unknown.
        FundamentalRecord {
            asset,
            as_reported: self.as_reported,
            period_end: self.period_end,
            // This table is one snapshot per fetch and ships no observation
            // stamp of its own. `lastupdated` is a table-refresh time and says
            // nothing about when these values were pulled, so the filing date
            // stands here and the curated file's provenance carries the rest.
            observed_at: self.as_reported,
            source: super::PROVIDER.to_string(),
            basis: ReportBasis::AsReported,
            scope: ReportScope::Quarterly,
            // This vendor ships no accession number. The column exists so a
            // provider that does fits without a schema break.
            filing_id: None,
            fields: self.fields,
        }
    }
}

/// The numeric columns kept from a fundamentals row.
///
/// A column the vendor sends as JSON null contributes no entry, so absence in
/// the map means the vendor had no figure rather than a figure of zero.
pub(crate) const FUNDAMENTAL_VALUES: &[&str] = &[
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

pub(crate) fn decode_fundamental(row: &NativeRow<'_>) -> Result<FundamentalRow, SourceError> {
    // Read and discarded, so a response that dropped the column is an error
    // rather than a silent success.
    row.text("ticker")?;

    let dimension = row.text("dimension")?.to_string();
    // Refused rather than mapped. A response carrying MR rows to an ARQ request
    // is the vendor disagreeing with the filter, and restated values dated by
    // period end are the exact lookahead this rail exists to keep out.
    if dimension != ARQ {
        return Err(malformed(format!(
            "the fundamentals row came back under dimension {dimension:?} for an {ARQ} \
             request. A restated dimension dates a row by its period end while its values \
             were corrected later, so it is refused rather than relabelled"
        )));
    }

    let mut fields = std::collections::BTreeMap::new();
    for name in FUNDAMENTAL_VALUES {
        if let Some(value) = row.optional_decimal(name)? {
            fields.insert((*name).to_string(), value);
        }
    }

    Ok(FundamentalRow {
        as_reported: row.date(native::FILING_DATE)?,
        period_end: row.date("reportperiod")?,
        dimension,
        fields,
    })
}

fn decode_price(row: &NativeRow<'_>) -> Result<SepRow, SourceError> {
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
}

// --- the provider trait implementation --------------------------------------

/// The security used to measure where the key's window starts.
///
/// It has to be one whose listing comfortably predates any plausible window,
/// or the measurement returns that security's first trading day instead of the
/// subscription's limit and the boundary comes out too late. Microsoft has
/// traded since 1986, which is early enough that anything measured here is the
/// key talking rather than the company.
const WINDOW_PROBE_TICKER: &str = "MSFT";

impl AdjustedPriceSource for SharadarClient {
    fn name(&self) -> &str {
        super::PROVIDER
    }

    fn earliest_available(&self) -> Result<Option<Date>, SourceError> {
        self.measured_earliest(WINDOW_PROBE_TICKER)
    }

    fn fetch_adjusted(
        &self,
        assets: &[AssetKey],
        range: DateRange,
    ) -> Result<Vec<AdjustedBar>, SourceError> {
        // The boundary is checked before any data is fetched, so an
        // out-of-window request costs one measurement rather than a partial
        // series the caller then has to notice is short.
        if let Some(earliest) = self.earliest_available()?
            && range.start() < earliest
        {
            return Err(refused(format!(
                "the range starts {} but this key serves nothing before {earliest}. \
                 The window is measured, not taken from vendor metadata, which \
                 reports a first price date years earlier than the key honours. \
                 Nothing here shortens a range quietly",
                range.start()
            )));
        }

        let mut bars = Vec::new();
        for asset in assets {
            // Serial, one ticker at a time. This host publishes no rate limits.
            for row in self.native_price_window(&asset.ticker, range.start(), range.end())? {
                bars.push(AdjustedBar {
                    // The caller's key, not one rebuilt from the ticker string,
                    // so a permanent id supplied by the caller survives.
                    asset: asset.clone(),
                    date: row.date,
                    open: row.open,
                    high: row.high,
                    low: row.low,
                    close: row.close,
                    volume: row.volume,
                    close_unadjusted: row.close_unadjusted,
                    // The vendor documents `close` as "the official exchange
                    // close price", which is the closing auction rather than a
                    // last trade, and its daily bars are regular-hours.
                    session: SessionScope::RegularHours,
                    close_kind: CloseKind::ClosingAuction,
                });
            }
        }
        Ok(bars)
    }
}
