//! X-R6, and the end-to-end shape of the panel export.
//!
//! X-R1 through X-R5 and X-R7 live in `crates/engine`, where the assembly is.
//! X-R8's refusals live in `crates/ingest`, where the metadata is read. What
//! only exists at this boundary is the claim that running the tool leaves the
//! trial log alone, and that the digests the run recorded are the ones that
//! reach the file.
//!
//! Every fixture is synthetic and every log written here is a temp file. The
//! committed scientific record is never opened by these tests.

use super::*;
use ingest::actions::{ActionRecord, CorporateAction, Delisting, DelistingReason, DividendKind};
use ingest::adjusted::AdjustedBar;
use ingest::marketcap::MarketCapRecord;
use ingest::provider::{FundamentalRecord, ReportBasis, ReportScope};
use ingest::schema::{AssetKey, CloseKind, PermanentId, SessionScope};
use ingest::universe::UniverseEntry;
use jiff::civil::{Date, date};
use rust_decimal::Decimal;
use std::fs;
use std::path::PathBuf;

/// The genesis line, copied rather than read from the committed record, so
/// these tests never touch it.
const GENESIS: &str = r#"{"config_hash":"0000000000000000000000000000000000000000000000000000000000000000","entry_hash":"bbdc8721955c4bb3f0ba915f1306070895d598345f3f5308d09cd78093af6db5","prev_hash":"0000000000000000000000000000000000000000000000000000000000000000","program":"genesis","sharpe":null,"timestamp":"2026-08-08T00:00:00Z"}"#;

/// The five input files and where the panel goes.
struct Fixture {
    prices: PathBuf,
    universe: PathBuf,
    actions: PathBuf,
    delistings: PathBuf,
    marketcap: PathBuf,
    filings: PathBuf,
    out: PathBuf,
}

/// Forty month-ends, the 28th of each month from 2018-01.
///
/// Forty because the export's lead-in is the thirty-six month volatility
/// window. A shorter panel exports nothing at all and the test would pass by
/// having no rows to be wrong about.
fn days() -> Vec<Date> {
    (0..40i16)
        .map(|step| {
            date(
                2018 + step / 12,
                i8::try_from(step % 12).expect("a month number fits") + 1,
                28,
            )
        })
        .collect()
}

fn key(permaticker: u64) -> AssetKey {
    AssetKey {
        ticker: format!("T{permaticker:03}"),
        permanent: Some(PermanentId::Sharadar(permaticker)),
    }
}

/// All five inputs written to a directory of this test's own.
///
/// Closes follow a per-name phase so the cross-section reorders between
/// formations and no characteristic is trivially constant. Every close clears
/// the five dollar floor.
fn fixture(name: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("export-cli-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("creating the fixture directory");
    let days = days();

    let mut bars = Vec::new();
    let mut caps = Vec::new();
    let mut filings = Vec::new();
    let mut actions = Vec::new();
    for permaticker in 1..=4u64 {
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
            caps.push(MarketCapRecord {
                asset: key(permaticker),
                date: *day,
                marketcap: Decimal::from((5 - permaticker) * 1000),
                source: "Synthetic".to_string(),
            });
        }
        // One filing per name, published 2020-07-28 and so 274 days old at the
        // last exported month-end, inside the 548 day bound.
        filings.push(FundamentalRecord {
            asset: key(permaticker),
            as_reported: days[30],
            period_end: days[28],
            observed_at: days[30],
            source: "Synthetic".to_string(),
            basis: ReportBasis::AsReported,
            scope: ReportScope::Quarterly,
            filing_id: None,
            fields: [(
                "equityusd".to_string(),
                Decimal::from(permaticker * 100_000_000),
            )]
            .into_iter()
            .collect(),
        });
        actions.push(ActionRecord {
            asset: key(permaticker),
            effective: days[33],
            action: CorporateAction::Dividend {
                amount: Decimal::from(1u64),
                kind: DividendKind::Cash,
            },
            source: "Synthetic".to_string(),
        });
    }

    let prices = dir.join("prices.parquet");
    ingest::parquet::write_prices(bars, &prices, "Synthetic").expect("writing fixture prices");

    let universe = dir.join("universe.jsonl");
    let entries: Vec<UniverseEntry> = (1..=4u64)
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

    let actions_path = dir.join("actions.parquet");
    ingest::parquet::write_actions(actions, &actions_path, "Synthetic")
        .expect("writing fixture actions");

    // One row, because the writers refuse an empty dataset and the export needs
    // the file attached to run at all. The name keeps trading, so the record
    // classifies nothing and changes no number.
    let delistings = dir.join("delistings.parquet");
    ingest::parquet::write_delistings(
        vec![Delisting {
            asset: key(4),
            date: days[39],
            reason: DelistingReason::Bankruptcy,
            listing: ingest::Listing::Other,
            terminal: ingest::TerminalValue::Unknown,
            final_market_cap: None,
            source: "Synthetic".to_string(),
        }],
        &delistings,
        "Synthetic",
    )
    .expect("writing fixture delistings");

    let marketcap = dir.join("marketcap.parquet");
    ingest::parquet::write_marketcap(caps, &marketcap, "Synthetic")
        .expect("writing fixture market caps");

    let filings_path = dir.join("filings.parquet");
    ingest::parquet::write_filings(filings, &filings_path, "Synthetic")
        .expect("writing fixture filings");

    Fixture {
        prices,
        universe,
        actions: actions_path,
        delistings,
        marketcap,
        filings: filings_path,
        out: dir.join("panel.parquet"),
    }
}

