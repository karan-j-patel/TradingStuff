//! X-F1 through X-F7, the engine half of the value rail.
//!
//! X-F8 and X-F9 are ingest's (the life window and the duplicate rule live at
//! the fetch and the writer). X-F10 is the CLI registry walk.
//!
//! Selection and lookahead are mandatory-test paths under `CLAUDE.md`, so every
//! expected figure below is derived in its comment and the derivation is the
//! test. Nothing here is copied out of a run.
//!
//! # Why these fixtures flip a portfolio rather than perturb a number
//!
//! A lookahead bug that moved a ratio slightly would be caught by an assertion
//! on that ratio and by nothing else. These fixtures are built so the wrong read
//! changes WHICH NAME IS HELD, because that is the failure that matters and it
//! is visible in one assertion on `chosen`. The one exception is X-F3, and its
//! comment says why an ordering assertion cannot catch a units error at all.

use ingest::marketcap::MarketCapRecord;
use ingest::provider::{FundamentalRecord, ReportBasis, ReportScope};
use ingest::schema::AssetKey;
use jiff::civil::{Date, date};
use rust_decimal::Decimal;

use super::{
    ACTIONS_SHA256, DELISTINGS_SHA256, MARKETCAP_SHA256, asset, dec, month_ends, panel_of,
};
use crate::config::{BacktestConfig, Strategy, VALUE_PROGRAM};
use crate::error::EngineError;
use crate::panel::Panel;
use crate::{momentum, run, value};

/// The placeholder digest the fixture panels attach filings under.
pub const FILINGS_SHA256: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

/// One synthetic filing. `equityusd` is the only field any strategy reads.
fn filing(
    asset: &AssetKey,
    as_reported: Date,
    period_end: Date,
    equity: &str,
) -> FundamentalRecord {
    FundamentalRecord {
        asset: asset.clone(),
        as_reported,
        period_end,
        observed_at: as_reported,
        source: "synthetic".to_string(),
        basis: ReportBasis::AsReported,
        scope: ReportScope::Quarterly,
        filing_id: None,
        fields: [("equityusd".to_string(), dec(equity))]
            .into_iter()
            .collect(),
    }
}

/// A market cap row, in the vendor's millions.
fn cap(asset: &AssetKey, date: Date, millions: &str) -> MarketCapRecord {
    MarketCapRecord {
        asset: asset.clone(),
        date,
        marketcap: dec(millions),
        source: "synthetic".to_string(),
    }
}

/// A panel over [`month_ends`] with market caps and filings attached.
fn valued_panel(
    prices: &[(AssetKey, &str)],
    caps: &[MarketCapRecord],
    filings: &[FundamentalRecord],
) -> Panel {
    let m = month_ends();
    let series: Vec<(AssetKey, Vec<(Date, Decimal)>)> = prices
        .iter()
        .map(|(key, close)| {
            (
                key.clone(),
                m.iter().map(|day| (*day, dec(close))).collect(),
            )
        })
        .collect();
    panel_of(&series)
        .with_marketcaps(caps, MARKETCAP_SHA256)
        .expect("fixture market caps attach")
        .with_filings(filings, FILINGS_SHA256)
        .expect("fixture filings attach")
}

/// The value rules over a two-month lead-in, wired for the fixtures.
fn value_config() -> BacktestConfig {
    BacktestConfig {
        strategy: Strategy::Value,
        book_staleness_days: Some(548),
        signal_lookback_months: 2,
        marketcap_sha256: Some(MARKETCAP_SHA256.to_string()),
        filings_sha256: Some(FILINGS_SHA256.to_string()),
        ..BacktestConfig::momentum_v0("0".repeat(64))
    }
}

