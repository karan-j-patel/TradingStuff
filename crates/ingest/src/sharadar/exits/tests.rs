//! The exit decoder and the assembly rules.
//!
//! Every row here is synthetic. The vendor's licence forbids redistributing
//! rows, and that applies to a test fixture as strictly as to a data directory.
//! What the fixtures reproduce is the *shape* the probe measured, never its
//! values.
//!
//! Each test names the mutation it exists to catch, because a decoder test that
//! only walks the happy path passes under every wrong decoder as well.

use super::*;
use crate::schema::{AssetKey, PermanentId};
use jiff::civil::date;
use serde_json::json;

fn dec(text: &str) -> Decimal {
    Decimal::from_str_exact(text).expect("test literal is an exact decimal")
}

fn asset(ticker: &str, permaticker: u64) -> AssetKey {
    AssetKey {
        ticker: ticker.to_string(),
        permanent: Some(PermanentId::Sharadar(permaticker)),
    }
}

fn universe(entries: &[(&str, u64)]) -> BTreeMap<String, AssetKey> {
    entries
        .iter()
        .map(|(ticker, id)| ((*ticker).to_string(), asset(ticker, *id)))
        .collect()
}

/// Decode a row from a JSON object, which is the form the native host ships.
///
/// Written as JSON rather than as a constructed `ExitRow` on purpose. The
/// hazards under test are about how a JSON value crosses into a `Decimal`, and
/// a fixture that skipped the JSON would skip the hazard.
fn decode(requested: &'static str, fields: serde_json::Value) -> Result<ExitRow, SourceError> {
    let map = match fields {
        serde_json::Value::Object(map) => map,
        other => panic!("a row fixture must be an object, got {other}"),
    };
    decode_exit(requested, &NativeRow::for_test(&map))
}

fn row(ticker: &str, day: Date, kind: &'static str, value: Option<Decimal>) -> ExitRow {
    ExitRow {
        ticker: ticker.to_string(),
        date: day,
        kind,
        value,
    }
}

// --- the decoder ------------------------------------------------------------

/// A null `value` is legal on an exit row and decodes as absent.
///
/// Measured shape: fund wind-ups ship `delisted` with `value` as JSON null. A
/// required-field decoder errors on exactly the rows an imputation rule needs
/// to count, so the tolerance is the point.
#[test]
fn a_null_value_is_legal_on_an_exit_row() {
    let decoded = decode(
        EXIT_MARKER,
        json!({"ticker": "WOUND", "date": "2021-03-01", "action": "delisted", "value": null}),
    )
    .expect("a null value must decode");

    assert_eq!(decoded.value, None);
    assert_eq!(decoded.ticker, "WOUND");
    assert_eq!(decoded.date, date(2021, 3, 1));
}

/// A `value` of zero is a real final market capitalisation, not a missing one.
///
/// This is the X-D7 test. A decoder written as "zero means missing" passes
/// every other test in this file and drops exactly the bankruptcy rows the
/// haircut is for. The assertion is on `Some(0)` rather than on "not None",
/// because a decoder that mapped zero to `Some(anything_else)` would also be
/// wrong.
#[test]
fn a_value_of_zero_is_a_figure_rather_than_a_missing_one() {
    let decoded = decode(
        EXIT_MARKER,
        json!({"ticker": "BUSTED", "date": "2021-03-01", "action": "delisted", "value": 0}),
    )
    .expect("a zero value must decode");

    assert_eq!(
        decoded.value,
        Some(Decimal::ZERO),
        "zero was read as a missing figure, which discards every bankruptcy \
         liquidation whose final market capitalisation really was zero"
    );
    assert_ne!(decoded.value, None);
}

/// A row of a kind the request did not ask for is an error, not a skip.
///
/// The server-side filter is what makes six requests cheap. A filter that has
/// quietly stopped being honoured looks exactly like a quiet quarter unless
/// somebody insists on the difference.
#[test]
fn a_row_of_the_wrong_kind_is_refused() {
    let error = decode(
        EXIT_MARKER,
        json!({"ticker": "SURVIVOR", "date": "2021-03-01", "action": "mergerfrom", "value": 10}),
    )
    .expect_err("a kind the request did not ask for must be refused");

    let message = error.to_string();
    assert!(message.contains("delisted"), "got {message}");
    assert!(message.contains("mergerfrom"), "got {message}");
}

