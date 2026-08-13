//! Securities that stopped trading, assembled from the vendor's actions table.
//!
//! # Why this is six market-wide walks rather than a per-name loop
//!
//! The dividends fetch asks once per security, because dividends are dense and
//! a market-wide walk over a decade would page for a very long time. Exits are
//! the opposite. Measured 2026-08-12 over the quarter-end week of
//! 2026-06-24 to 2026-06-30, the whole market produced fifteen `delisted` rows,
//! ten `acquisitionby`, two `regulatorydelisting`, one
//! `bankruptcyliquidation`, and none of the other two kinds. So one walk per
//! kind reads the entire table for a window in a handful of pages, where a
//! per-name loop would cost one request per security in the universe.
//!
//! # The shape the vendor actually ships, measured rather than assumed
//!
//! `delisted` is a **superset**, not a synonym for any one reason. Five of the
//! fifteen rows in the census week carried no explanatory kind anywhere, and
//! those five were exactly the five whose `value` was JSON null. So the
//! assembly below treats `delisted` as the spine and the explanatory kinds as
//! classification hung off it, rather than trying to build an exit out of an
//! explanatory row alone.
//!
//! Direction matters and is the trap. `acquisitionby` and `mergerto` are the
//! **exit** side, and six of six such tickers carried a same-date `delisted`
//! row. `acquisitionof` and `mergerfrom` are the **survivor** side, and none of
//! three such tickers carried one. AFBI is the case that settles it: the same
//! ticker holds `mergerfrom` in 2021, where it survived and was not delisted,
//! and `acquisitionby` in 2026, where it was acquired and was. A rule keyed on
//! the ticker rather than on the side gets that name wrong twice. The survivor
//! kinds are therefore never fetched.
//!
//! The evidence is in the gitignored `ContinuationDocs/2026-08-11-exits-probe.md`.
//! No vendor row appears here or in any test in this crate.

use std::collections::BTreeMap;

use jiff::civil::Date;
use rust_decimal::Decimal;

use super::client::SharadarClient;
use super::columns::native_tables;
use super::malformed;
use super::native::{ACTION_FIELDS, NativeRow};
use crate::actions::{Delisting, DelistingReason, Listing, TerminalValue};
use crate::provider::{DateRange, SourceError};
use crate::schema::AssetKey;

/// The kind every security that stopped trading carries, whatever the reason.
pub(crate) const EXIT_MARKER: &str = "delisted";

/// The kinds that explain a marker, and the reason each one means.
///
/// Only exit-side kinds. `acquisitionof` and `mergerfrom` describe the company
/// that carried on and are deliberately absent, for the reason recorded in the
/// module documentation.
///
/// `acquisitionby` maps to [`DelistingReason::Merger`] rather than to
/// [`DelistingReason::Acquired`]. The two are identical to every rule in this
/// platform, because neither is performance-related, and collapsing them keeps
/// one vendor vocabulary mapping to one shipped variant.
pub(crate) const EXPLANATORY_KINDS: &[(&str, DelistingReason)] = &[
    ("acquisitionby", DelistingReason::Merger),
    ("mergerto", DelistingReason::Merger),
    ("bankruptcyliquidation", DelistingReason::Bankruptcy),
    ("regulatorydelisting", DelistingReason::ExchangeRule),
    ("voluntarydelisting", DelistingReason::Voluntary),
];

/// One decoded vendor exit row.
///
/// Private to the crate, like the dividend and daily rows, so no caller outside
/// ends up holding a vendor row's idea of what a field means. What leaves is
/// [`Delisting`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExitRow {
    pub(crate) ticker: String,
    pub(crate) date: Date,
    pub(crate) kind: &'static str,
    /// The security's final market capitalisation in millions of US dollars,
    /// when the row carried one.
    ///
    /// Two hazards, both measured. Null is legal and ordinary, appearing on
    /// fund wind-ups. Zero is a **real figure** on a bankruptcy liquidation,
    /// not a missing one. A decoder reading zero as absent would discard
    /// exactly the rows an imputation rule most needs.
    pub(crate) value: Option<Decimal>,
}

