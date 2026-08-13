//! `ingest probe-exits`. Which side of a merger the vendor's action names, and
//! what an exit row's date and value mean.
//!
//! # The question this exists to answer
//!
//! A security that leaves the universe has to be given a return for the month
//! it left in, and the number depends entirely on why it left. A company
//! acquired for cash at a premium hands its holder a gain. A company delisted
//! into bankruptcy hands its holder close to nothing. Filling both with the
//! same figure, or with a zero, is the classic way a survivorship-bias-free
//! panel quietly becomes biased again in the other direction.
//!
//! Before any imputation rule can be written, three things about the vendor's
//! exit rows have to be measured rather than assumed:
//!
//! 1. **Which ticker on a merger row is the one that stopped trading.** The
//!    vendor documents `mergerto` as carrying the *non-surviving* company and
//!    `mergerfrom` as carrying the survivor, which reads backwards to most
//!    people on first contact. Getting it inverted would impute an exit return
//!    onto the company that is still trading and leave the one that vanished
//!    with no return at all. The test here does not read the documentation
//!    back. It asks whether a ticker drawn from each side also carries a
//!    `delisted` row of its own, because a company that stopped trading has
//!    one and a company that did not, does not.
//! 2. **What the date on an exit row marks.** A delisting has two dates in the
//!    real world, the day the exchange removes the listing and the day the last
//!    trade of any kind happens. Enron lost its NYSE listing in January 2002
//!    and traded over the counter until roughly November 2004, so the two are
//!    34 months apart for the most famous delisting there is. If the vendor's
//!    date is the exchange removal and its price series runs on past it, the
//!    last close is not the exit price and an imputation attached to it is
//!    attached to the wrong bar.
//! 3. **What the `value` column on an exit row is.** The documentation says
//!    final market capitalisation in millions of dollars. If that holds, an
//!    exit row carries the size of the company on its last day, which is a
//!    quantity an imputation rule can use directly rather than having to
//!    reconstruct.
//!
//! # What this command does not do
//!
//! It writes nothing, counts no trial, and draws no conclusion. Its whole
//! output is what the vendor sent, plus arithmetic on it that a reader would
//! otherwise do by hand. Which imputation rule the panel uses is decided from
//! this output rather than here.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;

use ingest::sharadar::{
    NATIVE_ACTIONS_TABLE, NATIVE_DAILY_TABLE, NATIVE_STOCKS_TABLE, SharadarClient,
};
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::Value;

use super::probe_wire::{ask, decimal_field, field, print_rows, rows_of};

/// The field naming the kind of action, measured 2026-08-10 by `probe-actions`.
const KIND_FIELD: &str = "action";

/// The kind every security that stopped trading is expected to carry.
///
/// The pivot of question 1. It is asked about rather than assumed to be the
/// only exit kind, because the census below counts how often it appears without
/// an explanatory kind beside it.
const DELISTED_KIND: &str = "delisted";

/// Which kinds the direction test draws its subjects from, and how many each.
///
/// The subjects are deliberately not written into this file. A hand-picked
/// merger is a merger somebody already knew the answer for, and the sample
/// would then be a test of the author's memory rather than of the vendor. The
/// probe asks the host for the most recent rows of each kind and takes the
/// first distinct tickers it is sent.
///
/// Both merger sides are drawn because the test needs a control. If exit-side
/// tickers carry `delisted` and survivor-side tickers do not, the difference is
/// the side. If both carry it, the field says nothing about direction.
const DRAWN_FROM: &[(&str, usize)] = &[("mergerto", 4), ("mergerfrom", 3), ("acquisitionby", 2)];

/// The window subjects are drawn from.
///
/// Recent, because a recent name is one whose fate can be checked against the
/// public record by a reader of this output, and wide enough that a quiet
/// quarter for mergers cannot empty it.
const CANDIDATE_FROM: Date = Date::constant(2020, 1, 1);
const CANDIDATE_TO: Date = Date::constant(2026, 8, 1);

/// Rows one candidate-drawing request may return.
///
/// Measured 2026-08-10: a capped response is the newest rows in the window. So
/// this returns the most recent rows of the kind, which is exactly the sample
/// wanted, and the cap is not a truncation problem here.
const CANDIDATE_LIMIT: usize = 20;

