//! `ingest probe-daily`. What the daily metrics table actually ships.
//!
//! # The question this exists to answer
//!
//! Three planned consumers need a per-name daily market capitalisation, the
//! *market cap* being share price times shares outstanding, which is what "how
//! big is this company" means when weighting a portfolio by size. None of them
//! is built here. This command measures the table so a decoder can be written
//! against evidence rather than against the vendor's documentation.
//!
//! Six things have to be measured rather than assumed:
//!
//! 1. **The table name.** A wrong-case *known* name is answered with
//!    `{"count":0,"data":[]}` and HTTP 200, so a typo reads as "this security
//!    has no data". An entirely unknown name is answered with HTTP 401, which
//!    reads as a credential problem while the credential is fine. Neither
//!    failure names the real fault, so the credential is proven against a table
//!    already known to work *before* any candidate is tried, and only then is a
//!    401 read as "no table by that name".
//! 2. **The field names**, and whether a `fields` parameter trims the row.
//! 3. **The units.** A figure of 3.8e12 and a figure of 3.8e6 are the same
//!    market cap in different units, and nothing in the response says which.
//!    The question is settled by dividing the shipped figure by the same day's
//!    unadjusted close and seeing which interpretation lands on a share count
//!    the public record already knows. Six orders of magnitude separate the two
//!    answers, so this survives the reference figures below being a few percent
//!    stale.
//! 4. **The share-count derivation**, `marketcap / closeunadj`, checked here
//!    once before anything is built that depends on it. `closeunadj` is the
//!    close *before* split adjustment, which is the price the shares actually
//!    traded at, so it is the only close that divides a market cap into the
//!    share count that existed on the day.
//! 5. **Null and zero behaviour**, market-wide and at the end of a delisted
//!    name's life. This decides whether the curated writer refuses a null or
//!    never sees one.
//! 6. **How far back the table reaches**, against the price window the curated
//!    prices already cover.
//!
//! # What this command does not do
//!
//! It writes nothing, counts no trial, and draws no conclusion. Its whole
//! output is what the vendor sent, plus arithmetic on it that a reader would
//! otherwise do by hand. Which spelling goes into `native_tables`, which units
//! label is stored, and what the null policy is, are decided from this output
//! rather than here.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use ingest::sharadar::{NATIVE_STOCKS_TABLE, SharadarClient};
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::Value;

use super::probe_wire::{ask, ask_candidate, decimal_field, field, print_rows, rows_of, shift};

/// Candidate spellings of the daily metrics table, most likely first.
///
/// Candidates rather than a constant precisely because a wrong name here fails
/// as an empty success or as a credential rejection, and never as "no such
/// table". The research file lists the vendor's code for this table as DAILY,
/// but the native host uses semantic words rather than codes: it answers to
/// `stocks` where the code is SEP, and to `fundamentals` where the code is SF1.
/// So the code is a hint about the first candidate and nothing more.
///
/// `metrics` is in the list and is *not* expected to win. The vendor documents
/// a separate METRICS table of price-based metrics, so a hit there would be a
/// different table that happens to answer. It is asked anyway because knowing
/// which of the two carries a market cap is exactly the confusion this probe
/// exists to prevent.
const CANDIDATE_TABLES: &[&str] = &["daily", "dailymetrics", "metrics", "dailymetric"];

/// The ticker the table-name candidates are tried against.
///
/// A company with continuous coverage for decades, so an empty answer is a
/// statement about the request rather than about the company.
const CANDIDATE_TICKER: &str = "AAPL";

/// The field name the research reports for the market cap column.
///
/// A hypothesis, used for one thing only: choosing which candidate table the
/// deeper questions run against when more than one answers. Every candidate's
/// full field list is printed either way, so a different spelling shows up in
/// the output rather than being silently missed. Nothing downstream of this
/// file takes the name from here.
const EXPECTED_CAP_FIELD: &str = "marketcap";

/// The security whose price window is measured to prove the credential.
///
/// Microsoft has traded since 1986, early enough that anything measured is the
/// key's window talking rather than the company's listing date.
const REACH_TICKER: &str = "MSFT";

