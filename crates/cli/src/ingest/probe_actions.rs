//! `ingest probe-actions`. What the corporate actions table actually ships.
//!
//! # The question this exists to answer
//!
//! Curated prices are split-and-stock-dividend adjusted closes, so a monthly
//! return computed from them is a *price return*: it misses the cash a holder
//! was paid over the month. Adding that cash back needs the vendor's corporate
//! actions, and every part of the pipeline for them exists except the fetch.
//!
//! Nothing about the actions table has been measured yet, and five things have
//! to be measured rather than assumed before a decoder can be written:
//!
//! 1. **The table name.** A wrong-case *known* name is answered with
//!    `{"count":0,"data":[]}` and HTTP 200, so a typo reads as "this security
//!    has no corporate actions". An entirely unknown name measured differently
//!    on 2026-08-10, HTTP 401, which reads as a credential problem instead.
//!    Neither failure names the real fault, so a name is only trusted here once
//!    a known dividend payer comes back non-empty under it.
//! 2. **The field names and shapes** of a returned row.
//! 3. **Which action kinds appear**, and whether the host honours a server-side
//!    filter on kind or quietly ignores the parameter and sends everything.
//! 4. **The dividend amount basis.** A dividend declared before a later split
//!    is either shipped as the per-share figure that was actually declared, or
//!    restated by the vendor onto today's share count. The two differ by the
//!    split ratio, which for the subjects below is a factor of 3 or 10, so the
//!    difference is not a rounding argument. It decides the formula that turns
//!    a shipped amount into cash a holder received.
//! 5. **What the row's date means.** If it is the ex-date, the *ex-date* being
//!    the first day the shares trade without the right to the declared
//!    dividend, then the price drops by roughly the dividend that day while the
//!    vendor's `closeadj` series, which is adjusted for dividends as well as
//!    splits, does not. Fetching `closeadj` around one dividend shows which
//!    date the row is naming.
//!
//! # What this command does not do
//!
//! It writes nothing, counts no trial, and draws no conclusion. Its whole
//! output is what the vendor sent, plus arithmetic on it that a reader would
//! otherwise do by hand. Which spelling goes into `native_tables`, and which
//! amount formula the engine uses, are decided from this output rather than
//! here.

use std::collections::BTreeMap;
use std::process::ExitCode;

use ingest::provider::SourceError;
use ingest::sharadar::{NATIVE_STOCKS_TABLE, SharadarClient};
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::Value;

/// Candidate spellings of the actions table, most likely first.
///
/// Candidates rather than a constant precisely because a wrong name here fails
/// as an empty success or as a credential rejection, and never as "no such
/// table". The winner earns its place in `sharadar::columns::native_tables` by
/// returning rows here first.
const CANDIDATE_TABLES: &[&str] = &["actions", "action", "corporateactions", "events"];

/// The ticker the table-name candidates are tried against.
///
/// A company that has paid a dividend every quarter for decades, so an empty
/// answer is a statement about the request rather than about the company.
const CANDIDATE_TICKER: &str = "KO";

/// One security and the window asked for it.
struct Subject {
    ticker: &'static str,
    from: Date,
    to: Date,
    note: &'static str,
}

/// Chosen so the amount-basis question is answered by arithmetic that cannot be
/// mistaken for rounding.
///
/// Each of the first three paid cash dividends in the quarters *before* a split
/// inside the window, at ratios of 3, 10 and 4. So a dividend shipped as
/// declared and one restated onto today's share count differ by a factor no
/// reader has to squint at. The control pays throughout and never splits, which
/// is what distinguishes "the vendor restates" from "this ticker is odd".
const SUBJECTS: &[Subject] = &[
    Subject {
        ticker: "WMT",
        from: Date::constant(2023, 1, 1),
        to: Date::constant(2025, 12, 31),
        note: "quarterly cash dividends before a 3-for-1 split on 2024-02-26",
    },
    Subject {
        ticker: "NVDA",
        from: Date::constant(2023, 1, 1),
        to: Date::constant(2025, 12, 31),
        note: "quarterly cash dividends before a 10-for-1 split on 2024-06-10",
    },
    Subject {
        ticker: "AAPL",
        from: Date::constant(2019, 1, 1),
        to: Date::constant(2021, 12, 31),
        note: "quarterly cash dividends before a 4-for-1 split on 2020-08-31, \
               served only if this key's window reaches back that far",
    },
    Subject {
        ticker: "KO",
        from: Date::constant(2023, 1, 1),
        to: Date::constant(2025, 12, 31),
        note: "a long-time payer with no split in the window, control",
    },
];