/// Rows one subject's whole action history may return.
///
/// No window is sent with a history request. The question is whether a
/// `delisted` row exists *anywhere* for that ticker, and a windowed request
/// that happened to miss it would answer "no" for the wrong reason.
const HISTORY_LIMIT: usize = 200;

/// The famously delisted names question 2 is posed with, in order.
///
/// Enron first, because the 34-month gap between its exchange removal and its
/// last over-the-counter trade is the widest separation between the two
/// candidate meanings of an exit date that any well known name offers.
/// WorldCom second, for the same reason and the same era. A name absent from
/// this vendor answers with an empty success rather than an error, so a miss
/// here is silent and has to be read from a row count.
const ERA_TICKERS: &[&str] = &["ENE", "WCOM"];

/// The window substitute subjects are drawn from if neither name above is
/// served.
///
/// Deliberately in the Enron era. If the key's history does not reach back this
/// far, an empty answer here is about the subscription rather than about ticker
/// identity, and that distinction is the whole reason this request exists.
const ERA_FROM: Date = Date::constant(1998, 1, 1);
const ERA_TO: Date = Date::constant(1999, 12, 31);

/// Rows the era request may return, and how many of its tickers are followed
/// up.
const ERA_LIMIT: usize = 20;
const SUBSTITUTES: usize = 3;

/// Price bars fetched at the end of a security's life.
///
/// A handful rather than one, so the output shows the series running out rather
/// than a single date that has to be taken on trust.
const TAIL_BARS: usize = 5;

/// Daily metrics rows fetched for the value cross-check.
///
/// More than a handful on purpose. If the exit `value` matched a market cap the
/// security sat near for a fortnight, the match would be luck. The preceding
/// bars are printed so a reader can see whether the matched figure is
/// distinguishable from its neighbours.
const DAILY_BARS: usize = 12;

/// How many names the value cross-check is run on.
const VALUE_NAMES: usize = 2;

/// The quarter-end week the market-wide census covers.
///
/// One recent week rather than a long window. Distributions and exits cluster
/// at quarter end, and a week is short enough that every kind's request stays
/// far below the cap, so a count from it is a count rather than a prefix.
const CENSUS_FROM: Date = Date::constant(2026, 6, 24);
const CENSUS_TO: Date = Date::constant(2026, 6, 30);

/// Rows one census request may return.
const CENSUS_LIMIT: usize = 500;

/// The kinds that can end a security's life in the universe.
///
/// Six, taken from the vocabulary `probe-actions` observed market-wide on
/// 2026-08-10 plus the two merger kinds this command exists to disambiguate.
/// One request per kind rather than one unfiltered request, because an
/// unfiltered week is dominated by dividends and would hit the cap long before
/// an exit kind was reached.
const EXIT_KINDS: &[&str] = &[
    "mergerto",
    "acquisitionby",
    DELISTED_KIND,
    "regulatorydelisting",
    "voluntarydelisting",
    "bankruptcyliquidation",
];

/// The client, and the number of requests it has been asked for.
///
/// A mutable counter in a codebase that otherwise builds new values rather than
/// changing old ones. It earns the exception the same way the trial counter
/// does. The count has to be reported honestly at the end of a run, a request
/// budget was agreed before the run started, and threading a running total
/// through every function by hand is the version of this that eventually
/// forgets a branch.
///
/// Every request in this file goes through [`Probe::ask`], so a branch that
/// adds a request cannot fail to be counted.
struct Probe {
    client: SharadarClient,
    requests: usize,
}

impl Probe {
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: SharadarClient::native_from_env()?,
            requests: 0,
        })
    }

    /// One request, counted whether or not it succeeded.
    ///
    /// Counted before the result is known, because the budget is a budget on
    /// what was asked of the host rather than on what came back.
    fn ask(
        &mut self,
        table: &str,
        params: &[(&str, String)],
        show_raw: bool,
    ) -> anyhow::Result<Option<String>> {
        self.requests += 1;
        ask(&self.client, table, params, show_raw)
    }

    /// The rows of one request, with a failure read as no rows.
    ///
    /// `ask` already printed the failure. A probe that stops at the first
    /// unhappy response leaves the remaining questions unasked, and the
    /// remaining questions are independent of this one.
    fn rows(
        &mut self,
        table: &str,
        params: &[(&str, String)],
        show_raw: bool,
    ) -> anyhow::Result<Vec<Value>> {
        Ok(self
            .ask(table, params, show_raw)?
            .map(|body| rows_of(&body))
            .unwrap_or_default())
    }
}

