//! `ingest probe-sep`. What the price table actually ships around a split.
//!
//! # The question this exists to answer
//!
//! The price table gives an unadjusted **close** and nothing else unadjusted.
//! `open`, `high` and `low` are split-adjusted. So reconstructing the prices a
//! trade actually happened at means dividing each of them by the implied factor
//! `close / closeunadj`.
//!
//! That division is where the problem lives. On a split whose factor is not
//! exactly representable in base ten, the vendor's adjusted price already
//! carries rounding, and [`ingest::parquet`] refuses any value needing more
//! than scale 9 rather than rounding it further. So the question is empirical:
//!
//! > does `open / (close / closeunadj)` come back to a clean traded price, or
//! > to a repeating decimal?
//!
//! The probe set contains both regimes deliberately. A 20-for-1 split gives a
//! factor of 0.05, which is exact in base ten. A 3-for-1 gives a repeating
//! third, which is not.
//!
//! # What this command does not do
//!
//! It writes nothing, counts no trial, and draws no conclusion. Whether to
//! store what the vendor gives with open/high/low recorded as adjusted, or to
//! source traded prices elsewhere, is not a decision this file makes.

use std::process::ExitCode;

use ingest::sharadar::{SepRow, SharadarClient};
use jiff::civil::Date;
use rust_decimal::Decimal;

/// One security, one split, and the factor that applies before it.
struct Subject {
    ticker: &'static str,
    /// The first day the post-split shares traded.
    split_date: Date,
    /// New shares per old share. 1 for the control.
    ratio: u32,
    note: &'static str,
}

/// Chosen so both arithmetic regimes appear, and all inside the window the
/// free key serves.
const SUBJECTS: &[Subject] = &[
    Subject {
        ticker: "GOOG",
        split_date: Date::constant(2022, 7, 18),
        ratio: 20,
        note: "20-for-1, factor 0.05 is exact in base ten",
    },
    Subject {
        ticker: "AMZN",
        split_date: Date::constant(2022, 6, 6),
        ratio: 20,
        note: "20-for-1, factor 0.05 is exact in base ten",
    },
    Subject {
        ticker: "TSLA",
        split_date: Date::constant(2022, 8, 25),
        ratio: 3,
        note: "3-for-1, factor is a repeating third",
    },
    Subject {
        ticker: "MSFT",
        split_date: Date::constant(2022, 7, 18),
        ratio: 1,
        note: "no split, control",
    },
];

/// Calendar days either side of the split date. Calendar rather than trading
/// days, so a window spanning a weekend simply returns fewer rows.
const WINDOW_DAYS: i64 = 5;

/// The scale the curated Parquet codec refuses to exceed.
const CODEC_SCALE: u32 = 9;

pub fn run() -> anyhow::Result<ExitCode> {
    println!("Price probe, native API. Reads only, writes nothing, counts no trial.");
    println!("Reports what the vendor shipped. Draws no conclusion.");
    println!();

    let client = SharadarClient::native_from_env()?;

    // Requests are serial and deliberately unhurried. This host publishes no
    // rate limits, so the only safe assumption is that it has one.
    match client.native_earliest_date("TSLA")? {
        Some(earliest) => println!("Free-key window: earliest TSLA row served is {earliest}"),
        None => println!("Free-key window: no TSLA rows served at all"),
    }
    println!();

    for subject in SUBJECTS {
        report(&client, subject)?;
        println!();
    }

    println!("Legend");
    println!("  factor       close / closeunadj, as the two columns arrived");
    println!("  recon open   open / factor, the traded open this implies");
    println!("  exact        the division came out exact under Decimal arithmetic");
    println!("  scale        decimal places the reconstruction needs");
    println!("               (the curated Parquet codec refuses more than {CODEC_SCALE})");
    println!();
    println!("  Read `exact` together with `scale`, not on its own. A reconstruction wider");
    println!("  than Decimal's 28 significant digits has already been truncated to fit, so");
    println!("  the round-trip check saturates and stops discriminating: that is why rows at");
    println!("  the same large scale can disagree. The verdict line requires both exactness");
    println!("  and a scale the codec would accept, which is the question that matters.");

    Ok(ExitCode::SUCCESS)
}