/// A temp log seeded with the genesis line.
fn temp_log(name: &str) -> String {
    let path = std::env::temp_dir().join(format!("export-trials-{name}.jsonl"));
    fs::write(&path, format!("{GENESIS}\n")).expect("seeding the temp log");
    path.to_string_lossy().into_owned()
}

fn export(trials: &str, fixture: &Fixture) -> String {
    panel_report(
        trials,
        &fixture.prices,
        &fixture.universe,
        &fixture.actions,
        &fixture.delistings,
        &fixture.marketcap,
        &fixture.filings,
        &fixture.out,
    )
    .expect("the fixture exports")
}

/// X-R6. The export leaves the trial log byte-identical, and says so in its own
/// output.
///
/// # Why the bytes rather than the count
///
/// A count is the weaker check: an append followed by anything that removed a
/// line keeps the count and changes the record. The file is small, so comparing
/// it whole is what being sure costs here.
///
/// The ruling line is asserted alongside. The property and the claim about the
/// property are two different things, and a transcript carrying an artifact's
/// provenance without the ruling invites a reader to treat the export as a
/// measurement.
#[test]
fn x_r6_the_export_records_no_trial() {
    let path = temp_log("untouched");
    let before = fs::read(&path).expect("reading the seeded log");

    let text = export(&path, &fixture("untouched"));

    assert_eq!(
        fs::read(&path).expect("reading the log after"),
        before,
        "the export changed the trial log, so it is a backtest wearing an artifact's \
         name and rule 2 applies to it"
    );
    assert!(
        text.contains(engine::export::NOT_A_TRIAL),
        "the output does not carry the ruling that this is not a trial, so a transcript \
         of it does not explain itself, got {text}"
    );
    // The word appears only inside the disclaimer that denies it. Written as
    // "strip the disclaimer, then look" because the disclaimer itself says
    // "no Sharpe", and a bare substring test would forbid the sentence that
    // makes the guarantee.
    let without_disclaimer = text.replace(engine::export::NOT_A_TRIAL, "");
    assert!(
        !without_disclaimer.contains("Sharpe"),
        "the export printed a Sharpe outside the disclaimer that denies it, got \
         {without_disclaimer}"
    );
}

