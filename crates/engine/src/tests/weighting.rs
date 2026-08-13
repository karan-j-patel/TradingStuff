//! X-V, the value-weighting variant. Weights, selection, and the wiring.
//!
//! Portfolio construction and cost application are mandatory-test paths under
//! `CLAUDE.md`, so every expected figure below is derived in its comment and the
//! derivation is the test. Nothing here is copied out of a run.
//!
//! # Why the fixtures hold two names of very different size
//!
//! The whole question this variant asks is whether the low-volatility advantage
//! survives when small names stop being overweighted. A fixture whose names had
//! similar market caps would produce weights near one half each, which is what
//! equal weighting also produces, and a broken implementation would pass by
//! coincidence. The caps here are 900 against 100, so equal weighting, value
//! weighting and inverse-value weighting land on three visibly different
//! portfolio returns.
//!
//! # Why the quintile divisor is 1 in the accounting fixtures
//!
//! Every eligible name is then held, so the ranking cannot change what the
//! portfolio contains and these tests measure the weighting and nothing else.
//! X-V4 is the one test about selection, and it sets a divisor that slices.

use ingest::marketcap::MarketCapRecord;
use ingest::schema::AssetKey;
use jiff::civil::Date;
use rust_decimal::Decimal;

use super::{asset, dec, month_ends, panel_of, test_config};
use crate::baseline;
use crate::config::{BacktestConfig, LOWVOL_PROGRAM, Strategy, VARIANT_VALUE_WEIGHTED, Weighting};
use crate::error::EngineError;
use crate::momentum;
use crate::panel::Panel;
use crate::portfolio;
use crate::run;