/// One ticker the host offered, and the row it was offered on.
struct Subject {
    ticker: String,
    drawn_from: &'static str,
    source_date: String,
}

/// A subject's whole action history, reduced to what question 1 asks.
struct History {
    subject: Subject,
    /// Dates of rows whose kind is the one the subject was drawn from.
    source_dates: Vec<String>,
    /// Dates of this ticker's `delisted` rows, and the value on each.
    delisted: Vec<(String, Option<Decimal>)>,
}

pub fn run() -> anyhow::Result<ExitCode> {
    println!("Exit actions probe, native API. Reads only, writes nothing, counts no trial.");
    println!("Reports what the vendor shipped. Draws no conclusion.");
    println!();

    let mut probe = Probe::new()?;

    let histories = report_direction(&mut probe)?;
    println!();
    report_exit_date(&mut probe)?;
    println!();
    report_value(&mut probe, &histories)?;
    println!();
    report_census(&mut probe)?;
    println!();

    println!("Total requests issued: {}", probe.requests);
    Ok(ExitCode::SUCCESS)
}

/// Question 1. Whether the exit side of a merger is the one carrying
/// `delisted`.
///
/// The evidence is a co-occurrence, which is weaker than a stated rule and
/// stronger than reading the documentation back. Its strength comes from the
/// two sides being drawn from the same events. If the same merger produces a
/// `delisted` row on one of its two tickers and not on the other, nothing about
/// the company, the date or the deal explains the difference, and only the side
/// is left.
fn report_direction(probe: &mut Probe) -> anyhow::Result<Vec<History>> {
    println!("Question 1. Which side of a merger row is the company that stopped trading.");
    println!("Subjects are whatever the host returns, not names written into this file.");
    println!();

    let mut subjects: Vec<Subject> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for (kind, wanted) in DRAWN_FROM {
        println!("Drawing up to {wanted} subject(s) from action={kind}");
        let params = [
            (KIND_FIELD, (*kind).to_string()),
            ("from", CANDIDATE_FROM.to_string()),
            ("to", CANDIDATE_TO.to_string()),
            ("limit", CANDIDATE_LIMIT.to_string()),
        ];
        let rows = probe.rows(NATIVE_ACTIONS_TABLE, &params, true)?;
        print_rows(&rows);

        let mut taken = 0;
        for row in &rows {
            if taken == *wanted {
                break;
            }
            let (Some(ticker), Some(date)) = (field(row, "ticker"), field(row, "date")) else {
                continue;
            };
            // Distinct across every kind, not just within one. A ticker that
            // appears on both sides would otherwise be counted twice and the
            // sample would be smaller than it looks.
            if !seen.insert(ticker.clone()) {
                continue;
            }
            subjects.push(Subject {
                ticker,
                drawn_from: kind,
                source_date: date,
            });
            taken += 1;
        }
        println!("  took {taken} subject(s)");
        println!();
    }

    let mut histories = Vec::new();
    for subject in subjects {
        histories.push(report_history(probe, subject)?);
        println!();
    }

    println!("Question 1 summary. Every row here is read out of a history above.");
    println!(
        "  {:<8} {:<16} {:<12} {:<12} {:<8}",
        "ticker", "drawn from", "source date", "delisted", "same day"
    );
    for history in &histories {
        let (delisted, same) = match history.delisted.first() {
            None => ("none".to_string(), "-".to_string()),
            Some((date, _)) => {
                let same = if history.source_dates.iter().any(|source| source == date) {
                    "yes"
                } else {
                    "no"
                };
                (date.clone(), same.to_string())
            }
        };
        println!(
            "  {:<8} {:<16} {:<12} {:<12} {:<8}",
            history.subject.ticker,
            history.subject.drawn_from,
            history.subject.source_date,
            delisted,
            same
        );
    }
    Ok(histories)
}

