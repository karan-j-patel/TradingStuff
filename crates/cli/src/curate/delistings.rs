//! `curate delistings`. The exits the engine's classification reads.
//!
//! Split out of `curate.rs` for file size alone. Every rule the sibling
//! datasets follow holds here too: validation before anything is written, a
//! rejected record failing the whole command, and no trial counted for writing
//! a file.
//!
//! # Why the fetch is market-wide rather than per security
//!
//! The prices and dividends walks ask once per universe member, because those
//! tables are dense. The exit kinds are not. A whole market produces a handful
//! of rows a week, so six market-wide requests read the entire table for a
//! window where a per-name loop would cost one request per security. The
//! per-kind split exists because the vendor honours the kind filter
//! server-side, and an unfiltered window is dominated by dividends and hits the
//! page cap long before an exit appears.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use ingest::provider::{AdjustedPriceSource, DateRange, SourceError};
use ingest::sharadar::SharadarClient;
use ingest::universe;
use ingest::{AssetKey, Delisting, UniverseEntry, validate_delisting, write_delistings};
use jiff::civil::Date;

/// Where records handed over by an operator are recorded as coming from.
const OPERATOR: &str = "operator-jsonl";

pub fn run(
    input: Option<&str>,
    fetch: bool,
    universe_path: Option<&str>,
    from: Option<Date>,
    to: Option<Date>,
    path: &Path,
) -> anyhow::Result<ExitCode> {
    if let Some(universe_path) = universe_path {
        if !fetch {
            anyhow::bail!("--universe describes what to fetch, so it needs --fetch");
        }
        return fetch_from_universe(universe_path, from, to, path);
    }
    if fetch {
        anyhow::bail!(
            "--fetch needs --universe. The actions table serves a ticker and nothing else, so \
             the universe file is what turns a vendor row into a record carrying the vendor's \
             permanent identifier"
        );
    }

    let input = input
        .ok_or_else(|| anyhow::anyhow!("delistings needs either --input <file> or --fetch"))?;
    // Named rather than left blank, on the same rule prices follows. A curated
    // file whose vendor is unknown cannot be reconciled, and "an operator
    // handed us a file" is itself the provenance.
    curate(super::read_jsonl(input)?, path, OPERATOR)
}

/// Fetch every exit kind market-wide and keep the rows the universe names.
///
/// # What is deliberately not tolerated
///
/// A rejected credential, on the same rule the other walks follow. That is one
/// fault affecting every remaining request, so it stops on the first one rather
/// than writing "declined" beside six more.
///
/// An empty answer for a kind is not a fault. Two of the six kinds returned
/// nothing over the week the census was measured on, so a quiet window is
/// ordinary.
fn fetch_from_universe(
    universe_path: &str,
    from: Option<Date>,
    to: Option<Date>,
    path: &Path,
) -> anyhow::Result<ExitCode> {
    let (from, to) = match (from, to) {
        (Some(from), Some(to)) => (from, to),
        _ => anyhow::bail!("--fetch needs --from and --to"),
    };
    let range = DateRange::new(from, to)?;

    let text = std::fs::read_to_string(universe_path)
        .with_context(|| format!("reading {universe_path}"))?;
    let entries = universe::from_jsonl(&text)?;
    if entries.is_empty() {
        anyhow::bail!("{universe_path} holds no securities, so there is nothing to fetch");
    }
    let (members, ambiguous) = by_ticker(&entries);

    let source = SharadarClient::native_from_env()?;
    println!(
        "Fetching exits for {} securities from {from} to {to}",
        entries.len()
    );
    println!("  one market-wide request per exit kind, then filtered to the universe file");
    println!("  the surviving side of a merger is never fetched, because it is not an exit");
    println!();

    let assembled = match source.fetch_delistings(&members, range) {
        Ok(assembled) => assembled,
        Err(SourceError::Unauthorized { provider }) => {
            anyhow::bail!(
                "{provider} rejected the credentials, so every remaining request would fail \
                 the same way. Nothing was written."
            );
        }
        Err(error) => return Err(error.into()),
    };

    println!("Rows served per kind");
    for (kind, count) in &assembled.rows_by_kind {
        println!("  {kind:<22} {count}");
    }
    println!();
    println!("Assembly outcomes");
    println!(
        "  exits inside the universe:          {}",
        assembled.delistings.len()
    );
    println!(
        "  rows naming a ticker outside it:    {}",
        assembled.outside_universe
    );
    println!(
        "  explanations with no exit row:      {}",
        assembled.unmatched_explanations
    );
    // Printed whatever its value. A universe holding two securities under one
    // ticker cannot have a ticker-keyed vendor row attributed to either of
    // them, so both are left out, and a reader has to be able to see how many.
    println!("  tickers the universe reuses:        {ambiguous}");
    println!();

    curate(assembled.delistings, path, source.name())
}

/// Map each universe ticker to its identity, dropping the ones used twice.
///
/// The actions table serves a ticker and nothing else. If two securities in the
/// universe share one, attributing a vendor row to either is a coin flip that
/// could put a 30 percent haircut on the wrong company, so neither is offered
/// for matching and the count is reported. Their exits then reach the engine as
/// unexplained, which is the state the report prints rather than hides.
fn by_ticker(entries: &[UniverseEntry]) -> (BTreeMap<String, AssetKey>, usize) {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in entries {
        *seen.entry(entry.asset.ticker.as_str()).or_insert(0) += 1;
    }

    let unique: BTreeMap<String, AssetKey> = entries
        .iter()
        .filter(|entry| seen[entry.asset.ticker.as_str()] == 1)
        .map(|entry| (entry.asset.ticker.clone(), entry.asset.clone()))
        .collect();
    let ambiguous = seen.values().filter(|count| **count > 1).count();

    (unique, ambiguous)
}

/// Validate and write the delistings dataset.
///
/// The pre-validation is for reporting quality only. `write_delistings` is the
/// guard and checks the same rules again, which is what stops a caller that
/// skips this command from writing a record the domain rejects.
fn curate(rows: Vec<Delisting>, path: &Path, source: &str) -> anyhow::Result<ExitCode> {
    let total = rows.len();

    let rejected: Vec<(&Delisting, _)> = rows
        .iter()
        .filter_map(|row| validate_delisting(row).err().map(|reason| (row, reason)))
        .collect();
    if !rejected.is_empty() {
        return Err(super::refuse(
            total,
            total - rejected.len(),
            &rejected,
            |row| format!("{} {}", row.asset.ticker, row.date),
        ));
    }

    let written = write_delistings(rows, path, source)
        .with_context(|| format!("writing {}", path.display()))?;

    println!("Curated {written} delistings from {total} records.");
    println!("  accepted: {written}");
    println!("  rejected: 0");
    println!("  written:  {}", path.display());
    println!("  source:   {source}");
    println!("  units:    {} (final_market_cap)", ingest::parquet::UNITS);
    Ok(ExitCode::SUCCESS)
}