/// A date the market cap question is asked on, and what the public record says.
///
/// `shares` is deliberately written as an approximation. It is a reference
/// figure quoted from a filing cover page, not vendor data and not a target to
/// match to the digit. The two unit interpretations it discriminates between
/// differ by a factor of a million, so a reference that is a few percent stale
/// still separates them completely, and a reader who wants precision should
/// check the filing rather than this file.
struct UnitSubject {
    ticker: &'static str,
    date: Date,
    shares: &'static str,
    note: &'static str,
}

/// Two names rather than one, so an odd security cannot be mistaken for a unit
/// convention. If both land on the same scale, the scale is the table's.
const UNIT_SUBJECTS: &[UnitSubject] = &[
    UnitSubject {
        ticker: "AAPL",
        date: Date::constant(2024, 12, 31),
        shares: "approximately 15.1 billion, per the Apple FY2024 Form 10-K cover page, \
                 stated as of 18 October 2024",
        note: "a mega-cap on the last trading day of 2024, market cap widely reported \
               near 3.8 trillion dollars",
    },
    UnitSubject {
        ticker: "MSFT",
        date: Date::constant(2024, 12, 31),
        shares: "approximately 7.4 billion, per the Microsoft FY2025 Form 10-K cover page",
        note: "a second mega-cap on the same date, market cap widely reported near \
               3.1 trillion dollars",
    },
    UnitSubject {
        ticker: "ETN",
        date: Date::constant(2024, 12, 31),
        shares: "approximately 390 million, an ordinary share count rather than an ADR line",
        note: "the one subject inside the sampled research universe, so the curated file \
               on disk answers as well as the live column and the two can be compared. \
               That comparison is the point here; the share count is the weaker check of \
               the three because this is not a name the reader knows by heart",
    },
];

/// Securities whose listing ended inside the key's window.
///
/// What a delisted name shows in its final days decides whether the curated
/// dataset ever sees a null, a zero, or simply no row at all. All three are
/// plausible and they need different handling: a missing row is absence, a zero
/// is a claim that the company was worthless.
struct DelistedSubject {
    ticker: &'static str,
    /// Roughly the last day the shares traded, from the public record.
    last_traded: Date,
    note: &'static str,
}

/// Four endings rather than one, because they fail in different places.
///
/// The middle two are taken from this key's own served universe rather than
/// from memory, which is how the last one earned its place: it was picked by
/// hand as a famous bank failure and turns out not to exist on this
/// subscription at all, on either table. A subject that is absent for a reason
/// unrelated to the question would have been read as an answer to it.
const DELISTED_SUBJECTS: &[DelistedSubject] = &[
    DelistedSubject {
        ticker: "TWTR",
        last_traded: Date::constant(2022, 10, 27),
        note: "taken private, so the listing ended with the company solvent",
    },
    DelistedSubject {
        ticker: "SNBRQ",
        last_traded: Date::constant(2026, 6, 22),
        note: "the Q suffix is the convention for a company in bankruptcy proceedings, \
               so this is where a zero or a null would show up if either ever does",
    },
    DelistedSubject {
        ticker: "CUTC1",
        last_traded: Date::constant(1998, 3, 30),
        note: "served with prices but delisted before this table's history begins, so an \
               empty answer here is expected and is the shape the fetch must tolerate",
    },
    DelistedSubject {
        ticker: "FRC",
        last_traded: Date::constant(2023, 4, 28),
        note: "a bank seized by regulators, named from memory rather than from the \
               master, and the control on that difference",
    },
];

/// Calendar days either side of a subject date.
const WINDOW_DAYS: i64 = 10;

/// Windows the market-wide null census samples, and its per-window row cap.
///
/// Separate short windows in different decades rather than one long one.
/// Measured 2026-08-10 on the actions table: a capped response is the *newest*
/// rows in the window, so asking for a whole month returns the last days of it
/// and the rest buys nothing. The earliest window doubles as a reading on how
/// populated this table was when its history began.
const CENSUS_WINDOWS: &[(Date, Date)] = &[
    (Date::constant(2024, 6, 24), Date::constant(2024, 6, 28)),
    (Date::constant(2010, 3, 25), Date::constant(2010, 3, 31)),
    (Date::constant(1999, 6, 24), Date::constant(1999, 6, 30)),
];

/// Rows one census window may return.
const CENSUS_LIMIT: usize = 500;

