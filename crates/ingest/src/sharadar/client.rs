//! The client: credentials, URL construction, retry, and pagination.
//!
//! # The API key, and why it cannot be printed
//!
//! Nasdaq's datatables endpoint takes the key as a query parameter, so it
//! appears in every URL this client builds. A key is a full substitute for a
//! username and password, and a URL reaches error messages, logs, and terminal
//! scrollback without anyone deciding that it should.
//!
//! Two independent measures keep it out of anything a user can see, and they
//! are independent on purpose, because either alone has a hole:
//!
//!   1. [`scrub_url`] drops the entire query string before a URL enters an
//!      error. Not the `api_key` parameter, the whole query. A filter that
//!      removes one named parameter fails the day something else carries the
//!      key, and the diagnostic value of the remaining parameters is not worth
//!      that.
//!   2. [`SharadarClient::redact`] replaces the key wherever it appears in any
//!      text on its way into an error. This catches what scrubbing cannot:
//!      `ureq::Error::BadUri` carries the URI it rejected, and a vendor's own
//!      400 body may echo the request it did not like.
//!
//! [`std::fmt::Debug`] is hand-written for the same reason. A derived one would
//! print the key the first time anybody wrote `dbg!(&client)`.

use std::fmt;
use std::time::Duration;

use super::decode::{Page, RowReader};
use super::http::{Http, UreqHttp};
use super::{
    BASE_URL, BODY_EXCERPT, KEY_VAR, MAX_ATTEMPTS, MAX_PAGES, NATIVE_BASE_URL, NATIVE_MAX_ATTEMPTS,
    NATIVE_PAGE_SIZE, NATIVE_RETRY_PAUSE, PROVIDER, RETRY_PAUSE, refused,
};
use crate::provider::SourceError;

pub struct SharadarClient {
    http: Box<dyn Http>,
    /// Read once at construction. Never logged, never printed, never in an
    /// error.
    key: String,
    /// Split out so tests can point the client at a fixture host without
    /// reaching the network.
    base: String,
    /// Total attempts per request, the first one included.
    ///
    /// Per host, because the two doors publish different limits and deserve
    /// different manners. The datatables endpoint documents a generous quota
    /// and gets three quick attempts. The native endpoint publishes no limits
    /// at all, so it gets one attempt, one long wait, and one more.
    attempts: u32,
    /// Zero under test, which is what keeps the retry path deterministic.
    pause: Duration,
    /// Rows per page on the native door. A field so a test can walk a
    /// boundary with three rows instead of three thousand.
    page_size: usize,
}

impl fmt::Debug for SharadarClient {
    /// Hand-written so the key cannot appear. There is deliberately no masked
    /// or truncated form of it here either: a prefix of a credential is still
    /// part of a credential, and presence is all a debug line needs.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharadarClient")
            .field("base", &self.base)
            .field("key", &"<redacted>")
            .finish()
    }
}

impl SharadarClient {
    /// Build a client from the process environment.
    ///
    /// The key is read once, here. A blank value counts as absent, because
    /// `.env.example` ships every key blank and a sourced but unfilled template
    /// would otherwise send an empty credential and produce a 403 that looks
    /// like a subscription problem.
    pub fn from_env() -> Result<Self, SourceError> {
        let key = std::env::var(KEY_VAR).unwrap_or_default();
        if key.trim().is_empty() {
            // Names the variable and never a value, which is the same rule
            // `cli ingest` reports under.
            return Err(refused(format!(
                "{KEY_VAR} is not set in the environment. \
                 To bring a .env into the current shell:  set -a; source .env; set +a"
            )));
        }

        Ok(SharadarClient {
            http: Box::new(UreqHttp::new()),
            key,
            base: BASE_URL.to_string(),
            attempts: MAX_ATTEMPTS,
            pause: RETRY_PAUSE,
            page_size: NATIVE_PAGE_SIZE,
        })
    }