/// A response that dropped the `value` column is an error, not an absent
/// figure.
///
/// The tolerance above is for a null *value*, not for a missing *field*. The
/// request names the column, so a row without it is the host disagreeing with
/// what it was asked for.
#[test]
fn a_missing_value_column_is_an_error_rather_than_an_absent_figure() {
    let error = decode(
        EXIT_MARKER,
        json!({"ticker": "SHORT", "date": "2021-03-01", "action": "delisted"}),
    )
    .expect_err("a dropped column must be refused");

    assert!(error.to_string().contains("value"), "got {error}");
}

// --- the classification mapping ---------------------------------------------

/// Every kind the fetch asks for maps to the reason the round settled on, and
/// the two merger-shaped kinds land on the same one.
#[test]
fn the_vendor_kinds_map_to_the_settled_reasons() {
    let mapping: BTreeMap<&str, DelistingReason> = EXPLANATORY_KINDS.iter().copied().collect();

    assert_eq!(mapping.get("acquisitionby"), Some(&DelistingReason::Merger));
    assert_eq!(mapping.get("mergerto"), Some(&DelistingReason::Merger));
    assert_eq!(
        mapping.get("bankruptcyliquidation"),
        Some(&DelistingReason::Bankruptcy)
    );
    assert_eq!(
        mapping.get("regulatorydelisting"),
        Some(&DelistingReason::ExchangeRule)
    );
    assert_eq!(
        mapping.get("voluntarydelisting"),
        Some(&DelistingReason::Voluntary)
    );
    assert_eq!(EXPLANATORY_KINDS.len(), 5, "a kind was added or lost");
}

/// The survivor side of a merger is never a kind this module classifies.
///
/// AFBI is the case that makes this load-bearing. One ticker carries
/// `mergerfrom` in the year it survived a merger and `acquisitionby` in the
/// year it was acquired, and only the second is an exit. A rule that classified
/// the survivor side would mark a living company as delisted.
#[test]
fn the_survivor_side_kinds_are_absent_from_the_mapping() {
    for survivor in ["mergerfrom", "acquisitionof"] {
        assert!(
            !EXPLANATORY_KINDS.iter().any(|(kind, _)| *kind == survivor),
            "{survivor} is the surviving company's row and is not an exit"
        );
    }
}

// --- the assembly -----------------------------------------------------------

/// A marker with an explanation beside it takes that explanation's reason, and
/// a marker without one is `Unknown` rather than a guess.
///
/// The two names are in one fixture so the same pass produces both, which is
/// what a per-name rule would get wrong.
#[test]
fn a_marker_takes_its_same_date_explanation_and_otherwise_is_unknown() {
    let day = date(2021, 3, 1);
    let rows = vec![
        row("EXPLAINED", day, EXIT_MARKER, Some(dec("58.7"))),
        row("EXPLAINED", day, "regulatorydelisting", Some(dec("58.7"))),
        row("BARE", day, EXIT_MARKER, None),
    ];
    let members = universe(&[("EXPLAINED", 1), ("BARE", 2)]);

    let assembled = assemble(&rows, &members, "synthetic").expect("the rows assemble");

    assert_eq!(assembled.delistings.len(), 2);
    let by_ticker: BTreeMap<&str, &Delisting> = assembled
        .delistings
        .iter()
        .map(|row| (row.asset.ticker.as_str(), row))
        .collect();

    assert_eq!(
        by_ticker["EXPLAINED"].reason,
        DelistingReason::ExchangeRule,
        "the same-date explanation did not reach the marker"
    );
    assert_eq!(
        by_ticker["BARE"].reason,
        DelistingReason::Unknown,
        "an unexplained marker was given a reason nothing supports"
    );
    assert_eq!(by_ticker["EXPLAINED"].final_market_cap, Some(dec("58.7")));
    assert_eq!(by_ticker["BARE"].final_market_cap, None);

    // Nothing observed a delisting return, so nothing may claim to have.
    for row in &assembled.delistings {
        assert_eq!(
            row.terminal,
            TerminalValue::Unknown,
            "curation invented a terminal value the vendor never published"
        );
    }
}