/// Rows one windowed request may return.
///
/// A cap rather than a page size. If `from` and `to` are not honoured on this
/// table, an unbounded request would pull a security's entire action history,
/// and a probe that floods its own output has hidden the thing it went to
/// measure. Every subject window holds well under this if the filter works, so
/// a response of exactly this many rows is itself the signal that it does not.
const WINDOW_LIMIT: usize = 50;

/// The kind filter this host is asked to honour, if it honours one at all.
const KIND_FILTER: (&str, &str) = ("action", "dividend");

/// Windows the market-wide kind census samples, and its per-window row cap.
///
/// Four short windows in four different years rather than one long one.
/// Measured 2026-08-10: a capped response is the newest rows in the window, so
/// asking for a whole month returns the last two days of it and the extra 28
/// days buy nothing. Separate windows spend the same cap on periods that cannot
/// overlap, which is what turns "one busy afternoon" into a sample.
///
/// A quarter-end week in each, because that is when distributions cluster.
const VOCABULARY_WINDOWS: &[(Date, Date)] = &[
    (Date::constant(2024, 6, 24), Date::constant(2024, 6, 28)),
    (Date::constant(2023, 3, 27), Date::constant(2023, 3, 31)),
    (Date::constant(2022, 9, 26), Date::constant(2022, 9, 30)),
    (Date::constant(2021, 12, 27), Date::constant(2021, 12, 31)),
];

/// Rows one census window may return.
const VOCABULARY_LIMIT: usize = 500;

/// Calendar days either side of an ex-date for the adjusted-close check.
///
/// Enough to bracket a weekend on both sides, so the previous trading day is in
/// the window whichever day of the week the ex-date falls on.
const EX_DATE_DAYS: i64 = 5;

/// One dividend to check the date semantics and the amount basis against.
struct ExDate {
    ticker: &'static str,
    /// The date on the action row, taken from the windows above.
    date: Date,
    note: &'static str,
}

/// Two dividends, chosen so the second discriminates what the first cannot.
///
/// The amount question is whether a shipped figure is already on the same basis
/// as the adjusted close, or has to be scaled onto it by that bar's
/// `close / closeunadj`. On a date after a security's last split those two
/// answers are identical, because the factor is 1, so a check run there proves
/// nothing about the formula. The pre-split date has a factor of a third, where
/// the two candidates differ by a factor of three and the vendor's own
/// dividend-adjusted series says which one it agrees with.
const EX_DATES: &[ExDate] = &[
    ExDate {
        ticker: "WMT",
        date: Date::constant(2025, 12, 12),
        note: "after the 2024 split, so close equals closeunadj and scaling cannot show",
    },
    ExDate {
        ticker: "WMT",
        date: Date::constant(2023, 3, 16),
        note: "before the 2024 3-for-1 split, so close is a third of closeunadj and the \
               two candidate formulas differ by a factor of three",
    },
];

/// Decimal places the diagnostic ratio columns are shown to.
///
/// Display only. Nothing computed here decides anything, and the underlying
/// arithmetic is `Decimal` throughout.
const RATIO_DP: u32 = 8;

