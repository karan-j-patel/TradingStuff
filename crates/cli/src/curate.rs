//! `curate`. Turn a JSONL file of validated records into a curated Parquet file.
//!
//! This is the single door for producing curated data. Everything downstream,
//! including the Python side, reads what this command writes, so the validation
//! gate lives here rather than in each caller.
//!
//! # What this is not
//!
//! Curation is not a backtest. Nothing here touches the trial counter, because
//! writing a file is not a hypothesis about returns and counting it as one would
//! inflate the denominator that every deflated Sharpe in this project divides
//! by.
//!
//! # Input format
//!
//! One JSON object per line, in the serde form of the record type. For prices:
//!
//! ```text
//! {"asset":{"ticker":"AAPL","permanent":{"Sharadar":199059}},"date":"2020-01-02",
//!  "open":"10.25","high":"10.90","low":"10.10","close":"10.75","volume":"1000",
//!  "close_unadjusted":"41.00","session":"RegularHours","close_kind":"ClosingAuction"}
//! ```
//!
//! Prices are [`ingest::AdjustedBar`] records: open, high, low and close on the
//! vendor's split-adjusted basis, and `close_unadjusted` as traded. See that
//! type for why the two cannot be mixed up.
//!
//! `permanent` is `null` when the provider supplies no stable identifier.
//! Numbers are quoted because they are decimals rather than floats, and quoting
//! them keeps a JSON parser from routing them through an `f64` on the way in.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Subcommand;
use ingest::parquet::{
    actions_path, delistings_path, marketcap_path, prices_path, write_actions, write_marketcap,
    write_prices,
};
use ingest::provider::{AdjustedPriceSource, DateRange, SourceError};
use ingest::sharadar::SharadarClient;
use ingest::universe::{self, FetchOutcome};
use ingest::{
    ActionRecord, AdjustedBar, AssetKey, MarketCapRecord, validate_action, validate_adjusted,
    validate_marketcap,
};
use jiff::civil::Date;
use serde::de::DeserializeOwned;

/// The delistings dataset, which has a market-wide fetch of its own.
mod delistings;

/// How many rejected records to name before the message stops being useful.
const REPORTED_REJECTS: usize = 5;

const DATA_ROOT_HELP: &str = "Root of the local data directory. Defaults to the DATA_ROOT environment variable, then to data/";

#[derive(Debug, Subcommand)]
pub enum Dataset {
    /// Curate price bars, from a JSONL file or straight from the provider
    Prices {
        /// JSONL file to read, one AdjustedBar per line
        #[arg(long)]
        input: Option<String>,
        /// Fetch from the configured provider instead of reading a file
        #[arg(long)]
        fetch: bool,
        /// Tickers to fetch, comma separated. Only with --fetch
        #[arg(long, value_delimiter = ',')]
        tickers: Vec<String>,
        /// Universe file to fetch, as written by `ingest fetch-universe`.
        ///
        /// Fetches one security at a time and tolerates a vendor declining
        /// any one of them, which `--tickers` does not. The file is rewritten
        /// afterwards with what each name actually returned
        #[arg(long, conflicts_with = "tickers")]
        universe: Option<String>,
        /// First date to fetch, inclusive. Only with --fetch
        #[arg(long)]
        from: Option<Date>,
        /// Last date to fetch, inclusive. Only with --fetch
        #[arg(long)]
        to: Option<Date>,
        #[arg(long, help = DATA_ROOT_HELP)]
        data_root: Option<String>,
    },

    /// Curate delistings, from a JSONL file or straight from the provider
    Delistings {
        /// JSONL file to read, one Delisting per line
        #[arg(long)]
        input: Option<String>,
        /// Fetch from the configured provider instead of reading a file
        #[arg(long)]
        fetch: bool,
        /// Universe file to fetch against, as written by `ingest fetch-universe`.
        ///
        /// The walk is market-wide, one request per exit kind, and the file is
        /// what turns a vendor ticker into a record carrying the vendor's
        /// permanent identifier. Rows naming anything outside it are discarded
        /// during curation and never stored
        #[arg(long)]
        universe: Option<String>,
        /// First date to fetch, inclusive. Only with --fetch
        #[arg(long)]
        from: Option<Date>,
        /// Last date to fetch, inclusive. Only with --fetch
        #[arg(long)]
        to: Option<Date>,
        #[arg(long, help = DATA_ROOT_HELP)]
        data_root: Option<String>,
    },