/// A final market cap of zero survives assembly into the stored record.
///
/// # Why this is not the decoder test over again
///
/// `a_value_of_zero_is_a_figure_rather_than_a_missing_one` covers
/// `decode_exit`, where the JSON crosses into a `Decimal`. This covers
/// `assemble`, which is a second place the same value can be dropped, and a
/// blind falsification round dropped it exactly there:
///
/// ```text
///   final_market_cap: final_market_cap.filter(|cap| *cap > Decimal::ZERO)
/// ```
///
/// That compiles, reads as care, and is wrong for the same reason the decoder
/// version is. It also has a plausible source inside this crate: the market cap
/// dataset's own reader refused non-positive figures until the first
/// full-universe walk found 4387 rows of legitimate zero, so carrying the old
/// rule across to the exits path is a refactor somebody would defend. The
/// decoder test does not reach `assemble` and stayed green through it.
///
/// The three states are asserted together in one pass, because what has to hold
/// is that they stay distinct, not that any one survives alone.
#[test]
fn a_zero_final_market_cap_survives_assembly() {
    let day = date(2021, 3, 1);
    let rows = vec![
        row("BUSTED", day, EXIT_MARKER, Some(dec("0"))),
        row("BUSTED", day, "bankruptcyliquidation", Some(dec("0"))),
        row("WOUND", day, EXIT_MARKER, None),
        row("SOLD", day, EXIT_MARKER, Some(dec("2047.5"))),
    ];
    let members = universe(&[("BUSTED", 1), ("WOUND", 2), ("SOLD", 3)]);

    let assembled = assemble(&rows, &members, "synthetic").expect("the rows assemble");
    let by_ticker: BTreeMap<&str, &Delisting> = assembled
        .delistings
        .iter()
        .map(|row| (row.asset.ticker.as_str(), row))
        .collect();

    assert_eq!(
        by_ticker["BUSTED"].final_market_cap,
        Some(dec("0")),
        "a bankruptcy liquidation's real final market capitalisation of zero was \
         stored as absent, so the row an imputation rule most needs reads as \
         though the vendor never sent a figure"
    );
    assert_eq!(by_ticker["WOUND"].final_market_cap, None);
    assert_eq!(by_ticker["SOLD"].final_market_cap, Some(dec("2047.5")));
    assert_ne!(
        by_ticker["BUSTED"].final_market_cap, by_ticker["WOUND"].final_market_cap,
        "a real zero and an absent figure must not collapse into one state"
    );

    // The zero row is still classified normally, so the assertion above is not
    // passing because the row fell out of the assembly altogether.
    assert_eq!(by_ticker["BUSTED"].reason, DelistingReason::Bankruptcy);
}

/// An explanation dated a day away from the marker does not classify it.
///
/// The measured alignment is exact rather than approximate, on five of five
/// names across 1999 and 2026, so a window here would be looser than the data.
#[test]
fn an_explanation_on_a_different_date_does_not_classify_the_marker() {
    let rows = vec![
        row("DRIFT", date(2021, 3, 1), EXIT_MARKER, None),
        row("DRIFT", date(2021, 3, 2), "bankruptcyliquidation", None),
    ];

    let assembled = assemble(&rows, &universe(&[("DRIFT", 1)]), "synthetic").expect("assembles");

    assert_eq!(assembled.delistings.len(), 1);
    assert_eq!(assembled.delistings[0].reason, DelistingReason::Unknown);
    assert_eq!(
        assembled.unmatched_explanations, 1,
        "the orphaned explanation was dropped without being counted"
    );
}