/// X-R8, the writing half. The digests the run recorded are the ones in the
/// file.
///
/// The refusals belong to the reader and are tested in `ingest`. What can only
/// be checked here is that the six values are not placeholders: the
/// configuration hash is the hash of the configuration that actually ran, and
/// each dataset digest is the digest of the file that was passed in. A
/// provenance block naming the wrong file is worse than none, because it reads
/// as a check that was made.
#[test]
fn the_written_panel_names_the_configuration_and_the_five_files_it_read() {
    let fixture = fixture("provenance");
    export(&temp_log("provenance"), &fixture);

    let provenance =
        ingest::parquet::panel_provenance(&fixture.out).expect("the written panel declares itself");

    // Re-derived here rather than read back out of the report, so this compares
    // the file against the inputs rather than against the tool's own account of
    // them.
    let sha = |path: &PathBuf| rigor::hash_bytes(&fs::read(path).expect("reading an input"));
    assert_eq!(provenance.actions_sha256, sha(&fixture.actions));
    assert_eq!(provenance.delistings_sha256, sha(&fixture.delistings));
    assert_eq!(provenance.marketcap_sha256, sha(&fixture.marketcap));
    assert_eq!(provenance.filings_sha256, sha(&fixture.filings));
    assert_eq!(provenance.universe_sha256, sha(&fixture.universe));

    let expected = engine::BacktestConfig {
        actions_sha256: Some(provenance.actions_sha256.clone()),
        delisting_convention: Some(engine::DELISTING_CONVENTION.to_string()),
        delistings_sha256: Some(provenance.delistings_sha256.clone()),
        marketcap_sha256: Some(provenance.marketcap_sha256.clone()),
        filings_sha256: Some(provenance.filings_sha256.clone()),
        ..engine::BacktestConfig::panel_export(provenance.universe_sha256.clone())
    };
    assert_eq!(
        provenance.config_hash,
        expected.config_hash().expect("hashes").as_str(),
        "the file's configuration hash is not the hash of the configuration the export \
         ran under, so the artifact cannot be reproduced from what it declares"
    );
}

/// The written file reads back, over the month-ends the lead-in leaves.
///
/// The end-to-end statement that the assembly runs against real curated files
/// rather than only against in-memory fixtures. This is what fails if a writer
/// column and a reader column disagree, or if the panel comes back empty.
#[test]
fn the_written_panel_reads_back_over_the_months_the_lead_in_leaves() {
    let fixture = fixture("roundtrip");
    let text = export(&temp_log("roundtrip"), &fixture);

    let rows = ingest::parquet::read_panel(&fixture.out).expect("the written panel reads back");
    let days = days();
    // Four names over the twenty-eight month-ends after the twelve-month
    // eligibility lead-in, which is 4 * (40 - 12).
    assert_eq!(rows.len(), 112, "got {} rows", rows.len());
    assert_eq!(rows.iter().map(|row| row.month_end).min(), Some(days[12]));
    assert_eq!(rows.iter().map(|row| row.month_end).max(), Some(days[39]));

    // Every characteristic is present once every window fits, which is what
    // makes the nulls before that mean something. A run whose columns were all
    // null would otherwise pass every structural assertion above.
    let full = rows
        .iter()
        .find(|row| row.month_end == days[36])
        .expect("a row at the first month-end where every window fits");
    assert!(full.momentum_12_1.is_some());
    assert!(full.vol_daily_12m.is_some());
    assert!(full.vol_monthly_36m.is_some());
    assert!(full.log_marketcap.is_some());
    assert!(full.book_to_market.is_some());
    assert!(full.dividend_yield_12m.is_some());
    assert!(full.share_change_24m.is_some());
    assert!(full.median_dollar_volume_12m.is_some());
    assert!(full.label_return_1m.is_some());
    assert_eq!(full.label_delisted_in_window, Some(false));

    // And the partial region survives the Parquet round trip as nulls rather
    // than as zeros. The engine's own test pins where the boundary is; this
    // one pins that a null column reaches the file and comes back a null,
    // which is the property the fit script depends on.
    let partial = rows
        .iter()
        .find(|row| row.month_end == days[12])
        .expect("a row at the first exported month-end");
    assert!(partial.momentum_12_1.is_some());
    assert_eq!(partial.vol_monthly_36m, None);
    assert_eq!(partial.share_change_24m, None);

    // The final month-end has no forward label anywhere.
    for row in rows.iter().filter(|row| row.month_end == days[39]) {
        assert_eq!(row.label_return_1m, None);
        assert_eq!(row.label_delisted_in_window, None);
    }

    assert!(text.contains("Wrote 112 rows"), "got {text}");
}