    /// Curate corporate actions, from a JSONL file or straight from the provider
    Actions {
        /// JSONL file to read, one ActionRecord per line
        #[arg(long)]
        input: Option<String>,
        /// Fetch from the configured provider instead of reading a file
        #[arg(long)]
        fetch: bool,
        /// Universe file to fetch, as written by `ingest fetch-universe`.
        ///
        /// Fetches one security at a time and tolerates a vendor declining any
        /// one of them. Only cash dividends are fetched, which is the only kind
        /// this platform consumes
        #[arg(long)]
        universe: Option<String>,
        /// First date to fetch, inclusive. Only with --fetch
        #[arg(long)]
        from: Option<Date>,
        /// Last date to fetch, inclusive. Only with --fetch
        #[arg(long)]
        to: Option<Date>,
        #[arg(long, help = DATA_ROOT_HELP)]
        data_root: Option<String>,
    },

    /// Curate daily market capitalisation, straight from the provider
    ///
    /// Fetch only. There is no JSONL door here, because nothing produces one
    /// of these files by hand and a figure typed by an operator would carry no
    /// units anybody could check
    Marketcap {
        /// Fetch from the configured provider. Required, and named anyway so
        /// the command reads the same as its siblings
        #[arg(long)]
        fetch: bool,
        /// Universe file to fetch, as written by `ingest fetch-universe`.
        ///
        /// Fetches one security at a time and tolerates a vendor declining any
        /// one of them
        #[arg(long)]
        universe: Option<String>,
        /// First date to fetch, inclusive
        #[arg(long)]
        from: Option<Date>,
        /// Last date to fetch, inclusive
        #[arg(long)]
        to: Option<Date>,
        #[arg(long, help = DATA_ROOT_HELP)]
        data_root: Option<String>,
    },
}

pub fn run(dataset: &Dataset) -> anyhow::Result<ExitCode> {
    match dataset {
        Dataset::Prices {
            input,
            fetch,
            tickers,
            universe,
            from,
            to,
            data_root,
        } => {
            let root = ingest::parquet::data_root(data_root.as_deref());
            let path = prices_path(&root);

            if let Some(universe) = universe {
                if !*fetch {
                    anyhow::bail!("--universe describes what to fetch, so it needs --fetch");
                }
                return curate_from_universe(universe, *from, *to, &path);
            }

            let (bars, source) = if *fetch {
                fetch_prices(tickers, *from, *to)?
            } else {
                let input = input.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("prices needs either --input <file> or --fetch")
                })?;
                // Named rather than left blank: a curated file whose vendor is
                // unknown is one nobody can trace, and "an operator handed us a
                // file" is itself the provenance.
                (read_jsonl(input)?, "operator-jsonl".to_string())
            };
            curate_prices(bars, &path, &source)
        }
        Dataset::Delistings {
            input,
            fetch,
            universe,
            from,
            to,
            data_root,
        } => {
            let root = ingest::parquet::data_root(data_root.as_deref());
            delistings::run(
                input.as_deref(),
                *fetch,
                universe.as_deref(),
                *from,
                *to,
                &delistings_path(&root),
            )
        }
        Dataset::Actions {
            input,
            fetch,
            universe,
            from,
            to,
            data_root,
        } => {
            let root = ingest::parquet::data_root(data_root.as_deref());
            let path = actions_path(&root);

            if let Some(universe) = universe {
                if !*fetch {
                    anyhow::bail!("--universe describes what to fetch, so it needs --fetch");
                }
                return fetch_actions_from_universe(universe, *from, *to, &path);
            }
            if *fetch {
                anyhow::bail!(
                    "--fetch needs --universe, so every record carries the vendor's \
                               permanent identifier rather than a ticker string"
                );
            }

            let input = input
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("actions needs either --input <file> or --fetch"))?;
            // Named rather than left blank, on the same rule prices follows: a
            // curated file whose vendor is unknown cannot be reconciled, and
            // "an operator handed us a file" is itself the provenance.
            curate_actions(read_jsonl(input)?, &path, "operator-jsonl")
        }
        Dataset::Marketcap {
            fetch,
            universe,
            from,
            to,
            data_root,
        } => {
            let root = ingest::parquet::data_root(data_root.as_deref());
            let path = marketcap_path(&root);

            let Some(universe) = universe else {
                anyhow::bail!(
                    "marketcap needs --universe, so every record carries the vendor's \
                     permanent identifier rather than a ticker string"
                );
            };
            if !*fetch {
                anyhow::bail!("--universe describes what to fetch, so it needs --fetch");
            }
            fetch_marketcap_from_universe(universe, *from, *to, &path)
        }
    }
}