/// Rows one windowed request may return.
///
/// A cap rather than a page size. If `from` and `to` are not honoured on this
/// table, an unbounded request would pull a security's entire daily history,
/// and a probe that floods its own output has hidden the thing it went to
/// measure. Every subject window here holds well under this if the filter
/// works, so a response of exactly this many rows is itself the signal that it
/// does not.
const WINDOW_LIMIT: usize = 50;

pub fn run(data_root: Option<&str>) -> anyhow::Result<ExitCode> {
    println!("Daily metrics probe, native API. Reads only, writes nothing, counts no trial.");
    println!("Reports what the vendor shipped. Draws no conclusion.");
    println!();

    let client = SharadarClient::native_from_env()?;

    // Requests are serial and deliberately unhurried. This host publishes no
    // rate limits, so the only safe assumption is that it has one.
    let stocks_reach = prove_credential(&client)?;
    println!();

    let Some(table) = discover_table(&client)? else {
        println!("No candidate spelling returned rows. Either none of them names the daily");
        println!("metrics table, or this key is not served it at all. Nothing below can run.");
        return Ok(ExitCode::FAILURE);
    };

    println!("Table used for everything below: {table}");
    println!();

    report_fields_parameter(&client, table)?;
    println!();

    for subject in UNIT_SUBJECTS {
        report_units(&client, table, subject, data_root)?;
        println!();
    }

    report_null_census(&client, table)?;
    println!();

    for subject in DELISTED_SUBJECTS {
        report_delisted(&client, table, subject)?;
        println!();
    }

    report_reach(&client, table, stocks_reach)?;
    println!();

    println!("Legend");
    println!("  raw            the response body exactly as the host sent it");
    println!("  rows           the same body's `data` array, one line per row, every field");
    println!("  derived shares the shipped market cap divided by that day's closeunadj,");
    println!("                 computed once per candidate unit interpretation");
    println!();
    println!("  The unit question is decided by which derived share count lands on the public");
    println!("  record. The two candidates differ by a factor of a million, so the reference");
    println!("  figures being slightly stale cannot change which one is right.");
    println!();
    println!("  Nothing here is stored and nothing here is a verdict. Store-as-shipped means");
    println!("  the arithmetic above never enters the codec: the label the mapping settles on");
    println!("  records which units the stored figure is in, and the figure itself is written");
    println!("  exactly as the vendor sent it.");

    Ok(ExitCode::SUCCESS)
}

/// Prove the credential against a table already known to work.
///
/// This has to come first and its failure has to be fatal. Every candidate
/// below reads a 401 as "no table by that name", and that reading is only sound
/// while the key is known good. Without this request a rejected credential
/// would print as four tables that do not exist.
///
/// It answers half of question 6 in the same round trip, because the reach of
/// the daily table is only interesting next to the reach of the price table.
fn prove_credential(client: &SharadarClient) -> anyhow::Result<Option<Date>> {
    println!("Question 0. Is this key good, asked of a table whose name is already known.");
    println!("Everything below reads a 401 as an unknown table name, which is only honest");
    println!("once the credential itself has been shown to work.");

    let earliest = client.native_earliest_date(REACH_TICKER)?;
    match earliest {
        Some(date) => println!(
            "  credential accepted. Earliest {REACH_TICKER} row on {NATIVE_STOCKS_TABLE}: {date}"
        ),
        None => println!(
            "  credential accepted, but no {REACH_TICKER} rows on {NATIVE_STOCKS_TABLE} at all"
        ),
    }
    Ok(earliest)
}

