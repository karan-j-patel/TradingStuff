//! X-C, the conservative formula. Selection, both legs, and the wiring.
//!
//! Selection and cost application are mandatory-test paths under `CLAUDE.md`,
//! so every expected figure below is derived in its comment and the derivation
//! is the test. Nothing here is copied out of a run.
//!
//! # Why the windows are short
//!
//! The real configuration looks back thirty-six months and averages twenty-four
//! month-ends of market cap, which would mean thirty-seven hand-computed prices
//! and caps per security to test one formation. The windows are shortened here
//! and the *rules* are what is under test. Whether
//! [`BacktestConfig::conservative_v0`] itself carries the source's numbers is a
//! separate question, and `the_conservative_program_carries_the_sources_windows`
//! is the test that asks it, exactly as `e9d` does for the strategy field.
//!
//! # Why the fixtures are not flat
//!
//! Each fixture gives its names distinct volatilities, distinct market caps and
//! distinct momentum, and the winner is never the first name in identity order
//! unless a comment says the tie-break put it there. A fixture whose names
//! agreed would let a broken implementation pass by coincidence, which is how a
//! previous round in this repository went blind.

use ingest::actions::Delisting;
use ingest::marketcap::MarketCapRecord;
use ingest::schema::AssetKey;
use jiff::civil::{Date, date};
use rust_decimal::Decimal;

use super::{asset, bar, dec};
use crate::config::{BacktestConfig, DELISTING_CONVENTION, Strategy};
use crate::conservative;
use crate::error::EngineError;
use crate::momentum;
use crate::panel::Panel;
use crate::run;

/// Ten month-ends, the last trading day of each month. Real calendar dates, so
/// the month-end derivation is exercised rather than bypassed by numbers that
/// are all the 31st.
///
/// Ten is the shortest length that supports the stride test: a lead-in of three
/// leaves formations at 3, 6 and 9 under a quarterly stride, which is the two
/// the loop needs plus one.
fn month_ends() -> [Date; 10] {
    [
        date(2020, 1, 31),
        date(2020, 2, 28),
        date(2020, 3, 31),
        date(2020, 4, 30),
        date(2020, 5, 29),
        date(2020, 6, 30),
        date(2020, 7, 31),
        date(2020, 8, 31),
        date(2020, 9, 30),
        date(2020, 10, 30),
    ]
}