/// One subject's entire action history, and what it holds.
fn report_history(probe: &mut Probe, subject: Subject) -> anyhow::Result<History> {
    println!(
        "{}, drawn from {} dated {}. Whole history, no kind filter.",
        subject.ticker, subject.drawn_from, subject.source_date
    );

    let params = [
        ("ticker", subject.ticker.clone()),
        ("limit", HISTORY_LIMIT.to_string()),
    ];
    let rows = probe.rows(NATIVE_ACTIONS_TABLE, &params, true)?;
    print_rows(&rows);

    if rows.len() == HISTORY_LIMIT {
        println!("  NOTE: exactly {HISTORY_LIMIT} rows came back, which is the cap, so a");
        println!("  `delisted` row could be outside what was returned.");
    }

    let kinds: BTreeMap<String, usize> = rows.iter().fold(BTreeMap::new(), |mut counts, row| {
        let kind = field(row, KIND_FIELD).unwrap_or_else(|| "<no action field>".to_string());
        *counts.entry(kind).or_default() += 1;
        counts
    });
    println!("  kinds in this history: {kinds:?}");

    let of_kind = |wanted: &str| -> Vec<&Value> {
        rows.iter()
            .filter(|row| field(row, KIND_FIELD).as_deref() == Some(wanted))
            .collect()
    };

    let source_dates: Vec<String> = of_kind(subject.drawn_from)
        .iter()
        .filter_map(|row| field(row, "date"))
        .collect();
    let delisted: Vec<(String, Option<Decimal>)> = of_kind(DELISTED_KIND)
        .iter()
        .filter_map(|row| Some((field(row, "date")?, decimal_field(row, "value"))))
        .collect();

    println!("  {} rows dated: {source_dates:?}", subject.drawn_from);
    println!("  {DELISTED_KIND} rows (date, value): {delisted:?}");

    Ok(History {
        subject,
        source_dates,
        delisted,
    })
}

/// Question 2. What the date on an exit row marks.
///
/// Posed with a name whose exchange removal and last trade are years apart, so
/// the two candidate meanings are separated by something no rounding argument
/// can close. If the vendor does not serve that name, the question is re-posed
/// on whatever names the vendor does carry from the same era, and the answer is
/// reported for what it is.
fn report_exit_date(probe: &mut Probe) -> anyhow::Result<()> {
    println!("Question 2. Whether an exit date is the exchange removal or the last trade.");
    println!("Enron lost its NYSE listing in January 2002 and traded over the counter until");
    println!("roughly November 2004, so on that name the two answers are 34 months apart.");
    println!();

    let mut served = None;
    for ticker in ERA_TICKERS {
        println!("{ticker}, actions then prices.");
        let action_params = [
            ("ticker", (*ticker).to_string()),
            ("limit", HISTORY_LIMIT.to_string()),
        ];
        let action_rows = probe.rows(NATIVE_ACTIONS_TABLE, &action_params, true)?;
        print_rows(&action_rows);

        let bars = last_bars(probe, ticker)?;
        println!(
            "  {} action row(s), {} price bar(s)",
            action_rows.len(),
            bars.len()
        );
        println!();

        if !action_rows.is_empty() {
            served = Some((*ticker, action_rows));
            break;
        }
    }

    let Some((ticker, _rows)) = served else {
        println!("None of {ERA_TICKERS:?} is served, on either table, so the question cannot be");
        println!("asked in the form it was posed. An empty answer has two explanations, that");
        println!("the ticker is spelled differently here and that the key's history does not");
        println!("reach the era at all, and the request below separates them.");
        println!();
        return report_substitutes(probe);
    };

    println!("{ticker} is served, so the question was answered on the name it was posed with.");
    Ok(())
}