/// Ask every candidate spelling, and report all of them.
///
/// Every candidate is asked rather than stopping at the first hit, which is the
/// opposite of what the actions probe does. The difference is that two of these
/// names could both answer: the vendor documents a DAILY table and a separate
/// METRICS table, and a market cap column may live on one, the other, or both.
/// Stopping early would answer "a table exists" when the question is "which
/// table carries the column".
///
/// The winner is the first candidate whose rows carry the expected market cap
/// field, falling back to the first candidate with any rows at all. Which rule
/// fired is printed, because the fallback means the sections below are running
/// against a table whose column was not found where it was expected.
fn discover_table(client: &SharadarClient) -> anyhow::Result<Option<&'static str>> {
    println!("Question 1. Which spelling names the daily metrics table.");
    println!("A wrong-case known name is answered with an empty success and an unknown name");
    println!("with a 401, so the ticker asked for is one with decades of coverage:");
    println!("{CANDIDATE_TICKER}. An empty answer is then a statement about the name.");
    println!();

    let mut with_rows: Option<&'static str> = None;
    let mut with_field: Option<&'static str> = None;

    for candidate in CANDIDATE_TABLES {
        let params = [
            ("ticker", CANDIDATE_TICKER.to_string()),
            ("limit", "1".to_string()),
        ];
        let rows = match ask_candidate(client, candidate, &params)? {
            Some(body) => rows_of(&body),
            None => Vec::new(),
        };

        println!("  verdict: {} row(s)", rows.len());
        for row in &rows {
            match row.as_object() {
                Some(fields) => {
                    let names: Vec<&str> = fields.keys().map(String::as_str).collect();
                    println!("  fields present: {}", names.join(", "));
                }
                None => println!("  a row that is not an object: {row}"),
            }
        }

        if !rows.is_empty() {
            with_rows = with_rows.or(Some(candidate));
            if rows.iter().any(|row| row.get(EXPECTED_CAP_FIELD).is_some()) {
                with_field = with_field.or(Some(candidate));
            }
        }
        println!();
    }

    match (with_field, with_rows) {
        (Some(table), _) => {
            println!(
                "  chosen: {table}, the first candidate carrying a {EXPECTED_CAP_FIELD} field"
            );
            Ok(Some(table))
        }
        (None, Some(table)) => {
            println!(
                "  chosen: {table}, the first candidate returning rows. NOTE: no candidate \
                 carried a field named {EXPECTED_CAP_FIELD:?}, so the column is spelled \
                 differently or lives elsewhere. Read the field lists above before trusting \
                 anything below"
            );
            Ok(Some(table))
        }
        (None, None) => Ok(None),
    }
}

/// Whether the host trims a row to the fields asked for, or ignores the ask.
///
/// Two requests for the same row, one with a `fields` parameter and one
/// without. A host that honours it answers with three keys, one that ignores it
/// answers with everything it has. Both bodies are printed, because the full
/// row is also the answer to question 2's first half.
fn report_fields_parameter(client: &SharadarClient, table: &str) -> anyhow::Result<()> {
    println!("Question 2. The field names, and whether `fields` trims the row.");

    let date = UNIT_SUBJECTS[0].date;
    let base = [
        ("ticker", UNIT_SUBJECTS[0].ticker.to_string()),
        ("from", date.to_string()),
        ("to", date.to_string()),
        ("limit", WINDOW_LIMIT.to_string()),
    ];

    println!("  first, with no `fields` parameter, so the host sends every column it has");
    let full = ask(client, table, &base, true)?.map(|body| rows_of(&body));
    if let Some(rows) = &full {
        print_rows(rows);
    }

    println!("  second, the same row asking for only ticker, date and {EXPECTED_CAP_FIELD}");
    let mut trimmed_params: Vec<(&str, String)> = base.to_vec();
    trimmed_params.push(("fields", format!("ticker,date,{EXPECTED_CAP_FIELD}")));
    let trimmed = ask(client, table, &trimmed_params, true)?.map(|body| rows_of(&body));
    if let Some(rows) = &trimmed {
        print_rows(rows);
    }

    println!(
        "  key count, unfiltered then filtered: {} then {}",
        key_count(full.as_deref()),
        key_count(trimmed.as_deref())
    );
    Ok(())
}

/// Keys on the first row of a response, as text, for the trimming comparison.
fn key_count(rows: Option<&[Value]>) -> String {
    match rows.and_then(<[Value]>::first).and_then(Value::as_object) {
        Some(fields) => format!("{} key(s)", fields.len()),
        None => "no row".to_string(),
    }
}

