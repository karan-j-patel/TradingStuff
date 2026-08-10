//! The one place in this crate that touches a socket.
//!
//! Everything above this file — pagination, retry, decoding — is driven by
//! scripted replies in the tests, so none of it needs a network or a local
//! server to exercise. That is the entire reason this seam exists.

use std::time::Duration;

/// What one HTTP GET came back with.
///
/// The status is carried rather than folded into an error because retry policy
/// is decided on it: 429 and 5xx are worth repeating, other 4xx are not.
pub(crate) struct HttpReply {
    pub status: u16,
    pub body: String,
}

/// A round trip that never produced a status at all: DNS, TLS, timeouts.
///
/// The message is whatever the transport said, and it is **not** trusted to be
/// free of the API key. `ureq::Error::BadUri` carries the URI it rejected, so
/// the caller redacts before this reaches a user-visible error.
pub(crate) struct HttpFailure(pub String);

/// A single blocking HTTP GET.
///
/// Blocking on purpose. Nothing in this system has a concurrency requirement
/// that justifies async, and the rate limit that matters here is the vendor's
/// rather than this machine's.
pub(crate) trait Http {
    fn get(&self, url: &str) -> Result<HttpReply, HttpFailure>;
}

/// The real transport.
pub(crate) struct UreqHttp {
    agent: ureq::Agent,
}

/// Whole-request deadline. A hung connection against a metered API is worse
/// than a failed one, because it holds the caller with nothing to report.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Ceiling on a response body, stated rather than inherited.
///
/// A page is capped at 10,000 rows by the vendor, so this is generous. It
/// exists so a server that streams forever cannot exhaust memory.
const BODY_LIMIT: u64 = 64 * 1024 * 1024;

impl UreqHttp {
    pub(crate) fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            // Statuses are data here, not errors. The retry policy needs to see
            // 429 and 5xx as values, and a 400's body carries the vendor's
            // explanation, which is lost if ureq converts the status first.
            .http_status_as_error(false)
            .build();
        UreqHttp {
            agent: config.into(),
        }
    }
}

impl Http for UreqHttp {
    fn get(&self, url: &str) -> Result<HttpReply, HttpFailure> {
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|error| HttpFailure(error.to_string()))?;

        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(BODY_LIMIT)
            .read_to_string()
            .map_err(|error| HttpFailure(error.to_string()))?;

        Ok(HttpReply { status, body })
    }
}