/// The same question, on names the vendor demonstrably carries from that era.
///
/// The substitutes are not chosen by hand for the same reason the merger
/// subjects are not. They are the tickers on the rows the host returns for an
/// Enron-era window, so what is being measured is the vendor's data rather than
/// the author's recall of which 1999 delisting would make the point.
fn report_substitutes(probe: &mut Probe) -> anyhow::Result<()> {
    println!("Substitutes, drawn from {DELISTED_KIND} rows in {ERA_FROM} to {ERA_TO}.");
    println!("A non-empty answer here proves the key's history reaches the era, which turns");
    println!("the empty answers above into a statement about ticker identity instead.");

    let params = [
        (KIND_FIELD, DELISTED_KIND.to_string()),
        ("from", ERA_FROM.to_string()),
        ("to", ERA_TO.to_string()),
        ("limit", ERA_LIMIT.to_string()),
    ];
    let rows = probe.rows(NATIVE_ACTIONS_TABLE, &params, true)?;
    print_rows(&rows);

    let dates: Vec<String> = rows.iter().filter_map(|row| field(row, "date")).collect();
    match (dates.iter().min(), dates.iter().max()) {
        (Some(earliest), Some(latest)) => {
            println!(
                "  {} row(s), dates present {earliest} to {latest}",
                rows.len()
            )
        }
        _ => println!("  {} row(s), no date field on any row", rows.len()),
    }
    if rows.len() == ERA_LIMIT {
        println!("  NOTE: exactly {ERA_LIMIT} rows came back, which is the cap. A capped answer");
        println!("  is the newest rows in the window, so these are the late end of it and");
        println!("  nothing here says how much further back the coverage goes.");
    }
    println!();

    println!(
        "  {:<10} {:<14} {:<12} {:<14} {:<8}",
        "ticker", "delisted date", "value", "last price bar", "aligned"
    );
    let mut taken = 0;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for row in &rows {
        if taken == SUBSTITUTES {
            break;
        }
        let (Some(ticker), Some(date)) = (field(row, "ticker"), field(row, "date")) else {
            continue;
        };
        if !seen.insert(ticker.clone()) {
            continue;
        }
        taken += 1;

        let value = field(row, "value").unwrap_or_else(|| "-".to_string());
        let bars = last_bars(probe, &ticker)?;
        let last = bars
            .iter()
            .filter_map(|bar| field(bar, "date"))
            .max()
            .unwrap_or_else(|| "none".to_string());
        let aligned = if last == date { "yes" } else { "no" };
        println!("  {ticker:<10} {date:<14} {value:<12} {last:<14} {aligned:<8}");
    }

    println!();
    println!("  Aligned means the last bar the price table has is the exit action's date");
    println!("  exactly. Aligned everywhere leaves no post-exit tail to reason about, and it");
    println!("  also means this data cannot distinguish an exchange removal from a last");
    println!("  trade, because the vendor's coverage ends where its own action says it ends.");
    Ok(())
}

/// The last few price bars a security has, newest first as the host sends them.
fn last_bars(probe: &mut Probe, ticker: &str) -> anyhow::Result<Vec<Value>> {
    let params = [
        ("ticker", ticker.to_string()),
        ("limit", TAIL_BARS.to_string()),
        ("fields", "ticker,date,close,closeunadj".to_string()),
    ];
    let rows = probe.rows(NATIVE_STOCKS_TABLE, &params, true)?;
    print_rows(&rows);
    Ok(rows)
}

/// Question 3. Whether an exit row's `value` is the final market cap.
///
/// The daily metrics table's `marketcap` was measured in millions of dollars on
/// 2026-08-11 by `probe-daily`, which is what makes this a comparison rather
/// than a units puzzle. The subjects are the first names from question 1 whose
/// exit row carries a value at all, so this is checked on the same rows the
/// direction was established on rather than on a fresh hand-picked pair.
fn report_value(probe: &mut Probe, histories: &[History]) -> anyhow::Result<()> {
    println!("Question 3. Whether an exit row's value is the final market capitalisation.");
    println!("Compared against the daily table's marketcap on the last trade date.");
    println!();

    let subjects: Vec<(&str, &str, Decimal)> = histories
        .iter()
        .filter_map(|history| {
            let (date, value) = history.delisted.first()?;
            Some((history.subject.ticker.as_str(), date.as_str(), (*value)?))
        })
        .take(VALUE_NAMES)
        .collect();

    if subjects.is_empty() {
        println!("  No subject from question 1 carries an exit row with a value, so there is");
        println!("  nothing to compare. This is itself worth recording.");
        return Ok(());
    }

    for (ticker, date, value) in &subjects {
        println!("{ticker}, exit dated {date}, shipped value {value}.");

        let bars = last_bars(probe, ticker)?;
        let last = bars
            .iter()
            .filter_map(|bar| field(bar, "date"))
            .max()
            .unwrap_or_else(|| "none".to_string());
        println!("  last price bar: {last}, exit action date: {date}");

        let params = [
            ("ticker", (*ticker).to_string()),
            ("limit", DAILY_BARS.to_string()),
            ("fields", "ticker,date,marketcap".to_string()),
        ];
        let daily = probe.rows(NATIVE_DAILY_TABLE, &params, true)?;
        print_rows(&daily);

        let matched = daily
            .iter()
            .find(|row| field(row, "date").as_deref() == Some(date))
            .and_then(|row| decimal_field(row, "marketcap"));
        match matched {
            None => println!("  no daily row dated {date}, so there is nothing to compare against"),
            Some(marketcap) => {
                println!("  action value           {value}");
                println!("  daily marketcap        {marketcap}");
                println!("  difference             {}", value - marketcap);
                println!(
                    "  ratio                  {}",
                    value
                        .checked_div(marketcap)
                        .map(|ratio| ratio.normalize().to_string())
                        .unwrap_or_else(|| "-".to_string())
                );
            }
        }
        println!();
    }

    println!("  The preceding bars are printed so the match can be told apart from luck. If");
    println!("  the neighbouring market caps are all different numbers, the figure matched");
    println!("  the last bar rather than a level the security sat near for a fortnight.");
    Ok(())
}