/// X-F1. A filing published after the formation is invisible at it.
///
/// # The fixture flips the portfolio
///
/// ```text
///   formation m2 = 2020-03-31, both names priced at 100, caps in millions
///
///   name    cap (m)   market (usd)   visible filing   B/M
///   EARLY      100      1.0e8        book 1.0e8       1.0     <- held
///   LATE       100      1.0e8        book 0.5e8       0.5
///
///   LATE also carries a filing published 2020-04-01, one day after the
///   formation, whose book is 1.0e9 and whose B/M would be 10. Reading it makes
///   LATE the highest-ranked name, so the portfolio becomes the other name's
///   entirely rather than a slightly different number.
/// ```
///
/// # Why the late filing's period end is BEFORE the formation
///
/// That is what makes this fixture discriminate the mutation the round cares
/// about most. A filing published on 2020-04-01 describes a period that ended
/// earlier, here 2020-01-31. An accessor reading `period_end <= as_of` instead
/// of `as_reported <= as_of` therefore admits it, which is both the classic
/// academic bug and exactly the semantics of the vendor's restated dimensions.
/// The two mutations, a shifted bound and the wrong date column, both land here.
#[test]
fn x_f1_a_filing_published_after_the_formation_is_invisible() {
    let m = month_ends();
    let early = asset("EARLY", 1);
    let late = asset("LATE", 2);

    let panel = valued_panel(
        &[(early.clone(), "100"), (late.clone(), "100")],
        &m.iter()
            .flat_map(|day| [cap(&early, *day, "100"), cap(&late, *day, "100")])
            .collect::<Vec<_>>(),
        &[
            filing(&early, m[1], m[0], "100000000"),
            filing(&late, m[1], m[0], "50000000"),
            // Published the day after the formation, describing a period that
            // ended two months before it.
            filing(&late, date(2020, 4, 1), m[0], "1000000000"),
        ],
    );

    // The property that makes the `period_end` mutation land here rather than
    // being caught by the bound: the late filing's period precedes the
    // formation, so an accessor reading the wrong date column admits it.
    assert!(
        filing(&late, date(2020, 4, 1), m[0], "1000000000").period_end < m[2],
        "the late filing's period end must precede the formation"
    );

    // The fixture's discriminating properties, asserted rather than assumed,
    // and asserted THROUGH the accessor because there is deliberately no way to
    // read a filing around it. See `book_equity_usd_as_of`.
    let late_series = &panel.securities()[1];
    assert_eq!(
        late_series.book_equity_usd_as_of(date(2020, 4, 1), 548),
        Some(dec("1000000000")),
        "LATE must carry the late filing and it must become visible the day it \
         is published, or this fixture proves nothing about where the bound sits"
    );

    let config = value_config();
    let rebalance = momentum::rebalance_at(&panel, &config, 2).expect("the formation resolves");
    assert_eq!(
        rebalance.chosen,
        vec![0],
        "the portfolio is not the one the visible filings imply, so a filing \
         published after the formation reached the ranking"
    );

    // And the accessor itself, with no ranking in the way.
    assert_eq!(
        late_series.book_equity_usd_as_of(m[2], 548),
        Some(dec("50000000")),
        "the book value read at the formation is not the one visible there"
    );
    assert_eq!(
        late_series.book_equity_usd_as_of(date(2020, 4, 1), 548),
        Some(dec("1000000000")),
        "the late filing must become visible the day it is published, or this \
         fixture proves nothing about the bound's position"
    );
}