pub fn run() -> anyhow::Result<ExitCode> {
    println!("Corporate actions probe, native API. Reads only, writes nothing, counts no trial.");
    println!("Reports what the vendor shipped. Draws no conclusion.");
    println!();

    let client = SharadarClient::native_from_env()?;

    // Requests are serial and deliberately unhurried. This host publishes no
    // rate limits, so the only safe assumption is that it has one.
    let Some(table) = discover_table(&client)? else {
        println!("No candidate spelling returned rows. Either none of them names the actions");
        println!("table, or this key is not served it at all. Nothing below can run.");
        return Ok(ExitCode::FAILURE);
    };

    println!("Table used for everything below: {table}");
    println!();

    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    for subject in SUBJECTS {
        for row in &report_subject(&client, table, subject)? {
            let kind = field(row, KIND_FILTER.0).unwrap_or_else(|| "<no action field>".to_string());
            *census.entry(kind).or_default() += 1;
        }
        println!();
    }

    println!("Kind census across every subject window above");
    for (kind, count) in &census {
        println!("  {count:>4}  {kind}");
    }
    println!();

    report_kind_vocabulary(&client, table)?;
    println!();

    // The first subject is the one whose window holds kinds other than
    // dividends, which is what makes the filter answer discriminating.
    report_kind_filter(&client, table, &SUBJECTS[0])?;
    println!();

    for ex_date in EX_DATES {
        report_ex_date(&client, table, ex_date)?;
        println!();
    }

    println!("Legend");
    println!("  raw          the response body exactly as the host sent it");
    println!("  rows         the same body's `data` array, one line per row, every field");
    println!("  price ratio  close_t / close_prev, the return a price series alone shows");
    println!("  adj ratio    closeadj_t / closeadj_prev, the vendor's own total return");
    println!("               (every ratio shown to {RATIO_DP} places, display only)");
    println!();
    println!("  On an ex-date the two separate by about the dividend yield, because the price");
    println!("  gives up the cash and the dividend-adjusted series does not. Which transition");
    println!("  they separate across is what the date on the action row means.");
    println!();
    println!("  The two candidate lines add the shipped amount to the close, once as shipped");
    println!("  and once scaled by that bar's close/closeunadj. Whichever reproduces the");
    println!("  vendor's own adjusted return is the basis the amount is already on. On a bar");
    println!("  whose factor is 1 the candidates coincide and the comparison says nothing.");

    Ok(ExitCode::SUCCESS)
}

/// Try candidate spellings until one returns rows, then stop.
///
/// Stops at the first hit rather than asking every candidate. Measured
/// 2026-08-10: this host answers an unknown table name with HTTP 401, not with
/// the empty success a wrong-case *known* name gets, so the later candidates
/// are requests that spend a credential rejection to learn nothing. A name that
/// returns rows for a company which has paid a dividend every quarter for
/// decades is the answer, and further candidates cannot improve on it.
fn discover_table(client: &SharadarClient) -> anyhow::Result<Option<&'static str>> {
    println!("Question 1. Which spelling names the actions table.");
    println!("A wrong-case known name is answered with an empty success, so the ticker asked");
    println!("for is one that has paid a dividend every quarter for decades: {CANDIDATE_TICKER}.");
    println!();

    let mut found = None;
    for candidate in CANDIDATE_TABLES {
        let params = [
            ("ticker", CANDIDATE_TICKER.to_string()),
            ("limit", "1".to_string()),
        ];
        let rows = match ask(client, candidate, &params, true)? {
            Some(body) => rows_of(&body),
            None => Vec::new(),
        };
        println!("  verdict: {} row(s)", rows.len());
        println!();
        if !rows.is_empty() {
            found = Some(*candidate);
            break;
        }
    }
    Ok(found)
}

/// Every action the host has for one subject over its window.
///
/// No `fields` parameter is sent, so the host returns every column it has,
/// which is what question 2 needs. No `sort` either: this is a raw request
/// rather than a paginated walk, and whatever order the rows arrive in is
/// itself an observation.
fn report_subject(
    client: &SharadarClient,
    table: &str,
    subject: &Subject,
) -> anyhow::Result<Vec<Value>> {
    println!("{}  {}", subject.ticker, subject.note);

    let params = [
        ("ticker", subject.ticker.to_string()),
        ("from", subject.from.to_string()),
        ("to", subject.to.to_string()),
        ("limit", WINDOW_LIMIT.to_string()),
    ];

    let Some(body) = ask(client, table, &params, true)? else {
        return Ok(Vec::new());
    };
    let rows = rows_of(&body);
    print_rows(&rows);

    if rows.len() == WINDOW_LIMIT {
        println!(
            "  NOTE: exactly {WINDOW_LIMIT} rows came back, which is the cap. Treat the window \
             filter as unproven."
        );
    }
    Ok(rows)
}

