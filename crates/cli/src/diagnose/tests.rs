//! X-T1 and the end-to-end shape of the turnover diagnostic.
//!
//! X-T2 through X-T5 live in `crates/engine`, where the replay, the fit and the
//! partition rule are. What only exists at this boundary is the claim that
//! running the tool leaves the trial log alone.
//!
//! Every fixture is synthetic and every log written here is a temp file. The
//! committed scientific record is never opened by these tests.

use super::*;
use ingest::adjusted::AdjustedBar;
use ingest::schema::{AssetKey, CloseKind, PermanentId, SessionScope};
use ingest::universe::UniverseEntry;
use jiff::civil::{Date, date};
use rust_decimal::Decimal;
use std::fs;

/// The genesis line, copied rather than read from the committed record, so
/// these tests never touch it.
const GENESIS: &str = r#"{"config_hash":"0000000000000000000000000000000000000000000000000000000000000000","entry_hash":"bbdc8721955c4bb3f0ba915f1306070895d598345f3f5308d09cd78093af6db5","prev_hash":"0000000000000000000000000000000000000000000000000000000000000000","program":"genesis","sharpe":null,"timestamp":"2026-08-08T00:00:00Z"}"#;

/// Forty names over twenty month-ends.
///
/// Forty is the smallest count that gives the fit any leverage under the
/// standard quintile: the full universe holds eight, each half four, and each
/// quarter two. A fixture with fewer names lands every sub-universe on the
/// `.max(1)` floor, leaves every `K` equal, and the fit is then refused for
/// having no spread, which would fail this test for a reason unrelated to what
/// it is testing.
///
/// Closes follow a per-name phase, `100 + ((step + permaticker) % 7) * 3`, so
/// the momentum ranking reorders between formations and the turnover is not
/// trivially zero. Every close clears the $5 floor.
fn dataset(name: &str) -> (PathBuf, PathBuf) {
    build(name, false)
}

/// The same fixture with one security carrying no permanent identifier, in the
/// prices AND in the universe file.
///
/// Both sides, because the universe filter matches on the whole `AssetKey`: an
/// entry stripped on one side only never matches its bars and the security is
/// dropped before the panel exists, which is a different thing from an
/// unidentified security reaching the sub-universe split.
fn dataset_with_unidentified(name: &str) -> (PathBuf, PathBuf) {
    build(name, true)
}

fn build(name: &str, strip_first: bool) -> (PathBuf, PathBuf) {
    let key = |permaticker: u64| AssetKey {
        ticker: format!("T{permaticker:03}"),
        permanent: (!(strip_first && permaticker == 1))
            .then_some(PermanentId::Sharadar(permaticker)),
    };
    let dir = std::env::temp_dir().join(format!("diagnose-cli-{name}"));
    fs::create_dir_all(&dir).expect("creating the fixture directory");

    let days: Vec<Date> = (0..20)
        .map(|step: i16| {
            date(
                2020 + step / 12,
                i8::try_from(step % 12).expect("month fits") + 1,
                28,
            )
        })
        .collect();

    let mut bars = Vec::new();
    for permaticker in 1..=40u64 {
        for (step, day) in days.iter().enumerate() {
            let close = Decimal::from(100 + ((step as u64 + permaticker) % 7) * 3);
            bars.push(AdjustedBar {
                asset: key(permaticker),
                date: *day,
                open: close,
                high: close,
                low: close,
                close,
                volume: Decimal::from(1000u64),
                close_unadjusted: close,
                session: SessionScope::RegularHours,
                close_kind: CloseKind::ClosingAuction,
            });
        }
    }

    let prices = dir.join("prices.parquet");
    ingest::parquet::write_prices(bars, &prices, "Synthetic").expect("writing fixture prices");

    let universe = dir.join("universe.jsonl");
    let entries: Vec<UniverseEntry> = (1..=40u64)
        .map(|permaticker| UniverseEntry {
            asset: key(permaticker),
            name: None,
            exchange: None,
            is_delisted: false,
            first_price_date: days[0],
            last_price_date: Some(days[days.len() - 1]),
            outcome: None,
        })
        .collect();
    fs::write(
        &universe,
        ingest::universe::to_jsonl(&entries).expect("serialising the fixture universe"),
    )
    .expect("writing the fixture universe");

    (prices, universe)
}

