//! The Sharadar connector, speaking the Nasdaq Data Link datatables API.
//!
//! # Which API this is, because there are two
//!
//! Sharadar data is served by two live and mutually incompatible APIs, and this
//! module speaks both.
//!
//! - `data.nasdaq.com/api/v3/datatables/SHARADAR/{TABLE}.json`, documented in
//!   the rest of this file, with cursor pagination and published quotas.
//! - `api.sharadar.com/v1.0/data/{table}`, in [`native`], with keyed rows and
//!   offset pagination.
//!
//! Which door a key opens is a property of the subscription rather than a
//! preference. A key issued by sharadar.com is anonymous at Nasdaq, and the
//! symptom is a rate limit rather than an authentication error, which is a
//! considerable distance from the cause.
//!
//! The two hosts disagree in ways that produce no error when confused: table
//! names differ in spelling and case, the `table` tag for equity coverage is
//! `SEP` on one and `stocks` on the other, and what Nasdaq calls `datekey`
//! Sharadar calls `date`. Every one of those lives in [`columns`] rather than at
//! a call site, because each is a silent wrong answer rather than a failure.
//!
//! What the two share is the safety core: one transport, one retry and status
//! classification, one pair of credential defences, and one reader turning a
//! JSON token into a `Decimal`. There is deliberately no second copy of any of
//! them.
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
pub mod native;
mod sep;
mod tickers;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_native;

pub use client::SharadarClient;
pub use sep::SepRow;
pub use tickers::TickerRow;

/// Column and table names that differ between the two doors.
///
/// Single-sourced here rather than spelled at call sites, because the two
/// hosts disagree in ways that produce no error when confused. The SF1 filing
/// date is `datekey` on the datatables endpoint and `date` on the native one:
/// same field, same type, different name, and a client that reads the wrong one
/// gets a missing column at best and a different date at worst. Table names are
/// worse still, because the native host answers a wrong-case name with an empty
/// result rather than an error.
pub(crate) mod columns {
    /// Names as the Nasdaq datatables endpoint spells them.
    pub(crate) mod datatables {
        /// The `table` value marking equity price coverage in TICKERS.
        pub(crate) const EQUITY_TABLE_TAG: &str = "SEP";
        /// The date a filing became public.
        ///
        /// Unused until a fundamentals fetch lands on this door, and defined
        /// now anyway: the pair is the point. A constant that appears only when
        /// its first caller does is a constant somebody invents inline instead,
        /// which is how the two spellings get confused. Locked by test against
        /// its native counterpart.
        #[allow(dead_code)]
        pub(crate) const FILING_DATE: &str = "datekey";
    }

    /// Names as `api.sharadar.com` spells them. Verified live, 2026-08-09.
    pub(crate) mod native {
        /// The `table` value marking equity price coverage. Observed live: the
        /// native host returns `stocks` here where the datatables host returns
        /// `SEP`, so the round-1 filter constant does not carry over.
        pub(crate) const EQUITY_TABLE_TAG: &str = "stocks";
        /// The same field the datatables host calls `datekey`.
        pub(crate) const FILING_DATE: &str = "date";
    }

    /// Native table names, lowercase.
    ///
    /// A wrong-case name is answered with an empty result rather than an error,
    /// so these exist once and are never spelled at a call site.
    pub(crate) mod native_tables {
        pub(crate) const STOCKS: &str = "stocks";
        pub(crate) const TICKERS: &str = "tickers";
        pub(crate) const FUNDAMENTALS: &str = "fundamentals";
    }
}

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

/// Total attempts per request, the first one included: one try, one wait, one
/// more, then fail.
///
/// One rule for both doors. Round 1 measured the alternative on the datatables
/// host: three attempts half a second apart against a speed limit turned a
/// throttle into a disabled account, because each retry was itself another
/// offence. A published quota does not make that less true, it only makes the
/// damage take longer to arrive.
const MAX_ATTEMPTS: u32 = 2;

/// The single wait between the two attempts.
///
/// Long enough to be worth making rather than short enough to feel responsive.
/// A throttle measured in seconds is cleared by a wait measured in seconds; one
/// measured in minutes is not, and neither host tells you which it is using.
///
/// Real in production and zero under test, which is what keeps the retry path
/// deterministic and instant to exercise. See [`SharadarClient::with_transport`].
const RETRY_PAUSE: Duration = Duration::from_secs(120);

/// Where the native API lives. See [`native`] for how it differs.
const NATIVE_BASE_URL: &str = "https://api.sharadar.com/v1.0/data";

/// Rows requested per native page.
///
/// Polite rather than maximal. This host publishes no limits, and a smaller
/// page costs one extra round trip while a larger one costs goodwill that
/// cannot be measured until it is gone.
const NATIVE_PAGE_SIZE: usize = 1_000;

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