/// X-F2. A filing exactly at the staleness bound is usable and one day past is
/// not.
///
/// ```text
///   filed 2020-01-31
///   + 547 days = 2021-07-31   usable
///   + 548 days = 2021-08-01   usable, the bound is inclusive
///   + 549 days = 2021-08-02   too old, the name drops
/// ```
///
/// Both sides are asserted. The delisting round proved that a one-sided window
/// test defends a deleted term: removing the staleness bound entirely leaves
/// every "is usable" assertion passing, and only the far side notices.
#[test]
fn x_f2_the_staleness_bound_is_inclusive_and_one_day_past_it_drops_the_name() {
    let m = month_ends();
    let one = asset("ONE", 1);
    let panel = valued_panel(
        &[(one.clone(), "100")],
        &m.iter()
            .map(|day| cap(&one, *day, "100"))
            .collect::<Vec<_>>(),
        // The period end is deliberately 215 days before the filing date. The
        // first version of this fixture used one date for both, which made the
        // clock source undetectable: ageing from `period_end` instead of from
        // `as_reported` gave the identical answer and the mutation slipped.
        // With them apart, the wrong clock reads 763 days at the bound instead
        // of 548 and every assertion below moves.
        &[filing(
            &one,
            date(2020, 1, 31),
            date(2019, 6, 30),
            "100000000",
        )],
    );
    let series = &panel.securities()[0];

    assert_eq!(
        series.book_equity_usd_as_of(date(2021, 7, 31), 548),
        Some(dec("100000000")),
        "a filing inside the bound is not readable"
    );
    assert_eq!(
        series.book_equity_usd_as_of(date(2021, 8, 1), 548),
        Some(dec("100000000")),
        "the bound is exclusive at exactly 548 days, and it is specified as \
         inclusive"
    );
    assert_eq!(
        series.book_equity_usd_as_of(date(2021, 8, 2), 548),
        None,
        "a filing 549 days old is still readable, so the staleness bound is not \
         being applied at all"
    );

    // The whole point of the bound: the name leaves the ranking rather than
    // being valued off a report nobody would call current.
    assert_eq!(
        value::book_to_market(&panel, 0, date(2021, 8, 2), 548),
        None,
        "a name whose only filing is stale still produced a book-to-market"
    );

    // The clock runs from publication, not from the period the figures
    // describe. Ageing from `period_end` would make this filing 763 days old at
    // the bound rather than 548, so the usable assertions above would all read
    // None. Stated here as its own assertion so the reason is not only in a
    // comment.
    assert!(
        series
            .book_equity_usd_as_of(date(2021, 8, 1), 548)
            .is_some(),
        "the staleness clock runs from the period end rather than from the \
         filing date, so a filing is aged by how old its figures are rather \
         than by how long it has been public"
    );
}

/// X-F3. The ratio's own value, because a units error preserves every rank.
///
/// # Why this test asserts a number and not an ordering
///
/// `equityusd` is in whole dollars and the market cap dataset is in millions.
/// Dropping the multiplication multiplies every book-to-market in the
/// cross-section by exactly one million. Every rank is preserved, every
/// portfolio is identical, and every assertion about which names are held passes
/// unchanged. An ordering-only test is blind to this by construction, which is
/// why the expected ratio is written out here.
///
/// ```text
///   book   = 5,000,000,000 dollars      (equityusd, as shipped)
///   cap    =         2,500 millions     (marketcap, as shipped)
///   market = 2,500 * 1e6 = 2,500,000,000 dollars
///   B/M    = 5.0e9 / 2.5e9 = 2
///
///   without the 1e6:  5.0e9 / 2,500 = 2,000,000
/// ```
#[test]
fn x_f3_book_to_market_is_a_ratio_of_two_figures_in_the_same_units() {
    let m = month_ends();
    let one = asset("ONE", 1);
    let panel = valued_panel(
        &[(one.clone(), "100")],
        &m.iter()
            .map(|day| cap(&one, *day, "2500"))
            .collect::<Vec<_>>(),
        &[filing(&one, m[0], m[0], "5000000000")],
    );

    assert_eq!(
        value::book_to_market(&panel, 0, m[2], 548),
        Some(dec("2")),
        "the book-to-market is not the ratio of two figures in whole dollars, \
         so the market cap's millions were not converted"
    );

    // The constant itself, pinned. A test that only read the ratio above would
    // still pass if the constant were changed and the fixture changed with it.
    assert_eq!(value::MILLIONS, dec("1000000"));
}

