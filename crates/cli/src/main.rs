//! The command surface.
//!
//! Deliberately thin. Every rule this platform runs on lives in a library crate
//! so that it holds whether or not anyone goes through the CLI. In particular
//! the trial chain belongs to `rigor` and none of its logic is repeated here.
//!
//! Four commands today.
//!
//! - `status` reports what the trial log says and what the gates permit.
//! - `backtest` runs the engine and records a trial whatever it produced.
//! - `ingest` reports which data providers this machine is configured for.
//! - `curate` writes validated records to Parquet, and counts no trial for it.
//!   Three datasets: prices, delistings, and corporate actions.
//! - `export` writes the characteristic panel, and counts no trial either. It
//!   is top-level rather than a `diagnose` subcommand because it produces an
//!   artifact rather than a measurement.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod backtest;
mod curate;
mod diagnose;
mod export;
mod gates;
mod ingest;
mod status;

/// Where the trial log lives, unless told otherwise.
///
/// Relative to the working directory, which is how a repository-local tool
/// finds a repository-local file. `rigor::DEFAULT_PATH` is the single
/// definition of that path.
const TRIAL_LOG_HELP: &str = "Path to the append-only trial log";

#[derive(Debug, Parser)]
#[command(
    name = "trading",
    version,
    about = "Equity research and execution platform",
    long_about = "Local research platform. Nothing here reaches a broker, and no command \
                  in this binary decides what to trade."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report the trial log, the spread of Sharpes, and gate progress
    Status {
        #[arg(long, default_value = rigor::DEFAULT_PATH, help = TRIAL_LOG_HELP)]
        trials: String,
        /// Research program to scope the counts to, on top of the lifetime
        /// figures which are always reported
        #[arg(long)]
        program: Option<String>,
    },

    /// Run a backtest and record the trial, which happens on every path
    ///
    /// Either `--config`, for a configuration put forward with no engine behind
    /// it, or `--prices` together with `--universe` to run the momentum engine.
    Backtest {
        #[arg(long, default_value = rigor::DEFAULT_PATH, help = TRIAL_LOG_HELP)]
        trials: String,
        /// Research program this trial belongs to, which scopes its N. ASCII
        /// letters, digits, and - _ . only, at most 64 characters
        #[arg(long)]
        program: String,
        /// Robustness variant of the program, when it has one. Recorded under
        /// the bare program name, so the variants of one hypothesis share a
        /// scoped N, with the difference carried by the configuration hash
        #[arg(long, requires = "prices")]
        variant: Option<String>,
        /// A strategy configuration with no engine behind it, hashed into the
        /// log. Records the attempt and reports no Sharpe
        #[arg(long, conflicts_with_all = ["prices", "universe"])]
        config: Option<String>,
        /// Curated price Parquet the momentum engine reads
        #[arg(long, requires = "universe")]
        prices: Option<PathBuf>,
        /// Universe JSONL. Its SHA-256 goes into the configuration hash, and
        /// its members are the only securities the run may see
        #[arg(long, requires = "prices")]
        universe: Option<PathBuf>,
        /// Curated corporate actions Parquet. Its cash dividends are added to
        /// holding returns and its SHA-256 goes into the configuration hash,
        /// so a run with dividends is a different trial from one without
        #[arg(long, requires = "prices")]
        actions: Option<PathBuf>,
        /// Curated delistings Parquet. It classifies the exits the engine
        /// already detects, so performance-related ones are imputed at the
        /// published convention instead of exiting at an unimputed last close.
        /// The convention name goes into the configuration hash, so a run that
        /// imputes is a different trial from one that does not
        #[arg(long, requires = "prices")]
        delistings: Option<PathBuf>,
        /// Curated market cap Parquet. Its figures rank the universe by size
        /// and, divided by the adjusted close, carry the share-count leg of
        /// net payout yield. Its SHA-256 goes into the configuration hash, so
        /// a run against a refetched file is a different trial
        #[arg(long, requires = "prices")]
        marketcap: Option<PathBuf>,
        /// Curated filings Parquet. Its `equityusd` is the book equity the
        /// value signal reads, taken from the latest filing published on or
        /// before each formation. Its SHA-256 goes into the configuration hash,
        /// so a run against a refetched file is a different trial
        #[arg(long, requires = "prices")]
        filings: Option<PathBuf>,
    },

    /// Report which data providers this machine is configured for
    ///
    /// With no action it reports presence and nothing else. `probe-sep`,
    /// `probe-actions` and `probe-daily` read a few vendor rows and print them.
    Ingest {
        #[command(subcommand)]
        action: Option<ingest::Action>,
    },

    /// Measure the machinery without producing a result
    ///
    /// Nothing here is a backtest. These tools compute no return series and no
    /// Sharpe, and record no trial.
    Diagnose {
        #[command(subcommand)]
        what: diagnose::What,
    },

    /// Write validated records to a curated Parquet file
    ///
    /// Curation is not a backtest, so nothing here touches the trial counter.
    Curate {
        #[command(subcommand)]
        dataset: curate::Dataset,
    },

    /// Write the characteristic panel every strategy's signal already produces
    ///
    /// An artifact rather than a measurement. It ranks nothing, holds nothing,
    /// computes no return series and no Sharpe, and records no trial. Every
    /// dataset is required: the panel writes a column per dataset, and one run
    /// without would write a column of nulls instead of refusing.
    Export {
        #[arg(long, default_value = rigor::DEFAULT_PATH, help = TRIAL_LOG_HELP)]
        trials: String,
        #[arg(long)]
        prices: PathBuf,
        /// Universe JSONL. Its SHA-256 goes into the exporting configuration's
        /// hash and into the written file's provenance metadata
        #[arg(long)]
        universe: PathBuf,
        #[arg(long)]
        actions: PathBuf,
        #[arg(long)]
        delistings: PathBuf,
        #[arg(long)]
        marketcap: PathBuf,
        #[arg(long)]
        filings: PathBuf,
        /// Where the characteristic panel Parquet is written
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let outcome = match cli.command {
        Command::Status { trials, program } => status::run(&trials, program.as_deref()),
        Command::Backtest {
            trials,
            program,
            variant,
            config,
            prices,
            universe,
            actions,
            delistings,
            marketcap,
            filings,
        } => backtest::Strategy::from_args(
            config.as_deref(),
            variant.as_deref(),
            prices.as_ref(),
            universe.as_ref(),
            actions.as_ref(),
            delistings.as_ref(),
            marketcap.as_ref(),
            filings.as_ref(),
        )
        .and_then(|strategy| backtest::run(&trials, &program, variant.as_deref(), &strategy)),
        Command::Ingest { action } => ingest::run(action.as_ref()),
        Command::Diagnose { what } => diagnose::run(&what),
        Command::Curate { dataset } => curate::run(&dataset),
        Command::Export {
            trials,
            prices,
            universe,
            actions,
            delistings,
            marketcap,
            filings,
            out,
        } => export::run(
            &trials,
            &prices,
            &universe,
            &actions,
            &delistings,
            &marketcap,
            &filings,
            &out,
        ),
    };

    match outcome {
        Ok(code) => code,
        Err(error) => {
            // `{:#}` prints the whole chain of causes on one line, so a failure
            // to read the log says which file and which OS error rather than
            // only that something went wrong.
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
