//! X-W1. The wiring guard compares dataset identity, not merely presence.
//!
//! # What presence agreeing does not establish
//!
//! Every guard before this round asked one question per dataset: does the
//! configuration name a file, and did the panel get one. Both answers are
//! booleans, so a configuration recording the digest of file A run against a
//! panel built from file B passed. The result is a published figure under a hash
//! describing data the run never read, and the digest is the only description of
//! the input the trial log keeps, so no reader can detect it afterwards.
//!
//! The panel now carries the digest of the bytes each attachment was built from
//! and the guard compares the two for equality. These are the tests that hold
//! it, one per attachment, because a fourth dataset arriving with a presence
//! check and no equality check is the near-miss worth catching.

use ingest::actions::{Delisting, DelistingReason};
use ingest::marketcap::MarketCapRecord;

use super::{
    ACTIONS_SHA256, DELISTINGS_SHA256, MARKETCAP_SHA256, asset, cash_dividend, dec, delisting,
    month_ends, test_config, two_asset_panel,
};
use crate::config::{BacktestConfig, DELISTING_CONVENTION};
use crate::error::EngineError;
use crate::panel::Panel;
use crate::run;

/// A digest that is a well-formed SHA-256 and is not any of the placeholders the
/// fixtures attach under.
///
/// Sixty-four hex characters, because a value of the wrong shape could be
/// refused by something other than the guard under test and the test would pass
/// for a reason it did not mean.
const OTHER_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The panel with one dataset attached, under whichever digest is passed.
fn panel_with(dataset: &str, sha256: &str) -> Panel {
    let panel = two_asset_panel();
    let m = month_ends();
    match dataset {
        "actions" => panel
            .with_dividends(&[cash_dividend(&asset("AAA", 1), m[3], dec("0.5"))], sha256)
            .expect("dividends attach"),
        "delistings" => panel
            .with_delistings(
                &[delisting(
                    asset("BBB", 2),
                    m[4],
                    DelistingReason::Bankruptcy,
                )],
                sha256,
            )
            .expect("delistings attach"),
        "market cap" => panel
            .with_marketcaps(
                &[MarketCapRecord {
                    asset: asset("AAA", 1),
                    date: m[2],
                    marketcap: dec("100"),
                    source: "synthetic".to_string(),
                }],
                sha256,
            )
            .expect("market caps attach"),
        other => unreachable!("no such dataset {other}"),
    }
}

/// The configuration recording one dataset's digest, and nothing else changed.
fn config_recording(dataset: &str, sha256: &str) -> BacktestConfig {
    let base = test_config(2);
    match dataset {
        "actions" => BacktestConfig {
            actions_sha256: Some(sha256.to_string()),
            ..base
        },
        "delistings" => BacktestConfig {
            delisting_convention: Some(DELISTING_CONVENTION.to_string()),
            delistings_sha256: Some(sha256.to_string()),
            ..base
        },
        "market cap" => BacktestConfig {
            marketcap_sha256: Some(sha256.to_string()),
            ..base
        },
        other => unreachable!("no such dataset {other}"),
    }
}

/// X-W1. A configuration recording one file's digest, run against a panel built
/// from another, is refused.
///
/// Parameterised over the three attachments rather than written three times,
/// because the hazard is identical for each and a copy that drifted would leave
/// one dataset guarded and another not.
///
/// Both halves are asserted for every dataset. The agreeing case must run, or
/// the test would pass against an engine that refuses everything and would say
/// nothing about identity.
#[test]
fn x_w1_a_digest_the_panel_disagrees_with_is_refused() {
    for (dataset, fixture) in [
        ("actions", ACTIONS_SHA256),
        ("delistings", DELISTINGS_SHA256),
        ("market cap", MARKETCAP_SHA256),
    ] {
        // The agreeing case. This is the control, and it is what says the
        // refusal below is about the digest rather than about the fixture.
        run::backtest(
            &panel_with(dataset, fixture),
            &config_recording(dataset, fixture),
        )
        .unwrap_or_else(|error| {
            panic!("the {dataset} fixture must run when the digests agree, got {error}")
        });

        // The same panel and the same configuration, with the recorded digest
        // moved to a different file. Presence still agrees on both sides, which
        // is exactly why a presence check cannot see this.
        let error = run::backtest(
            &panel_with(dataset, fixture),
            &config_recording(dataset, OTHER_SHA256),
        )
        .err()
        .unwrap_or_else(|| {
            panic!(
                "a run recording the {dataset} digest {OTHER_SHA256} against a panel \
                 built from {fixture} produced a figure, so the guard compares presence \
                 rather than identity"
            )
        });
        assert!(
            matches!(
                error,
                EngineError::DatasetDigestMismatch {
                    dataset: named, ..
                } if named == dataset
            ),
            "the {dataset} run failed for some reason other than the digest \
             disagreeing: {error}"
        );
    }
}

/// The refusal names both digests, because a reader chasing it down needs to
/// know which file the configuration meant and which one the run actually read.
#[test]
fn the_digest_refusal_names_the_expected_and_the_actual() {
    let error = run::backtest(
        &panel_with("market cap", MARKETCAP_SHA256),
        &config_recording("market cap", OTHER_SHA256),
    )
    .expect_err("the mismatched digests must be refused");

    match &error {
        EngineError::DatasetDigestMismatch {
            dataset,
            recorded,
            attached,
        } => {
            assert_eq!(*dataset, "market cap");
            assert_eq!(
                recorded, OTHER_SHA256,
                "the recorded digest is not reported"
            );
            assert_eq!(
                attached, MARKETCAP_SHA256,
                "the digest the panel was built from is not reported"
            );
        }
        other => panic!("the run failed for some other reason: {other}"),
    }

    let message = error.to_string();
    assert!(
        message.contains(OTHER_SHA256) && message.contains(MARKETCAP_SHA256),
        "the printed message hides one of the two digests, got {message}"
    );
}

/// An empty attachment still carries its digest, so a run that read an empty
/// file cannot be confused with one that read a full one.
///
/// The empty case is where a presence check and an identity check look most
/// alike, because nothing about the panel's contents differs from never having
/// attached at all except the flag.
#[test]
fn an_attachment_that_matched_nothing_still_carries_its_digest() {
    let none: [Delisting; 0] = [];
    let panel = two_asset_panel()
        .with_delistings(&none, DELISTINGS_SHA256)
        .expect("an empty attachment is still an attachment");

    assert!(panel.delistings_attached());
    assert_eq!(panel.delistings_sha256(), Some(DELISTINGS_SHA256));
    assert!(
        run::backtest(&panel, &config_recording("delistings", OTHER_SHA256)).is_err(),
        "an empty delistings file recorded under the wrong digest was accepted"
    );
}