/// X-F4. Book equity at or below zero leaves the ranking.
///
/// Fama and French (1993) exclude negative-book firms from the breakpoints and
/// from the portfolios alike. The boundary sits at `<= 0` here: the French
/// library's factor construction requires strictly positive book equity, Asness
/// and Frazzini (2013) admit zero, the two conflict, and this project took
/// positive-only before any result existed.
///
/// Both shapes are in the fixture because they fail to different mutations. A
/// `>` flipped to `>=` admits the zero and not the negative; a dropped sign
/// check admits both.
#[test]
fn x_f4_non_positive_book_equity_leaves_before_the_ranking() {
    let m = month_ends();
    let positive = asset("POS", 1);
    let zero = asset("ZERO", 2);
    let negative = asset("NEG", 3);
    let names = [positive.clone(), zero.clone(), negative.clone()];

    let panel = valued_panel(
        &names
            .iter()
            .map(|key| (key.clone(), "100"))
            .collect::<Vec<_>>(),
        &m.iter()
            .flat_map(|day| names.iter().map(|key| cap(key, *day, "100")))
            .collect::<Vec<_>>(),
        &[
            // The smallest book of the three that is still positive, so the
            // excluded names would outrank it if they were admitted.
            filing(&positive, m[1], m[0], "1000000"),
            filing(&zero, m[1], m[0], "0"),
            filing(&negative, m[1], m[0], "-500000000"),
        ],
    );

    assert_eq!(
        value::book_to_market(&panel, 1, m[2], 548),
        None,
        "a name with exactly zero book equity produced a book-to-market"
    );
    assert_eq!(
        value::book_to_market(&panel, 2, m[2], 548),
        None,
        "a name with negative book equity produced a book-to-market"
    );
    assert!(value::book_to_market(&panel, 0, m[2], 548).is_some());

    let rebalance = momentum::rebalance_at(&panel, &value_config(), 2).expect("formation");
    assert_eq!(
        rebalance.eligible,
        vec![0],
        "an unvaluable name is still in the field the portfolio is chosen out \
         of, so the quintile is a quintile of names that cannot be valued"
    );
    assert_eq!(rebalance.chosen, vec![0]);
}

/// X-F5. A restated row is refused at the attachment, by name.
///
/// The vendor's restated dimensions date a row by its period end while the
/// values were corrected later. That is lookahead by construction and it is the
/// single worst failure this rail can have, so it is refused at the door rather
/// than filtered downstream: a filter is a thing a later caller can forget to
/// apply and a refusal is not.
#[test]
fn x_f5_a_restated_filing_is_refused_at_the_attachment() {
    let m = month_ends();
    let one = asset("ONE", 1);
    let restated = FundamentalRecord {
        basis: ReportBasis::Restated,
        ..filing(&one, m[1], m[0], "100000000")
    };

    let error = panel_of(&[(
        one.clone(),
        m.iter().map(|day| (*day, dec("100"))).collect::<Vec<_>>(),
    )])
    .with_filings(&[restated], FILINGS_SHA256)
    .expect_err("a restated row must be refused");

    match error {
        EngineError::RestatedFilingRefused { ticker, period_end } => {
            assert_eq!(ticker, "ONE");
            assert_eq!(period_end, m[0]);
        }
        other => panic!("the attachment failed for some other reason: {other}"),
    }

    // The control. The same attachment with an as-reported row succeeds, so the
    // refusal above is about the basis rather than about the fixture.
    assert!(
        panel_of(&[(
            one.clone(),
            m.iter().map(|day| (*day, dec("100"))).collect::<Vec<_>>(),
        )])
        .with_filings(&[filing(&one, m[1], m[0], "100000000")], FILINGS_SHA256)
        .is_ok()
    );
}

/// X-F6. The fourth attachment is wired like the other three.
///
/// Presence in both directions, digest equality, and the program's refusal to
/// run without every dataset it needs.
#[test]
fn x_f6_the_filings_attachment_is_wired_like_the_others() {
    let m = month_ends();
    let one = asset("ONE", 1);
    let prices: Vec<(AssetKey, Vec<(Date, Decimal)>)> = vec![(
        one.clone(),
        m.iter().map(|day| (*day, dec("100"))).collect(),
    )];
    let caps: Vec<MarketCapRecord> = m.iter().map(|day| cap(&one, *day, "100")).collect();
    let filings = [filing(&one, m[0], m[0], "100000000")];

    let attached = panel_of(&prices)
        .with_marketcaps(&caps, MARKETCAP_SHA256)
        .expect("caps")
        .with_filings(&filings, FILINGS_SHA256)
        .expect("filings");
    let bare = panel_of(&prices)
        .with_marketcaps(&caps, MARKETCAP_SHA256)
        .expect("caps");

    // Presence, configuration says yes and panel says no.
    assert!(matches!(
        run::backtest(&bare, &value_config()),
        Err(EngineError::FilingsWiringMismatch { .. })
    ));
    // Presence, panel says yes and configuration says no.
    assert!(matches!(
        run::backtest(
            &attached,
            &BacktestConfig {
                filings_sha256: None,
                ..value_config()
            }
        ),
        Err(EngineError::FilingsWiringMismatch { .. })
    ));

    // Identity. Presence agrees on both sides and the digests do not.
    let other = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    match run::backtest(
        &attached,
        &BacktestConfig {
            filings_sha256: Some(other.to_string()),
            ..value_config()
        },
    ) {
        Err(EngineError::DatasetDigestMismatch {
            dataset,
            recorded,
            attached,
        }) => {
            assert_eq!(dataset, "filings");
            assert_eq!(recorded, other, "the recorded digest is not reported");
            assert_eq!(
                attached, FILINGS_SHA256,
                "the digest the panel was built from is not reported"
            );
        }
        other => panic!("a mismatched filings digest was not refused: {other:?}"),
    }
}