/// Fetch daily market capitalisation for every security in a universe file.
///
/// Mirrors [`fetch_actions_from_universe`] down to the conduct: serial, one
/// security at a time, a decline recorded against the name and the walk
/// continued, and a rejected credential fatal because that is one fault
/// affecting every remaining request.
///
/// The empty case is counted rather than treated as a decline, and it is not
/// rare. Measured 2026-08-11, this table's history starts around 1998-12-01 and
/// a security that stopped trading before then is served with prices and with
/// no market cap row at all. A name returning nothing is a fact about coverage,
/// so it is reported separately from a name the vendor refused.
///
/// The universe file is not rewritten. Its `outcome` field records what the
/// price fetch served, and overwriting that with market cap counts would lose
/// the record of which names have prices.
fn fetch_marketcap_from_universe(
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

    let source = SharadarClient::native_from_env()?;
    let total = entries.len();
    println!("Fetching daily market capitalisation for {total} securities from {from} to {to}");
    println!("  figures are stored exactly as the vendor ships them, in millions of US dollars,");
    println!("  and the units are recorded in the file's metadata rather than converted here");
    println!("  serial, one security at a time, and a decline is recorded rather than fatal");
    println!();

    let mut records: Vec<MarketCapRecord> = Vec::new();
    let mut declined = 0usize;
    let mut no_coverage = 0usize;

    for (index, entry) in entries.iter().enumerate() {
        let position = index + 1;
        let ticker = &entry.asset.ticker;

        match source.fetch_marketcaps(&entry.asset, range) {
            Ok(fetched) if fetched.is_empty() => {
                no_coverage += 1;
            }
            Ok(fetched) => {
                println!("  [{position}/{total}] {ticker}: {} days", fetched.len());
                records.extend(fetched);
            }
            Err(SourceError::Unauthorized { provider }) => {
                anyhow::bail!(
                    "{provider} rejected the credentials at security {position} of {total}, so \
                     every remaining fetch would fail the same way. Nothing was written."
                );
            }
            Err(error) => {
                declined += 1;
                println!("  [{position}/{total}] {ticker}: DECLINED, {error}");
            }
        }
    }

    println!();
    println!("Fetch outcomes");
    println!(
        "  securities with a market cap: {}",
        total - no_coverage - declined
    );
    println!("  securities the table has no rows for: {no_coverage}");
    println!("  declined:                             {declined}");
    println!();

    curate_marketcap(records, path, source.name())
}

/// Validate and write the market cap dataset.
///
/// The pre-validation is for reporting quality only. `write_marketcap` is the
/// guard and checks the same rules again, which is what stops a caller that
/// skips this command from writing a figure the domain rejects.
fn curate_marketcap(
    rows: Vec<MarketCapRecord>,
    path: &Path,
    source: &str,
) -> anyhow::Result<ExitCode> {
    let total = rows.len();

    let rejected: Vec<(&MarketCapRecord, _)> = rows
        .iter()
        .filter_map(|row| validate_marketcap(row).err().map(|reason| (row, reason)))
        .collect();
    if !rejected.is_empty() {
        return Err(refuse(total, total - rejected.len(), &rejected, |row| {
            format!("{} {}", row.asset.ticker, row.date)
        }));
    }

    let written = write_marketcap(rows, path, source)
        .with_context(|| format!("writing {}", path.display()))?;

    println!("Curated {written} market cap rows from {total} records.");
    println!("  accepted: {written}");
    println!("  rejected: 0");
    println!("  written:  {}", path.display());
    println!("  source:   {source}");
    println!("  units:    {}", ingest::parquet::UNITS);
    Ok(ExitCode::SUCCESS)
}