fn report(client: &SharadarClient, subject: &Subject) -> anyhow::Result<()> {
    let from = shift(subject.split_date, -WINDOW_DAYS)?;
    let to = shift(subject.split_date, WINDOW_DAYS)?;

    println!(
        "{}  {}  split date {}  window {from} to {to}",
        subject.ticker, subject.note, subject.split_date
    );

    let rows = client.native_price_window(subject.ticker, from, to)?;

    println!(
        "  {:<12} {:>12} {:>12} {:>24} {:>18} {:>6} {:>6}",
        "date", "close", "closeunadj", "factor", "recon open", "exact", "scale"
    );

    let mut pre_split = 0usize;
    let mut pre_split_exact = 0usize;
    let mut worst_scale = 0u32;

    for row in &rows {
        let before_split = row.date < subject.split_date;

        // The factor as the two shipped columns imply it, not as the known
        // split ratio says it should be. The difference between those two is
        // the vendor's own rounding, and it is what makes this a measurement
        // rather than an arithmetic exercise.
        let factor = row.close.checked_div(row.close_unadjusted);

        // open / factor, rearranged to one division so the result is not
        // rounded twice.
        let numerator = row.open * row.close_unadjusted;
        let reconstructed = numerator.checked_div(row.close);

        let (exact, scale, shown) = match reconstructed {
            None => (false, 0, "close is zero".to_string()),
            Some(value) => {
                // A division that round-trips is exact; one that does not is a
                // repeating decimal that Decimal truncated to fit.
                let exact = value * row.close == numerator;
                let scale = value.normalize().scale();
                (exact, scale, value.normalize().to_string())
            }
        };

        if before_split {
            pre_split += 1;
            if exact && scale <= CODEC_SCALE {
                pre_split_exact += 1;
            }
            worst_scale = worst_scale.max(scale);
        }

        println!(
            "  {:<12} {:>12} {:>12} {:>24} {:>18} {:>6} {:>6}",
            row.date.to_string(),
            row.close.to_string(),
            row.close_unadjusted.to_string(),
            factor
                .map(|f| f.normalize().to_string())
                .unwrap_or_else(|| "-".to_string()),
            shown,
            if exact { "yes" } else { "NO" },
            scale,
        );
    }

    if pre_split == 0 {
        println!("  no pre-split rows in this window, so nothing to reconstruct");
    } else {
        println!(
            "  verdict: {pre_split_exact} of {pre_split} pre-split rows reconstruct exactly \
             within scale {CODEC_SCALE}; widest scale seen {worst_scale}"
        );
    }

    report_shipped_rounding(&rows, subject);
    Ok(())
}

/// Whether the vendor's own adjusted close is exact at the known split ratio.
///
/// Separate from the reconstruction question, and upstream of it. If
/// `close * ratio` does not reproduce `closeunadj`, the adjusted column was
/// already rounded before this code saw it, and no arithmetic here can undo
/// that.
fn report_shipped_rounding(rows: &[SepRow], subject: &Subject) {
    let ratio = Decimal::from(subject.ratio);
    let mut checked = 0usize;
    let mut exact = 0usize;
    let mut worst = Decimal::ZERO;

    for row in rows.iter().filter(|row| row.date < subject.split_date) {
        checked += 1;
        let residual = row.close_unadjusted - row.close * ratio;
        if residual.is_zero() {
            exact += 1;
        }
        worst = worst.max(residual.abs());
    }

    if checked > 0 {
        println!(
            "  shipped rounding: close x {} reproduces closeunadj on {exact} of {checked} \
             pre-split rows, largest residual {worst}",
            subject.ratio
        );
    }
}

fn shift(date: Date, days: i64) -> anyhow::Result<Date> {
    Ok(date.checked_add(jiff::SignedDuration::from_hours(days * 24))?)
}