/// The units of the shipped figure, and the share-count derivation on it.
///
/// Questions 3 and 4 together, because they are the same division read two
/// ways. The unadjusted close is taken from two independent places, the curated
/// file already on disk and the vendor's own price table, so a disagreement
/// between them is visible rather than assumed away. The curated file is the
/// one the derivation will actually use in production.
fn report_units(
    client: &SharadarClient,
    table: &str,
    subject: &UnitSubject,
    data_root: Option<&str>,
) -> anyhow::Result<()> {
    println!("Questions 3 and 4. The units, and the share count they imply.");
    println!("{} on {}. {}", subject.ticker, subject.date, subject.note);

    let params = [
        ("ticker", subject.ticker.to_string()),
        ("from", subject.date.to_string()),
        ("to", subject.date.to_string()),
        ("limit", WINDOW_LIMIT.to_string()),
    ];
    let Some(body) = ask(client, table, &params, true)? else {
        return Ok(());
    };
    let rows = rows_of(&body);
    print_rows(&rows);

    let Some(row) = rows.first() else {
        println!("  no row on that date, so there is no figure to read units off");
        return Ok(());
    };

    // The shipped token before any parsing, so the spelling the vendor used is
    // in the output even if Decimal cannot read it.
    let shipped_token = field(row, EXPECTED_CAP_FIELD)
        .unwrap_or_else(|| format!("<no {EXPECTED_CAP_FIELD} field on this row>"));
    println!("  shipped {EXPECTED_CAP_FIELD} token: {shipped_token}");

    let live = live_close_unadjusted(client, subject.ticker, subject.date)?;
    let curated = curated_close_unadjusted(subject.ticker, subject.date, data_root);
    report_close(" closeunadj, live stocks table   ", live);
    report_close(" closeunadj, curated file on disk", curated);

    let Some(cap) = decimal_field(row, EXPECTED_CAP_FIELD) else {
        println!("  the shipped figure did not parse as a Decimal, so no arithmetic follows");
        return Ok(());
    };
    // The curated file is what the production derivation would read, so it is
    // preferred here. Falling back to the live column keeps the arithmetic
    // available on a machine whose curated prices do not cover this name.
    let Some(close) = curated.or(live) else {
        println!("  no unadjusted close from either source, so the derivation cannot be run");
        return Ok(());
    };

    let millions = Decimal::from(1_000_000);
    println!(
        "  derived shares if the figure is whole dollars       {}",
        quotient(cap, close)
    );
    println!(
        "  derived shares if the figure is millions of dollars {}",
        cap.checked_mul(millions)
            .map(|scaled| quotient(scaled, close))
            .unwrap_or_else(|| "overflow".to_string())
    );
    println!("  public record, approximate: {}", subject.shares);
    Ok(())
}

fn report_close(label: &str, close: Option<Decimal>) {
    match close {
        Some(value) => println!("  {label} {value}"),
        None => println!("  {label} not available"),
    }
}

/// `numerator / denominator`, normalised, or a dash where it cannot be divided.
///
/// Display only. `Decimal` throughout, and nothing computed here decides
/// anything: the whole point of the round is that the stored figure is written
/// as shipped and never rescaled by arithmetic like this.
fn quotient(numerator: Decimal, denominator: Decimal) -> String {
    match numerator.checked_div(denominator) {
        None => "-".to_string(),
        Some(value) => value.normalize().to_string(),
    }
}

/// The unadjusted close for one name and date, from the vendor's price table.
fn live_close_unadjusted(
    client: &SharadarClient,
    ticker: &str,
    date: Date,
) -> anyhow::Result<Option<Decimal>> {
    let params = [
        ("ticker", ticker.to_string()),
        ("from", date.to_string()),
        ("to", date.to_string()),
        ("fields", "ticker,date,close,closeunadj".to_string()),
    ];
    let Some(body) = ask(client, NATIVE_STOCKS_TABLE, &params, true)? else {
        return Ok(None);
    };
    Ok(rows_of(&body)
        .first()
        .and_then(|row| decimal_field(row, "closeunadj")))
}

/// The unadjusted close for one name and date, from the curated file on disk.
///
/// Absence is not an error. The curated prices cover the sampled research
/// universe over a fetched window, so a name outside it or a machine that has
/// not run the fetch simply has nothing to say here, and the live column
/// answers the same question.
fn curated_close_unadjusted(ticker: &str, date: Date, data_root: Option<&str>) -> Option<Decimal> {
    let root = ingest::parquet::data_root(data_root);
    let path = ingest::parquet::prices_path(&root);
    if !Path::new(&path).exists() {
        println!("  curated prices not present at {}", path.display());
        return None;
    }
    match ingest::parquet::read_prices(&path) {
        Err(error) => {
            println!(
                "  curated prices at {} did not read: {error}",
                path.display()
            );
            None
        }
        Ok(bars) => bars
            .iter()
            .find(|bar| bar.asset.ticker == ticker && bar.date == date)
            .map(|bar| bar.close_unadjusted),
    }
}