/// Fetch cash dividends for every security in a universe file.
///
/// Mirrors [`curate_from_universe`], which is the prices version, down to the
/// conduct: serial, one security at a time, a decline recorded against the name
/// and the walk continued, and a rejected credential fatal because that is one
/// fault affecting every remaining request.
///
/// Two differences from the prices walk, both because dividends are sparser
/// than prices. A security that paid nothing is a success with no rows rather
/// than a decline, since most securities pay nothing. And the universe file is
/// not rewritten, because its `outcome` field records what the price fetch
/// served and overwriting that with dividend counts would lose the record of
/// which names have prices at all.
fn fetch_actions_from_universe(
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

    let source = SharadarClient::native_from_env()?;
    let total = entries.len();
    println!("Fetching cash dividends for {total} securities from {from} to {to}");
    println!("  cash dividends only. Splits and stock dividends are already in the adjusted");
    println!("  close, so fetching them as cash would count the same event twice");
    println!("  serial, one security at a time, and a decline is recorded rather than fatal");
    println!();

    let mut records: Vec<ActionRecord> = Vec::new();
    let mut declined = 0usize;
    let mut paid_nothing = 0usize;

    for (index, entry) in entries.iter().enumerate() {
        let position = index + 1;
        let ticker = &entry.asset.ticker;

        match source.fetch_cash_dividends(&entry.asset, range) {
            Ok(fetched) if fetched.is_empty() => {
                paid_nothing += 1;
            }
            Ok(fetched) => {
                println!(
                    "  [{position}/{total}] {ticker}: {} dividends",
                    fetched.len()
                );
                records.extend(fetched);
            }
            Err(SourceError::Unauthorized { provider }) => {
                anyhow::bail!(
                    "{provider} rejected the credentials at security {position} of {total}, so \
                     every remaining fetch would fail the same way. Nothing was written."
                );
            }
            Err(error) => {
                declined += 1;
                println!("  [{position}/{total}] {ticker}: DECLINED, {error}");
            }
        }
    }

    println!();
    println!("Fetch outcomes");
    println!(
        "  securities paying dividends: {}",
        total - paid_nothing - declined
    );
    println!("  securities paying none:      {paid_nothing}");
    println!("  declined:                    {declined}");
    println!();

    curate_actions(records, path, source.name())
}

/// Pull bars from the provider, refusing an incomplete instruction.
///
/// The window is enforced by the source rather than here, so a range reaching
/// before what the key serves is an error naming the boundary instead of a
/// quietly shortened series.
fn fetch_prices(
    tickers: &[String],
    from: Option<Date>,
    to: Option<Date>,
) -> anyhow::Result<(Vec<AdjustedBar>, String)> {
    if tickers.is_empty() {
        anyhow::bail!("--fetch needs --tickers");
    }
    let (from, to) = match (from, to) {
        (Some(from), Some(to)) => (from, to),
        _ => anyhow::bail!("--fetch needs --from and --to"),
    };
    let range = DateRange::new(from, to)?;

    let source = SharadarClient::native_from_env()?;
    let assets: Vec<AssetKey> = tickers.iter().map(AssetKey::ticker_only).collect();

    println!(
        "Fetching {} tickers from {} for {from} to {to}",
        assets.len(),
        source.name()
    );
    if let Some(earliest) = source.earliest_available()? {
        println!("  provider serves from {earliest} (measured, not claimed)");
    }

    Ok((
        source.fetch_adjusted(&assets, range)?,
        source.name().to_string(),
    ))
}