    /// Build a client for the native `api.sharadar.com` door.
    ///
    /// Same key, same safety core, different manners. This host publishes no
    /// rate limits at all, so the only safe assumption is that it has one and
    /// we cannot see it. Round 1 established what fast retries do to a speed
    /// limit: three attempts 0.5 seconds apart turned a throttle into a
    /// disabled account. So this door retries once, after a wait long enough
    /// to be worth making.
    pub fn native_from_env() -> Result<Self, SourceError> {
        Ok(SharadarClient {
            base: NATIVE_BASE_URL.to_string(),
            attempts: NATIVE_MAX_ATTEMPTS,
            pause: NATIVE_RETRY_PAUSE,
            page_size: NATIVE_PAGE_SIZE,
            ..Self::from_env()?
        })
    }

    /// Build a client over a supplied transport, for tests.
    #[cfg(test)]
    pub(crate) fn with_transport(http: Box<dyn Http>, key: &str, base: &str) -> Self {
        SharadarClient {
            http,
            key: key.to_string(),
            base: base.to_string(),
            attempts: MAX_ATTEMPTS,
            // No sleeping in tests. The retry policy is exercised for its
            // decisions, not for its timing.
            pause: Duration::ZERO,
            page_size: NATIVE_PAGE_SIZE,
        }
    }

    /// Shrink the native page size so a boundary is reachable in a fixture.
    #[cfg(test)]
    pub(crate) fn with_page_size(mut self, rows: usize) -> Self {
        self.page_size = rows;
        self
    }

    pub(crate) fn page_size(&self) -> usize {
        self.page_size
    }

    /// One native request. Same retry, same scrubbing, different URL shape.
    ///
    /// `format=json` is explicit because this host will otherwise answer in
    /// whatever it considers its default, and a client that assumes a default
    /// is one vendor change away from parsing the wrong thing.
    pub(crate) fn get_native(
        &self,
        table: &str,
        params: &[(&str, String)],
    ) -> Result<String, SourceError> {
        let mut url = format!("{}/{table}?api_key={}&format=json", self.base, self.key);
        for (name, value) in params {
            url.push_str(&format!("&{name}={value}"));
        }
        self.get(&url)
    }

    /// Replace the key wherever it appears in text bound for an error.
    ///
    /// The second line of defence. Scrubbing handles URLs this client built;
    /// this handles text it did not build, which is the transport's own error
    /// messages and the vendor's response bodies.
    pub(crate) fn redact(&self, text: &str) -> String {
        text.replace(&self.key, "<redacted>")
    }

    /// Build a `Transport` error, scrubbed and redacted.
    fn transport(&self, detail: &str, url: &str) -> SourceError {
        SourceError::Transport {
            provider: PROVIDER.to_string(),
            source: format!("{} while fetching {}", self.redact(detail), scrub_url(url)).into(),
        }
    }

    /// Quote enough of a failing response to diagnose it, and no more.
    fn excerpt(&self, body: &str) -> String {
        let redacted = self.redact(body.trim());
        match redacted.char_indices().nth(BODY_EXCERPT) {
            None => redacted,
            Some((cut, _)) => format!("{}...", &redacted[..cut]),
        }
    }

    /// One GET, retried on the statuses worth retrying.
    fn get(&self, url: &str) -> Result<String, SourceError> {
        let mut last = String::from("no attempt was made");

        for attempt in 1..=self.attempts {
            match self.http.get(url) {
                Ok(reply) if (200..300).contains(&reply.status) => return Ok(reply.body),

                // The credential is wrong. Repeating it produces the same
                // answer and burns quota against a rate limit.
                Ok(reply) if reply.status == 401 || reply.status == 403 => {
                    return Err(SourceError::Unauthorized {
                        provider: PROVIDER.to_string(),
                    });
                }

                // Worth repeating: a rate limit clears and a 5xx is the
                // vendor's side rather than this request's content.
                Ok(reply) if reply.status == 429 || reply.status >= 500 => {
                    last = format!("HTTP {} ({})", reply.status, self.excerpt(&reply.body));
                }

                // Any other 4xx is a request this client got wrong. Sending it
                // again unchanged cannot produce a different answer.
                Ok(reply) => {
                    return Err(self.transport(
                        &format!("HTTP {} ({})", reply.status, self.excerpt(&reply.body)),
                        url,
                    ));
                }

                // No status at all: DNS, TLS, a timeout. Retryable, and the
                // message is untrusted because ureq's `BadUri` carries the URI.
                Err(failure) => last = failure.0,
            }

            if attempt < self.attempts && !self.pause.is_zero() {
                // Doubling, bounded by the attempt count rather than by a
                // deadline. On the native door there is only one wait, so the
                // doubling never comes into play there.
                std::thread::sleep(self.pause * 2u32.pow(attempt - 1));
            }
        }

        Err(self.transport(
            &format!(
                "giving up after {} attempts, last was {last}",
                self.attempts
            ),
            url,
        ))
    }