/// One synthetic name over the six month-ends of [`month_ends`].
struct Named {
    asset: AssetKey,
    /// Split-adjusted month-end closes, which the fixture also uses unadjusted.
    closes: [&'static str; 6],
    /// Market cap at each month-end, in the units the vendor ships. `None` is a
    /// month-end the vendor shipped no row for, which is what a name that cannot
    /// be value-weighted looks like.
    caps: [Option<&'static str>; 6],
}

/// A name with a market cap at every month-end.
fn capped(ticker: &str, permaticker: u64, closes: [&'static str; 6], cap: &'static str) -> Named {
    Named {
        asset: asset(ticker, permaticker),
        closes,
        caps: [Some(cap); 6],
    }
}

/// Build a panel from the names and attach their market caps.
///
/// A month-end with no cap contributes no record at all rather than a record
/// carrying zero, because the two are different states and X-V4 asserts on both.
fn weighted_panel(names: &[Named]) -> Panel {
    let m = month_ends();
    let prices: Vec<(AssetKey, Vec<(Date, Decimal)>)> = names
        .iter()
        .map(|name| {
            (
                name.asset.clone(),
                m.iter()
                    .zip(name.closes)
                    .map(|(day, close)| (*day, dec(close)))
                    .collect(),
            )
        })
        .collect();

    let records: Vec<MarketCapRecord> = names
        .iter()
        .flat_map(|name| {
            m.iter()
                .zip(name.caps)
                .filter_map(|(day, cap)| {
                    cap.map(|cap| MarketCapRecord {
                        asset: name.asset.clone(),
                        date: *day,
                        marketcap: dec(cap),
                        source: "synthetic".to_string(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect();

    panel_of(&prices)
        .with_marketcaps(&records, super::MARKETCAP_SHA256)
        .expect("fixture market caps attach")
}

/// The value-weighting rules over a two-month lead-in.
///
/// `marketcap_sha256` is a placeholder rather than a real digest, because the
/// wiring guard compares presence and not content.
fn value_config(quintile_divisor: usize) -> BacktestConfig {
    BacktestConfig {
        weighting: Weighting::ValueByMarketcap,
        marketcap_sha256: Some(super::MARKETCAP_SHA256.to_string()),
        quintile_divisor,
        ..test_config(2)
    }
}

/// Two names whose caps stand 900 to 100 and whose returns disagree in sign.
///
/// ```text
///           m0     m1     m2     m3     m4     m5
///   BIG    100    100    100    130    130    130     cap 900 throughout
///   SMALL  100    100    100     80     80     80     cap 100 throughout
/// ```
///
/// Value weights are therefore 0.9 and 0.1 at every formation, and the only
/// month with any price movement in it is m2 to m3, where BIG returns +0.3 and
/// SMALL returns -0.2. Those three numbers are what separate the weightings:
///
/// ```text
///   value weight    0.9(0.3) + 0.1(-0.2)  =  0.27 - 0.02  =  0.25
///   equal weight    0.5(0.3) + 0.5(-0.2)  =  0.15 - 0.10  =  0.05
///   inverse value   0.1(0.3) + 0.9(-0.2)  =  0.03 - 0.18  = -0.15
///   unnormalised    900(0.3) + 100(-0.2)  =  270  - 20    =  250
/// ```
fn big_and_small() -> Panel {
    weighted_panel(&[
        capped("BIG", 1, ["100", "100", "100", "130", "130", "130"], "900"),
        capped("SMALL", 2, ["100", "100", "100", "80", "80", "80"], "100"),
    ])
}

/// X-V1. The weight is proportional to market cap, not to its inverse.
///
/// Asserted at both levels. The weight vector itself pins the direction with no
/// accounting in the way, and the portfolio return pins that the vector is the
/// one the run actually held. An inversion moves the first from {0.9, 0.1} to
/// {0.1, 0.9} and the second from +0.25 to -0.15, so the sign of the month
/// changes and not merely its size.
///
/// ```text
///   formation m2:  hold {} -> {BIG:0.9, SMALL:0.1}   traded 1.0   cost 0.0010
///     hold m2->m3: gross = 0.9(0.3) + 0.1(-0.2)                   =  0.25
///     net = (1 - 0.0010)(1.25) - 1 = 0.999 * 1.25 - 1             =  0.24875
///     drift: BIG 0.9(1.3)/1.25 = 0.936,  SMALL 0.1(0.8)/1.25 = 0.064
///
///   formation m3:  hold {0.936, 0.064} -> {0.9, 0.1}
///     traded = |0.9 - 0.936| + |0.1 - 0.064| = 0.036 + 0.036       =  0.072
///     cost = 0.0010 * 0.072                                       =  0.000072
///     hold m3->m4: both flat, gross                                =  0
///     net = (1 - 0.000072)(1) - 1                                 = -0.000072
///
///   formation m4:  weights unchanged, traded 0, cost 0
///     hold m4->m5: both flat, gross 0, net                         =  0
///
///   formation m5:  liquidation, traded 1.0, cost 0.0010, landing on the
///     month just finished:  (1 - 0)(1 - 0.0010) - 1               = -0.001
/// ```
#[test]
fn x_v1_weights_are_proportional_to_market_cap_rather_than_to_its_inverse() {
    let panel = big_and_small();
    let config = value_config(1);
    let m = month_ends();

    let weights = portfolio::weights_at(&panel, config.weighting, &[0, 1], m[2])
        .expect("the fixture weights");
    assert_eq!(
        weights[&0],
        dec("0.9"),
        "the larger name did not take the larger weight, so the weight is not \
         proportional to market cap"
    );
    assert_eq!(weights[&1], dec("0.1"));

    let report = run::backtest(&panel, &config).expect("the fixture backtests");
    assert_eq!(
        report.strategy.gross_monthly,
        vec![dec("0.25"), dec("0"), dec("0")],
        "the held portfolio's return is not the value-weighted combination of \
         its two securities"
    );
    assert_eq!(
        report.strategy.net_monthly,
        vec![dec("0.24875"), dec("-0.000072"), dec("-0.001")]
    );
    assert_eq!(
        report.strategy.traded,
        vec![dec("1"), dec("0.072"), dec("0"), dec("1")],
        "costs are charged on traded weight, so a weighting change must move the \
         notional the rebalance trades"
    );
    assert_eq!(report.total_net_return, dec("0.24741142991"));
}

/// X-V2. The weights are divided by their sum, so a portfolio is one unit of
/// value rather than one unit of the vendor's market cap scale.
///
/// The fixture's caps are 900 and 100, so skipping the divide leaves weights of
/// 900 and 100. Those still stand in the right ratio, which is why the weight
/// direction test above cannot hold this: the ratio is preserved and only the
/// scale is wrong. What the scale breaks is the arithmetic downstream, where a
/// gross return of 250 and a traded notional of 1000 are both a thousand times
/// what a portfolio can produce.
#[test]
fn x_v2_the_weights_are_normalised_to_sum_to_one() {
    let panel = big_and_small();
    let config = value_config(1);
    let m = month_ends();

    let weights = portfolio::weights_at(&panel, config.weighting, &[0, 1], m[2])
        .expect("the fixture weights");
    let total = weights.values().try_fold(Decimal::ZERO, |running, weight| {
        running.checked_add(*weight)
    });
    assert_eq!(
        total,
        Some(Decimal::ONE),
        "the weights do not sum to one, so the portfolio is not fully invested \
         and is not only invested in itself"
    );

    let report = run::backtest(&panel, &config).expect("the fixture backtests");
    assert_eq!(
        report.strategy.gross_monthly[0],
        dec("0.25"),
        "the first month's gross return is not a weighted average, so the \
         weights were used at the vendor's scale rather than normalised"
    );
    assert_eq!(report.strategy.net_monthly[0], dec("0.24875"));
    assert_eq!(
        report.strategy.traded[0],
        dec("1"),
        "an initial purchase moves exactly one unit of portfolio value, and \
         anything else means the weights were not normalised before they were \
         costed"
    );
}

/// X-V3. The weights are the caps at the formation date and nothing later.
///
/// Rule 4. The fixture's caps swap between the formation at m2 and the one at
/// m3, so a weight taken one month late is not a slightly different weight, it
/// is the other name's. The prices are the same as [`big_and_small`], so the
/// only thing that moved is which cap the formation read.
///
/// ```text
///           m0     m1     m2     m3     m4     m5
///   BIG    cap 900  900    900    100    100    100
///   SMALL  cap 100  100    100    900    900    900
///
///   formation m2 reads the m2 caps:  {BIG:0.9, SMALL:0.1}
///     hold m2->m3: gross = 0.9(0.3) + 0.1(-0.2)                   =  0.25
///     reading the m3 caps instead:   {BIG:0.1, SMALL:0.9}
///     hold m2->m3: gross = 0.1(0.3) + 0.9(-0.2)                   = -0.15
///
///   formation m3 reads the m3 caps:  {BIG:0.1, SMALL:0.9}
///     drifted holding is {0.936, 0.064}, so
///     traded = |0.1 - 0.936| + |0.9 - 0.064| = 0.836 + 0.836      =  1.672
///     cost = 0.0010 * 1.672                                       =  0.001672
///     hold m3->m4: both flat, net = (1 - 0.001672)(1) - 1         = -0.001672
///
///   formation m4:  weights unchanged, traded 0, net                =  0
///   formation m5:  liquidation, traded 1.0, landing on the month
///     just finished:  (1 - 0)(1 - 0.0010) - 1                     = -0.001
/// ```
#[test]
fn x_v3_the_weights_read_the_market_cap_at_the_formation_and_nothing_later() {
    let panel = weighted_panel(&[
        Named {
            asset: asset("BIG", 1),
            closes: ["100", "100", "100", "130", "130", "130"],
            caps: [
                Some("900"),
                Some("900"),
                Some("900"),
                Some("100"),
                Some("100"),
                Some("100"),
            ],
        },
        Named {
            asset: asset("SMALL", 2),
            closes: ["100", "100", "100", "80", "80", "80"],
            caps: [
                Some("100"),
                Some("100"),
                Some("100"),
                Some("900"),
                Some("900"),
                Some("900"),
            ],
        },
    ]);
    let m = month_ends();

    // The fixture's discriminating property, asserted rather than assumed. If
    // the caps ever stop swapping between these two month-ends this test cannot
    // tell a formation-date read from a read one month later, and it should say
    // so here rather than pass silently.
    let big = &panel.securities()[0];
    assert_eq!(
        big.marketcap_at_month_end(m[2]).expect("a cap at m2"),
        dec("900")
    );
    assert_eq!(
        big.marketcap_at_month_end(m[3]).expect("a cap at m3"),
        dec("100"),
        "the caps must move between the formation and the month after it, or \
         this test cannot discriminate"
    );

    let report = run::backtest(&panel, &value_config(1)).expect("the fixture backtests");
    assert_eq!(
        report.strategy.gross_monthly,
        vec![dec("0.25"), dec("0"), dec("0")],
        "the first month's return is not the one the formation-date caps \
         produce, so the weighting read a cap dated after the formation"
    );
    assert_eq!(
        report.strategy.net_monthly,
        vec![dec("0.24875"), dec("-0.001672"), dec("-0.001")]
    );
}

/// X-V4. A name that cannot be value-weighted leaves the field before the
/// ranking, not the portfolio after it.
///
/// Both shapes of unweightable are in the fixture, because they are different
/// states in the data and the rule has to cover each. `NOCAP` has no market cap
/// row at the formation at all, which is what the vendor shipping nothing looks
/// like. `ZERO` has a row carrying exactly zero, which the provider really does
/// ship for a company below its quantum of a tenth of a million dollars, and a
/// zero cap divides into no weight.
///
/// The two excluded names carry the fixture's best momentum, so admitting
/// either one changes what is held rather than merely how much of it:
///
/// ```text
///            m0     m1      signal at the m2 formation
///   MID     100    110      0.10
///   BEST    100    120      0.20
///   NOCAP   100    200      1.00      no cap row at m2
///   ZERO    100    150      0.50      a cap row at m2 carrying 0
///
///   excluded before the ranking: eligible {MID, BEST}, a divisor of 2 gives a
///     quintile of 1, and the held name is BEST
///   admitted instead:            eligible {MID, BEST, NOCAP, ZERO}, a quintile
///     of 2, and the held names are NOCAP and ZERO
/// ```
///
/// Excluding after the ranking would produce the same eligible set as admitting
/// and a portfolio of one name drawn from a quintile of four, which is neither
/// of the two above. That is the failure the ordering exists to prevent: the
/// quintile has to be a quintile of the names that can actually be held.
#[test]
fn x_v4_a_name_that_cannot_be_weighted_leaves_before_the_ranking() {
    let panel = weighted_panel(&[
        capped("MID", 1, ["100", "110", "110", "110", "110", "110"], "500"),
        capped("BEST", 2, ["100", "120", "120", "120", "120", "120"], "400"),
        Named {
            asset: asset("NOCAP", 3),
            closes: ["100", "200", "200", "200", "200", "200"],
            caps: [Some("300"), Some("300"), None, None, None, None],
        },
        capped("ZERO", 4, ["100", "150", "150", "150", "150", "150"], "0"),
    ]);
    let config = value_config(2);
    let m = month_ends();

    // The fixture's discriminating properties. A cap that reached back into an
    // earlier month, or a zero that arrived as an absent row, would make the
    // two exclusion arms indistinguishable from each other.
    assert_eq!(
        panel.securities()[2].marketcap_at_month_end(m[2]),
        None,
        "NOCAP must have no market cap at the formation"
    );
    assert_eq!(
        panel.securities()[3].marketcap_at_month_end(m[2]),
        Some(Decimal::ZERO),
        "ZERO must have a market cap row at the formation carrying zero"
    );

    let rebalance = momentum::rebalance_at(&panel, &config, 2).expect("the formation resolves");
    assert_eq!(
        rebalance.eligible,
        vec![0, 1],
        "an unweightable name is still in the field the portfolio is chosen out \
         of, so the quintile is a quintile of names that cannot be held"
    );
    assert_eq!(
        rebalance.chosen,
        vec![1],
        "the portfolio holds a name with no market cap to weight it by"
    );
}

/// X-V5. The weighting reaches the configuration hash as a lowercase string, so
/// the variant and the base run cannot record as one trial.
///
/// `e7` covers the field by mutation, which proves that *some* change to it
/// moves the hash. This covers the pair that actually gets run, on the rule
/// `e9d` follows for the strategy field: the base program and its variant over
/// an identical universe, whose only difference is the weighting.
#[test]
fn x_v5_the_weighting_field_reaches_the_hash() {
    let universe = "0".repeat(64);
    let base = BacktestConfig::lowvol_v0(&universe);
    let variant =
        BacktestConfig::for_program(LOWVOL_PROGRAM, Some(VARIANT_VALUE_WEIGHTED), &universe)
            .expect("the registry lists the value-weighted pair");

    assert_eq!(base.weighting, Weighting::Equal);
    assert_eq!(
        variant.weighting,
        Weighting::ValueByMarketcap,
        "the value-weighted variant does not select value weighting"
    );

    // A lowercase string rather than a nested object, matching the case of
    // every other token in the canonical form.
    let canonical = variant.canonical_json().expect("serialises");
    assert!(
        canonical.contains(r#""weighting":"value_by_marketcap""#),
        "got {canonical}"
    );
    assert!(
        base.canonical_json()
            .expect("serialises")
            .contains(r#""weighting":"equal""#)
    );

    assert_ne!(
        base.config_hash().expect("hashes").as_str(),
        variant.config_hash().expect("hashes").as_str(),
        "the base run and the value-weighted variant hash to the same \
         configuration, so the trial log cannot tell them apart"
    );
}

/// The variant is the base configuration with the weighting changed and nothing
/// else, which is what makes the comparison between them a comparison.
///
/// Written as an equality against a reconstructed base rather than as a field
/// list, so a parameter added to `lowvol_v0` later is covered the moment it
/// exists rather than when somebody remembers to add a line here.
#[test]
fn the_value_weighted_variant_inherits_every_other_parameter() {
    let universe = "0".repeat(64);
    let variant =
        BacktestConfig::for_program(LOWVOL_PROGRAM, Some(VARIANT_VALUE_WEIGHTED), &universe)
            .expect("the registry lists the value-weighted pair");

    assert_eq!(variant.strategy, Strategy::LowVolatility);
    assert_eq!(
        BacktestConfig {
            weighting: Weighting::Equal,
            ..variant
        },
        BacktestConfig::lowvol_v0(&universe),
        "the value-weighted variant changes something other than the weighting, \
         so a difference between it and the base run is not attributable to the \
         weighting alone"
    );
}

/// X-V6. The variant refuses to run without market caps rather than falling
/// back to equal weights.
///
/// The configuration's own market cap hash is absent here, so the existing
/// wiring guard is satisfied and this is the guard that has to fire. That is the
/// state `--variant value-weighted` with no `--marketcap` argument arrives in,
/// and the failure it prevents is an equal-weighted run recorded under the
/// value-weighted hash.
#[test]
fn x_v6_the_variant_refuses_to_run_without_the_market_cap_attachment() {
    let bare = panel_of(
        &[
            ("BIG", 1, ["100", "100", "100", "130", "130", "130"]),
            ("SMALL", 2, ["100", "100", "100", "80", "80", "80"]),
        ]
        .map(|(ticker, permaticker, closes)| {
            (
                asset(ticker, permaticker),
                month_ends()
                    .iter()
                    .zip(closes)
                    .map(|(day, close)| (*day, dec(close)))
                    .collect::<Vec<_>>(),
            )
        }),
    );
    assert!(
        !bare.marketcaps_attached(),
        "the fixture panel must carry no market caps, or this test cannot \
         discriminate"
    );

    let unwired = BacktestConfig {
        marketcap_sha256: None,
        ..value_config(1)
    };
    let error = run::backtest(&bare, &unwired)
        .expect_err("a value-weighted run without market caps produced a figure");
    assert!(
        matches!(error, EngineError::ValueWeightingMissingMarketcaps),
        "the run failed for some other reason than the missing market caps: {error}"
    );

    // The positive control. Without it this test would pass against an engine
    // that refuses every run, which proves nothing about this guard.
    assert!(
        run::backtest(&big_and_small(), &value_config(1)).is_ok(),
        "the same configuration over a panel that does carry market caps must run"
    );
}

/// X-V7. The buy-and-hold baseline is weighted the way the strategy is.
///
/// Comparing a value-weighted strategy against an equal-weighted buy-and-hold
/// confounds the weighting change with the strategy change, and the confounded
/// comparison is the one that looks like a result. Both figures are pinned, so
/// the test fails whichever of the two the baseline is given.
///
/// ```text
///   entry at m2, value weighted, traded 1.0, cost 0.0010
///     m2->m3: gross = 0.9(0.3) + 0.1(-0.2)                        =  0.25
///             net = (1 - 0.0010)(1.25) - 1                        =  0.24875
///     m3->m4: both flat, gross 0, no trading, net                 =  0
///     m4->m5: both flat, then the exit sells 1.0 of notional
///             net = (1 + 0)(1 - 0.0010) - 1                       = -0.001
///     total = 1.24875 * 1 * 0.999 - 1                             =  0.24750125
///
///   the same entry weighted equally instead:
///     m2->m3: gross = 0.5(0.3) + 0.5(-0.2)                        =  0.05
///             net = (1 - 0.0010)(1.05) - 1                        =  0.04895
/// ```
#[test]
fn x_v7_the_buy_and_hold_baseline_is_weighted_like_the_strategy() {
    let panel = big_and_small();
    let config = value_config(1);

    let rebalances: Vec<_> = (2..6)
        .map(|index| momentum::rebalance_at(&panel, &config, index).expect("rebalance"))
        .collect();
    assert_eq!(
        rebalances[0].eligible,
        vec![0, 1],
        "buy-and-hold must enter holding both names"
    );

    let matched = baseline::buy_and_hold(&panel, &config, &rebalances, config.weighting)
        .expect("baseline runs");
    assert_eq!(
        matched.net_monthly,
        vec![dec("0.24875"), dec("0"), dec("-0.001")],
        "the buy-and-hold baseline is not weighted by market cap, so it is not \
         matched to the strategy it is being compared against"
    );
    assert_eq!(matched.total_net_return, dec("0.24750125"));

    let equal = baseline::buy_and_hold(&panel, &config, &rebalances, Weighting::Equal)
        .expect("baseline runs");
    assert_eq!(
        equal.net_monthly[0],
        dec("0.04895"),
        "the equal-weighted reading of the same baseline is not the hand-computed \
         figure, so the two weightings are not actually being distinguished"
    );
}

/// The equal-weighted buy-and-hold survives beside the matched one, because
/// every other program reports against it and gate G2.4 names it.
///
/// Absent under equal weighting, where the matched baseline already is the
/// equal-weighted one and a second copy would suggest a comparison had been
/// made.
#[test]
fn the_equal_weighted_buy_and_hold_is_reported_beside_the_matched_one() {
    let panel = big_and_small();

    let weighted = run::backtest(&panel, &value_config(1)).expect("the fixture backtests");
    assert_eq!(
        weighted.buy_and_hold.net_monthly[0],
        dec("0.24875"),
        "the reported buy-and-hold is not the weighting-matched one"
    );
    let reference = weighted
        .equal_weighted_buy_and_hold
        .as_ref()
        .expect("a weighted run carries the equal-weighted cross-variant reference");
    assert_eq!(
        reference.net_monthly[0],
        dec("0.04895"),
        "the cross-variant reference is not the equal-weighted figure"
    );

    let plain = run::backtest(
        &panel,
        &BacktestConfig {
            weighting: Weighting::Equal,
            ..value_config(1)
        },
    )
    .expect("the fixture backtests");
    assert_eq!(
        plain.buy_and_hold.net_monthly[0],
        dec("0.04895"),
        "an equal-weighted run's own baseline is not the equal-weighted figure"
    );
    assert!(
        plain.equal_weighted_buy_and_hold.is_none(),
        "an equal-weighted run printed its own baseline twice"
    );
}
