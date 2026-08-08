//! Reconstruct a LOBSTER session from its message file and diff every row
//! against LOBSTER's own reconstructed book.
//!
//! Usage:
//!   cargo run --release --example lobster_validate -- <message.csv> <orderbook.csv> [max_rows]
//!
//! The session date is read from the filename, which LOBSTER formats as
//! `TICKER_YYYY-MM-DD_start_end_message_LEVEL.csv`.

use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::process::ExitCode;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use marketdata::event::Side;
use marketdata::lobster::{Message, Reconstructor, RowOutcome, Snapshot, Validator};

fn main() -> ExitCode {
    match run() {
        Ok(clean) if clean => ExitCode::SUCCESS,
        Ok(_) => {
            eprintln!("\nvalidation finished with divergence at the inside of the book");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("lobster_validate: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<bool, Box<dyn Error>> {
    const USAGE: &str =
        "usage: lobster_validate <message.csv> <orderbook.csv> [max_rows] [session]";

    let mut positional = Vec::new();
    // `session` selects continuous replay, which is a diagnostic of how fast
    // the file's information runs out rather than a test of the book. Isolated
    // stepping is the default because it is the only mode that can fail for a
    // reason we are responsible for.
    let mut isolated = true;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "session" => isolated = false,
            "step" => isolated = true,
            _ => positional.push(argument),
        }
    }

    let mut positional = positional.into_iter();
    let message_path = positional.next().ok_or(USAGE)?;
    let book_path = positional.next().ok_or(USAGE)?;
    let max_rows: usize = match positional.next() {
        Some(value) => value.parse()?,
        None => usize::MAX,
    };

    let midnight = midnight_epoch_nanos(&message_path)?;

    let mut messages = BufReader::new(File::open(&message_path)?).lines();
    let mut books = BufReader::new(File::open(&book_path)?).lines();

    // Row 1 is the state after message 1, so it is the earliest snapshot we can
    // start from. Seeding here rather than from an empty book is what makes the
    // pre-open liquidity problem tractable. See Reconstructor::seeded.
    let first_message = messages.next().ok_or("message file is empty")??;
    let first_book = books.next().ok_or("orderbook file is empty")??;
    let seed = Snapshot::parse(&first_book)?;
    let seed_time = Message::parse(&first_message, midnight)?.time;

    // The published depth comes from the raw column count, not from the
    // stripped snapshot, because a thin first row would otherwise under-report
    // it and truncate the book too aggressively for the rest of the session.
    let levels = (first_book.split(',').count() / 4).max(1);
    let mut validator = Validator::new(
        Reconstructor::seeded(&seed, seed_time)?.with_max_depth(levels),
        levels,
    );

    println!(
        "seeded {levels} levels a side from row 1 at {}",
        seed_time.0
    );

    println!(
        "mode {}",
        if isolated {
            "step, each message applied to a book seeded from the row before it"
        } else {
            "session, continuous replay, diagnostic only"
        }
    );

    let mut previous = seed.clone();
    let mut moving_rows = 0u64;
    let mut first_divergence: Option<(u64, Side, usize)> = None;
    let mut first_inside: Option<u64> = None;
    let mut inside_divergences = 0u64;
    let mut deepest_clean_prefix = levels;

    for (index, pair) in messages.zip(books).take(max_rows).enumerate() {
        let (message_line, book_line) = pair;
        let row = index as u64 + 2; // row 1 was the seed

        let message = Message::parse(&message_line?, midnight)
            .map_err(|error| format!("message row {row}: {error}"))?;
        let reference =
            Snapshot::parse(&book_line?).map_err(|error| format!("book row {row}: {error}"))?;

        let outcome = if isolated {
            validator.step_isolated(&previous, &message, &reference)
        } else {
            validator.step(&message, &reference)
        };

        match outcome.map_err(|error| format!("row {row}: {error}"))? {
            RowOutcome::Match => {}
            RowOutcome::Divergent { side, level } => {
                deepest_clean_prefix = deepest_clean_prefix.min(level);
                if level == 0 {
                    inside_divergences += 1;
                    // The inside is the number that decides whether the
                    // reconstruction is usable at all, so it gets its own
                    // report rather than being buried in a depth histogram.
                    if first_inside.is_none() {
                        first_inside = Some(row);
                        report_divergence(&validator, &reference, row, side, level, "inside");
                    }
                }
                if first_divergence.is_none() {
                    first_divergence = Some((row, side, level));
                    report_divergence(&validator, &reference, row, side, level, "any level");
                }
            }
        }

        // A row where LOBSTER's own book did not move is a weak assertion. The
        // message landed outside the published depth, so both books correctly
        // do nothing and the row would pass even if the message were misread.
        // Counting these separately keeps the headline number honest.
        if reference != previous {
            moving_rows += 1;
        }
        previous = reference;
    }

    println!(
        "\nrows where LOBSTER's book actually moved {moving_rows} ({:.2}% of compared)",
        100.0 * moving_rows as f64 / validator.rows.max(1) as f64
    );

    report(
        &validator,
        first_divergence,
        inside_divergences,
        deepest_clean_prefix,
    );

    // The pass condition is the price-keyed one. Rank agreement cannot be
    // required, because LOBSTER sees levels the message file never mentions and
    // no amount of correct bookkeeping recovers them. A price both books hold
    // at different sizes has no such excuse.
    Ok(validator.totals.size_mismatches == 0 && validator.crossed_rows == 0)
}

/// LOBSTER timestamps are seconds after local midnight in New York. Converting
/// through a real time zone rather than a fixed offset is what keeps this right
/// across daylight saving, and the crate's Nanos type is documented as epoch
/// nanoseconds, so leaving them as seconds-after-midnight would be a silent
/// unit error.
fn midnight_epoch_nanos(path: &str) -> Result<i64, Box<dyn Error>> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let date_field = name
        .split('_')
        .nth(1)
        .ok_or("filename does not carry a TICKER_YYYY-MM-DD_ prefix")?;
    let date: Date = date_field.parse()?;
    let zoned = date.to_zoned(TimeZone::get("America/New_York")?)?;
    Ok(i64::try_from(zoned.timestamp().as_nanosecond())?)
}