/// The same kind repeating, one row per counterparty, is one exit rather than
/// an ambiguity or a duplicate.
///
/// Measured shape: an acquisition of one company by several parties ships one
/// row per party, with the same date and the same value on each. The curated
/// writer refuses a duplicate `(asset, date)` outright, so the collapse has to
/// happen here or the whole write fails.
#[test]
fn a_kind_repeating_on_one_date_collapses_to_one_exit() {
    let day = date(2021, 3, 1);
    let value = Some(dec("2047.5"));
    let rows = vec![
        row("MANY", day, EXIT_MARKER, value),
        row("MANY", day, "acquisitionby", value),
        row("MANY", day, "acquisitionby", value),
        row("MANY", day, "acquisitionby", value),
    ];

    let assembled = assemble(&rows, &universe(&[("MANY", 1)]), "synthetic").expect("assembles");

    assert_eq!(
        assembled.delistings.len(),
        1,
        "repeated counterparty rows became repeated exits"
    );
    assert_eq!(assembled.delistings[0].reason, DelistingReason::Merger);
    assert_eq!(assembled.unmatched_explanations, 0);
}

/// Two explanatory kinds meaning different things on one date is an error.
///
/// A merger exits at the last close and a regulatory delisting takes a 30
/// percent haircut, so picking either one silently is picking a number. The
/// refusal names the ticker and both reasons.
#[test]
fn conflicting_explanations_on_one_date_are_refused() {
    let day = date(2021, 3, 1);
    let rows = vec![
        row("BOTH", day, EXIT_MARKER, None),
        row("BOTH", day, "acquisitionby", None),
        row("BOTH", day, "regulatorydelisting", None),
    ];

    let error = assemble(&rows, &universe(&[("BOTH", 1)]), "synthetic")
        .expect_err("two incompatible reasons on one date must be refused");

    let message = error.to_string();
    assert!(message.contains("BOTH"), "got {message}");
    assert!(message.contains("Merger"), "got {message}");
    assert!(message.contains("ExchangeRule"), "got {message}");
}

/// Rows naming a ticker outside the universe are counted, not stored.
///
/// The walk is market-wide and the universe is a sample of the master, so
/// overhang is the normal case. What is not acceptable is nobody being able to
/// tell how much of the walk applied.
#[test]
fn rows_outside_the_universe_are_counted_rather_than_stored() {
    let day = date(2021, 3, 1);
    let rows = vec![
        row("INSIDE", day, EXIT_MARKER, None),
        row("OUTSIDE", day, EXIT_MARKER, None),
        row("OUTSIDE", day, "voluntarydelisting", None),
    ];

    let assembled = assemble(&rows, &universe(&[("INSIDE", 1)]), "synthetic").expect("assembles");

    assert_eq!(assembled.delistings.len(), 1);
    assert_eq!(assembled.delistings[0].asset.ticker, "INSIDE");
    assert_eq!(
        assembled.outside_universe, 2,
        "the discarded rows were not counted"
    );
    assert_eq!(assembled.unmatched_explanations, 0);
}

/// The stored key is the universe entry's, permanent identifier included.
///
/// The actions table serves a ticker and nothing else. A curated exit keyed on
/// the ticker string would attach to the wrong price series the day a ticker
/// was reused, which is the failure `AssetKey` exists to prevent.
#[test]
fn the_stored_key_carries_the_permanent_identifier() {
    let rows = vec![row("KEYED", date(2021, 3, 1), EXIT_MARKER, None)];

    let assembled = assemble(&rows, &universe(&[("KEYED", 4242)]), "synthetic").expect("assembles");

    assert_eq!(
        assembled.delistings[0].asset,
        asset("KEYED", 4242),
        "the vendor ticker was stored without its permanent identity"
    );
    assert!(assembled.delistings[0].asset.is_stable());
}

/// Two markers on one date disagreeing about the final market cap is an error.
#[test]
fn two_markers_disagreeing_about_the_final_market_cap_are_refused() {
    let day = date(2021, 3, 1);
    let rows = vec![
        row("SPLIT", day, EXIT_MARKER, Some(dec("100"))),
        row("SPLIT", day, EXIT_MARKER, Some(dec("200"))),
    ];

    let error = assemble(&rows, &universe(&[("SPLIT", 1)]), "synthetic")
        .expect_err("two different final values for one exit must be refused");

    assert!(error.to_string().contains("SPLIT"), "got {error}");
}
