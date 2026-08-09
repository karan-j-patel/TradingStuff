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
//!  "session":"RegularHours","close_kind":"ClosingAuction"}
//! ```
//!
//! `permanent` is `null` when the provider supplies no stable identifier.
//! Numbers are quoted because they are decimals rather than floats, and quoting
//! them keeps a JSON parser from routing them through an `f64` on the way in.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Subcommand;
use ingest::parquet::{delistings_path, prices_path, write_delistings, write_prices};
use ingest::{Delisting, PriceBar, validate_batch, validate_delisting};
use serde::de::DeserializeOwned;

/// How many rejected records to name before the message stops being useful.
const REPORTED_REJECTS: usize = 5;

const DATA_ROOT_HELP: &str = "Root of the local data directory. Defaults to the DATA_ROOT environment variable, then to data/";

#[derive(Debug, Subcommand)]
pub enum Dataset {
    /// Curate price bars from a JSONL file of PriceBar records
    Prices {
        /// JSONL file to read, one PriceBar per line
        #[arg(long)]
        input: String,
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
}

pub fn run(dataset: &Dataset) -> anyhow::Result<ExitCode> {
    match dataset {
        Dataset::Prices { input, data_root } => {
            let root = ingest::parquet::data_root(data_root.as_deref());
            curate_prices(input, &prices_path(&root))
        }
        Dataset::Delistings { input, data_root } => {
            let root = ingest::parquet::data_root(data_root.as_deref());
            curate_delistings(input, &delistings_path(&root))
        }
    }
}

fn curate_prices(input: &str, path: &Path) -> anyhow::Result<ExitCode> {
    let bars: Vec<PriceBar> = read_jsonl(input)?;
    let total = bars.len();

    // Validation happens before anything is written. A rejected record fails
    // the whole command rather than being dropped, because a pipeline that
    // silently discards a few percent of its rows produces a backtest that
    // looks fine and is quietly wrong.
    let report = validate_batch(bars);
    if !report.rejected.is_empty() {
        return Err(refuse(
            total,
            report.accepted.len(),
            &report.rejected,
            |bar| format!("{} {}", bar.asset.ticker, bar.date),
        ));
    }

    let written = write_prices(report.accepted, path)
        .with_context(|| format!("writing {}", path.display()))?;

    println!("Curated {written} price bars from {total} records.");
    println!("  accepted: {written}");
    println!("  rejected: 0");
    println!("  written:  {}", path.display());
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
mod tests {
    use super::*;
    use ingest::parquet::read_prices;
    use std::path::PathBuf;

    /// All fixture data is synthetic. Vendor licences forbid redistributing
    /// rows, and that applies to a test file as much as to a data directory.
    const ONE_GOOD_BAR: &str = r#"{"asset":{"ticker":"AAPL","permanent":{"Sharadar":199059}},"date":"2020-01-02","open":"10.25","high":"10.90","low":"10.10","close":"10.75","volume":"1000","session":"RegularHours","close_kind":"ClosingAuction"}"#;

    /// Same shape, no permanent identifier, and a different security.
    const ONE_ANONYMOUS_BAR: &str = r#"{"asset":{"ticker":"ZZZZ","permanent":null},"date":"2020-01-02","open":"3.00","high":"3.50","low":"2.90","close":"3.10","volume":"25","session":"IncludingExtended","close_kind":"LastTrade"}"#;

    /// `high` below `low`, which `PriceBar::validate` rejects.
    const ONE_INVALID_BAR: &str = r#"{"asset":{"ticker":"BAD","permanent":null},"date":"2020-01-03","open":"10.00","high":"9.00","low":"11.00","close":"10.50","volume":"5","session":"RegularHours","close_kind":"ClosingAuction"}"#;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("curate-cli-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creating the scratch directory");
        dir
    }

    fn jsonl(dir: &Path, lines: &[&str]) -> String {
        let path = dir.join("input.jsonl");
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("writing the fixture");
        path.to_string_lossy().into_owned()
    }

    /// T6, the working half. The command produces a file the reader round
    /// trips, at the path the data root implies.
    #[test]
    fn t6_curate_prices_writes_a_file_the_reader_round_trips() {
        let dir = scratch("ok");
        let input = jsonl(&dir, &[ONE_GOOD_BAR, ONE_ANONYMOUS_BAR]);

        let dataset = Dataset::Prices {
            input,
            data_root: Some(dir.to_string_lossy().into_owned()),
        };
        let code = run(&dataset).expect("curating two valid bars succeeds");
        assert_eq!(code, ExitCode::SUCCESS);

        let path = prices_path(&dir);
        assert!(
            path.exists(),
            "the curated file was not written to {path:?}"
        );

        let bars = read_prices(&path).expect("the curated file reads back");
        assert_eq!(bars.len(), 2);

        // The identified row sorts first, and its identity survived the trip
        // through JSON, validation, Parquet, and back.
        assert_eq!(bars[0].asset.ticker, "AAPL");
        assert!(
            bars[0].asset.is_stable(),
            "the permanent id did not survive the CLI path"
        );
        assert_eq!(
            bars[0].close,
            rust_decimal::Decimal::from_str_exact("10.75").unwrap()
        );
        assert_eq!(bars[1].asset.ticker, "ZZZZ");
        assert!(!bars[1].asset.is_stable());
    }

    /// T6, the refusing half. One invalid bar fails the command, writes
    /// nothing, and names the reject.
    ///
    /// The "writes nothing" half is the part worth testing. A command that
    /// wrote the good rows and reported the bad ones would pass a test that
    /// only checked the exit code, and would have silently dropped data.
    #[test]
    fn t6_one_invalid_bar_fails_the_command_and_writes_nothing() {
        let dir = scratch("invalid");
        let input = jsonl(&dir, &[ONE_GOOD_BAR, ONE_INVALID_BAR]);

        let dataset = Dataset::Prices {
            input,
            data_root: Some(dir.to_string_lossy().into_owned()),
        };
        let error = run(&dataset).expect_err("an invalid bar must fail the command");

        // `{:#}` prints the whole chain of causes, which is what the binary
        // prints, so this asserts on what a person would actually see.
        let message = format!("{error:#}");
        assert!(
            message.contains("BAD"),
            "the error must name the rejected record: {message}"
        );
        assert!(
            message.contains("high 9.00 is below low 11.00"),
            "the error must give the reason: {message}"
        );
        assert!(
            message.contains("1 of 2"),
            "the error must report the counts: {message}"
        );

        assert!(
            !prices_path(&dir).exists(),
            "a refused command wrote a file anyway"
        );
    }

    /// A malformed line is an error rather than a skipped record.
    #[test]
    fn t6_a_malformed_line_fails_the_command_by_line_number() {
        let dir = scratch("malformed");
        let input = jsonl(&dir, &[ONE_GOOD_BAR, "{not json"]);

        let dataset = Dataset::Prices {
            input,
            data_root: Some(dir.to_string_lossy().into_owned()),
        };
        let error = run(&dataset).expect_err("a malformed line must fail the command");
        let message = format!("{error:#}");
        assert!(
            message.contains("line 2"),
            "the error must name the line: {message}"
        );
        assert!(!prices_path(&dir).exists());
    }

    /// Synthetic. An imputed value that matches its convention's published
    /// figure, which is what `Delisting::imputed()` produces.
    const ONE_GOOD_DELISTING: &str = r#"{"asset":{"ticker":"GONE","permanent":{"Sharadar":4242}},"date":"2021-03-01","reason":"Bankruptcy","listing":"Nasdaq","terminal":{"Imputed":{"value":"-0.55","convention":"ShumwayWarther1999Nasdaq"}},"source":"synthetic"}"#;

    /// Claims the Nasdaq convention while carrying the NYSE/AMEX figure, so
    /// the record is incoherent rather than merely unusual.
    const ONE_INCOHERENT_DELISTING: &str = r#"{"asset":{"ticker":"WRONG","permanent":null},"date":"2021-03-02","reason":"Bankruptcy","listing":"Nasdaq","terminal":{"Imputed":{"value":"-0.30","convention":"ShumwayWarther1999Nasdaq"}},"source":"synthetic"}"#;

    /// A holder cannot lose more than everything.
    const ONE_IMPOSSIBLE_DELISTING: &str = r#"{"asset":{"ticker":"DEEP","permanent":null},"date":"2021-03-03","reason":"Bankruptcy","listing":"Nasdaq","terminal":{"Observed":"-1.5"},"source":"synthetic"}"#;

    #[test]
    fn curate_delistings_writes_a_file_the_reader_round_trips() {
        let dir = scratch("del-ok");
        let input = jsonl(&dir, &[ONE_GOOD_DELISTING]);

        let dataset = Dataset::Delistings {
            input,
            data_root: Some(dir.to_string_lossy().into_owned()),
        };
        run(&dataset).expect("curating a valid delisting succeeds");

        let rows = ingest::parquet::read_delistings(&delistings_path(&dir))
            .expect("the curated file reads back");
        assert_eq!(rows.len(), 1);
        assert!(
            !rows[0].terminal.is_observed(),
            "an imputed value came back as observed"
        );
    }

    /// An imputed value disagreeing with its own convention fails the command
    /// and writes nothing.
    #[test]
    fn a_delisting_disagreeing_with_its_convention_fails_the_command() {
        let dir = scratch("del-incoherent");
        let input = jsonl(&dir, &[ONE_GOOD_DELISTING, ONE_INCOHERENT_DELISTING]);

        let dataset = Dataset::Delistings {
            input,
            data_root: Some(dir.to_string_lossy().into_owned()),
        };
        let error = run(&dataset).expect_err("an incoherent delisting must fail the command");
        let message = format!("{error:#}");

        assert!(
            message.contains("WRONG"),
            "the error must name the rejected record: {message}"
        );
        assert!(
            message.contains("1 of 2"),
            "the error must report the counts: {message}"
        );
        assert!(
            !delistings_path(&dir).exists(),
            "a refused command wrote a file anyway"
        );
    }

    /// A return below total loss fails the command too, so the check is not
    /// only about conventions.
    #[test]
    fn a_delisting_losing_more_than_everything_fails_the_command() {
        let dir = scratch("del-impossible");
        let input = jsonl(&dir, &[ONE_IMPOSSIBLE_DELISTING]);

        let dataset = Dataset::Delistings {
            input,
            data_root: Some(dir.to_string_lossy().into_owned()),
        };
        let error = run(&dataset).expect_err("an impossible return must fail the command");
        assert!(format!("{error:#}").contains("DEEP"), "got {error:#}");
        assert!(!delistings_path(&dir).exists());
    }

    /// An empty input file is refused rather than written as a zero-row file.
    #[test]
    fn t6_an_empty_input_file_is_refused() {
        let dir = scratch("empty");
        let path = dir.join("input.jsonl");
        std::fs::write(&path, "").expect("writing an empty fixture");

        let dataset = Dataset::Prices {
            input: path.to_string_lossy().into_owned(),
            data_root: Some(dir.to_string_lossy().into_owned()),
        };
        let error = run(&dataset).expect_err("an empty file must fail the command");
        assert!(format!("{error:#}").contains("no rows"), "got {error:#}");
        assert!(!prices_path(&dir).exists());
    }
}