/// Which kinds the table uses at all, sampled across the whole market.
///
/// Four hand-picked securities show the kinds those four happened to have, and
/// a kind nobody in the sample experienced is invisible. The one that matters
/// most here is a *stock dividend*, a distribution paid in shares rather than
/// cash. Curated closes are already adjusted for those, so mapping one to cash
/// would count it twice and bias returns up. Whether this vendor spells it as
/// its own kind, or folds it into `dividend`, cannot be answered from four
/// large-cap payers.
///
/// Four short windows across every ticker are the cheapest question that can
/// see it. The row dump is suppressed because it would run to two thousand
/// rows; what is printed instead is the census and one whole row per distinct
/// kind, which is the evidence a mapping needs.
fn report_kind_vocabulary(client: &SharadarClient, table: &str) -> anyhow::Result<()> {
    println!("Question 3, second half. Which kinds this table uses, market-wide.");

    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    let mut example: BTreeMap<String, String> = BTreeMap::new();

    for (from, to) in VOCABULARY_WINDOWS {
        let params = [
            ("from", from.to_string()),
            ("to", to.to_string()),
            ("limit", VOCABULARY_LIMIT.to_string()),
        ];
        let Some(body) = ask(client, table, &params, false)? else {
            continue;
        };
        let rows = rows_of(&body);

        // How much of the window the sample actually reached. A capped response
        // is the newest rows in it, so the span printed here is what the census
        // below is a census of, which is not the same as the window asked for.
        let dates: Vec<String> = rows.iter().filter_map(|row| field(row, "date")).collect();
        let span = match (dates.iter().min(), dates.iter().max()) {
            (Some(earliest), Some(latest)) => format!("{earliest} to {latest}"),
            _ => "no date field on any row".to_string(),
        };
        println!("  rows: {}, dates present: {span}", rows.len());
        if rows.len() == VOCABULARY_LIMIT {
            println!(
                "  NOTE: exactly {VOCABULARY_LIMIT} rows came back, which is the cap, so this is \
                 a prefix of the window. A kind can be missing from a prefix."
            );
        }

        for row in &rows {
            let kind = field(row, KIND_FILTER.0).unwrap_or_else(|| "<no action field>".to_string());
            *census.entry(kind.clone()).or_default() += 1;
            example.entry(kind).or_insert_with(|| row.to_string());
        }
    }

    println!("  census over every window above");

    for (kind, count) in &census {
        println!("  {count:>4}  {kind}");
    }
    println!("  one whole row per kind, as the host sent it");
    for (kind, row) in &example {
        println!("    {kind}: {row}");
    }
    Ok(())
}

/// Whether the host honours a server-side filter on the kind of action.
///
/// The parameter name is a guess, which is the point: a host that ignores an
/// unknown parameter answers with every kind it has, and that answer is the
/// evidence. A host that honours it answers with one kind.
///
/// The subject must be one whose unfiltered window above holds more than one
/// kind. Against a security that only ever paid dividends, an honoured filter
/// and an ignored one return the identical rows, and a check that cannot come
/// out two ways is not a check.
fn report_kind_filter(
    client: &SharadarClient,
    table: &str,
    subject: &Subject,
) -> anyhow::Result<()> {
    let (name, value) = KIND_FILTER;
    println!("Question 3. Whether {name}={value} is honoured server-side or ignored.");
    println!(
        "Asked of {}, whose unfiltered window above carries kinds this filter should remove.",
        subject.ticker
    );

    let params = [
        ("ticker", subject.ticker.to_string()),
        ("from", subject.from.to_string()),
        ("to", subject.to.to_string()),
        (name, value.to_string()),
        ("limit", WINDOW_LIMIT.to_string()),
    ];

    let Some(body) = ask(client, table, &params, true)? else {
        return Ok(());
    };
    let rows = rows_of(&body);
    print_rows(&rows);

    let kinds: BTreeMap<String, usize> = rows.iter().fold(BTreeMap::new(), |mut counts, row| {
        let kind = field(row, name).unwrap_or_else(|| "<no action field>".to_string());
        *counts.entry(kind).or_default() += 1;
        counts
    });
    println!("  kinds returned under the filter: {kinds:?}");
    Ok(())
}

/// One bar, as the three close columns arrived.
struct Bar {
    date: Date,
    close: Decimal,
    adjusted: Decimal,
    unadjusted: Decimal,
}

