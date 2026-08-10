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

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod backtest;
mod curate;
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
    },

    /// Report which data providers this machine is configured for
    ///
    /// With no action it reports presence and nothing else. `probe-sep` reads a
    /// few vendor rows and prints them.
    Ingest {
        #[command(subcommand)]
        action: Option<ingest::Action>,
    },

    /// Write validated records to a curated Parquet file
    ///
    /// Curation is not a backtest, so nothing here touches the trial counter.
    Curate {
        #[command(subcommand)]
        dataset: curate::Dataset,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let outcome = match cli.command {
        Command::Status { trials, program } => status::run(&trials, program.as_deref()),
        Command::Backtest {
            trials,
            program,
            config,
            prices,
            universe,
        } => backtest::Strategy::from_args(config.as_deref(), prices.as_ref(), universe.as_ref())
            .and_then(|strategy| backtest::run(&trials, &program, &strategy)),
        Command::Ingest { action } => ingest::run(action.as_ref()),
        Command::Curate { dataset } => curate::run(&dataset),
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