/// Fetch every security in a universe file, one at a time, and curate the lot.
///
/// # Why this is not `--tickers` with a longer list
///
/// Two differences, and both are the reason the universe file exists.
///
/// The identity survives. A universe entry carries the vendor's permanent
/// identifier, and `--tickers` cannot: it builds an [`AssetKey`] from a string,
/// so the curated rows would be keyed on a label that companies change. See
/// [`ingest::AssetKey`] for what that costs.
///
/// One security declining does not lose the other six hundred. `fetch_adjusted`
/// over a list is all or nothing, which is the right behaviour when a caller
/// named the securities it wants and the wrong behaviour when the list came
/// from a rule. A name the vendor will not serve is a fact about the data, so it
/// is recorded against that name and the fetch continues.
///
/// # What is deliberately not tolerated
///
/// A rejected credential. That is one fault affecting every security, and
/// carrying on would spend an hour writing "declined" beside six hundred names
/// that were never asked properly. It stops on the first one.
fn curate_from_universe(
    universe_path: &str,
    from: Option<Date>,
    to: Option<Date>,
    prices_path: &Path,
) -> anyhow::Result<ExitCode> {
    let (from, to) = match (from, to) {
        (Some(from), Some(to)) => (from, to),
        _ => anyhow::bail!("--fetch needs --from and --to"),
    };
    let range = DateRange::new(from, to)?;

    let text = std::fs::read_to_string(universe_path)
        .with_context(|| format!("reading {universe_path}"))?;
    let mut entries = universe::from_jsonl(&text)?;
    if entries.is_empty() {
        anyhow::bail!("{universe_path} holds no securities, so there is nothing to fetch");
    }

    let source = SharadarClient::native_from_env()?;
    println!(
        "Fetching {} securities from {} for {from} to {to}",
        entries.len(),
        source.name()
    );
    if let Some(earliest) = source.earliest_available()? {
        println!("  provider serves from {earliest} (measured, not claimed)");
    }
    println!("  serial, one security at a time, and a decline is recorded rather than fatal");
    println!();

    let mut bars: Vec<AdjustedBar> = Vec::new();
    let mut declined = 0usize;
    let total = entries.len();

    for (index, entry) in entries.iter_mut().enumerate() {
        let position = index + 1;
        let ticker = entry.asset.ticker.clone();

        match source.fetch_adjusted(std::slice::from_ref(&entry.asset), range) {
            Ok(fetched) => {
                // The dates are read off what arrived rather than off the
                // request, because a security delisted mid-window returns a
                // shorter series and that is exactly what needs recording.
                let span = fetched
                    .iter()
                    .map(|bar| bar.date)
                    .min()
                    .zip(fetched.iter().map(|bar| bar.date).max());
                match span {
                    Some((first, last)) => {
                        println!(
                            "  [{position}/{total}] {ticker}: {} bars, {first} to {last}",
                            fetched.len()
                        );
                        entry.outcome = Some(FetchOutcome::Served {
                            bars: fetched.len(),
                            first,
                            last,
                        });
                        bars.extend(fetched);
                    }
                    // The source refuses an empty answer, so this is
                    // unreachable through the Sharadar path and is still not
                    // silently treated as a success.
                    None => {
                        declined += 1;
                        println!("  [{position}/{total}] {ticker}: the vendor served no rows");
                        entry.outcome = Some(FetchOutcome::Declined {
                            reason: "the vendor served no rows".to_string(),
                        });
                    }
                }
            }
            Err(SourceError::Unauthorized { provider }) => {
                anyhow::bail!(
                    "{provider} rejected the credentials at security {position} of {total}, so \
                     every remaining fetch would fail the same way. Nothing was written."
                );
            }
            Err(error) => {
                declined += 1;
                println!("  [{position}/{total}] {ticker}: DECLINED, {error}");
                entry.outcome = Some(FetchOutcome::Declined {
                    reason: error.to_string(),
                });
            }
        }
    }

    println!();
    let outcome = curate_prices(bars, prices_path, source.name())?;

    // Written after the prices, so a universe file claiming data landed is
    // never left behind by a run whose curated write failed.
    std::fs::write(universe_path, universe::to_jsonl(&entries)?)
        .with_context(|| format!("rewriting {universe_path}"))?;

    println!();
    println!("Universe outcomes");
    println!("  served:   {}", entries.len() - declined);
    println!("  declined: {declined}");
    println!("  recorded in {universe_path}");
    if declined > 0 {
        println!();
        println!("Declined securities, with the vendor's reason:");
        for entry in entries.iter() {
            if let Some(FetchOutcome::Declined { reason }) = &entry.outcome {
                println!("  {:<10} {reason}", entry.asset.ticker);
            }
        }
    }

    Ok(outcome)
}

