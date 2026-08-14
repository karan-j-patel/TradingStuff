//! E7, engine half. The configuration hash covers every constant.
//!
//! The CLI half of E7, which checks that a run appends exactly one trial whose
//! recorded Sharpe is the one printed, lives in `crates/cli/src/backtest.rs`
//! because that is where the append happens.

use super::test_config;
use crate::config::{BacktestConfig, Strategy, Weighting};

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
        // Two strategies over the same universe, the same window, and the same
        // costs are two hypotheses about what predicts returns, so they cannot
        // share a hash.
        BacktestConfig {
            strategy: Strategy::LowVolatility,
            ..base.clone()
        },
        // The identical selection held at two different sets of weights is two
        // portfolios, not one measured twice. Equal weighting overweights small
        // names, so the two runs are two hypotheses about where an advantage
        // lives rather than one hypothesis measured better.
        BacktestConfig {
            weighting: Weighting::ValueByMarketcap,
            ..base.clone()
        },
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
        // Engaging the liquidity screen changes which names the ranking sees,
        // so the screened variant and the base run are two trials rather than
        // one measured twice.
        BacktestConfig {
            liquidity_floor_fraction: Some(rust_decimal::Decimal::new(2, 1)),
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
        // Supplying an actions file changes what the returns mean, so it has to
        // change the hash. A run with cash dividends and the same run without
        // are two trials, not one measured twice.
        BacktestConfig {
            actions_sha256: Some("1".repeat(64)),
            ..base.clone()
        },
        // Imputing delisting returns changes what an exit is worth, so the run
        // that imputes and the run that does not are two hypotheses about what
        // a delisting cost a holder rather than one measured twice.
        BacktestConfig {
            delisting_convention: Some(crate::config::DELISTING_CONVENTION.to_string()),
            ..base.clone()
        },
        // Which securities the convention reached is a property of the file,
        // not of the rule. A refetch that explains one more name produces a
        // different return series under the identical convention, so the file
        // has to reach the hash on its own or the two runs record as one trial.
        BacktestConfig {
            delistings_sha256: Some("1".repeat(64)),
            ..base.clone()
        },
        // Which market caps the size screen ranked on and the payout leg
        // divided by is a property of the file. A refetch that fills in one
        // more month-end moves which names were held under an identical rule,
        // so the file has to reach the hash on its own.
        BacktestConfig {
            marketcap_sha256: Some("1".repeat(64)),
            ..base.clone()
        },
        // A quarterly schedule and a monthly one over the same signal are two
        // strategies, not one measured twice: they hold different names for
        // different lengths of time and pay different costs to do it.
        BacktestConfig {
            rebalance_every_months: 3,
            ..base.clone()
        },
        // Each window below is a parameter the source fixes and this project
        // refuses to sweep. A sweep would be one trial per point, and a sweep
        // whose points shared a hash would be one trial recorded for all of
        // them, which is the failure rule 2 exists to prevent.
        BacktestConfig {
            volatility_lookback_months: Some(36),
            ..base.clone()
        },
        BacktestConfig {
            payout_share_average_months: Some(24),
            ..base.clone()
        },
        BacktestConfig {
            payout_dividend_trailing_months: Some(12),
            ..base.clone()
        },
        // Engaging the size screen changes which names the ranking sees, on the
        // same rule the liquidity screen is covered under.
        BacktestConfig {
            size_floor_fraction: Some(rust_decimal::Decimal::new(5, 1)),
            ..base.clone()
        },
        // Which filings the book values came from is a property of the file. A
        // refetch that fills in one more filing changes which names were held
        // under an identical rule, so the file has to reach the hash on its own.
        BacktestConfig {
            filings_sha256: Some("1".repeat(64)),
            ..base.clone()
        },
        // How old a filing may be is half the visibility rule. Two runs at 548
        // and at 365 days hold different names out of the same data, so they are
        // two hypotheses about what counts as current rather than one measured
        // twice.
        BacktestConfig {
            book_staleness_days: Some(548),
            ..base.clone()
        },
        // The prices file every run reads. It had no key of its own until the
        // predictions binding needed one to compare against, and a digest that
        // lives only inside another hash can be compared with nothing.
        BacktestConfig {
            prices_sha256: Some("1".repeat(64)),
            ..base.clone()
        },
        // Which predictions the ranking read is the whole of the ridge signal
        // rather than an input to computing it, so the file has to reach the
        // hash on its own. Two fits of the same model over the same panel are
        // two hypotheses about what predicts returns, and without this they
        // would record as one trial.
        BacktestConfig {
            predictions_sha256: Some("1".repeat(64)),
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

/// E9d. The strategy reaches the hash, so the two research programs cannot
/// record as the same configuration.
///
/// `e7` above covers the field by mutation, which proves that *some* change to
/// it moves the hash. This covers the pair that actually gets run: the two
/// constructors, over an identical universe, whose only difference is the
/// strategy. If `lowvol_v0` were a copy of `momentum_v0` the mutation test
/// would still pass and this one would not.
#[test]
fn e9d_the_strategy_field_reaches_the_hash() {
    let universe = "0".repeat(64);
    let momentum = BacktestConfig::momentum_v0(&universe);
    let lowvol = BacktestConfig::lowvol_v0(&universe);

    assert_eq!(momentum.strategy, Strategy::Momentum);
    assert_eq!(
        lowvol.strategy,
        Strategy::LowVolatility,
        "lowvol_v0 does not select the low-volatility strategy"
    );

    // The canonical form carries a lowercase string rather than a nested
    // object, which is what keeps the hashed bytes readable in a diff.
    let canonical = lowvol.canonical_json().expect("serialises");
    assert!(
        canonical.contains(r#""strategy":"low_volatility""#),
        "got {canonical}"
    );
    assert!(
        momentum
            .canonical_json()
            .expect("serialises")
            .contains(r#""strategy":"momentum""#)
    );

    assert_ne!(
        momentum.config_hash().expect("hashes").as_str(),
        lowvol.config_hash().expect("hashes").as_str(),
        "the two research programs hash to the same configuration, so the trial \
         log cannot tell a momentum run from a low-volatility one"
    );
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
        canonical.starts_with(r#"{"actions_sha256":"#),
        "got {canonical}"
    );
    assert!(!canonical.contains(' '), "got {canonical}");

    // The two delisting keys land adjacent, and in an order that reads
    // backwards at a glance: the sort is over bytes, and `_` is 0x5F against
    // `s` at 0x73, so `delisting_convention` precedes `delistings_sha256`.
    // Pinned because a rename that swapped them would move every recorded hash
    // while changing nothing anyone meant to change.
    assert!(
        canonical.contains(r#""delisting_convention":null,"delistings_sha256":null"#),
        "got {canonical}"
    );

    // Both ends of the sorted form pinned rather than only the first. A field
    // added at either end moves every recorded hash, and the prefix pin above
    // is silent about the last key. `weighting` sorts last because `w` is the
    // highest first byte any field name here starts with.
    assert!(
        canonical.ends_with(r#","weighting":"equal"}"#),
        "got {canonical}"
    );

    // The two fields this round added sort inside those ends rather than past
    // them, which is why both pins above still read the same. Asserted rather
    // than assumed: a field named after `weighting` would move the tail pin and
    // a field before `actions_sha256` would move the head one, and either would
    // move every recorded hash while looking like a rename.
    assert!(
        canonical.contains(r#""book_staleness_days":null"#),
        "got {canonical}"
    );
    assert!(
        canonical.contains(r#""filings_sha256":null"#),
        "got {canonical}"
    );
}