    /// Walk every page of one query, decoding each row by name.
    pub(crate) fn fetch_rows<T>(
        &self,
        table: &str,
        params: &[(&str, String)],
        decode: impl Fn(&RowReader<'_>) -> Result<T, SourceError>,
    ) -> Result<Vec<T>, SourceError> {
        let mut collected = Vec::new();
        let mut cursor: Option<String> = None;

        for _ in 0..MAX_PAGES {
            let url = self.url(table, params, cursor.as_deref());
            let page = Page::parse(&self.get(&url)?)?;

            for row in page.rows() {
                collected.push(decode(&row)?);
            }

            match page.next_cursor() {
                None => return Ok(collected),
                Some(next) => {
                    // The cursor is vendor data on its way back into a URL this
                    // client builds, so it gets the same check a caller's
                    // ticker gets rather than being trusted for having come
                    // from the other end.
                    check_query_safe("cursor", next)?;
                    cursor = Some(next.to_string());
                }
            }
        }

        Err(refused(format!(
            "the {table} query was still paginating after {MAX_PAGES} pages, \
             which means the cursor is not terminating"
        )))
    }

    /// Assemble the request URL for one page.
    ///
    /// Parameters are joined verbatim, and there is no percent-encoding
    /// dependency, because of how the two kinds of value that get here are
    /// produced. Anything originating outside this crate — a caller's ticker,
    /// a cursor the vendor sent back — has been through [`check_query_safe`]
    /// individually. Everything else is built here from constants: the column
    /// lists, and the commas joining already-checked tickers, which are the
    /// vendor's own separator and so must survive unescaped.
    ///
    /// The consequence worth stating is that a new parameter taking an
    /// unchecked value from anywhere would break that, and would do it
    /// silently.
    fn url(&self, table: &str, params: &[(&str, String)], cursor: Option<&str>) -> String {
        let mut url = format!("{}/{table}.json?api_key={}", self.base, self.key);
        for (name, value) in params {
            url.push_str(&format!("&{name}={value}"));
        }
        if let Some(cursor) = cursor {
            url.push_str(&format!("&qopts.cursor_id={cursor}"));
        }
        url
    }
}

/// Drop a URL's query string entirely.
///
/// Everything before the `?` is kept, so an error still says which table was
/// being fetched, which is the part a reader needs.
pub(crate) fn scrub_url(url: &str) -> String {
    match url.split_once('?') {
        Some((path, _)) => format!("{path}?<query scrubbed>"),
        None => url.to_string(),
    }
}

/// Refuse a value that would change the meaning of the query string.
///
/// Tickers arrive from callers and go straight into a URL. Rather than depend
/// on a percent-encoding crate for a field that is a stock ticker, the
/// permitted character set is stated and anything else is refused by name. An
/// `&` in a ticker would otherwise silently append a parameter.
pub(crate) fn check_query_safe(what: &str, value: &str) -> Result<(), SourceError> {
    if value.is_empty() {
        return Err(refused(format!(
            "an empty {what} is not a query this client will send"
        )));
    }

    if let Some(bad) = value
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
    {
        return Err(refused(format!(
            "the {what} {value:?} contains {bad:?}, and only ASCII letters, digits, \
             and the characters . - _ can go into a query string unescaped"
        )));
    }

    Ok(())
}
