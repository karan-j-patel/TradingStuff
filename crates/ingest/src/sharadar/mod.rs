//! The Sharadar connector, speaking the Nasdaq Data Link datatables API.
//!
//! # Which API this is, because there are two
//!
//! Sharadar data is served by two live and mutually incompatible APIs. This
//! module targets `data.nasdaq.com/api/v3/datatables/SHARADAR/{TABLE}.json`,
//! which is the one with documented cursor pagination, bulk export, and
//! published quotas. Sharadar's own `api.sharadar.com` uses different table
//! names, a different parameter scheme, and in at least one load-bearing case a
//! different column name for the same field: what Nasdaq calls `datekey`
//! Sharadar calls `date`. A client written against one and pointed at the other
//! picks up the wrong column and says nothing, which is why only one of them is
//! spoken here.
//!
//! # The shape of a response, and the bug it invites
//!
//! ```json
//! {
//!   "datatable": {
//!     "columns": [{"name": "ticker", "type": "String"}, {"name": "close", "type": "double"}],
//!     "data": [["AAPL", 1.23]]
//!   },
//!   "meta": {"next_cursor_id": null}
//! }
//! ```
//!
//! Rows are **positional arrays, not keyed objects**. Zipping them against
//! whatever order the columns happened to arrive in is the single most likely
//! place for a silent misalignment, and a misaligned price column is a bug that
//! looks like data rather than like a crash. So every value in this module is
//! read through [`decode::RowReader`], which binds by column name and errors
//! naming the column it could not find. Reordered columns decode identically;
//! that property has a test, and a mutation that binds by position fails it.
//!
//! # Numbers
//!
//! No value reaches a [`rust_decimal::Decimal`] through `f64`. The workspace
//! enables `serde_json`'s `arbitrary_precision`, which keeps a JSON number as
//! the exact text the vendor sent, and that text is parsed with
//! `Decimal::from_str_exact`. `from_str_exact` rather than `from_str` because
//! the latter rounds silently when a token carries more precision than
//! `Decimal` holds, and a price that quietly rounds is the class of error this
//! project exists to not have.
//!
//! # What is deliberately absent
//!
//! There is no [`crate::provider::PriceSource`] implementation here. SEP ships
//! split-adjusted `open`/`high`/`low`/`close` alongside an unadjusted
//! `closeunadj`, while [`crate::schema::PriceBar`] stores prices exactly as
//! traded. Whether the traded prices can be reconstructed exactly from the
//! adjusted ones is an empirical question about vendor rounding, not something
//! to settle by argument, so the mapping waits on what
//! [`SharadarClient::sep_window`] actually observes.

use std::fmt;
use std::time::Duration;

mod client;
mod decode;
mod http;
mod sep;
mod tickers;

#[cfg(test)]
mod tests;

pub use client::SharadarClient;
pub use sep::SepRow;
pub use tickers::TickerRow;

/// The name this connector reports in errors and in stored provenance.
pub const PROVIDER: &str = "Sharadar";

/// The environment variable holding the API key.
///
/// Mirrors `.env.example` and the `cli ingest` presence report. A rename in
/// either place has to happen in both.
pub const KEY_VAR: &str = "SHARADAR_API_KEY";

/// Where the datatables live. One trailing path segment per table.
const BASE_URL: &str = "https://data.nasdaq.com/api/v3/datatables/SHARADAR";

/// How many pages a single query may walk before the client gives up.
///
/// The API caps a page at 10,000 rows, so this bound permits a hundred million
/// rows from one query, which is far past any query this codebase issues. It is
/// not a tuning parameter, it is a guard against a cursor that never returns
/// null turning a fetch into an unbounded loop against a metered API.
const MAX_PAGES: usize = 10_000;

/// Total attempts per request, the first one included.
const MAX_ATTEMPTS: u32 = 3;

/// Base delay between retries, doubled on each further attempt.
///
/// Real in production and zero under test, which is what keeps the retry path
/// deterministic and instant to exercise. See [`SharadarClient::with_transport`].
const RETRY_PAUSE: Duration = Duration::from_millis(500);

/// Cap on how much of a failing response body is quoted back in an error.
///
/// Enough to carry the vendor's explanation of a 400, short enough that a
/// stray HTML error page does not become the error message.
const BODY_EXCERPT: usize = 200;

/// Build a `Malformed` error naming the provider.
///
/// Details here are built from column names and response structure, never from
/// the request, so nothing that passes through this function can carry the API
/// key.
pub(crate) fn malformed(detail: impl fmt::Display) -> crate::provider::SourceError {
    crate::provider::SourceError::Malformed {
        provider: PROVIDER.to_string(),
        detail: detail.to_string(),
    }
}

/// Build a `Refused` error naming the provider.
///
/// Used for the things this client declines to send, as distinct from things
/// the vendor sent badly.
pub(crate) fn refused(detail: impl fmt::Display) -> crate::provider::SourceError {
    crate::provider::SourceError::Refused {
        provider: PROVIDER.to_string(),
        detail: detail.to_string(),
    }
}
