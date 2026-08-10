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
    actions_path, delistings_path, prices_path, write_actions, write_delistings, write_prices,
};
use ingest::provider::{AdjustedPriceSource, DateRange};
use ingest::sharadar::SharadarClient;
use ingest::{
    ActionRecord, AdjustedBar, AssetKey, Delisting, validate_action, validate_adjusted,
    validate_delisting,
};
use jiff::civil::Date;
use serde::de::DeserializeOwned;

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
        /// First date to fetch, inclusive. Only with --fetch
        #[arg(long)]
        from: Option<Date>,
        /// Last date to fetch, inclusive. Only with --fetch
        #[arg(long)]
        to: Option<Date>,
        #[arg(long, help = DATA_ROOT_HELP)]
        data_root: Option<String>,
    },

    /// Curate delistings from a JSONL file of Delisting records
    Delistings {
        /// JSONL file to read, one Delisting per line
        #[arg(long)]
        input: String,
        #[arg(long, help = DATA_ROOT_HELP)]
        data_root: Option<String>,
    },

    /// Curate corporate actions from a JSONL file of ActionRecord records
    Actions {
        /// JSONL file to read, one ActionRecord per line
        #[arg(long)]
        input: String,
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
            from,
            to,
            data_root,
        } => {
            let root = ingest::parquet::data_root(data_root.as_deref());
            let path = prices_path(&root);
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
        Dataset::Delistings { input, data_root } => {
            let root = ingest::parquet::data_root(data_root.as_deref());
            curate_delistings(input, &delistings_path(&root))
        }
        Dataset::Actions { input, data_root } => {
            let root = ingest::parquet::data_root(data_root.as_deref());
            curate_actions(input, &actions_path(&root))
        }
    }
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

fn curate_delistings(input: &str, path: &Path) -> anyhow::Result<ExitCode> {
    let rows: Vec<Delisting> = read_jsonl(input)?;
    let total = rows.len();

    // Same refusal behaviour as prices. Rejected records fail the command
    // before anything is written rather than being dropped.
    let rejected: Vec<(&Delisting, _)> = rows
        .iter()
        .filter_map(|row| validate_delisting(row).err().map(|reason| (row, reason)))
        .collect();
    if !rejected.is_empty() {
        return Err(refuse(total, total - rejected.len(), &rejected, |row| {
            format!("{} {}", row.asset.ticker, row.date)
        }));
    }

    let written =
        write_delistings(rows, path).with_context(|| format!("writing {}", path.display()))?;

    println!("Curated {written} delistings from {total} records.");
    println!("  accepted: {written}");
    println!("  rejected: 0");
    println!("  written:  {}", path.display());
    Ok(ExitCode::SUCCESS)
}

fn curate_actions(input: &str, path: &Path) -> anyhow::Result<ExitCode> {
    let rows: Vec<ActionRecord> = read_jsonl(input)?;
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
        write_actions(rows, path).with_context(|| format!("writing {}", path.display()))?;

    println!("Curated {written} corporate actions from {total} records.");
    println!("  accepted: {written}");
    println!("  rejected: 0");
    println!("  written:  {}", path.display());
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