/// What one assembly pass produced, and what it could not use.
#[derive(Debug, Clone, PartialEq)]
pub struct Assembled {
    pub delistings: Vec<Delisting>,
    /// Rows served per vendor kind, in fetch order. Printed so a run that saw
    /// nothing of a kind says so rather than looking like a run that saw
    /// everything.
    pub rows_by_kind: Vec<(&'static str, usize)>,
    /// Rows naming a ticker the universe map does not hold. Expected rather
    /// than exceptional, since the walk is market-wide and the universe is a
    /// sample.
    pub outside_universe: usize,
    /// Explanatory rows inside the universe with no `delisted` row on the same
    /// date for the same ticker.
    ///
    /// Zero on every sample measured so far. Counted rather than assumed away,
    /// because the alternative is a classification quietly going missing.
    pub unmatched_explanations: usize,
}

/// Decode one exit row, refusing a kind the request did not ask for.
///
/// Exact equality against the requested kind, and an error rather than a skip,
/// on the same rule the dividend decoder follows. A server-side filter that has
/// silently stopped being honoured otherwise looks exactly like a quiet quarter.
fn decode_exit(requested: &'static str, row: &NativeRow<'_>) -> Result<ExitRow, SourceError> {
    let kind = row.text("action")?;
    if kind != requested {
        return Err(malformed(format!(
            "the actions query filtered on {requested:?} and got a row marked {kind:?}, so the \
             server-side filter was not honoured. Nothing here maps an unexpected kind to an \
             exit: the survivor side of a merger ships under its own kind and is not a delisting"
        )));
    }

    Ok(ExitRow {
        ticker: row.text("ticker")?.to_string(),
        date: row.date("date")?,
        kind: requested,
        value: row.optional_decimal("value")?,
    })
}

impl SharadarClient {
    /// Walk every exit kind market-wide over a window and assemble delistings.
    ///
    /// # Why an empty answer is not an error here
    ///
    /// Two of the six kinds returned zero rows over the measured census week,
    /// so a kind with nothing in it is an ordinary fact about a window rather
    /// than a fault. That leaves the wrong-case table trap uncovered by a
    /// runtime check, and it is covered by a test instead, exactly as the
    /// dividend and daily fetches are. `the_actions_table_name_matches_the_wire`
    /// pins the constant against a separately written literal.
    pub fn fetch_delistings(
        &self,
        by_ticker: &BTreeMap<String, AssetKey>,
        range: DateRange,
    ) -> Result<Assembled, SourceError> {
        let mut rows: Vec<ExitRow> = Vec::new();
        let mut rows_by_kind: Vec<(&'static str, usize)> = Vec::new();

        // The marker first, then the explanations in a fixed order, so two runs
        // over the same window build the same vector.
        let kinds =
            std::iter::once(EXIT_MARKER).chain(EXPLANATORY_KINDS.iter().map(|(kind, _)| *kind));

        for kind in kinds {
            let params = [
                ("from", range.start().to_string()),
                ("to", range.end().to_string()),
                // No ticker parameter. This is the whole market for the window.
                ("action", kind.to_string()),
                ("fields", ACTION_FIELDS.to_string()),
            ];
            let served = self.fetch_native(native_tables::ACTIONS, &params, "date", |row| {
                decode_exit(kind, row)
            })?;
            rows_by_kind.push((kind, served.len()));
            rows.extend(served);
        }

        let mut assembled = assemble(&rows, by_ticker, super::PROVIDER)?;
        assembled.rows_by_kind = rows_by_kind;
        Ok(assembled)
    }
}

/// Turn decoded vendor rows into delistings, keeping a census of what was
/// dropped.
///
/// Pure, so the classification rules are testable without a network. The
/// `rows_by_kind` field of the result is left empty here and filled by the
/// fetch, which is the only caller that knows how many rows each request
/// served.
pub(crate) fn assemble(
    rows: &[ExitRow],
    by_ticker: &BTreeMap<String, AssetKey>,
    source: &str,
) -> Result<Assembled, SourceError> {
    let reason_of: BTreeMap<&str, DelistingReason> = EXPLANATORY_KINDS.iter().copied().collect();

    // Explanations first, keyed by the pair a marker is matched on. The value
    // is every distinct reason claimed for that pair, because two kinds meaning
    // different things on one date is an ambiguity rather than a preference.
    let mut explanations: BTreeMap<(&str, Date), Vec<DelistingReason>> = BTreeMap::new();
    let mut outside_universe = 0usize;

    for row in rows {
        if row.kind == EXIT_MARKER {
            continue;
        }
        let reason = *reason_of.get(row.kind).ok_or_else(|| {
            malformed(format!(
                "the actions walk produced a row of kind {:?}, which is neither the exit \
                 marker nor an explanatory kind this decoder asked for",
                row.kind
            ))
        })?;
        if !by_ticker.contains_key(row.ticker.as_str()) {
            outside_universe += 1;
            continue;
        }
        let claimed = explanations
            .entry((row.ticker.as_str(), row.date))
            .or_default();
        // The same kind repeats, one row per counterparty, with the same date
        // and the same value each time. Repetition is not ambiguity, so only
        // distinct reasons are collected.
        if !claimed.contains(&reason) {
            claimed.push(reason);
        }
    }

    // Markers, deduplicated. One exit is one row in the curated file, and the
    // writer refuses a duplicate `(asset, date)` outright, so a repeat has to
    // be collapsed here or the whole write fails.
    let mut markers: BTreeMap<(&str, Date), Option<Decimal>> = BTreeMap::new();
    for row in rows {
        if row.kind != EXIT_MARKER {
            continue;
        }
        if !by_ticker.contains_key(row.ticker.as_str()) {
            outside_universe += 1;
            continue;
        }
        let key = (row.ticker.as_str(), row.date);
        match markers.get(&key) {
            None => {
                markers.insert(key, row.value);
            }
            Some(held) if *held == row.value => {}
            Some(held) => {
                return Err(malformed(format!(
                    "{} carries two {EXIT_MARKER} rows on {} with different values, {held:?} \
                     and {:?}, so there is no single final market capitalisation to store",
                    row.ticker, row.date, row.value
                )));
            }
        }
    }

    let mut delistings = Vec::with_capacity(markers.len());
    for ((ticker, date), final_market_cap) in &markers {
        let reason = match explanations.get(&(*ticker, *date)) {
            None => DelistingReason::Unknown,
            Some(claimed) if claimed.len() == 1 => claimed[0],
            Some(claimed) => {
                let mut named: Vec<String> =
                    claimed.iter().map(|reason| format!("{reason:?}")).collect();
                named.sort();
                return Err(malformed(format!(
                    "{ticker} carries explanatory rows on {date} claiming {} at once, and \
                     those do not mean the same thing to the imputation rule, so nothing \
                     here picks one",
                    named.join(" and ")
                )));
            }
        };

        delistings.push(Delisting {
            // The caller's key, carrying the vendor's permanent identifier. The
            // actions table serves a ticker and nothing else, and a curated
            // exit keyed on a label companies reuse would attach to the wrong
            // price series the day the ticker moved.
            asset: by_ticker[*ticker].clone(),
            date: *date,
            reason,
            // Not recorded. The actions row carries no exchange, and the venue
            // vocabulary of the master has not been measured, so a mapping
            // written from memory would put a wrong value where a real one
            // reads. Nothing consults this field: the engine's convention is
            // flat by venue. See `ingest::flat_convention_for`.
            listing: Listing::Other,
            // The vendor publishes no delisting return on any row, so nothing
            // here was observed and nothing here is imputed yet. The engine
            // imputes at run time under the convention its configuration
            // records, which is what keeps the recorded config hash a true
            // description of what the arithmetic did.
            terminal: TerminalValue::Unknown,
            final_market_cap: *final_market_cap,
            source: source.to_string(),
        });
    }

    let unmatched_explanations = explanations
        .keys()
        .filter(|key| !markers.contains_key(*key))
        .count();

    Ok(Assembled {
        delistings,
        rows_by_kind: Vec::new(),
        outside_universe,
        unmatched_explanations,
    })
}

#[cfg(test)]
mod tests;
