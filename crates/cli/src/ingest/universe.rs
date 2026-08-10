//! `ingest fetch-universe`. Build the research universe from the whole security
//! master and write it where a backtest can find it.
//!
//! # What this command is for
//!
//! It is the answer to "which securities may a result be about", and it has to
//! be produced by a rule rather than by a person, or every number downstream
//! inherits the survivorship bias of whoever wrote the list. The rule itself
//! lives in [`ingest::universe`]; this file fetches, applies it, reports, and
//! writes.
//!
//! # What it does not do
//!
//! It fetches no prices and counts no trial. Choosing a universe is a
//! configuration decision, and the trial that uses this file records the file's
//! hash so the decision travels with the result.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use ingest::provider::AdjustedPriceSource;
use ingest::sharadar::SharadarClient;
use ingest::universe::{self, COVERAGE_CUTOFF, SAMPLE_MODULUS, SAMPLE_RESIDUE};
use rust_decimal::Decimal;

pub fn run(out: &str) -> anyhow::Result<ExitCode> {
    println!("Building the research universe. Fetches no prices and counts no trial.");
    println!(
        "  sample rule:     permaticker % {SAMPLE_MODULUS} == {SAMPLE_RESIDUE}, applied to \
         every security the vendor has"
    );
    println!("  coverage cutoff: first price on or before {COVERAGE_CUTOFF}");
    println!("  delisted names:  kept, which is the reason this universe is defensible");
    println!();

    let client = SharadarClient::native_from_env()?;

    // Measured rather than assumed, and printed beside the constant so the two
    // can be compared. They are no longer the same thing: `COVERAGE_CUTOFF` is
    // frozen for reproducibility while the measured start moves with the
    // subscription, so a reader needs to see both rather than either alone.
    match client.earliest_available()? {
        Some(measured) => {
            println!("Measured window start: {measured}");
            if measured > COVERAGE_CUTOFF {
                println!(
                    "  NOTE: the served window now starts after the recorded cutoff \
                     {COVERAGE_CUTOFF}."
                );
                println!(
                    "  Securities selected on that cutoff may have less lead-in history than the"
                );
                println!("  cutoff implies. Re-deriving the cutoff is a decision, not a default.");
            }
        }
        None => println!("Measured window start: the vendor served nothing at all"),
    }
    println!();

    println!("Fetching the security master. Serial, paginated, one page at a time.");
    let master = client.native_ticker_master()?;
    println!("  {} securities with equity price coverage", master.len());

    let selected = universe::select(&master)?;
    let delisted = selected.iter().filter(|entry| entry.is_delisted).count();

    println!();
    println!("Universe");
    println!("  selected:  {} of {}", selected.len(), master.len());
    println!("  delisted:  {delisted} (kept)");
    println!("  still listed: {}", selected.len() - delisted);

    println!();
    report_listing_eras(&master)?;

    let text = universe::to_jsonl(&selected).context("serialising the universe")?;
    write_out(Path::new(out), &text)?;

    // The hash of the file, not of the entries, because the file is what a
    // trial's configuration will name and what a later reader will open.
    println!("  written:   {out}");
    println!("  sha256:    {}", rigor::hash_bytes(text.as_bytes()));

    if selected.is_empty() {
        anyhow::bail!(
            "the sample rule selected no securities at all, so there is no universe to fetch \
             prices for"
        );
    }

    Ok(ExitCode::SUCCESS)
}

/// Where the covered securities sit by listing era, against where the sample
/// took them from.
///
/// # The question
///
/// Permatickers appear to be issued in roughly increasing order over time, and
/// the sample rule is a modulo over them. A modulo is uniform within a dense
/// run of identifiers, but nothing guarantees the vendor issued them at a
/// steady rate, so the sample could be weighted towards one listing era rather
/// than being a cross-section of them all. The rate column is what answers it,
/// and it should sit near one in [`SAMPLE_MODULUS`] in every row.
fn report_listing_eras(master: &[ingest::sharadar::TickerRow]) -> anyhow::Result<()> {
    let counts = universe::first_price_year_counts(master)?;

    println!("First price date by listing year, covered against sampled");
    println!("  A modulo over a chronologically issued identifier could take one era more");
    println!("  heavily than another. Every rate below should sit near 1 in {SAMPLE_MODULUS}.");
    println!();
    println!(
        "  {:>6}  {:>9}  {:>8}  {:>8}",
        "year", "covered", "sampled", "rate"
    );

    let (mut covered_total, mut sampled_total) = (0usize, 0usize);
    for (year, year_counts) in &counts {
        covered_total += year_counts.eligible;
        sampled_total += year_counts.sampled;
        println!(
            "  {year:>6}  {:>9}  {:>8}  {:>8}",
            year_counts.eligible,
            year_counts.sampled,
            rate(year_counts.sampled, year_counts.eligible)
        );
    }

    println!(
        "  {:>6}  {:>9}  {:>8}  {:>8}",
        "all",
        covered_total,
        sampled_total,
        rate(sampled_total, covered_total)
    );
    println!(
        "  one in {SAMPLE_MODULUS} is {}",
        rate(1, SAMPLE_MODULUS as usize)
    );
    Ok(())
}

/// A percentage to one decimal place, through `Decimal` rather than `f64`.
///
/// Nothing decides on this number, but the project's rule against `f64` in
/// numeric paths is worth not carving an exception into for a display string.
fn rate(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "-".to_string();
    }
    let percent = Decimal::from(part) * Decimal::ONE_HUNDRED / Decimal::from(whole);
    format!("{percent:.1}%")
}

/// Write the file, creating the directory it lives in.
fn write_out(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(directory) = path
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
    {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}