/// What the date on a dividend row means, and what basis its amount is on.
///
/// `closeadj` is fetched here and in no other command. It is the vendor's
/// dividend-and-split-adjusted close, so its day-over-day ratio is the vendor's
/// own total return, computed by somebody who knows the answer. It is not
/// stored and the production price fetch does not ask for it: a series already
/// adjusted for dividends would double-count the moment cash dividends are
/// added to returns separately. Here it serves as an independent oracle.
fn report_ex_date(client: &SharadarClient, table: &str, subject: &ExDate) -> anyhow::Result<()> {
    println!("Question 5 and 4 together. {}", subject.note);
    println!("{}, the action row dated {}.", subject.ticker, subject.date);

    // The amount is read back from the table rather than written into this
    // file, so nothing here can disagree with what the fetch would see.
    let action_params = [
        ("ticker", subject.ticker.to_string()),
        ("from", subject.date.to_string()),
        ("to", subject.date.to_string()),
        ("limit", WINDOW_LIMIT.to_string()),
    ];
    let Some(action_body) = ask(client, table, &action_params, true)? else {
        return Ok(());
    };
    let action_rows = rows_of(&action_body);
    print_rows(&action_rows);

    let amount = action_rows
        .iter()
        .find(|row| field(row, KIND_FILTER.0).as_deref() == Some(KIND_FILTER.1))
        .and_then(|row| decimal_field(row, "value"));
    let Some(amount) = amount else {
        println!("  no dividend row on that date, so there is no amount to check a basis for");
        return Ok(());
    };

    let from = shift(subject.date, -EX_DATE_DAYS)?;
    let to = shift(subject.date, EX_DATE_DAYS)?;
    let price_params = [
        ("ticker", subject.ticker.to_string()),
        ("from", from.to_string()),
        ("to", to.to_string()),
        (
            "fields",
            "ticker,date,close,closeadj,closeunadj".to_string(),
        ),
    ];
    let Some(price_body) = ask(client, NATIVE_STOCKS_TABLE, &price_params, true)? else {
        return Ok(());
    };
    let bars = bars_of(&rows_of(&price_body));
    print_close_ratios(&bars);
    print_basis_candidates(&bars, subject.date, amount);
    Ok(())
}

/// The close columns of each row, in date order.
///
/// Sorted here rather than trusted, because this is a raw request with no sort
/// key and the host returned newest first. A ratio table built on the order the
/// rows arrived in would silently be reporting reciprocals.
fn bars_of(rows: &[Value]) -> Vec<Bar> {
    let mut bars: Vec<Bar> = rows
        .iter()
        .filter_map(|row| {
            Some(Bar {
                date: field(row, "date")?.parse().ok()?,
                close: decimal_field(row, "close")?,
                adjusted: decimal_field(row, "closeadj")?,
                unadjusted: decimal_field(row, "closeunadj")?,
            })
        })
        .collect();
    bars.sort_by_key(|bar| bar.date);
    bars
}

/// Day-over-day ratios on both close series, oldest first.
///
/// The ex-date is the transition where they separate. Arithmetic is `Decimal`
/// throughout and the rounding is applied to the printed string only.
fn print_close_ratios(bars: &[Bar]) {
    println!(
        "  {:<12} {:>12} {:>12} {:>12} {:>14} {:>14}",
        "date", "close", "closeunadj", "closeadj", "price ratio", "adj ratio"
    );

    let mut previous: Option<&Bar> = None;
    for bar in bars {
        let (price_ratio, adjusted_ratio) = match previous {
            None => ("-".to_string(), "-".to_string()),
            Some(last) => (
                ratio(bar.close, last.close),
                ratio(bar.adjusted, last.adjusted),
            ),
        };
        println!(
            "  {:<12} {:>12} {:>12} {:>12} {:>14} {:>14}",
            bar.date.to_string(),
            bar.close.to_string(),
            bar.unadjusted.to_string(),
            bar.adjusted.to_string(),
            price_ratio,
            adjusted_ratio
        );
        previous = Some(bar);
    }
}

