//! `ingest`. Which data providers this machine is configured for.
//!
//! This is the "what do I need to bring" check. The platform may ship bring
//! your own key, so a new operator's first question is which subscriptions
//! they have to hold, and their second is which of them this machine can
//! already see.
//!
//! # The one rule this file must not break
//!
//! It reports presence and never a value. Not truncated, not masked, not the
//! first four characters. A secret printed to a terminal reaches the scrollback
//! buffer, the shell history of whoever piped it, and any log the terminal
//! keeps. There is no version of showing part of a key that is worth the risk,
//! and presence is the only thing the question actually needs.
//!
//! # What it does not do
//!
//! It does not read `.env`. Loading a dotenv file would mean a dependency whose
//! job is to put secrets into this process, for a command whose entire purpose
//! is to avoid handling them. It reads the process environment, and the output
//! says how to get `.env` into that environment.

use std::process::ExitCode;

use clap::Subcommand;

mod probe;
mod probe_actions;
mod universe;

/// What `ingest` can do beyond reporting presence.
///
/// An optional subcommand rather than a replacement, so bare `ingest` keeps
/// meaning what it has always meant.
#[derive(Debug, Subcommand)]
pub enum Action {
    /// Report what SEP ships around known split dates
    ///
    /// Reads a handful of rows and prints them. Writes nothing, counts no
    /// trial, and draws no conclusion.
    ProbeSep,

    /// Report what the corporate actions table ships
    ///
    /// Reads a handful of rows and prints them. Writes nothing, counts no
    /// trial, and draws no conclusion.
    ProbeActions,

    /// Build the research universe from the whole security master
    ///
    /// Applies a deterministic sample rule to every security the vendor has,
    /// delisted ones included, and writes the result as JSONL. Fetches no
    /// prices and counts no trial.
    FetchUniverse {
        /// Where to write the universe. Belongs under data/, which is
        /// gitignored
        #[arg(long)]
        out: String,
    },
}

/// A provider and the variables it needs, mirroring `.env.example`.
///
/// `.env.example` is the committed template and stays blank. If a name changes
/// there it must change here, and the mismatch shows up as a variable this
/// command reports missing forever.
struct Provider {
    name: &'static str,
    purpose: &'static str,
    /// Without all of these the provider cannot be used at all.
    required: &'static [&'static str],
    /// Useful but not load bearing, usually because a default exists.
    optional: &'static [&'static str],
}

const PROVIDERS: &[Provider] = &[
    Provider {
        name: "Sharadar",
        purpose: "slow strand prices, fundamentals, and corporate actions",
        required: &["SHARADAR_API_KEY"],
        optional: &[],
    },
    Provider {
        name: "Alpaca paper",
        purpose: "paper trading, the last milestone before real money",
        required: &["ALPACA_PAPER_KEY", "ALPACA_PAPER_SECRET"],
        optional: &["ALPACA_PAPER_ENDPOINT", "ALPACA_RECOVERY_KEY"],
    },
    Provider {
        name: "Local",
        purpose: "on-disk Parquet root and the local model host",
        required: &["DATA_ROOT"],
        optional: &["OLLAMA_HOST"],
    },
];

/// Tick data for the fast strand, which has no provider chosen yet, so there
/// is nothing to check for. Named rather than omitted, because a reader
/// deciding what to subscribe to needs to know it is outstanding.
const UNCHOSEN: &[(&str, &str)] = &[(
    "Tick and quote data",
    "no vendor chosen, and Alpaca's free tier is IEX only which is unusable for microstructure work",
)];

pub fn run(action: Option<&Action>) -> anyhow::Result<ExitCode> {
    match action {
        Some(Action::ProbeSep) => return probe::run(),
        Some(Action::ProbeActions) => return probe_actions::run(),
        Some(Action::FetchUniverse { out }) => return universe::run(out),
        None => {}
    }

    println!("Environment check. Presence only, values are never printed.");
    println!();

    for provider in PROVIDERS {
        report_provider(provider);
        println!();
    }

    println!("Not yet available from any provider");
    for (name, reason) in UNCHOSEN {
        println!("  {name}");
        println!("    {reason}");
    }
    println!();

    println!("This reads the process environment and does not load .env by itself.");
    println!("To bring a .env into the current shell:  set -a; source .env; set +a");

    // A report rather than a check, so it succeeds whatever it finds. Which
    // providers an operator needs depends on what they are doing, and a machine
    // configured for the slow strand alone is correctly configured.
    Ok(ExitCode::SUCCESS)
}

fn report_provider(provider: &Provider) {
    let ready = provider.required.iter().all(|name| is_present(name));
    let verdict = if ready {
        "configured"
    } else {
        "NOT configured"
    };

    println!("{}  [{verdict}]", provider.name);
    println!("  {}", provider.purpose);

    for name in provider.required {
        println!("    {:<24} required   {}", name, presence(name));
    }
    for name in provider.optional {
        println!("    {:<24} optional   {}", name, presence(name));
    }
}

fn presence(name: &str) -> &'static str {
    if is_present(name) {
        "present"
    } else {
        "missing or blank"
    }
}

/// Present means set to something non-empty.
///
/// `.env.example` ships every key blank, so a sourced but unfilled template
/// leaves variables set to the empty string. Treating that as configured would
/// send the operator off to debug an authentication failure instead.
///
/// `var_os` rather than `var` because a value that is not valid UTF-8 is still
/// a value, and this function's only question is whether one is there.
fn is_present(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}
