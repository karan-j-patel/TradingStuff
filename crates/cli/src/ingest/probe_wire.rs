//! Raw-wire helpers shared by the probes.
//!
//! A probe exists to find out how a table is shaped before any decoder can be
//! written for it, so everything here reads a response *without* naming its
//! fields. Naming them would be assuming the answer to the question the probe
//! went to ask.
//!
//! Extracted rather than copied into each probe. Two copies of "print every
//! field of every row" drift, and the one that drifts is the one nobody reruns.
//!
//! Nothing here decodes into a platform type, writes anything, or counts a
//! trial. It prints what the host sent.

use ingest::provider::SourceError;
use ingest::sharadar::SharadarClient;
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::Value;

/// One request, printed as it was asked and as it came back.
///
/// `show_raw` is false only where a body is hundreds of rows on one line. Every
/// request that answers a mapping question prints its body verbatim, because a
/// decode written against a paraphrase of a response is a decode written
/// against nothing.
///
/// A failure prints and returns `None` so the remaining questions still get
/// asked, with one exception: a rejected credential ends the run. Repeating a
/// request whose credential is wrong produces the same answer and spends
/// goodwill against a host that publishes no rate limits.
pub(super) fn ask(
    client: &SharadarClient,
    table: &str,
    params: &[(&str, String)],
    show_raw: bool,
) -> anyhow::Result<Option<String>> {
    print_request(table, params);

    match client.native_raw(table, params) {
        Ok(body) => {
            if show_raw {
                println!("  raw: {body}");
            }
            Ok(Some(body))
        }
        Err(error @ SourceError::Unauthorized { .. }) => Err(error.into()),
        Err(error) => {
            println!("  error: {error}");
            Ok(None)
        }
    }
}

/// The same request, but a rejected credential is reported and survived.
///
/// Only for asking whether a *name* exists. This host answers an unknown table
/// name with HTTP 401, measured 2026-08-10, which is indistinguishable from a
/// bad key by looking at it. The distinction has to be made by the caller
/// proving the key works against a table it already knows before asking this,
/// and every caller here does that first. Without that proof this function
/// turns one broken credential into "none of these names exist".
pub(super) fn ask_candidate(
    client: &SharadarClient,
    table: &str,
    params: &[(&str, String)],
) -> anyhow::Result<Option<String>> {
    print_request(table, params);

    match client.native_raw(table, params) {
        Ok(body) => {
            println!("  raw: {body}");
            Ok(Some(body))
        }
        Err(error @ SourceError::Unauthorized { .. }) => {
            println!("  unauthorized: {error}");
            println!(
                "  read as: no table by this name. The credential was proven live above, so a \
                 401 here is this host's way of saying the name is unknown"
            );
            Ok(None)
        }
        Err(error) => {
            println!("  error: {error}");
            Ok(None)
        }
    }
}

fn print_request(table: &str, params: &[(&str, String)]) {
    let shown: Vec<String> = params
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    println!("  request: table={table} {}", shown.join(" "));
}

/// The `data` array of a native response, or nothing if it has none.
pub(super) fn rows_of(body: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|envelope| envelope.get("data").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

/// Print every field of every row, without knowing any field's name.
///
/// The map `serde_json` parses into is ordered by key, so the columns line up
/// across rows without the caller deciding what they are.
pub(super) fn print_rows(rows: &[Value]) {
    for row in rows {
        let Some(fields) = row.as_object() else {
            println!("  a row that is not an object: {row}");
            continue;
        };
        let shown: Vec<String> = fields
            .iter()
            .map(|(name, value)| format!("{name}={}", scalar(value)))
            .collect();
        println!("  {}", shown.join("  "));
    }
}

/// One field of one row as text, or nothing if it is absent.
pub(super) fn field(row: &Value, name: &str) -> Option<String> {
    row.get(name).map(scalar)
}

/// One field of one row as a `Decimal`, parsed from the exact token the vendor
/// sent.
///
/// `arbitrary_precision` keeps a JSON number as its original text, so this
/// never routes a value through `f64`.
pub(super) fn decimal_field(row: &Value, name: &str) -> Option<Decimal> {
    Decimal::from_str_exact(&field(row, name)?).ok()
}

/// A JSON value as a bare token, so a row of values reads as values.
pub(super) fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A date shifted by whole days, erroring rather than saturating.
pub(super) fn shift(date: Date, days: i64) -> anyhow::Result<Date> {
    Ok(date.checked_add(jiff::SignedDuration::from_hours(days * 24))?)
}