/// The value program refuses a run missing any one of its four datasets.
///
/// One test over all four rather than four tests, because the hazard is the
/// same for each and a copy that drifted would leave one dataset guarded and
/// another not.
#[test]
fn the_value_program_refuses_a_run_missing_any_of_its_datasets() {
    let m = month_ends();
    let one = asset("ONE", 1);
    let prices: Vec<(AssetKey, Vec<(Date, Decimal)>)> = vec![(
        one.clone(),
        m.iter().map(|day| (*day, dec("100"))).collect(),
    )];
    let caps: Vec<MarketCapRecord> = m.iter().map(|day| cap(&one, *day, "100")).collect();
    let filings = [filing(&one, m[0], m[0], "100000000")];
    let no_delistings: [ingest::actions::Delisting; 0] = [];

    // Everything but dividends. The configuration matches the panel on presence
    // and on digest, so what fires is the program's own requirement.
    let panel = panel_of(&prices)
        .with_delistings(&no_delistings, DELISTINGS_SHA256)
        .expect("delistings")
        .with_marketcaps(&caps, MARKETCAP_SHA256)
        .expect("caps")
        .with_filings(&filings, FILINGS_SHA256)
        .expect("filings");
    let config = BacktestConfig {
        delisting_convention: Some(crate::config::DELISTING_CONVENTION.to_string()),
        delistings_sha256: Some(DELISTINGS_SHA256.to_string()),
        ..value_config()
    };
    assert!(
        matches!(
            run::backtest(&panel, &config),
            Err(EngineError::ValueMissingInputs {
                dividends: false,
                ..
            })
        ),
        "a value run without dividends was accepted"
    );

    // All four, which must reach the arithmetic rather than the guard. The
    // fixture is too flat to produce a Sharpe, so the run fails further in;
    // what matters is that it is no longer this guard refusing it.
    let complete = panel_of(&prices)
        .with_dividends(&[], ACTIONS_SHA256)
        .expect("dividends")
        .with_delistings(&no_delistings, DELISTINGS_SHA256)
        .expect("delistings")
        .with_marketcaps(&caps, MARKETCAP_SHA256)
        .expect("caps")
        .with_filings(&filings, FILINGS_SHA256)
        .expect("filings");
    let wired = BacktestConfig {
        actions_sha256: Some(ACTIONS_SHA256.to_string()),
        ..config
    };
    assert!(
        !matches!(
            run::backtest(&complete, &wired),
            Err(EngineError::ValueMissingInputs { .. })
        ),
        "a value run with all four datasets attached was still refused for \
         missing one"
    );
}