/// A market-wide census of exit kinds over one quarter-end week.
///
/// Two things come out of it for free, and both are re-derivations rather than
/// new questions. Every ticker carrying an explanatory exit kind should also
/// appear in the same week's `delisted` list, which tests question 1's finding
/// on a sample sharing no ticker with the subjects. And where a ticker carries
/// both an explanatory kind and a `delisted` row, the two rows carry a value
/// each, which tests question 3 without another request.
fn report_census(probe: &mut Probe) -> anyhow::Result<()> {
    println!("Market-wide exit census, {CENSUS_FROM} to {CENSUS_TO}, one request per kind.");
    println!();

    let mut counts: Vec<(&str, usize)> = Vec::new();
    let mut tickers: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    // (kind, ticker, date) to the value on that row, so the two sides of the
    // free value check can be looked up against each other.
    let mut values: Vec<(&str, String, String, Option<Decimal>)> = Vec::new();

    for kind in EXIT_KINDS {
        let params = [
            (KIND_FIELD, (*kind).to_string()),
            ("from", CENSUS_FROM.to_string()),
            ("to", CENSUS_TO.to_string()),
            ("limit", CENSUS_LIMIT.to_string()),
        ];
        let rows = probe.rows(NATIVE_ACTIONS_TABLE, &params, true)?;
        println!("  {kind}: {} row(s)", rows.len());
        print_rows(&rows);
        if rows.len() == CENSUS_LIMIT {
            println!("  NOTE: exactly {CENSUS_LIMIT} rows came back, which is the cap, so this");
            println!("  count is a floor rather than a count.");
        }
        println!();

        counts.push((kind, rows.len()));
        for row in &rows {
            let (Some(ticker), Some(date)) = (field(row, "ticker"), field(row, "date")) else {
                continue;
            };
            tickers.entry(kind).or_default().insert(ticker.clone());
            values.push((kind, ticker, date, decimal_field(row, "value")));
        }
    }

    println!("Census by kind");
    for (kind, count) in &counts {
        println!("  {count:>4}  {kind}");
    }
    println!();

    let empty = BTreeSet::new();
    let delisted = tickers.get(DELISTED_KIND).unwrap_or(&empty);
    println!("Re-derivation of question 1 on this week, which shares no ticker with the");
    println!("subjects above. Every explanatory exit kind should be a subset of {DELISTED_KIND}.");
    println!("  {DELISTED_KIND} tickers: {delisted:?}");
    for (kind, set) in &tickers {
        if *kind == DELISTED_KIND {
            continue;
        }
        let outside: Vec<&String> = set.difference(delisted).collect();
        println!("  {kind} tickers: {set:?}");
        println!("  {kind} not carrying {DELISTED_KIND}: {outside:?}");
    }
    println!();

    println!("Re-derivation of question 3 on the same week. Where a ticker carries both an");
    println!("explanatory kind and a {DELISTED_KIND} row on the same date, the two values are");
    println!("compared. No request was spent on this.");
    for (kind, ticker, date, value) in &values {
        if *kind == DELISTED_KIND {
            continue;
        }
        let other = values
            .iter()
            .find(|(other_kind, other_ticker, other_date, _)| {
                *other_kind == DELISTED_KIND && other_ticker == ticker && other_date == date
            });
        let shown = |value: &Option<Decimal>| {
            value
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".to_string())
        };
        match other {
            None => {
                println!("  {kind:<22} {ticker:<8} {date}  no {DELISTED_KIND} row on that date")
            }
            Some((_, _, _, delisted_value)) => println!(
                "  {kind:<22} {ticker:<8} {date}  value={} {DELISTED_KIND}={} match={}",
                shown(value),
                shown(delisted_value),
                value == delisted_value
            ),
        }
    }
    Ok(())
}