/// One synthetic security over [`month_ends`].
struct CfSeries {
    asset: AssetKey,
    /// Split-adjusted month-end closes.
    closes: [&'static str; 10],
    /// Unadjusted month-end closes. Equal to `closes` unless the fixture is
    /// exercising a split.
    unadjusted: [&'static str; 10],
    /// Market cap at each month-end, in the units the vendor ships.
    caps: [&'static str; 10],
    /// Month-end indices this name has no bar and no market cap on at all,
    /// which is what a name that had not listed yet looks like.
    absent: &'static [usize],
}

/// A name whose adjusted and unadjusted closes agree, present at every
/// month-end.
fn plain(
    ticker: &str,
    permaticker: u64,
    closes: [&'static str; 10],
    caps: [&'static str; 10],
) -> CfSeries {
    CfSeries {
        asset: asset(ticker, permaticker),
        closes,
        unadjusted: closes,
        caps,
        absent: &[],
    }
}

/// Build a panel with all three datasets attached.
///
/// Dividends and delistings may both be empty. Attaching an empty set still
/// sets the panel's flag, which is the state the conservative formula requires
/// and is different from never having attached at all.
fn cf_panel(names: &[CfSeries], dividends: &[(AssetKey, Date, Decimal)]) -> Panel {
    let days = month_ends();
    let mut bars = Vec::new();
    let mut caps = Vec::new();
    for name in names {
        for (index, day) in days.iter().enumerate() {
            if name.absent.contains(&index) {
                continue;
            }
            bars.push(bar(
                &name.asset,
                *day,
                dec(name.closes[index]),
                dec(name.unadjusted[index]),
            ));
            caps.push(MarketCapRecord {
                asset: name.asset.clone(),
                date: *day,
                marketcap: dec(name.caps[index]),
                source: "synthetic".to_string(),
            });
        }
    }

    let records: Vec<ingest::actions::ActionRecord> = dividends
        .iter()
        .map(|(asset, ex_date, amount)| super::cash_dividend(asset, *ex_date, *amount))
        .collect();
    let no_delistings: [Delisting; 0] = [];
    Panel::from_bars(bars)
        .expect("fixture panel builds")
        .with_dividends(&records)
        .expect("fixture dividends attach")
        .with_delistings(&no_delistings)
        .expect("fixture delistings attach")
        .with_marketcaps(&caps)
        .expect("fixture market caps attach")
}

/// The conservative rules with every window shortened to fit ten month-ends.
///
/// A lead-in of `max(2, 3, 2) = 3`, so the first formation is at index 3.
/// The size screen is off by default and switched on by the one test that is
/// about it, so a test of the ranking fails only when the ranking is wrong.
fn conservative_config() -> BacktestConfig {
    BacktestConfig {
        strategy: Strategy::ConservativeFormula,
        signal_lookback_months: 2,
        signal_skip_months: 1,
        volatility_lookback_months: Some(3),
        payout_share_average_months: Some(2),
        payout_dividend_trailing_months: Some(1),
        size_floor_fraction: None,
        rebalance_every_months: 3,
        price_floor: Decimal::ZERO,
        ..BacktestConfig::momentum_v0("0".repeat(64))
    }
}

/// The same configuration with the three file hashes the wiring guards demand.
///
/// The values are placeholders rather than real digests, because what the
/// guards compare is presence and not content.
fn wired(config: BacktestConfig) -> BacktestConfig {
    BacktestConfig {
        actions_sha256: Some("a".repeat(64)),
        delisting_convention: Some(DELISTING_CONVENTION.to_string()),
        delistings_sha256: Some("d".repeat(64)),
        marketcap_sha256: Some("m".repeat(64)),
        ..config
    }
}

/// Five names, three calm and two wild, whose two legs disagree by design.
///
/// Closes at the four month-ends the formation at index 3 reads, then a tail
/// that keeps every name alive and moving:
///
/// ```text
///   name      m0    m1    m2    m3     dividend at m3   DY
///   ISSUER   100   100   105   105          1.05        0.01
///   BUYBACK  100   100   110   110          1.10        0.01
///   NEUTRAL  100   100   103   103          1.03        0.01
///   HIGHA    100   100   130   130          none        0
///   HIGHB    100   100   150   150          none        0
/// ```
///
/// The three dividends are proportional to their closes, so the yields are
/// exactly equal at 0.01 and the payout leg is moved only by the share ratio.
///
/// **Volatility**, over the three monthly total returns from m0 to m3. The
/// dividend enters the last one, which is what makes these total returns:
///
/// ```text
///   ISSUER   0, 0.05, (105 + 1.05)/105 - 1 = 0.01     sd 0.0264575
///   BUYBACK  0, 0.10, (110 + 1.10)/110 - 1 = 0.01     sd 0.0550757
///   NEUTRAL  0, 0.03, (103 + 1.03)/103 - 1 = 0.01     sd 0.0152753
///   HIGHA    0, 0.30, 0                               sd 0.1732051
///   HIGHB    0, 0.50, 0                               sd 0.2886751
/// ```
///
/// So the calm half of five is the three names `{NEUTRAL, ISSUER, BUYBACK}`,
/// which in identity order is `[0, 1, 2]`, and the two wild names are outside
/// it. None of the five ties with another.
///
/// **Momentum**, `close(m2) / close(m1) - 1`: ISSUER 0.05, BUYBACK 0.10,
/// NEUTRAL 0.03, HIGHA 0.30, HIGHB 0.50.
///
/// **Share ratio.** Every name's market cap over its adjusted close is exactly
/// 10 at m1 and at m2, so the two-month average is 10 for all of them and the
/// ratio is the m3 level over 10:
///
/// ```text
///   name      cap(m3)   close(m3)   level   change   NPY = DY - change
///   ISSUER      1260       105        12      +0.2         -0.19
///   BUYBACK      880       110         8      -0.2         +0.21
///   NEUTRAL     1030       103        10       0.0         +0.01
///   HIGHA       1950       130        15      +0.5         -0.50
///   HIGHB        750       150         5      -0.5         +0.50
/// ```
fn five_names() -> (Panel, Vec<(AssetKey, Date, Decimal)>) {
    let names = [
        plain(
            "ISSUER",
            1,
            [
                "100", "100", "105", "105", "106", "107", "108", "109", "110", "111",
            ],
            [
                "1000", "1000", "1050", "1260", "1060", "1070", "1080", "1090", "1100", "1110",
            ],
        ),
        plain(
            "BUYBACK",
            2,
            [
                "100", "100", "110", "110", "111", "112", "113", "114", "115", "116",
            ],
            [
                "1000", "1000", "1100", "880", "1110", "1120", "1130", "1140", "1150", "1160",
            ],
        ),
        plain(
            "NEUTRAL",
            3,
            [
                "100", "100", "103", "103", "104", "105", "106", "107", "108", "109",
            ],
            [
                "1000", "1000", "1030", "1030", "1040", "1050", "1060", "1070", "1080", "1090",
            ],
        ),
        plain(
            "HIGHA",
            4,
            [
                "100", "100", "130", "130", "131", "132", "133", "134", "135", "136",
            ],
            [
                "1000", "1000", "1300", "1950", "1310", "1320", "1330", "1340", "1350", "1360",
            ],
        ),
        plain(
            "HIGHB",
            5,
            [
                "100", "100", "150", "150", "151", "152", "153", "154", "155", "156",
            ],
            [
                "1000", "1000", "1500", "750", "1510", "1520", "1530", "1540", "1550", "1560",
            ],
        ),
    ];
    let m = month_ends();
    let dividends = vec![
        (names[0].asset.clone(), m[3], dec("1.05")),
        (names[1].asset.clone(), m[3], dec("1.10")),
        (names[2].asset.clone(), m[3], dec("1.03")),
    ];
    (cf_panel(&names, &dividends), dividends)
}

/// X-C1. The calm half is kept and the wild half is not.
///
/// Inside the calm half `[0, 1, 2]` the two legs rank as follows, best first
/// and ranks starting at zero:
///
/// ```text
///   momentum   BUYBACK 0.10 -> 0   ISSUER 0.05 -> 1   NEUTRAL 0.03 -> 2
///   payout     BUYBACK 0.21 -> 0   NEUTRAL 0.01 -> 1  ISSUER -0.19 -> 2
///   average    BUYBACK 0           ISSUER 1.5         NEUTRAL 1.5
/// ```
///
/// Three names give a held count of `max(3 / 5, 1) = 1`, so `BUYBACK`, at
/// index 1, is the portfolio. It is neither the first name in identity order
/// nor the calmest, so an implementation that took either would fail here.
///
/// Taking the wild half instead gives `{HIGHB, HIGHA, BUYBACK}`, where HIGHB
/// leads both legs and is held at index 4.
#[test]
fn xc1_the_calm_half_is_the_one_kept() {
    let (panel, _) = five_names();
    let formed =
        momentum::rebalance_at(&panel, &conservative_config(), 3).expect("the formation forms");

    assert_eq!(
        formed.eligible,
        vec![0, 1, 2, 3, 4],
        "every name clears the coverage rule and has a market cap at the formation"
    );
    assert_eq!(
        formed.census.vol_half, 3,
        "the calm half of five names is three, taking the inclusive side"
    );
    assert_eq!(
        formed.chosen,
        vec![1],
        "the held name is not BUYBACK, so the wild half was kept, or the split \
         did not happen at all"
    );
}

/// X-C2. Issuance lowers the payout yield and a buyback raises it.
///
/// `ISSUER` and `BUYBACK` carry exactly equal dividend yields and opposite
/// share changes of twenty percent, so the sign of the subtraction is the only
/// thing separating them. Under the correct sign the payout ranking is
/// `BUYBACK, NEUTRAL, ISSUER` and `BUYBACK` leads both legs, so it is held.
///
/// Adding the share change instead of subtracting it gives payouts of
/// `ISSUER +0.21, NEUTRAL +0.01, BUYBACK -0.19`, so the payout ranking reverses
/// to `ISSUER, NEUTRAL, BUYBACK` while momentum is untouched:
///
/// ```text
///   average    ISSUER (1 + 0)/2 = 0.5   BUYBACK (0 + 2)/2 = 1   NEUTRAL 1.5
/// ```
///
/// and `ISSUER` at index 0 is held instead. No tie decides either answer.
#[test]
fn xc2_issuance_lowers_the_payout_yield_and_a_buyback_raises_it() {
    let (panel, _) = five_names();
    let month_ends = panel.month_ends();
    let config = conservative_config();

    // The two legs as values, so a failure says which of them moved rather than
    // only that the portfolio changed.
    let payout = |position: usize| {
        conservative::net_payout_yield(&panel.securities()[position], month_ends, 3, 2, 1)
            .expect("the arithmetic holds")
            .expect("both legs are computable")
    };
    assert_eq!(
        payout(0),
        dec("-0.19"),
        "the issuer's payout yield is not its 0.01 dividend yield less a 0.2 \
         share increase"
    );
    assert_eq!(
        payout(1),
        dec("0.21"),
        "the repurchaser's payout yield is not its 0.01 dividend yield plus a \
         0.2 share reduction"
    );
    assert_eq!(
        payout(2),
        dec("0.01"),
        "a flat share count is not a zero term"
    );

    let formed = momentum::rebalance_at(&panel, &config, 3).expect("the formation forms");
    assert_eq!(
        formed.chosen,
        vec![1],
        "the repurchaser is not held, so issuance is being rewarded"
    );
}

/// The split fixture. Three names, one of which splits two-for-one between m2
/// and m3 while issuing nothing.
///
/// ```text
///   name     basis         m0    m1    m2    m3
///   PLAIN    adjusted     100   100   105   105
///            unadjusted   100   100   105   105
///   SPLIT    adjusted     100   100   110   110
///            unadjusted   200   200   220   110
///   NOISY    adjusted     100   100   140   140
/// ```
///
/// `SPLIT` has ten million shares before the split and twenty after, so its
/// market cap is unchanged by the event: `200 * 10` and `110 * 20` are both
/// 2,200 at the month-ends either side of it. Market cap over the *adjusted* close
/// is therefore 20 at every month-end and the share change is exactly zero.
/// Market cap over the *unadjusted* close is 10 before the split and 20 after,
/// which reads as a hundred percent issuance that never happened.
///
/// `PLAIN` really does issue: its level goes 10, 10, 15, a change of +0.5.
/// Neither pays a dividend, so every payout yield here is the share term alone.
///
/// Volatilities are `0.0288675`, `0.0577350` and `0.2309401`, so the calm half
/// of three is `{PLAIN, SPLIT}` in identity order `[0, 1]`.
fn split_names() -> Panel {
    let names = [
        plain(
            "PLAIN",
            1,
            [
                "100", "100", "105", "105", "106", "107", "108", "109", "110", "111",
            ],
            [
                "1000", "1000", "1050", "1575", "1060", "1070", "1080", "1090", "1100", "1110",
            ],
        ),
        CfSeries {
            asset: asset("SPLIT", 2),
            closes: [
                "100", "100", "110", "110", "111", "112", "113", "114", "115", "116",
            ],
            unadjusted: [
                "200", "200", "220", "110", "111", "112", "113", "114", "115", "116",
            ],
            caps: [
                "2000", "2000", "2200", "2200", "2220", "2240", "2260", "2280", "2300", "2320",
            ],
            absent: &[],
        },
        plain(
            "NOISY",
            3,
            [
                "100", "100", "140", "140", "141", "142", "143", "144", "145", "146",
            ],
            [
                "1000", "1000", "1400", "1400", "1410", "1420", "1430", "1440", "1450", "1460",
            ],
        ),
    ];
    cf_panel(&names, &[])
}

/// X-C3. The share ratio is taken on the adjusted close, so a split is not
/// issuance.
///
/// ```text
///   momentum   SPLIT 0.10 -> 0    PLAIN 0.05 -> 1
///   payout     SPLIT  0.00 -> 0   PLAIN -0.50 -> 1
///   average    SPLIT 0            PLAIN 1
/// ```
///
/// so `SPLIT`, at index 1, is held. Computing the ratio on the unadjusted close
/// instead gives `SPLIT` a payout of -1.0 against `PLAIN`'s -0.5, which
/// reverses the payout ranking and leaves both names on an average of 0.5. The
/// identity tie-break then hands the portfolio to `PLAIN` at index 0.
#[test]
fn xc3_a_split_is_not_issuance() {
    let panel = split_names();
    let month_ends = panel.month_ends();

    assert_eq!(
        conservative::share_change(&panel.securities()[1], month_ends, 3, 2),
        Some(Decimal::ZERO),
        "the two-for-one split moved the share ratio, so the ratio is being \
         taken on the raw price rather than on the adjusted one"
    );
    assert_eq!(
        conservative::share_change(&panel.securities()[0], month_ends, 3, 2),
        Some(dec("0.5")),
        "a real fifty percent issuance is not being seen, so the fixture cannot \
         tell a phantom one from a real one"
    );

    let formed =
        momentum::rebalance_at(&panel, &conservative_config(), 3).expect("the formation forms");
    assert_eq!(
        formed.chosen,
        vec![1],
        "the splitting name is not held, so its split was scored as dilution"
    );
}

/// The stride fixture. Five names on saw-tooth paths, so every name's
/// volatility is the same at every formation and the calm half never moves.
///
/// Name `k` steps up by `g_k` in every second month and is flat in between, so
/// each three-month window sees the returns `0, g - 1, 0` or `g - 1, 0, g - 1`
/// and the sample deviation is the same either way:
///
/// ```text
///   name    g       month-end closes                                    sd
///   SLOW    1.05    100 100 105 105 110.25 110.25 115.7625 ...          0.0288675
///   TENTH   1.10    100 100 110 110 121 121 133.1 133.1 146.41 146.41   0.0577350
///   FIFTH   1.20    100 100 120 120 144 144 172.8 172.8 207.36 207.36   0.1154701
///   WIDE    1.40    100 100 140 140 196 196 274.4 274.4 384.16 384.16   0.2309401
///   WIDEST  1.80    100 100 180 180 324 324 583.2 583.2 1049.76 ...     0.4618802
/// ```
///
/// Market caps are ten times the close at every month-end, so every share ratio
/// is exactly one and every payout yield is zero. With no dividends either, the
/// payout ranking is the identity order at every formation.
///
/// The calm half of five is `{SLOW, TENTH, FIFTH}`. At a formation whose
/// momentum window straddles a step the momentum ranking is the reverse of the
/// identity order and all three names average to 1, which the identity break
/// resolves to `SLOW`; at one that does not, every momentum is zero and the
/// ranking is the identity order, which puts `SLOW` first outright. Either way
/// `SLOW`, at index 0, is held, so the portfolio is the same at every formation
/// and the only thing the stride can change is how many formations there are.
///
/// `SLOW` is deliberately not flat. A held name that never moved would leave the
/// monthly series with no dispersion once the marks were coarsened, and a
/// mutation that coarsens them would then fail by producing no Sharpe at all
/// rather than by producing the wrong number of observations, which is the thing
/// actually under test.
/// The stride fixture with `SLOW` stopping trading after m4, which is inside
/// the first holding quarter rather than at a formation.
///
/// This is the only shape that reaches the carried-forward branch in
/// [`crate::portfolio::advance`]. Under a monthly stride a holding is always
/// re-formed at the month-end after it stopped, so the branch is unreachable
/// and the delisting tests never touch it. Under a quarterly one the portfolio
/// has to be marked twice more before the next formation can sell it.
fn stride_names_with_a_mid_quarter_exit() -> Panel {
    let full = stride_series();
    let names: Vec<CfSeries> = full
        .into_iter()
        .map(|series| {
            if series.asset.ticker == "SLOW" {
                CfSeries {
                    absent: &[5, 6, 7, 8, 9],
                    ..series
                }
            } else {
                series
            }
        })
        .collect();
    cf_panel(&names, &[])
}

/// The mid-quarter exit is marked once, carried at that mark, and sold at the
/// next formation.
///
/// `SLOW` prints its last bar at m4 and the portfolio is marked at m5 and at m6
/// before the formation at index 6 can trade out of it:
///
/// ```text
///   m3 -> m4   SLOW 110.25 / 105 - 1 = 0.05, less the entry cost   0.04895
///   m4 -> m5   no bar at m5, so the holding exits at its last close
///              of 110.25 with no imputation: 110.25 / 110.25 - 1        0
///   m5 -> m6   no bar at m4's successor either, so the position is
///              carried at the mark it was already marked out at         0
///   m6 -> m7   TENTH is formed instead, a full rotation costing
///              0.002 on two units of traded notional                -0.002
///   m7 -> m8   TENTH 146.41 / 133.1 - 1                                0.1
///   m8 -> m9   0, then the liquidation's 0.001                      -0.001
/// ```
///
/// The exit is counted once. Counting it again at m5 would report two delisting
/// exits for one delisting, and every figure resting on that census would be
/// double.
#[test]
fn a_holding_that_exits_mid_quarter_is_marked_once_and_carried() {
    let panel = stride_names_with_a_mid_quarter_exit();
    let report =
        run::backtest(&panel, &wired(conservative_config())).expect("the fixture backtests");

    assert_eq!(
        report.strategy.exits.unexplained, 1,
        "the mid-quarter exit was counted a second time when the portfolio was \
         marked again the month after it"
    );
    assert_eq!(report.strategy.delisting_exits, 1);
    assert_eq!(
        report.strategy.traded,
        vec![dec("1"), dec("2"), dec("1")],
        "the exited name was not sold at the next formation, so the rotation out \
         of it went uncharged"
    );
    assert_eq!(
        report.strategy.net_monthly,
        vec![
            dec("0.04895"),
            dec("0"),
            dec("0"),
            dec("-0.002"),
            dec("0.1"),
            dec("-0.001")
        ],
        "the carried position did not hold its mark flat between the exit and \
         the formation that sold it"
    );
}

/// The five names of the stride fixture, before any of them is made to exit.
fn stride_series() -> Vec<CfSeries> {
    vec![
        plain(
            "SLOW",
            1,
            [
                "100",
                "100",
                "105",
                "105",
                "110.25",
                "110.25",
                "115.7625",
                "115.7625",
                "121.550625",
                "121.550625",
            ],
            [
                "1000",
                "1000",
                "1050",
                "1050",
                "1102.5",
                "1102.5",
                "1157.625",
                "1157.625",
                "1215.50625",
                "1215.50625",
            ],
        ),
        plain(
            "TENTH",
            2,
            [
                "100", "100", "110", "110", "121", "121", "133.1", "133.1", "146.41", "146.41",
            ],
            [
                "1000", "1000", "1100", "1100", "1210", "1210", "1331", "1331", "1464.1", "1464.1",
            ],
        ),
        plain(
            "FIFTH",
            3,
            [
                "100", "100", "120", "120", "144", "144", "172.8", "172.8", "207.36", "207.36",
            ],
            [
                "1000", "1000", "1200", "1200", "1440", "1440", "1728", "1728", "2073.6", "2073.6",
            ],
        ),
        plain(
            "WIDE",
            4,
            [
                "100", "100", "140", "140", "196", "196", "274.4", "274.4", "384.16", "384.16",
            ],
            [
                "1000", "1000", "1400", "1400", "1960", "1960", "2744", "2744", "3841.6", "3841.6",
            ],
        ),
        plain(
            "WIDEST",
            5,
            [
                "100", "100", "180", "180", "324", "324", "583.2", "583.2", "1049.76", "1049.76",
            ],
            [
                "1000", "1000", "1800", "1800", "3240", "3240", "5832", "5832", "10497.6",
                "10497.6",
            ],
        ),
    ]
}

fn stride_names() -> Panel {
    cf_panel(&stride_series(), &[])
}

/// X-C4. The stride moves the formations and leaves the marks monthly.
///
/// A lead-in of three over ten month-ends puts the quarterly formations at
/// indices 3, 6 and 9, which is three of them. The same panel at a stride of
/// one has formations at 3 through 9, which is seven. Both mark the portfolio
/// at every month-end from 3 to 9, which is six monthly returns, and that is
/// the property the annualisation rests on: a stride that coarsened the marks
/// would leave two quarterly observations here rather than six monthly ones.
///
/// `SLOW` is held at every formation and is a single name at full weight, so
/// the drifted weights are unchanged by its price and the only trading is
/// getting in and getting out:
///
/// ```text
///   traded, quarterly   1.0 at the first formation, 0 at the second,
///                       1.0 at the liquidation                    -> [1, 0, 1]
///   traded, monthly     the same two trades spread over seven formations
///                                                     -> [1, 0, 0, 0, 0, 0, 1]
/// ```
///
/// `SLOW` steps up five percent in every second month, so the six gross monthly
/// returns from m3 to m9 are `0.05, 0, 0.05, 0, 0.05, 0`. The entry cost lands
/// on the first of them and the liquidation on the last:
///
/// ```text
///   m3 -> m4   (1 - 0.001)(1.05) - 1 = 0.04895
///   m4 -> m5   0
///   m5 -> m6   0.05
///   m6 -> m7   0            nothing traded at the second formation
///   m7 -> m8   0.05
///   m8 -> m9   (1 - 0.001)(1 + 0) - 1 = -0.001
/// ```
///
/// The monthly series is identical under both strides, which is why the pinned
/// assertions are the formation count and the traded vector rather than the
/// returns. What the returns pin instead is that there are six of them: marking
/// once per formation would leave two, and two quarterly returns annualised by
/// the square root of twelve is the failure the stride was written to avoid.
#[test]
fn xc4_the_stride_moves_the_formations_and_not_the_marks() {
    let panel = stride_names();
    let config = wired(conservative_config());
    let report = run::backtest(&panel, &config).expect("the fixture backtests");

    let m = month_ends();
    assert_eq!(report.first_rebalance, m[3]);
    assert_eq!(report.last_rebalance, m[9]);
    assert_eq!(
        report.formations.len(),
        3,
        "a quarterly stride over these ten month-ends forms three times, and a \
         different count means the loop stepped by something other than the \
         configured stride"
    );
    assert_eq!(
        report.strategy.traded,
        vec![dec("1"), dec("0"), dec("1")],
        "the traded notional is not one entry per formation, so turnover is \
         being reported over a different schedule from the one that traded"
    );
    assert_eq!(
        report.months, 6,
        "the marks are not monthly, so the sqrt(12) annualisation is being \
         applied to a series that is not a monthly one"
    );
    assert_eq!(
        report.strategy.gross_monthly,
        vec![
            dec("0.05"),
            dec("0"),
            dec("0.05"),
            dec("0"),
            dec("0.05"),
            dec("0")
        ],
        "the marks are not one per month-end between the formations"
    );
    assert_eq!(
        report.strategy.net_monthly,
        vec![
            dec("0.04895"),
            dec("0"),
            dec("0.05"),
            dec("0"),
            dec("0.05"),
            dec("-0.001")
        ],
        "the cost did not land on the month the trade happened in"
    );
    assert_eq!(
        report.buy_and_hold.net_monthly.len(),
        report.months,
        "the baseline is sampled at a different frequency from the strategy, so \
         the two annualised Sharpes printed beside each other are not comparable"
    );
}

/// X-C5. The lowest average rank wins, and both legs are ranked best-first.
///
/// The averages inside the calm half are `BUYBACK 0`, `ISSUER 1.5` and
/// `NEUTRAL 1.5`. Taking the largest average instead hands the portfolio to the
/// tied pair, which the identity break resolves to `ISSUER` at index 0. Ranking
/// momentum from the worst return up instead reverses that leg to
/// `NEUTRAL 0, ISSUER 1, BUYBACK 2`, which averages to `NEUTRAL 0.5`,
/// `BUYBACK 1`, `ISSUER 1.5` and holds `NEUTRAL` at index 2. Neither is index 1.
#[test]
fn xc5_the_lowest_average_of_two_best_first_ranks_wins() {
    let (panel, _) = five_names();
    let formed =
        momentum::rebalance_at(&panel, &conservative_config(), 3).expect("the formation forms");

    // The averages themselves, so a failure says whether the combination or the
    // direction moved.
    let mut averages = formed.signals.clone();
    averages.sort_by_key(|(position, _)| *position);
    assert_eq!(
        averages,
        vec![(0, dec("1.5")), (1, dec("0")), (2, dec("1.5")),],
        "the averaged ranks are not the ones the two legs produce"
    );
    assert_eq!(
        formed.chosen,
        vec![1],
        "the best average is not held, so the composite is being read from the \
         wrong end"
    );
}

/// The eligibility-bound fixture. `SHORT` has no bar at all in the window's
/// first calendar month and a full set from the second on.
///
/// The gap is placed at the *first* month-end deliberately. The coverage rule
/// counts bars in the half-open window `(m0, m3]`, which contains nothing from
/// January at all, so `SHORT` has the same three bars in it as everyone else
/// and clears the eighty percent requirement outright. The only rule it fails
/// is the one that requires a close at every month-end the window spans. A
/// fixture whose gap was anywhere else would be caught by coverage first and
/// would say nothing about the bound.
fn gap_names() -> Panel {
    let names = [
        plain(
            "FULL",
            1,
            [
                "100", "100", "105", "105", "106", "107", "108", "109", "110", "111",
            ],
            [
                "1000", "1000", "1050", "1050", "1060", "1070", "1080", "1090", "1100", "1110",
            ],
        ),
        CfSeries {
            asset: asset("SHORT", 2),
            closes: [
                "100", "100", "110", "110", "111", "112", "113", "114", "115", "116",
            ],
            unadjusted: [
                "100", "100", "110", "110", "111", "112", "113", "114", "115", "116",
            ],
            caps: [
                "1000", "1000", "1100", "1100", "1110", "1120", "1130", "1140", "1150", "1160",
            ],
            absent: &[0],
        },
        plain(
            "OTHER",
            3,
            [
                "100", "100", "140", "140", "141", "142", "143", "144", "145", "146",
            ],
            [
                "1000", "1000", "1400", "1400", "1410", "1420", "1430", "1440", "1450", "1460",
            ],
        ),
    ];
    cf_panel(&names, &[])
}

/// X-C6. A name one month-end short of the full window is refused, and one with
/// exactly the full window is admitted.
///
/// Both directions are asserted from the same fixture, because a rule that
/// admitted nothing would satisfy the refusal half on its own. Relaxing the
/// bound by a month, which is a single token in the window's start index,
/// admits `SHORT` and fails this.
#[test]
fn xc6_a_name_short_of_the_full_window_is_refused() {
    let panel = gap_names();
    let formed =
        momentum::rebalance_at(&panel, &conservative_config(), 3).expect("the formation forms");

    assert_eq!(
        formed.eligible,
        vec![0, 2],
        "SHORT has no close at the first month-end of the window and must be \
         refused, while FULL and OTHER have every one and must be admitted"
    );
    assert_eq!(formed.census.eligible, 2);
    assert!(
        panel.securities()[1]
            .close_at_month_end(month_ends()[3])
            .is_some(),
        "SHORT does not trade at the formation date, so it would be refused by \
         a rule other than the one under test"
    );
}

/// X-C7. The three wiring guards and the conservative formula's refusal to run
/// without its inputs.
///
/// Each case asserts the *variant* rather than only that an error came back. A
/// bypass of the guards does not make these runs succeed, it makes them fail
/// somewhere further in with a different error, so an `is_err` assertion would
/// pass under the very mutation this exists to catch.
#[test]
fn xc7_the_wiring_guards_refuse_a_run_that_would_be_mislabelled() {
    let panel = stride_names();
    let config = wired(conservative_config());

    // (a) A configuration naming a market cap file over a panel carrying none.
    let bare = {
        let names = [plain(
            "FLAT",
            1,
            [
                "100", "100", "100", "100", "100", "100", "100", "100", "100", "100",
            ],
            [
                "1000", "1000", "1000", "1000", "1000", "1000", "1000", "1000", "1000", "1000",
            ],
        )];
        let days = month_ends();
        let bars = days
            .iter()
            .map(|day| bar(&names[0].asset, *day, dec("100"), dec("100")))
            .collect();
        let no_delistings: [Delisting; 0] = [];
        Panel::from_bars(bars)
            .expect("panel builds")
            .with_dividends(&[])
            .expect("dividends attach")
            .with_delistings(&no_delistings)
            .expect("delistings attach")
    };
    assert!(
        matches!(
            run::backtest(&bare, &config),
            Err(EngineError::MarketcapWiringMismatch { .. })
        ),
        "a configuration recording a market cap file over a panel carrying none \
         was allowed to run"
    );

    // (b) The mirror: a panel carrying market caps under a configuration that
    // records none, so nothing in the log would say which file was ranked on.
    assert!(
        matches!(
            run::backtest(
                &panel,
                &BacktestConfig {
                    marketcap_sha256: None,
                    ..config.clone()
                }
            ),
            Err(EngineError::MarketcapWiringMismatch { .. })
        ),
        "a run ranked on attached market caps without recording which file they \
         came from"
    );

    // (c) The conservative formula without its payout leg. The panel here
    // carries market caps and delistings but no dividends, which is a strategy
    // whose net payout yield is a share count with no payout in it.
    let no_dividends = {
        let days = month_ends();
        let name = asset("FLAT", 1);
        let bars = days
            .iter()
            .map(|day| bar(&name, *day, dec("100"), dec("100")))
            .collect();
        let caps: Vec<MarketCapRecord> = days
            .iter()
            .map(|day| MarketCapRecord {
                asset: name.clone(),
                date: *day,
                marketcap: dec("1000"),
                source: "synthetic".to_string(),
            })
            .collect();
        let no_delistings: [Delisting; 0] = [];
        Panel::from_bars(bars)
            .expect("panel builds")
            .with_delistings(&no_delistings)
            .expect("delistings attach")
            .with_marketcaps(&caps)
            .expect("market caps attach")
    };
    assert!(
        matches!(
            run::backtest(
                &no_dividends,
                &BacktestConfig {
                    actions_sha256: None,
                    ..config.clone()
                }
            ),
            Err(EngineError::ConservativeFormulaMissingInputs { .. })
        ),
        "the conservative formula ran without cash dividends, so its payout leg \
         was a share count with no payout in it"
    );
}

/// The dividend-boundary fixture. One name, flat at 100 at every month-end, with
/// a dividend going ex exactly on the window's opening month-end and another
/// exactly on its closing one.
///
/// ```text
///   ex-date   amount   inside (m2, m3]?
///   m2           6     no, it belongs to whoever held the share the day before
///   m3          12     yes, the drop it causes is inside the return measured
/// ```
///
/// With the close flat at 100 the three monthly total returns over m0 to m3 are
/// `0`, `(100 + 6)/100 - 1 = 0.06` and `(100 + 12)/100 - 1 = 0.12`. Their mean
/// is 0.06 and the deviations are `-0.06, 0, 0.06`, so the sample variance is
/// `0.0072 / 2 = 0.0036` and the deviation is exactly `0.06`. The trailing
/// yield over `(m2, m3]` is `12 / 100 = 0.12`.
///
/// A window closed at its start and open at its end instead sums 6 into the
/// yield, giving 0.06, and shifts both dividends one return earlier in the
/// volatility, giving returns of `0, 0, 0.06` and a deviation of `0.0346410`.
#[test]
fn xc9_the_dividend_window_is_open_at_its_start_and_closed_at_its_end() {
    let name = plain(
        "PAYER",
        1,
        [
            "100", "100", "100", "100", "100", "100", "100", "100", "100", "100",
        ],
        [
            "1000", "1000", "1000", "1000", "1000", "1000", "1000", "1000", "1000", "1000",
        ],
    );
    let m = month_ends();
    let dividends = [
        (name.asset.clone(), m[2], dec("6")),
        (name.asset.clone(), m[3], dec("12")),
    ];
    let panel = cf_panel(&[name], &dividends);
    let series = &panel.securities()[0];

    assert_eq!(
        conservative::dividend_yield(series, m[2], m[3]).expect("the arithmetic holds"),
        Some(dec("0.12")),
        "the yield is not the 12 that went ex on the closing month-end over the \
         100 close; 0.06 means the window took the opening month-end's 6 instead"
    );
    assert_eq!(
        conservative::monthly_total_return_volatility(series, &m[0..=3])
            .expect("the arithmetic holds"),
        Some(dec("0.06")),
        "the monthly total returns are not 0, 0.06, 0.12, so a dividend is \
         landing in the wrong month's return"
    );
}

/// X-C10. The size screen drops the smallest names, not the largest.
///
/// Market caps at the formation are `HIGHB 750`, `BUYBACK 880`, `NEUTRAL 1030`,
/// `ISSUER 1260` and `HIGHA 1950`. A half of five names is `floor(2.5) = 2`, so
/// the two smallest go and `[0, 2, 3]` survive in identity order. Dropping from
/// the large end instead would leave `[1, 2, 4]`, which is a different set with
/// the same size, so a screen that merely removed the right *number* of names
/// still fails here.
#[test]
fn xc10_the_size_screen_drops_the_smallest_names() {
    let (panel, _) = five_names();
    let config = BacktestConfig {
        size_floor_fraction: Some(dec("0.5")),
        ..conservative_config()
    };
    let formed = momentum::rebalance_at(&panel, &config, 3).expect("the formation forms");

    assert_eq!(
        formed.eligible,
        vec![0, 2, 3],
        "the two smallest names by market cap were not the ones removed"
    );
    assert_eq!(
        formed.census,
        crate::momentum::FormationCensus {
            eligible: 5,
            size_screened: 2,
            vol_half: 2,
            npy_ineligible: 0,
            held: 1,
        },
        "the formation census does not describe the funnel the screen produced"
    );
}

/// A fraction outside `[0, 1)` is refused rather than clamped, exactly as the
/// liquidity screen's is.
///
/// At 1.0 the screen would remove every eligible name and the run would report
/// a flat series as a result. `CLAUDE.md` is explicit that malformed input
/// errors.
#[test]
fn a_size_fraction_outside_the_unit_interval_is_refused() {
    let (panel, _) = five_names();
    for fraction in ["-0.1", "1", "1.5"] {
        let config = BacktestConfig {
            size_floor_fraction: Some(dec(fraction)),
            ..conservative_config()
        };
        assert!(
            matches!(
                momentum::rebalance_at(&panel, &config, 3),
                Err(EngineError::SizeFloorFractionOutOfRange { .. })
            ),
            "a size floor fraction of {fraction} was accepted"
        );
    }
}

/// The zero-market-cap fixture. `BUYBACK` carries a market cap of exactly zero
/// at m1, which is inside the two month-ends its share average reads.
///
/// The other two caps at m3 are moved so that the correct answer does not rest
/// on a tie:
///
/// ```text
///   name      cap(m1)   cap(m3)   close(m3)   level(m3)   change   NPY
///   ISSUER      1000      1050       105          10        0.0     +0.01
///   BUYBACK        0       440       110           4         -       none
///   NEUTRAL     1000      1133       103          11       +0.1     -0.09
/// ```
///
/// Correctly, `BUYBACK` has no share ratio at all and is dropped before the
/// ranking, leaving `ISSUER` ahead of `NEUTRAL` on both legs and held at
/// index 0. Reading the zero as a level instead gives `BUYBACK` an average of
/// `0 / 10 = 0` against a m3 level of 4, so its change is `4 / 5 - 1 = -0.2`,
/// its payout yield is `0.01 + 0.2 = 0.21`, and it leads both legs and is held
/// at index 1.
fn zero_cap_names() -> Panel {
    let names = [
        plain(
            "ISSUER",
            1,
            [
                "100", "100", "105", "105", "106", "107", "108", "109", "110", "111",
            ],
            [
                "1000", "1000", "1050", "1050", "1060", "1070", "1080", "1090", "1100", "1110",
            ],
        ),
        plain(
            "BUYBACK",
            2,
            [
                "100", "100", "110", "110", "111", "112", "113", "114", "115", "116",
            ],
            [
                "1000", "0", "1100", "440", "1110", "1120", "1130", "1140", "1150", "1160",
            ],
        ),
        plain(
            "NEUTRAL",
            3,
            [
                "100", "100", "103", "103", "104", "105", "106", "107", "108", "109",
            ],
            [
                "1000", "1000", "1030", "1133", "1040", "1050", "1060", "1070", "1080", "1090",
            ],
        ),
        plain(
            "HIGHA",
            4,
            [
                "100", "100", "130", "130", "131", "132", "133", "134", "135", "136",
            ],
            [
                "1000", "1000", "1300", "1950", "1310", "1320", "1330", "1340", "1350", "1360",
            ],
        ),
        plain(
            "HIGHB",
            5,
            [
                "100", "100", "150", "150", "151", "152", "153", "154", "155", "156",
            ],
            [
                "1000", "1000", "1500", "750", "1510", "1520", "1530", "1540", "1550", "1560",
            ],
        ),
    ];
    let m = month_ends();
    let dividends = [
        (names[0].asset.clone(), m[3], dec("1.05")),
        (names[1].asset.clone(), m[3], dec("1.10")),
        (names[2].asset.clone(), m[3], dec("1.03")),
    ];
    cf_panel(&names, &dividends)
}

/// X-C11. A market cap of exactly zero anywhere in the share window makes the
/// name ineligible for the payout leg.
///
/// The vendor quantises to a tenth of a million dollars, so zero is a real
/// value below the quantum rather than a gap. It carries no information about
/// the share count, and a level computed off it is arithmetic rather than
/// measurement.
#[test]
fn xc11_a_zero_market_cap_in_the_window_makes_the_payout_leg_unavailable() {
    let panel = zero_cap_names();
    let month_ends = panel.month_ends();

    assert_eq!(
        conservative::share_change(&panel.securities()[1], month_ends, 3, 2),
        None,
        "a zero market cap inside the window produced a share ratio, so a value \
         below the vendor's quantum is being read as a share count"
    );

    let formed =
        momentum::rebalance_at(&panel, &conservative_config(), 3).expect("the formation forms");
    assert_eq!(
        formed.census.npy_ineligible, 1,
        "exactly one name in the calm half has no payout leg, and the census \
         does not say so"
    );
    assert_eq!(
        formed.chosen,
        vec![0],
        "the name with the zero market cap was ranked anyway, and it won"
    );
}

/// The conservative program carries the source's own windows.
///
/// The tests above shorten every window so that a formation fits in ten
/// month-ends, which proves the rules and says nothing about the constants. If
/// `conservative_v0` carried the momentum defaults, every one of them would
/// still pass. This is the test that would not, and it is the same division of
/// labour `e9d` draws for the strategy field.
#[test]
fn the_conservative_program_carries_the_sources_windows() {
    let config = BacktestConfig::conservative_v0("0".repeat(64));

    assert_eq!(config.strategy, Strategy::ConservativeFormula);
    assert_eq!(
        config.rebalance_every_months, 3,
        "quarterly, per the source"
    );
    assert_eq!(config.volatility_lookback_months, Some(36));
    assert_eq!(config.payout_share_average_months, Some(24));
    assert_eq!(config.payout_dividend_trailing_months, Some(12));
    assert_eq!(config.size_floor_fraction, Some(dec("0.5")));
    assert_eq!(
        config.price_floor,
        Decimal::ZERO,
        "the source has no price floor and its size screen does the liquidity work"
    );
    assert_eq!(
        config.liquidity_floor_fraction, None,
        "the source has no dollar-volume screen either"
    );
    assert_eq!(
        config.min_coverage,
        dec("0.80"),
        "coverage is this project's data-quality rule rather than a strategy \
         parameter, and it stays"
    );
    assert_eq!(
        config.signal_lookback_months, 12,
        "the momentum leg is the existing 12-1 signal, unchanged"
    );
    assert_eq!(config.signal_skip_months, 1);

    // The deepest of the windows is what the loop waits for. A lead-in
    // taken from the momentum window alone would form portfolios at month 12
    // with no volatility and no share history, and every name would silently
    // drop out rather than the formation being refused.
    assert_eq!(
        config.required_lead_in(),
        36,
        "the lead-in is not the deepest window the strategy reads"
    );

    // All FOUR windows compete for that maximum, including the dividend
    // trailing window. With the shipped constants the dividend window (12) is
    // shadowed by volatility (36), so only a deeper-than-everything value can
    // prove it participates: a config whose dividend window is the deepest
    // must wait for it, or the early formations run with every name
    // NPY-ineligible and the census quietly reports a full field of drops.
    let deep_dividends = BacktestConfig {
        payout_dividend_trailing_months: Some(48),
        ..config.clone()
    };
    assert_eq!(
        deep_dividends.required_lead_in(),
        48,
        "the dividend trailing window does not extend the lead-in, so early \
         formations would form with zero NPY-eligible names instead of waiting"
    );

    // The canonical form carries a lowercase string rather than a nested
    // object, matching every other token in it.
    let canonical = config.canonical_json().expect("serialises");
    assert!(
        canonical.contains(r#""strategy":"conservative_formula""#),
        "got {canonical}"
    );
}