/// X-F7's companion: the program resolves and carries the settled parameters.
///
/// `e7` covers the two new fields by mutation. This covers the configuration
/// that actually gets run, on the rule `e9d` follows for the strategy field.
#[test]
fn x_f7_the_value_program_carries_its_settled_parameters() {
    let universe = "0".repeat(64);
    let config = BacktestConfig::for_program(VALUE_PROGRAM, None, &universe)
        .expect("the registry lists the value program");

    assert_eq!(config.strategy, Strategy::Value);
    assert_eq!(
        config.book_staleness_days,
        Some(548),
        "the staleness bound is not the eighteen months the spec fixes"
    );
    assert_eq!(config.quintile_divisor, 5);
    assert_eq!(config.rebalance_every_months, 1);
    assert_eq!(config.weighting, crate::config::Weighting::Equal);
    assert_eq!(config.size_floor_fraction, None);
    assert_eq!(config.liquidity_floor_fraction, None);
    assert_eq!(config.price_floor, dec("5"));
    assert_eq!(config.min_coverage, dec("0.80"));

    // Everything else is momentum's, so the difference between this trial and
    // that one is the signal and its bound rather than the scaffolding.
    assert_eq!(
        BacktestConfig {
            strategy: Strategy::Momentum,
            book_staleness_days: None,
            ..config.clone()
        },
        BacktestConfig::momentum_v0(&universe),
        "the value program changes something other than the signal and its \
         staleness bound"
    );

    let canonical = config.canonical_json().expect("serialises");
    assert!(
        canonical.contains(r#""strategy":"value""#),
        "got {canonical}"
    );
    assert!(
        canonical.contains(r#""book_staleness_days":548"#),
        "got {canonical}"
    );
}

/// A value configuration with no staleness bound is refused rather than
/// defaulted.
///
/// A default invented here would value names off reports of unknown age under a
/// configuration hash that records no bound at all.
#[test]
fn a_value_run_with_no_staleness_bound_is_refused() {
    let m = month_ends();
    let one = asset("ONE", 1);
    let panel = valued_panel(
        &[(one.clone(), "100")],
        &m.iter()
            .map(|day| cap(&one, *day, "100"))
            .collect::<Vec<_>>(),
        &[filing(&one, m[0], m[0], "100000000")],
    );

    assert!(matches!(
        momentum::rebalance_at(
            &panel,
            &BacktestConfig {
                book_staleness_days: None,
                ..value_config()
            },
            2
        ),
        Err(EngineError::ValueStalenessMissing)
    ));
}

/// A staleness bound too large for the age arithmetic is refused, not wrapped.
///
/// `book_equity_usd_as_of` compares ages as i64 days. A bound above i64::MAX
/// cast unchecked would wrap negative, silently marking every filing stale and
/// emptying the book, which is degradation wearing a configuration's clothes.
/// Cross-model review found the cast; this pins the refusal.
#[test]
fn a_staleness_bound_the_age_arithmetic_cannot_hold_is_refused() {
    let m = month_ends();
    let one = asset("ONE", 1);
    let panel = valued_panel(
        &[(one.clone(), "100")],
        &m.iter()
            .map(|day| cap(&one, *day, "100"))
            .collect::<Vec<_>>(),
        &[filing(&one, m[0], m[0], "100000000")],
    );

    let no_delistings: [ingest::actions::Delisting; 0] = [];
    let panel = panel
        .with_dividends(&[], super::ACTIONS_SHA256)
        .expect("empty dividends attach")
        .with_delistings(&no_delistings, super::DELISTINGS_SHA256)
        .expect("empty delistings attach");

    assert!(matches!(
        run::backtest(
            &panel,
            &BacktestConfig {
                book_staleness_days: Some((i64::MAX as usize) + 1),
                actions_sha256: Some(super::ACTIONS_SHA256.to_string()),
                delisting_convention: Some(crate::config::DELISTING_CONVENTION.to_string()),
                delistings_sha256: Some(super::DELISTINGS_SHA256.to_string()),
                ..value_config()
            },
        ),
        Err(EngineError::BookStalenessOutOfRange { .. })
    ));
}

/// Two filings published on one day resolve to the later fiscal period.
///
/// A delinquent filer catching up lodges two quarters at once, which the Phase A
/// probe saw in its 160-day delayed case. The upstream duplicate rule keys on
/// `(asset, basis, scope, period_end)` and so permits it, which is why the
/// series sorts on both dates and this test pins which of the two wins.
#[test]
fn filings_sharing_a_publication_date_resolve_to_the_later_period() {
    let m = month_ends();
    let one = asset("ONE", 1);
    let earlier_period = filing(&one, m[1], m[0], "100000000");
    let later_period = filing(&one, m[1], m[1], "200000000");

    // Both input orders, because one of them is not a test.
    //
    // The first version of this supplied the earlier period first, which is the
    // order a STABLE sort leaves correct even with the `period_end` term
    // dropped from the key. The mutation slipped: the assertion was passing
    // because of the fixture's input order rather than because of the sort. The
    // adversarial order is `later_period` first, and running both is what stops
    // the next edit reintroducing the same blindness.
    for (label, filings) in [
        (
            "earlier period supplied first",
            vec![earlier_period.clone(), later_period.clone()],
        ),
        (
            "later period supplied first",
            vec![later_period.clone(), earlier_period.clone()],
        ),
    ] {
        let panel = valued_panel(
            &[(one.clone(), "100")],
            &m.iter()
                .map(|day| cap(&one, *day, "100"))
                .collect::<Vec<_>>(),
            &filings,
        );

        assert_eq!(
            panel.securities()[0].book_equity_usd_as_of(m[2], 548),
            Some(dec("200000000")),
            "with the {label}, two filings published on one day did not resolve \
             to the later fiscal period, so the read depends on the order the \
             records happened to arrive in"
        );
    }
}

/// D1. The census reports names actually held, not the quintile size intended.
///
/// The two agree at every formation that holds anything, which is why reporting
/// the intended size slipped a whole fan-out. They part at an all-excluded
/// formation: the quintile floors at one so a thin month still holds something,
/// but when nothing is valuable there is nothing to take, and `chosen` is empty
/// while the floor still says one.
///
/// A census that reported the intention would say a name was held at a formation
/// that held none, which is the reading that makes an empty portfolio invisible.
#[test]
fn d1_the_census_counts_names_held_rather_than_the_quintile_size() {
    let m = month_ends();
    let one = asset("ONE", 1);

    // Filings attached and none of them this name's, so every name is excluded
    // for want of a book value and the formation holds nothing.
    let panel = valued_panel(
        &[(one.clone(), "100")],
        &m.iter()
            .map(|day| cap(&one, *day, "100"))
            .collect::<Vec<_>>(),
        &[],
    );

    let rebalance = momentum::rebalance_at(&panel, &value_config(), 2).expect("formation");
    assert!(
        rebalance.chosen.is_empty(),
        "the fixture must exclude every name, or held and the quintile agree \
         and this test cannot tell them apart"
    );
    assert_eq!(
        rebalance.census.held, 0,
        "the census reports a held name at a formation that held none, so it is \
         reporting the quintile the code intended rather than the portfolio it \
         formed"
    );

    // The control: with a book value the same formation holds one name and the
    // two readings agree again, which is why only the empty case discriminates.
    let valued = valued_panel(
        &[(one.clone(), "100")],
        &m.iter()
            .map(|day| cap(&one, *day, "100"))
            .collect::<Vec<_>>(),
        &[filing(&one, m[1], m[0], "100000000")],
    );
    let held = momentum::rebalance_at(&valued, &value_config(), 2).expect("formation");
    assert_eq!(held.census.held, 1);
    assert_eq!(held.chosen.len(), 1);
}

/// D2. `unmatched_filings` counts RECORDS the panel could not place, not names.
///
/// The two agree whenever every off-panel name carries exactly one filing, which
/// is what every fixture happened to supply. A filings table carries one row per
/// quarter, so the real ratio is nearer forty to one: a count of names would
/// report a rounding error where the record count reports the size of the
/// overhang.
#[test]
fn d2_unmatched_filings_counts_records_rather_than_names() {
    let m = month_ends();
    let held = asset("HELD", 1);
    let absent = asset("ABSENT", 2);

    let panel = panel_of(&[(
        held.clone(),
        m.iter().map(|day| (*day, dec("100"))).collect::<Vec<_>>(),
    )])
    .with_filings(
        &[
            filing(&held, m[1], m[0], "100000000"),
            // One name the panel does not hold, carrying three filings.
            filing(&absent, m[1], m[0], "1"),
            filing(&absent, m[2], m[1], "2"),
            filing(&absent, m[3], m[2], "3"),
        ],
        FILINGS_SHA256,
    )
    .expect("filings attach");

    assert_eq!(
        panel.unmatched_filings(),
        3,
        "the unmatched count is not the number of records the panel could not \
         place, so it is counting names and would report one overhanging \
         security's whole filing history as a single row"
    );
}