/// A temp log seeded with the genesis line.
fn temp_log(name: &str) -> String {
    let path = std::env::temp_dir().join(format!("diagnose-trials-{name}.jsonl"));
    fs::write(&path, format!("{GENESIS}\n")).expect("seeding the temp log");
    path.to_string_lossy().into_owned()
}

fn report(trials: &str, fixture: &str) -> String {
    let (prices, universe) = dataset(fixture);
    turnover_report(
        trials,
        engine::PROGRAM,
        None,
        &prices,
        &universe,
        None,
        None,
        None,
    )
    .expect("the fixture diagnoses")
}

/// X-T1. The diagnostic leaves the trial log byte-identical, and says so in its
/// own output.
///
/// # Why the bytes rather than the count
///
/// A count is what a reader reaches for first and it is the weaker check: an
/// append followed by anything that removed a line keeps the count and changes
/// the record. The file is small, so comparing it whole costs nothing, which is
/// what being sure costs here.
///
/// The ruling line is asserted alongside. The property and the claim about the
/// property are two different things, and a transcript carrying these numbers
/// without the ruling invites a reader to treat them as a result.
#[test]
fn x_t1_the_diagnostic_records_no_trial() {
    let path = temp_log("untouched");
    let before = fs::read(&path).expect("reading the seeded log");

    let text = report(&path, "untouched");

    assert_eq!(
        fs::read(&path).expect("reading the log after"),
        before,
        "the diagnostic changed the trial log, so it is a backtest wearing a \
         diagnostic's name and rule 2 applies to it"
    );
    assert!(
        text.contains(engine::NOT_A_TRIAL),
        "the output does not carry the ruling that this is not a trial, so a \
         transcript of it does not explain itself, got {text}"
    );
}

/// The report carries every sub-universe and the fit, in one string.
///
/// The end-to-end statement that the seven replays run against a real panel
/// rather than only in the engine's unit tests. This is what fails if a
/// sub-universe comes back empty or the fit is refused for want of spread.
#[test]
fn the_report_carries_all_seven_replays_and_the_estimate() {
    let text = report(&temp_log("shape"), "shape");

    assert!(text.contains("full universe"), "got {text}");
    for (modulus, residue) in engine::SUB_UNIVERSES.iter().filter(|(m, _)| *m != 1) {
        assert!(
            text.contains(&format!("permaticker % {modulus} == {residue}")),
            "the report is missing the sub-universe {modulus}/{residue}, got {text}"
        );
    }
    assert!(
        text.contains("Book-size-free turnover estimate"),
        "got {text}"
    );
    assert_eq!(
        text.matches("residual").count(),
        engine::SUB_UNIVERSES.len(),
        "the report does not carry one residual per replayed sub-universe, got {text}"
    );
}

/// A security with no vendor identifier is refused rather than dropped.
///
/// Dropping it would leave the halves and quarters no longer summing to the full
/// universe, silently, and the fit would then be comparing a whole against parts
/// that are not its parts.
#[test]
fn a_security_with_no_permaticker_is_refused() {
    let (prices, universe) = dataset_with_unidentified("no-permaticker");

    let error = turnover_report(
        &temp_log("no-permaticker"),
        engine::PROGRAM,
        None,
        &prices,
        &universe,
        None,
        None,
        None,
    )
    .expect_err("a universe with an unidentified security must be refused");
    assert!(
        format!("{error:#}").contains("no vendor permanent identifier"),
        "the run failed for some other reason: {error:#}"
    );
}
