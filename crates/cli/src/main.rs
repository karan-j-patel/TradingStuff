//! The command surface.
//!
//! Deliberately thin. Every rule this platform runs on lives in a library crate
//! so that it holds whether or not anyone goes through the CLI. In particular
//! the trial chain belongs to `rigor` and none of its logic is repeated here.
//!
//! Three commands today.
//!
//! - `status` reports what the trial log says and what the gates permit.
//! - `backtest` records a trial, then admits there is no engine to run.
//! - `ingest` reports which data providers this machine is configured for.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod backtest;
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

    /// Record a trial in the log, which happens before anything else runs
    Backtest {
        #[arg(long, default_value = rigor::DEFAULT_PATH, help = TRIAL_LOG_HELP)]
        trials: String,
        /// Research program this trial belongs to, which scopes its N
        #[arg(long)]
        program: String,
        /// The strategy configuration, hashed into the log. Free text for now,
        /// and the serialised config of a real engine once one exists
        #[arg(long)]
        config: String,
    },

    /// Report which data providers this machine is configured for
    Ingest,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let outcome = match cli.command {
        Command::Status { trials, program } => status::run(&trials, program.as_deref()),
        Command::Backtest {
            trials,
            program,
            config,
        } => backtest::run(&trials, &program, &config),
        Command::Ingest => ingest::run(),
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
