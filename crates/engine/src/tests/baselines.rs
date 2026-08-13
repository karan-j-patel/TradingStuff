//! E6. The baselines rule 5 requires, and whether they reproduce.

use super::{asset, dec, month_ends, panel_of, test_config, two_asset_panel};
use crate::baseline::{self, SplitMix64};
use crate::momentum;
use crate::run;

/// E6, first half. Equal-weight buy-and-hold on a hand-computed fixture.
///
/// Both securities move identically, which keeps the drifted weights at exactly
/// one half and makes every figure terminate.
///
/// ```text
///          m0     m1     m2     m3     m4     m5
///   AAA    10     10     10     20     40     80
///   BBB    10     10     10     20     40     80
///
///   entry at m2, equal weight, traded 1.0, cost 0.0010
///     m2->m3: both 20/10 - 1 = 1.0, gross = 0.5(1.0) + 0.5(1.0) = 1.0
///             net = (1 - 0.0010)(2.0) - 1                        = 0.998
///     m3->m4: gross 1.0, no trading, net                         = 1.0
///     m4->m5: gross 1.0, then the exit sells 1.0 of notional
///             net = (2.0)(1 - 0.0010) - 1                        = 0.998
///
///   total = 1.998 * 2 * 1.998 - 1 = 7.984008 - 1                 = 6.984008
/// ```
#[test]
fn e6_buy_and_hold_matches_hand_math() {
    let m = month_ends();
    let prices = ["10", "10", "10", "20", "40", "80"];
    let points: Vec<_> = m.iter().zip(prices).map(|(d, p)| (*d, dec(p))).collect();
    let panel = panel_of(&[(asset("AAA", 1), points.clone()), (asset("BBB", 2), points)]);
    let config = test_config(2);

    let rebalances: Vec<_> = (2..6)
        .map(|index| momentum::rebalance_at(&panel, &config, index).expect("rebalance"))
        .collect();
    assert_eq!(
        rebalances[0].eligible,
        vec![0, 1],
        "buy-and-hold must hold the whole eligible universe"
    );

    let held = baseline::buy_and_hold(&panel, &config, &rebalances, config.weighting)
        .expect("baseline runs");
    assert_eq!(
        held.net_monthly,
        vec![dec("0.998"), dec("1.0"), dec("0.998")]
    );
    assert_eq!(held.total_net_return, dec("6.984008"));
}

/// E6, second half. The random-ranking baseline reproduces from its seed.
///
/// Reproducibility is the whole claim of a seeded baseline, so it is asserted
/// at both levels: the generator itself, and the twenty Sharpes that come out
/// of a full run.
#[test]
fn e6_the_random_baseline_reproduces_from_its_seed() {
    let pool: Vec<usize> = (0..10).collect();

    let first = SplitMix64::new(7).sample(&pool, 4);
    let second = SplitMix64::new(7).sample(&pool, 4);
    assert_eq!(first, second, "the same seed drew a different sample");
    assert_eq!(first.len(), 4);
    assert_eq!(
        first
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4,
        "a draw without replacement repeated a name"
    );

    let different = SplitMix64::new(8).sample(&pool, 4);
    assert_ne!(
        first, different,
        "two seeds drew identically, so the seed is not reaching the draw"
    );

    let panel = two_asset_panel();
    let config = test_config(2);
    let one = run::backtest(&panel, &config).expect("backtests");
    let two = run::backtest(&panel, &config).expect("backtests");

    assert_eq!(one.random.sharpes.len(), config.random_draws);
    assert_eq!(
        one.random.sharpes, two.random.sharpes,
        "two runs of the same configuration produced different random baselines"
    );
    assert_eq!(one.random.mean_sharpe, two.random.mean_sharpe);
}

/// The sentence rule 5's third baseline is replaced by, pinned so it cannot
/// quietly become a fabricated comparison.
#[test]
fn the_linear_baseline_is_stated_rather_than_invented() {
    assert_eq!(
        baseline::LINEAR_BASELINE_NOTE,
        "The linear-model baseline degenerates to the signal itself with one feature."
    );
}