/// The two candidate amount bases, against the vendor's own total return.
///
/// Both candidates add the shipped amount to the ex-date close and divide by
/// the previous close. They differ only in whether the amount is scaled onto
/// the adjusted-close basis by that bar's `close / closeunadj` first. The
/// vendor's `closeadj` ratio is the answer neither candidate can see, so
/// whichever candidate reproduces it is the basis the amount is already on.
fn print_basis_candidates(bars: &[Bar], ex_date: Date, amount: Decimal) {
    let Some(index) = bars.iter().position(|bar| bar.date == ex_date) else {
        println!("  {ex_date} is not a trading day in the window, so there is no transition here");
        return;
    };
    let Some(previous) = index.checked_sub(1).map(|before| &bars[before]) else {
        println!("  no bar before {ex_date} in the window, so there is no transition to measure");
        return;
    };
    let bar = &bars[index];

    let factor = bar.close.checked_div(bar.unadjusted);
    let scaled = factor.map(|factor| amount * factor);

    println!("  ex-date arithmetic, {} over {}", bar.date, previous.date);
    println!("    shipped amount                                  {amount}");
    println!(
        "    factor close/closeunadj on that bar              {}",
        factor
            .map(|value| value.normalize().to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "    price only     close_t / close_prev              {}",
        ratio(bar.close, previous.close)
    );
    println!(
        "    amount as shipped   (close_t + amount) / prev    {}",
        ratio(bar.close + amount, previous.close)
    );
    println!(
        "    amount scaled  (close_t + amount * factor) / prev {}",
        scaled
            .map(|scaled| ratio(bar.close + scaled, previous.close))
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "    vendor         closeadj_t / closeadj_prev        {}",
        ratio(bar.adjusted, previous.adjusted)
    );
}

/// `numerator / denominator`, or a dash where the denominator is zero.
fn ratio(numerator: Decimal, denominator: Decimal) -> String {
    match numerator.checked_div(denominator) {
        None => "-".to_string(),
        Some(value) => value.round_dp(RATIO_DP).to_string(),
    }
}

/// One request, printed as it was asked and as it came back.
///
/// `show_raw` is false only for the market-wide census, whose body is hundreds
/// of rows on one line. Every request that answers a mapping question prints
/// its body verbatim, because a decode written against a paraphrase of a
/// response is a decode written against nothing.
///
/// A failure prints and returns `None` so the remaining questions still get
/// asked, with one exception: a rejected credential ends the run. Repeating a
/// request whose credential is wrong produces the same answer and spends
/// goodwill against a host that publishes no rate limits.
fn ask(
    client: &SharadarClient,
    table: &str,
    params: &[(&str, String)],
    show_raw: bool,
) -> anyhow::Result<Option<String>> {
    let shown: Vec<String> = params
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    println!("  request: table={table} {}", shown.join(" "));

    match client.native_raw(table, params) {
        Ok(body) => {
            if show_raw {
                println!("  raw: {body}");
            }
            Ok(Some(body))
        }
        Err(error @ SourceError::Unauthorized { .. }) => Err(error.into()),
        Err(error) => {
            println!("  error: {error}");
            Ok(None)
        }
    }
}

/// The `data` array of a native response, or nothing if it has none.
fn rows_of(body: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|envelope| envelope.get("data").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

/// Print every field of every row, without knowing any field's name.
///
/// Naming the fields here would be assuming the answer to question 2. The map
/// `serde_json` parses into is ordered by key, so the columns line up across
/// rows without this file deciding what they are.
fn print_rows(rows: &[Value]) {
    for row in rows {
        let Some(fields) = row.as_object() else {
            println!("  a row that is not an object: {row}");
            continue;
        };
        let shown: Vec<String> = fields
            .iter()
            .map(|(name, value)| format!("{name}={}", scalar(value)))
            .collect();
        println!("  {}", shown.join("  "));
    }
}

/// One field of one row as text, or nothing if it is absent.
fn field(row: &Value, name: &str) -> Option<String> {
    row.get(name).map(scalar)
}

/// One field of one row as a `Decimal`, parsed from the exact token the vendor
/// sent.
///
/// `arbitrary_precision` keeps a JSON number as its original text, so this
/// never routes a price through `f64`.
fn decimal_field(row: &Value, name: &str) -> Option<Decimal> {
    Decimal::from_str_exact(&field(row, name)?).ok()
}

/// A JSON value as a bare token, so a row of values reads as values.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn shift(date: Date, days: i64) -> anyhow::Result<Date> {
    Ok(date.checked_add(jiff::SignedDuration::from_hours(days * 24))?)
}
