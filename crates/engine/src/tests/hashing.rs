//! E7, engine half. The configuration hash covers every constant.
//!
//! The CLI half of E7, which checks that a run appends exactly one trial whose
//! recorded Sharpe is the one printed, lives in `crates/cli/src/backtest.rs`
//! because that is where the append happens.

use super::test_config;
use crate::config::BacktestConfig;

/// Every field of the configuration reaches the hash.
///
/// Written as a list of mutations rather than as a hand-written canonical
/// string, because a hand-written expected string is written by someone looking
/// at the struct and drifts the moment a field is added. A mutation per field
/// fails loudly when a new field is added without being covered here, which is
/// the failure worth having.
#[test]
fn e7_the_config_hash_changes_when_any_constant_changes() {
    let base = test_config(12);
    let base_hash = base.config_hash().expect("hashes");

    let variants = [
        BacktestConfig {
            signal_lookback_months: 11,
            ..base.clone()
        },
        BacktestConfig {
            signal_skip_months: 0,
            ..base.clone()
        },
        BacktestConfig {
            quintile_divisor: 10,
            ..base.clone()
        },
        BacktestConfig {
            price_floor: rust_decimal::Decimal::new(1, 0),
            ..base.clone()
        },
        BacktestConfig {
            min_coverage: rust_decimal::Decimal::new(90, 2),
            ..base.clone()
        },
        BacktestConfig {
            cost_per_side: rust_decimal::Decimal::new(20, 4),
            ..base.clone()
        },
        BacktestConfig {
            random_seed: 1,
            ..base.clone()
        },
        BacktestConfig {
            random_draws: 21,
            ..base.clone()
        },
        BacktestConfig {
            sample_modulus: 19,
            ..base.clone()
        },
        BacktestConfig {
            sample_residue: 4,
            ..base.clone()
        },
        BacktestConfig {
            universe_sha256: "1".repeat(64),
            ..base.clone()
        },
    ];

    // One mutation per field. If the struct grows a field, this count is the
    // reminder that the new field needs a mutation of its own.
    let field_count = serde_json::to_value(&base)
        .expect("config serialises")
        .as_object()
        .expect("config is an object")
        .len();
    assert_eq!(
        variants.len(),
        field_count,
        "the configuration has {field_count} fields and {} are covered here",
        variants.len()
    );

    for variant in variants {
        assert_ne!(
            variant.config_hash().expect("hashes").as_str(),
            base_hash.as_str(),
            "a changed constant left the config hash alone, so the trial log \
             would record two different runs as the same configuration"
        );
    }
}

/// A `Decimal` written two ways is the same configuration and must hash the
/// same, because `0.80` and `0.8` are the same coverage requirement.
///
/// This is the reason the fields carry the string codec: without it the scale
/// rides along into the hash and a configuration typed with a trailing zero
/// records as a distinct trial.
#[test]
fn the_config_hash_ignores_decimal_scale() {
    let base = test_config(12);
    let rescaled = BacktestConfig {
        min_coverage: rust_decimal::Decimal::from_str_exact("0.8").expect("valid"),
        ..base.clone()
    };
    assert_eq!(
        base.min_coverage, rescaled.min_coverage,
        "0.80 and 0.8 must compare equal"
    );
    assert_eq!(
        base.config_hash().expect("hashes").as_str(),
        rescaled.config_hash().expect("hashes").as_str()
    );
}

/// The canonical form sorts its keys, so reordering the struct's fields cannot
/// move the hash.
#[test]
fn the_canonical_form_is_sorted_and_compact() {
    let canonical = test_config(12).canonical_json().expect("serialises");
    assert!(
        canonical.starts_with(r#"{"cost_per_side":"#),
        "got {canonical}"
    );
    assert!(!canonical.contains(' '), "got {canonical}");
}