/// How the table spells "no market cap", market-wide.
///
/// Four hand-picked securities show what those four happened to have, and a
/// state nobody in the sample reached is invisible. This asks whole days across
/// every ticker instead, which is the cheapest question that can see a null.
///
/// The row dump is suppressed because it would run to hundreds of rows. What is
/// printed instead is the census and one whole row per state, which is the
/// evidence a null policy needs.
fn report_null_census(client: &SharadarClient, table: &str) -> anyhow::Result<()> {
    println!("Question 5, first half. Whether rows carry a missing or non-positive figure.");

    let mut census: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut example: BTreeMap<&'static str, String> = BTreeMap::new();

    for (from, to) in CENSUS_WINDOWS {
        let params = [
            ("from", from.to_string()),
            ("to", to.to_string()),
            ("limit", CENSUS_LIMIT.to_string()),
        ];
        let Some(body) = ask(client, table, &params, false)? else {
            continue;
        };
        let rows = rows_of(&body);

        // How much of the window the sample actually reached. A capped response
        // is the newest rows in it, so the span printed here is what the census
        // is a census of, which is not the window asked for.
        let dates: Vec<String> = rows.iter().filter_map(|row| field(row, "date")).collect();
        let span = match (dates.iter().min(), dates.iter().max()) {
            (Some(earliest), Some(latest)) => format!("{earliest} to {latest}"),
            _ => "no date field on any row".to_string(),
        };
        println!("  rows: {}, dates present: {span}", rows.len());
        if rows.len() == CENSUS_LIMIT {
            println!(
                "  NOTE: exactly {CENSUS_LIMIT} rows came back, which is the cap, so this is a \
                 prefix of the window. A state can be missing from a prefix."
            );
        }

        for row in &rows {
            let state = classify(row);
            *census.entry(state).or_default() += 1;
            example.entry(state).or_insert_with(|| row.to_string());
        }
    }

    println!("  census over every window above");
    for (state, count) in &census {
        println!("  {count:>4}  {state}");
    }
    println!("  one whole row per state, as the host sent it");
    for (state, row) in &example {
        println!("    {state}: {row}");
    }
    Ok(())
}

/// Which of the states a curated writer would have to handle this row is in.
///
/// Absent, null, and zero are three different claims and they are separated
/// here rather than merged into "no data". A missing row says nothing was
/// recorded, a null says the vendor recorded that it does not know, and a zero
/// says the vendor recorded that the company is worth nothing.
fn classify(row: &Value) -> &'static str {
    match row.get(EXPECTED_CAP_FIELD) {
        None => "field absent from the row",
        Some(Value::Null) => "null",
        Some(value) => match Decimal::from_str_exact(&super::probe_wire::scalar(value)) {
            Err(_) => "present but not parseable as a decimal",
            Ok(cap) if cap.is_zero() => "zero",
            Ok(cap) if cap.is_sign_negative() => "negative",
            Ok(_) => "positive",
        },
    }
}