fn curate_prices(bars: Vec<AdjustedBar>, path: &Path, source: &str) -> anyhow::Result<ExitCode> {
    let total = bars.len();

    // Validation happens before anything is written. A rejected record fails
    // the whole command rather than being dropped, because a pipeline that
    // silently discards a few percent of its rows produces a backtest that
    // looks fine and is quietly wrong.
    let report = validate_adjusted(bars);
    if !report.rejected.is_empty() {
        return Err(refuse(
            total,
            report.accepted.len(),
            &report.rejected,
            |bar| format!("{} {}", bar.asset.ticker, bar.date),
        ));
    }

    let written = write_prices(report.accepted, path, source)
        .with_context(|| format!("writing {}", path.display()))?;

    println!("Curated {written} price bars from {total} records.");
    println!("  accepted: {written}");
    println!("  rejected: 0");
    println!("  written:  {}", path.display());
    println!("  source:   {source}");
    Ok(ExitCode::SUCCESS)
}

fn curate_actions(rows: Vec<ActionRecord>, path: &Path, source: &str) -> anyhow::Result<ExitCode> {
    let total = rows.len();

    // Pre-validation here is for reporting quality. `write_actions` is the
    // guard, and it checks the same rules again.
    let rejected: Vec<(&ActionRecord, _)> = rows
        .iter()
        .filter_map(|row| validate_action(row).err().map(|reason| (row, reason)))
        .collect();
    if !rejected.is_empty() {
        return Err(refuse(total, total - rejected.len(), &rejected, |row| {
            format!("{} {}", row.asset.ticker, row.effective)
        }));
    }

    let written =
        write_actions(rows, path, source).with_context(|| format!("writing {}", path.display()))?;

    println!("Curated {written} corporate actions from {total} records.");
    println!("  accepted: {written}");
    println!("  rejected: 0");
    println!("  written:  {}", path.display());
    println!("  source:   {source}");
    Ok(ExitCode::SUCCESS)
}

/// The error a refused batch fails with.
///
/// Shared by both datasets so neither can quietly report less than the other,
/// and so the counts always come from the same arithmetic.
fn refuse<T, E: std::fmt::Display>(
    total: usize,
    accepted: usize,
    rejected: &[(T, E)],
    label: impl Fn(&T) -> String,
) -> anyhow::Error {
    let mut named = String::new();
    for (record, reason) in rejected.iter().take(REPORTED_REJECTS) {
        named.push_str(&format!("\n  {}: {reason}", label(record)));
    }
    if rejected.len() > REPORTED_REJECTS {
        named.push_str(&format!(
            "\n  and {} more",
            rejected.len() - REPORTED_REJECTS
        ));
    }
    anyhow::anyhow!(
        "{} of {total} records failed validation, so nothing was written. \
         {accepted} would have been accepted.{named}",
        rejected.len(),
    )
}

/// One JSON object per line, with the line number in any parse error.
///
/// A blank or malformed line is an error rather than a skipped record. Skipping
/// would mean the count this command reports is not the count the file holds,
/// and nobody would find out until a number downstream looked slightly wrong.
fn read_jsonl<T: DeserializeOwned>(path: &str) -> anyhow::Result<Vec<T>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).with_context(|| format!("parsing {path} line {}", index + 1))
        })
        .collect()
}

#[cfg(test)]
mod tests;