fn report_divergence(
    validator: &Validator,
    reference: &Snapshot,
    row: u64,
    side: Side,
    level: usize,
    label: &str,
) {
    let ours = validator.book().depth(side, level + 3);
    let theirs = match side {
        Side::Ask => &reference.asks,
        Side::Bid => &reference.bids,
    };
    println!("\nfirst divergence ({label}) at row {row}, {side:?} level {level}");
    for index in 0..(level + 3) {
        let ours = ours
            .get(index)
            .map(|(price, size)| format!("{} x{size}", price.to_decimal().round_dp(4)));
        let theirs = theirs
            .get(index)
            .map(|(price, size)| format!("{} x{size}", price.to_decimal().round_dp(4)));
        let marker = if index == level { "<-" } else { "  " };
        println!(
            "  {marker} level {index}: ours {:<22} lobster {}",
            ours.unwrap_or_else(|| "-".into()),
            theirs.unwrap_or_else(|| "-".into()),
        );
    }
    println!();
}

fn report(
    validator: &Validator,
    first_divergence: Option<(u64, Side, usize)>,
    inside_divergences: u64,
    deepest_clean_prefix: usize,
) {
    let rows = validator.rows.max(1);
    let reconstructor = validator.reconstructor();

    let totals = validator.totals;
    let compared = (totals.matched + totals.size_mismatches).max(1);

    println!("\nrows compared        {}", validator.rows);
    println!("crossed books        {}", validator.crossed_rows);
    println!("hidden executions    {}", reconstructor.hidden_executions);
    println!("unseen removals      {}", reconstructor.unseen_removals);
    println!("halts                {}", reconstructor.halts);

    // The verdict. A price both books hold at different sizes is the only
    // observation here that our own bookkeeping has to answer for.
    println!("\nprice-keyed comparison, the one that judges the reconstruction");
    println!("  levels agreed      {}", totals.matched);
    println!(
        "  SIZE MISMATCHES    {} ({:.6}% of shared levels)",
        totals.size_mismatches,
        100.0 * totals.size_mismatches as f64 / compared as f64
    );
    match validator.first_size_mismatch {
        Some(mismatch) => println!(
            "  first at row {}, {:?} {} ours x{} lobster x{}",
            mismatch.row,
            mismatch.side,
            mismatch.price.to_decimal().round_dp(4),
            mismatch.ours,
            mismatch.theirs
        ),
        None => println!("  first mismatch     none"),
    }
    println!(
        "  levels we lack     {} (submitted outside the level-10 range, unknowable)",
        totals.missing
    );
    println!(
        "  levels only we had {} (inside their reported window)",
        totals.phantom
    );

    println!("\nrank-for-rank comparison, informative but cascades on one missing level");
    println!(
        "  identical rows     {} ({:.4}%)",
        validator.full_matches,
        100.0 * validator.full_matches as f64 / rows as f64
    );
    println!(
        "  inside disagrees   {inside_divergences} ({:.4}%)",
        100.0 * inside_divergences as f64 / rows as f64
    );
    match first_divergence {
        Some((row, side, level)) => {
            println!("  first divergence   row {row}, {side:?} level {level}")
        }
        None => println!("  first divergence   none"),
    }
    println!("  clean prefix       {deepest_clean_prefix} levels on every row");

    println!("\nper-level agreement, level 0 is the inside of the book");
    for level in 0..validator.ask_level_matches.len() {
        let ask = validator.ask_level_matches[level];
        let bid = validator.bid_level_matches[level];
        println!(
            "  level {level:<2} ask {:>9.5}%   bid {:>9.5}%",
            100.0 * ask as f64 / rows as f64,
            100.0 * bid as f64 / rows as f64,
        );
    }
}
