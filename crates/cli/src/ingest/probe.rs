//! `ingest probe-sep`. What SEP actually ships around a known split.
//!
//! # Why this exists rather than an argument
//!
//! SEP carries `close`, which is "adjusted for stock splits and stock
//! dividends", and `closeunadj`, which is "not adjusted for stock splits, stock
//! dividends, cash dividends or spinoffs". [`ingest::PriceBar`] stores prices
//! exactly as traded, so building one needs the unadjusted number.
//!
//! The open question is whether the traded price can be reconstructed exactly
//! from the adjusted one, or whether the vendor's adjustment rounds. That is a
//! fact about the vendor's pipeline, not something to settle by reasoning, and
//! the answer decides whether the price mapping can use `close` at all or has
//! to insist on `closeunadj`.
//!
//! # What this command does not do
//!
//! It writes nothing, to `data/` or anywhere else. It counts no trial, because
//! it is not a backtest. And it draws no conclusion: it prints what it saw and
//! stops. The mapping is specified from this output, elsewhere.
//!
//! Three tickers around dates that are public record, so nothing here depends
//! on privileged knowledge of the vendor's data.

use std::process::ExitCode;

use ingest::sharadar::{SepRow, SharadarClient};
use jiff::civil::Date;
use rust_decimal::Decimal;

/// One security, one date, and what the split was.
struct Subject {
    ticker: &'static str,
    /// The first day the post-split shares traded.
    split_date: Date,
    /// New shares per old share. 4 for a 4-for-1. 1 for the control.
    ratio: u32,
    note: &'static str,
}

/// Chosen because these splits are public record and easy to check by hand.
///
/// MSFT is the control: its last split was in 2003, so across a 2020 window
/// there is no adjustment to undo and `close` should already equal
/// `closeunadj`. A control matters because "the numbers agree" only means
/// something if there is a case where they are supposed to.
const SUBJECTS: &[Subject] = &[
    Subject {
        ticker: "AAPL",
        split_date: Date::constant(2020, 8, 31),
        ratio: 4,
        note: "4-for-1 split",
    },
    Subject {
        ticker: "TSLA",
        split_date: Date::constant(2022, 8, 25),
        ratio: 3,
        note: "3-for-1 split",
    },
    Subject {
        ticker: "MSFT",
        split_date: Date::constant(2020, 8, 31),
        ratio: 1,
        note: "no split, control",
    },
];

/// Calendar days fetched either side of the split date.
///
/// Calendar rather than trading days, so a window spanning a weekend returns
/// fewer rows than it asks for. That is fine, and preferable to this command
/// carrying a trading calendar it would otherwise have no use for.
const WINDOW_DAYS: i64 = 5;

pub fn run() -> anyhow::Result<ExitCode> {
    println!("SEP split-adjustment probe. Reads only, writes nothing, counts no trial.");
    println!("Reports what the vendor shipped. Draws no conclusion.");
    println!();

    let client = SharadarClient::from_env()?;

    for subject in SUBJECTS {
        report(&client, subject)?;
        println!();
    }

    println!("Legend");
    println!("  shipped ratio     closeunadj / close, as the two columns arrived");
    println!("  expected          the split factor that should apply on that date");
    println!("  exact             close * expected == closeunadj under Decimal arithmetic");
    println!("  residual          closeunadj - close * expected, zero when exact");

    Ok(ExitCode::SUCCESS)
}

fn report(client: &SharadarClient, subject: &Subject) -> anyhow::Result<()> {
    let from = shift(subject.split_date, -WINDOW_DAYS)?;
    let to = shift(subject.split_date, WINDOW_DAYS)?;

    println!(
        "{}  {}  split date {}  window {from} to {to}",
        subject.ticker, subject.note, subject.split_date
    );

    let rows = client.sep_window(subject.ticker, from, to)?;
    if rows.is_empty() {
        println!("  no rows returned for this window");
        return Ok(());
    }

    println!(
        "  {:<12} {:>14} {:>14} {:>22} {:>9} {:>6} {:>16}",
        "date", "close", "closeunadj", "shipped ratio", "expected", "exact", "residual"
    );

    let mut worst: Option<Decimal> = None;
    let mut exact_rows = 0usize;

    for row in &rows {
        // The vendor adjusts a row for every split after it, so only rows
        // before the split date carry this split's factor. On and after it,
        // there is nothing left to undo and the factor is 1.
        let expected = if row.date < subject.split_date {
            Decimal::from(subject.ratio)
        } else {
            Decimal::ONE
        };

        let reconstructed = row.close * expected;
        let residual = row.close_unadjusted - reconstructed;
        let is_exact = residual.is_zero();
        if is_exact {
            exact_rows += 1;
        }

        let magnitude = residual.abs();
        if worst.is_none_or(|current| magnitude > current) {
            worst = Some(magnitude);
        }

        let shipped = row
            .close_unadjusted
            .checked_div(row.close)
            .map(|ratio| ratio.normalize().to_string())
            .unwrap_or_else(|| "close is zero".to_string());

        println!(
            "  {:<12} {:>14} {:>14} {:>22} {:>9} {:>6} {:>16}",
            row.date.to_string(),
            row.close.to_string(),
            row.close_unadjusted.to_string(),
            shipped,
            expected.to_string(),
            if is_exact { "yes" } else { "NO" },
            residual.to_string(),
        );
    }

    let worst = worst.unwrap_or(Decimal::ZERO);
    if exact_rows == rows.len() {
        println!("  verdict: exact reconstruction on all {} rows", rows.len());
    } else {
        println!(
            "  verdict: rounded. {} of {} rows reconstruct exactly, largest error {worst}",
            exact_rows,
            rows.len()
        );
    }

    if subject.ratio == 1 {
        report_control(&rows);
    }

    Ok(())
}

/// On a name with no split in the window, every price column is unadjusted, so
/// the spec asks whether each of open/high/low/close equals `closeunadj`.
///
/// Reported per column rather than as one verdict, because `close` matching
/// while `open` does not is a different finding from nothing matching at all.
fn report_control(rows: &[SepRow]) {
    let mut matching = [0usize; 4];
    for row in rows {
        for (slot, value) in [row.open, row.high, row.low, row.close]
            .into_iter()
            .enumerate()
        {
            if value == row.close_unadjusted {
                matching[slot] += 1;
            }
        }
    }

    let counts = ["open", "high", "low", "close"]
        .into_iter()
        .zip(matching)
        .map(|(name, count)| format!("{name} {count}/{}", rows.len()))
        .collect::<Vec<_>>()
        .join("  ");
    println!("  control, columns equal to closeunadj:  {counts}");
}

fn shift(date: Date, days: i64) -> anyhow::Result<Date> {
    Ok(date.checked_add(jiff::SignedDuration::from_hours(days * 24))?)
}