/// What a name shows on its way off the exchange.
///
/// Question 5's second half, and the one the census cannot answer: a whole-day
/// sample across the market is dominated by living securities, so whatever
/// happens in a delisting's final week is a rounding error in it.
fn report_delisted(
    client: &SharadarClient,
    table: &str,
    subject: &DelistedSubject,
) -> anyhow::Result<()> {
    println!(
        "Question 5, second half. {} around {}. {}",
        subject.ticker, subject.last_traded, subject.note
    );

    let from = shift(subject.last_traded, -WINDOW_DAYS)?;
    let to = shift(subject.last_traded, WINDOW_DAYS)?;
    let params = [
        ("ticker", subject.ticker.to_string()),
        ("from", from.to_string()),
        ("to", to.to_string()),
        ("limit", WINDOW_LIMIT.to_string()),
    ];

    let Some(body) = ask(client, table, &params, true)? else {
        return Ok(());
    };
    let rows = rows_of(&body);
    print_rows(&rows);

    let last = rows.iter().filter_map(|row| field(row, "date")).max();
    // Matched by reference. `match last` would move the String out of the
    // Option, and the comparison further down still needs it.
    match &last {
        Some(date) => println!("  last date served for this name in the window: {date}"),
        None => println!("  no rows in the window at all"),
    }
    if rows.len() == WINDOW_LIMIT {
        println!(
            "  NOTE: exactly {WINDOW_LIMIT} rows came back, which is the cap. Treat the window \
             filter as unproven."
        );
    }

    // The same window on the price table, always, not only when the first
    // answer was empty. What the fetch needs to know is not whether a name has
    // a market cap on its last day, but whether an empty answer here can happen
    // for a security that was demonstrably still trading. That is exactly the
    // silent-empty trap, and one table cannot see it alone: an empty answer and
    // a name with genuinely no coverage look identical from inside this table.
    println!("  the same window on {NATIVE_STOCKS_TABLE}, for coverage against coverage");
    let price_params = [
        ("ticker", subject.ticker.to_string()),
        ("from", from.to_string()),
        ("to", to.to_string()),
        ("fields", "ticker,date,close,closeunadj".to_string()),
    ];
    let price_rows = match ask(client, NATIVE_STOCKS_TABLE, &price_params, true)? {
        Some(body) => rows_of(&body),
        None => Vec::new(),
    };
    let last_price = price_rows.iter().filter_map(|row| field(row, "date")).max();
    println!(
        "  rows on {table}: {}, rows on {NATIVE_STOCKS_TABLE}: {}",
        rows.len(),
        price_rows.len()
    );
    match (&last, &last_price) {
        (Some(daily), Some(price)) => {
            println!("  last date, {table} {daily}, {NATIVE_STOCKS_TABLE} {price}")
        }
        (None, Some(price)) => println!(
            "  this name was still trading through {price} and has no {table} row in the \
             window at all, so an empty answer from {table} does not mean the security \
             was absent from the market"
        ),
        (_, None) => println!("  no price rows either, so the window is outside this name's life"),
    }

    // Only worth a request when the window came back empty: is the name in the
    // table at all, or was it merely absent from that window?
    if rows.is_empty() {
        println!("  and whether {table} carries this name on any date");
        let any = [
            ("ticker", subject.ticker.to_string()),
            ("sort", "date".to_string()),
            ("limit", "1".to_string()),
        ];
        if let Some(body) = ask(client, table, &any, true)? {
            let any_rows = rows_of(&body);
            match any_rows.first().and_then(|row| field(row, "date")) {
                Some(date) => println!("  earliest row this table has for the name: {date}"),
                None => println!("  the table has no row for this name on any date"),
            }
        }
    }
    Ok(())
}

/// How far back the daily table is served, against the price table's reach.
///
/// One request, `limit=1` ascending, rather than a paginated walk. The answer
/// is the first row, and fetching a whole history to look at its head would be
/// thousands of round trips against a host that publishes no rate limits.
///
/// The comparison is what matters. Curated prices start at 1997-12-31, so a
/// daily table that begins later leaves the earliest part of the price history
/// with no market cap beside it, and that is a fact about what can be built
/// rather than a fault to fix here.
fn report_reach(
    client: &SharadarClient,
    table: &str,
    stocks_reach: Option<Date>,
) -> anyhow::Result<()> {
    println!("Question 6. How far back this table reaches for a long-lived name.");

    let params = [
        ("ticker", REACH_TICKER.to_string()),
        ("sort", "date".to_string()),
        ("limit", "1".to_string()),
    ];
    let Some(body) = ask(client, table, &params, true)? else {
        return Ok(());
    };
    let rows = rows_of(&body);
    print_rows(&rows);

    let earliest = rows.first().and_then(|row| field(row, "date"));
    match (&earliest, stocks_reach) {
        (Some(daily), Some(stocks)) => {
            println!("  earliest {REACH_TICKER} row on {table}: {daily}");
            println!("  earliest {REACH_TICKER} row on {NATIVE_STOCKS_TABLE}: {stocks}");
        }
        (Some(daily), None) => println!("  earliest {REACH_TICKER} row on {table}: {daily}"),
        (None, _) => println!("  no row came back, so the reach is unmeasured"),
    }
    Ok(())
}
