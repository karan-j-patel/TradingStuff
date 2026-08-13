//! E11. Delisting returns reaching the exit mark, and the boundaries around
//! them.
//!
//! These are mandatory-test paths under `CLAUDE.md`. This is cost and return
//! application on the one class of holding whose treatment used to be a known
//! bias, and a haircut silently missing from one of the two return loops is
//! exactly the shape of bug that leaves every test green and every number
//! wrong.
//!
//! Each expected figure below is derived in its comment, and the derivation is
//! the test. A number updated to match what the code produced would turn this
//! file into a transcript of a past run.
//!
//! # The composition rule these tests pin
//!
//! ```text
//! security_return = (close * (1 + delisting_return) + dividend_cash) / open - 1
//! ```
//!
//! The haircut applies to the terminal mark alone. Cash the holder already
//! received is not clawed back by what happened to the company afterwards. The
//! fixtures carry a dividend precisely so that the two candidate compositions
//! give different numbers, and E11a records both.

use super::{asset, bar, dec, delisting, month_ends, two_asset_panel};
use super::{test_config, with_cash_dividends, with_delistings};
use crate::baseline;
use crate::config::{BacktestConfig, DELISTING_CONVENTION};
use crate::momentum;
use crate::panel::Panel;
use crate::portfolio::{self, Weights};
use crate::run;
use ingest::actions::DelistingReason;
use jiff::civil::date;

/// A single holding that trades twice, pays once, and then stops.
///
/// ```text
///   2020-03-31   close 20        entry
///   2020-04-10   0.5 goes ex
///   2020-04-15   close 18        last print
///   2020-04-30   no bar          the exit is noticed here
/// ```
///
/// This is E8e's fixture, deliberately, so the delisting tests differ from the
/// dividend one by the classification and by nothing else. E8e asserts -0.075
/// on it with no delisting record attached.
fn one_exit_panel() -> Panel {
    let gone = asset("GONE", 1);
    let bars = vec![
        bar(&gone, date(2020, 3, 31), dec("20"), dec("20")),
        bar(&gone, date(2020, 4, 15), dec("18"), dec("18")),
    ];
    with_cash_dividends(
        Panel::from_bars(bars).expect("fixture panel builds"),
        &[(gone, date(2020, 4, 10), dec("0.5"))],
    )
}

/// Hold the one name over the month its exit falls in.
fn hold_over_the_exit(panel: &Panel) -> portfolio::Advance {
    let held: Weights = [(0usize, dec("1"))].into_iter().collect();
    portfolio::advance(panel, &held, date(2020, 3, 31), date(2020, 4, 30))
        .expect("the holding advances")
}

/// E11a. A performance-related exit is marked down 30 percent, to the penny.
///
/// ```text
///   no bar on 04-30, so the holding is marked at its last close, 18
///   the exit is classified Bankruptcy, which is performance-related
///   terminal mark = 18 * (1 - 0.30)                          = 12.60
///   cash collected in (03-31, 04-30] is the 0.5 that went ex on 04-10
///
///   (12.60 + 0.5) / 20 - 1 = 13.10 / 20 - 1 = 0.655 - 1      = -0.345
/// ```
///
/// Three numbers this discriminates between, all reachable by a plausible
/// implementation:
///
/// ```text
///   -0.075   no haircut at all, which is E8e's figure
///   -0.3525  haircut applied to (close + cash), clawing back cash already paid
///   -0.345   haircut applied to the terminal mark alone
/// ```
#[test]
fn e11a_a_performance_exit_is_imputed_at_the_convention_to_the_penny() {
    let panel = with_delistings(
        one_exit_panel(),
        &[delisting(
            asset("GONE", 1),
            date(2020, 4, 15),
            DelistingReason::Bankruptcy,
        )],
    );

    let advance = hold_over_the_exit(&panel);

    assert_eq!(
        advance.exits,
        vec![0],
        "the fixture stopped exercising the exit path at all"
    );
    assert_eq!(
        advance.gross_return,
        dec("-0.345"),
        "the delisting return did not reach the terminal mark, or it reached the \
         dividend cash as well"
    );

    // Stated separately so a future edit cannot satisfy the figure above by
    // clawing back the cash and deepening the haircut to compensate.
    assert_ne!(
        advance.gross_return,
        dec("-0.3525"),
        "the haircut was applied to the cash the holder had already received"
    );

    assert_eq!(advance.exit_census.imputed, 1);
    assert_eq!(advance.exit_census.observed, 0);
    assert_eq!(advance.exit_census.unexplained, 0);
}

/// E11b. The buy-and-hold baseline applies the identical treatment.
///
/// This is the test that dies if the haircut is wired into `portfolio::advance`
/// alone. Buy-and-hold does not call `advance`; it carries its own copy of the
/// return arithmetic, and the two have to move together or the baseline beats
/// the strategy on treatment rather than on selection.
///
/// ```text
///          m0   m1   m2   m3   05-15   m4   m5
///   AAA    10   10   10   20     40     -    -
///   BBB    10   10   10   20     -     28   28
///
///   AAA is classified Bankruptcy on 05-15, its last print.
///   entry at m2, equal weight over {AAA, BBB}, traded 1.0, cost 0.0010
///
///   m2->m3:  both 20/10 - 1 = 1.0, gross = 1.0
///            net = (1 - 0.0010)(2.0) - 1                     =  0.998
///            weights stay 0.5 and 0.5
///
///   m3->m4:  AAA has no bar on m4, marked at its LAST CLOSE 40 * 0.70 = 28
///              AAA 28/20 - 1 = 0.4
///            BBB 28/20 - 1                                   =  0.4
///            gross = 0.5(0.4) + 0.5(0.4)                     =  0.4
///            AAA's grown half = 0.5(1.4)/1.4 = 0.5, sold at its marked value
///              exit charge = 0.0010 * 0.5                    =  0.0005
///            net = (1 - 0.0005)(1.4) - 1 = 1.3993 - 1        =  0.3993
///
///   m4->m5:  BBB flat, gross = 0, half the portfolio is cash
///            the final exit sells the live half, 0.0010 * 0.5
///            net = (1 - 0.0005)(1) - 1                       = -0.0005
/// ```
///
/// Both names move by the same fraction in the middle month on purpose, so the
/// portfolio growth divides the weights exactly and every figure terminates.
///
/// # Why AAA's last print is not its entry price
///
/// It was 20, equal to the close AAA is bought at on m3, and that made this
/// test blind. A blind falsification round marked the delisted holding at
/// `open` instead of at `last_close_on_or_before`, which is the exact bug
/// `portfolio::advance`'s doc comment warns about, and nothing failed: the two
/// prices were the same `Decimal`, so the mutation was invisible. `e4b` has the
/// same degenerate shape and is left alone as a pre-existing regression.
///
/// 40 is therefore load-bearing. It is not the entry price, and it is not the
/// unimputed mark either, so this month separates three implementations that a
/// single number could not:
///
/// ```text
///   0.4      marked at the last close, haircut applied     correct
///   1.0      marked at the last close, no haircut          the X-D2 shape
///  -0.3      marked at the entry price, haircut applied    the fan-out shape
/// ```
#[test]
fn e11b_buy_and_hold_applies_the_identical_delisting_treatment() {
    let m = month_ends();
    let aaa = asset("AAA", 1);
    let panel = super::panel_of(&[
        (
            aaa.clone(),
            vec![
                (m[0], dec("10")),
                (m[1], dec("10")),
                (m[2], dec("10")),
                (m[3], dec("20")),
                (date(2020, 5, 15), dec("40")),
            ],
        ),
        (
            asset("BBB", 2),
            vec![
                (m[0], dec("10")),
                (m[1], dec("10")),
                (m[2], dec("10")),
                (m[3], dec("20")),
                (m[4], dec("28")),
                (m[5], dec("28")),
            ],
        ),
    ]);
    let panel = with_delistings(
        panel,
        &[delisting(
            aaa,
            date(2020, 5, 15),
            DelistingReason::Bankruptcy,
        )],
    );
    let config = test_config(2);

    let rebalances: Vec<_> = (2..6)
        .map(|index| momentum::rebalance_at(&panel, &config, index).expect("rebalance"))
        .collect();
    assert_eq!(
        rebalances[0].eligible,
        vec![0, 1],
        "buy-and-hold must enter holding both names"
    );

    // The fixture's discriminating property, asserted rather than assumed. AAA
    // is bought at 20 on m3 and last prints at 40, so a mark taken from the
    // wrong one of those two prices changes the answer. An edit that makes them
    // equal again fails here rather than silently blinding the test below.
    let aaa = &panel.securities()[0];
    assert_eq!(aaa.close_on(m[3]).expect("bar"), dec("20"));
    assert_eq!(
        aaa.last_close_on_or_before(m[4]).expect("bar").1,
        dec("40"),
        "the entry price and the last close must differ, or this test cannot \
         tell a mark at the last close from a mark at the entry"
    );

    let held = baseline::buy_and_hold(&panel, &config, &rebalances).expect("baseline runs");

    assert_eq!(
        held.net_monthly[1],
        dec("0.3993"),
        "the delisted half left the buy-and-hold portfolio at a price that is \
         neither its haircut last close nor the strategy loop's treatment of it"
    );
    assert_eq!(
        held.net_monthly,
        vec![dec("0.998"), dec("0.3993"), dec("-0.0005")]
    );
}

/// E11c. A merger-classified exit is not imputed.
///
/// The same panel as E11a with one field different, so nothing except the
/// classification can explain the difference between the two figures.
///
/// ```text
///   terminal mark = 18 * (1 + 0)                             = 18
///   (18 + 0.5) / 20 - 1 = 18.5 / 20 - 1                      = -0.075
/// ```
///
/// The reasoning is the CRSP convention. A merger's last traded price already
/// reflected the announced deal, and about 99 percent of such returns are
/// recorded near zero to positive, so a negative correction here would
/// introduce a pessimistic bias while removing an optimistic one.
#[test]
fn e11c_a_merger_exit_is_not_imputed() {
    let panel = with_delistings(
        one_exit_panel(),
        &[delisting(
            asset("GONE", 1),
            date(2020, 4, 15),
            DelistingReason::Merger,
        )],
    );

    let advance = hold_over_the_exit(&panel);

    assert_eq!(
        advance.exits,
        vec![0],
        "the fixture stopped exercising the exit path at all"
    );
    assert_eq!(
        advance.gross_return,
        dec("-0.075"),
        "a merger exit was charged a performance haircut, which invents a loss on \
         a deal whose price was already in the last close"
    );
    // Counted as imputed even though the convention is zero. The run applied a
    // published convention to this exit, and a census that hid the zero ones
    // would understate how much of the result rests on the classification being
    // right.
    assert_eq!(advance.exit_census.imputed, 1);
    assert_eq!(advance.exit_census.unexplained, 0);
}

/// E11d. An exit the dataset cannot explain keeps the old treatment and is
/// counted.
///
/// The delistings file is attached and covers a different security, so this
/// exercises "attached and silent about this name" rather than "no file at
/// all". Both must behave the same way, and only this one can distinguish a
/// lookup that quietly matches anything.
///
/// ```text
///   terminal mark = 18, unchanged
///   (18 + 0.5) / 20 - 1                                      = -0.075
/// ```
#[test]
fn e11d_an_unexplained_exit_keeps_its_last_close_and_is_counted() {
    let panel = with_delistings(
        one_exit_panel(),
        &[delisting(
            asset("ELSEWHERE", 99),
            date(2020, 4, 15),
            DelistingReason::Bankruptcy,
        )],
    );
    assert!(
        panel.delistings_attached(),
        "an attachment that matched nothing still read a delistings file"
    );
    assert_eq!(
        panel.unmatched_delistings(),
        1,
        "the record naming a security outside the panel was not counted"
    );

    let advance = hold_over_the_exit(&panel);

    assert_eq!(
        advance.gross_return,
        dec("-0.075"),
        "an exit nothing explains was imputed anyway, which charges a loss no \
         record supports"
    );
    assert_eq!(
        advance.exit_census.unexplained, 1,
        "the unexplained exit is missing from the census the caveat block points at"
    );
    assert_eq!(advance.exit_census.imputed, 0);
}

/// E11e. A mid-window gap for a name that trades again is not an exit to
/// impute.
///
/// One fixture, two windows, because the discriminating property is that the
/// same security reaches the imputation path in one and not the other. A test
/// showing only the first would also pass against a lookup that never matches
/// anything.
///
/// ```text
///   GAP trades 2020-03-31 at 20, has no bar in April, trades 2020-05-29 at 18,
///   and then stops. Its delisting is classified on 2020-05-29.
///
///   held 03-31 -> 04-30:  no bar, marked at the last close 20
///     the classification is dated 05-29, after the window closes, so the gap
///     is unexplained and takes 20 unchanged
///     20 / 20 - 1                                            =  0
///
///   held 05-29 -> 06-30:  no bar, marked at the last close 18
///     the classification is dated 05-29, inside the window
///     18 * 0.70 / 18 - 1                                     = -0.3
/// ```
///
/// Dropping the upper end of the classification window makes the first figure
/// -0.3 as well, which is a month of missing data charged as a delisting.
#[test]
fn e11e_a_temporary_gap_is_never_imputed() {
    let gap = asset("GAP", 1);
    let bars = vec![
        bar(&gap, date(2020, 3, 31), dec("20"), dec("20")),
        bar(&gap, date(2020, 5, 29), dec("18"), dec("18")),
    ];
    let panel = with_delistings(
        Panel::from_bars(bars).expect("fixture panel builds"),
        &[delisting(
            gap,
            date(2020, 5, 29),
            DelistingReason::Bankruptcy,
        )],
    );
    let held: Weights = [(0usize, dec("1"))].into_iter().collect();

    let gap_month = portfolio::advance(&panel, &held, date(2020, 3, 31), date(2020, 4, 30))
        .expect("the holding advances over the gap");
    assert_eq!(
        gap_month.exits,
        vec![0],
        "the fixture stopped reaching the absent-bar path"
    );
    assert_eq!(
        gap_month.gross_return,
        dec("0"),
        "a gap in the middle of a security's life was charged the delisting \
         return of an exit that had not happened yet"
    );
    assert_eq!(gap_month.exit_census.unexplained, 1);
    assert_eq!(gap_month.exit_census.imputed, 0);

    // The same security, the same attached record, the window that really does
    // contain the exit. Without this the assertions above would pass against a
    // classification lookup that matched nothing at all.
    let exit_month = portfolio::advance(&panel, &held, date(2020, 5, 29), date(2020, 6, 30))
        .expect("the holding advances over the exit");
    assert_eq!(
        exit_month.gross_return,
        dec("-0.3"),
        "the fixture no longer reaches the imputation path, so the gap assertion \
         above proves nothing"
    );
    assert_eq!(exit_month.exit_census.imputed, 1);
}

/// A record dated before the security's last bar cannot explain its exit.
///
/// The other half of E11e's window. E11e pins the upper bound, that a record
/// dated after the as-of date is not read, which is the lookahead direction.
/// This pins the lower bound, that a record the security went on trading past
/// is not read either, which is the stale direction.
///
/// # Why the lower bound is not redundant
///
/// Its own doc comment argues that it is: the probe found the last bar dated
/// exactly on the exit action on five of five names, so a record dated before
/// the series stops should not arise. A blind falsification round deleted the
/// term on exactly that reasoning, underscored the now-unused parameter because
/// the compiler suggests it, and the whole suite stayed green. A term removed
/// because a comment says its case never happens is the shape to defend
/// against, and five names is five names.
///
/// ```text
///   GONE trades 2020-03-31 at 20 and 2020-04-15 at 18, then stops.
///   Its Bankruptcy record is dated 2020-03-31, and the 04-15 bar proves the
///   security kept trading afterwards, so that record is not what ended it.
///
///   held 03-31 -> 04-30:  no bar, marked at the last close 18
///     the record is dated before that last bar, so nothing explains the exit
///     (18 + 0.5) / 20 - 1                                     = -0.075
/// ```
///
/// Under the dropped bound the stale record classifies the exit and the month
/// becomes -0.345, a loss no record supports, while the unexplained count the
/// caveat block points a reader at silently drops to zero.
#[test]
fn a_record_predating_the_last_bar_does_not_explain_the_exit() {
    let panel = with_delistings(
        one_exit_panel(),
        &[delisting(
            asset("GONE", 1),
            date(2020, 3, 31),
            DelistingReason::Bankruptcy,
        )],
    );

    // The fixture's discriminating property. The security traded after the
    // record's date, which is what makes the record stale rather than late.
    let gone = &panel.securities()[0];
    assert!(
        gone.close_on(date(2020, 4, 15)).is_some(),
        "the security must trade after the record's date, or the record is not stale"
    );

    let advance = hold_over_the_exit(&panel);

    assert_eq!(
        advance.gross_return,
        dec("-0.075"),
        "a record dated before the last bar was used to impute an exit it cannot \
         explain, charging a loss no record supports"
    );
    assert_eq!(
        advance.exit_census.unexplained, 1,
        "the exit was booked as explained by a record that predates it, so the \
         printed unexplained count understates what rests on no data"
    );
    assert_eq!(advance.exit_census.imputed, 0);
}

/// E11f. A voluntary delisting is performance-related.
///
/// The correction this round makes to the shipped types. Shumway and Warther
/// (1999) classify the company-request delisting codes 570, 572 and 573 as
/// performance-related, and the CIZ delisting set Jensen, Kelly and Pedersen
/// (2023) use includes CORQ, company request. A company that asks to be
/// removed is usually asking on its way down.
///
/// The routing was written before that research existed and sent Voluntary to
/// the zero convention, which is the merger treatment. Under the correction the
/// figure is E11a's.
#[test]
fn e11f_a_voluntary_delisting_is_performance_related() {
    assert!(
        DelistingReason::Voluntary.is_performance_related(),
        "a voluntary delisting routed to the merger convention, so a company \
         that asked to be removed on its way down exits at its last close"
    );

    let panel = with_delistings(
        one_exit_panel(),
        &[delisting(
            asset("GONE", 1),
            date(2020, 4, 15),
            DelistingReason::Voluntary,
        )],
    );

    assert_eq!(
        hold_over_the_exit(&panel).gross_return,
        dec("-0.345"),
        "the classification says performance-related and the arithmetic did not \
         follow it"
    );
}

/// E11g. The convention reaches the configuration hash, in both states.
///
/// `e7` covers the field by mutation, which proves that some change to it moves
/// the hash. This covers the pair a run actually produces: the same
/// configuration with and without a delistings file behind it. Those are two
/// hypotheses about what a delisting cost a holder, so they cannot record as
/// one trial.
///
/// The convention is varied alone here, deliberately, rather than through the
/// `imputing` helper that sets the file hash with it. Moving both at once would
/// pass whichever field reached the hash, and the point of this test is that
/// the convention does.
#[test]
fn e11g_the_delisting_convention_reaches_the_config_hash() {
    let base = test_config(12);
    let with_convention = BacktestConfig {
        delisting_convention: Some(DELISTING_CONVENTION.to_string()),
        ..base.clone()
    };

    assert_eq!(
        base.delisting_convention, None,
        "the default must not impute"
    );
    assert_eq!(
        base.delistings_sha256, with_convention.delistings_sha256,
        "the file hash must be held fixed, or this test cannot say which field moved the hash"
    );
    assert_ne!(
        base.config_hash().expect("hashes").as_str(),
        with_convention.config_hash().expect("hashes").as_str(),
        "a run imputing delisting returns and one not recorded as the same trial"
    );

    // The name travels into the hashed bytes, not merely a boolean. A later
    // round changing the figure changes the name, and that has to move the hash
    // on its own.
    let canonical = with_convention.canonical_json().expect("serialises");
    assert!(
        canonical.contains(&format!(
            r#""delisting_convention":"{DELISTING_CONVENTION}""#
        )),
        "got {canonical}"
    );
    assert!(
        base.canonical_json()
            .expect("serialises")
            .contains(r#""delisting_convention":null"#)
    );
}

/// E11h. The caveat block follows the delisting flag, both ways.
///
/// The UP bias sentence is only true while delistings exit at an unimputed last
/// close. Publishing it beside imputed returns would describe a bias that has
/// been corrected. Dropping it while the correction is off would hide one that
/// is still there.
///
/// The closing line is the part that matters most. With both corrections
/// applied no bias with a known direction is left, and that is precisely not a
/// claim that the numbers are right, because a convention now stands where a
/// measurement would go.
#[test]
fn e11h_the_caveat_block_follows_the_delisting_flag() {
    let bare = run::caveats(false, false);
    let dividends_only = run::caveats(true, false);
    let delistings_only = run::caveats(false, true);
    let both = run::caveats(true, true);

    // Off. The bias is stated as a direction because the exits really do
    // overstate the result.
    for block in [bare, dividends_only] {
        assert!(
            block.contains("no delisting-return") && block.contains("UP"),
            "an unimputed run must still state the upward delisting bias"
        );
    }

    // On. The direction sentence is gone, replaced by what was assumed.
    for block in [delistings_only, both] {
        assert!(
            !block.contains("no delisting-return imputation"),
            "an imputing run published the sentence that says it did not impute"
        );
        assert!(
            block.contains("-30 percent"),
            "the block does not say what was assumed"
        );
        assert!(
            block.contains("Shumway"),
            "the assumption is published without its citation"
        );
        assert!(
            block.contains("not assumption-free"),
            "an imputed result was published without saying it rests on an assumption"
        );
        assert!(
            block.contains("cannot explain"),
            "the block does not tell a reader that unexplained exits are still \
             taking their last close"
        );
    }

    // The round's target state. No direction is claimed, in either direction.
    assert!(
        !both.contains("UP") && !both.contains("DOWN") && !both.contains("UNKNOWN"),
        "the fully corrected block still names a bias direction"
    );
    // And the one bias that genuinely survives when only delistings are
    // corrected is still named.
    assert!(
        delistings_only.contains("DOWN"),
        "the missing-dividend bias was dropped by a round that did not fix it"
    );

    for block in [bare, dividends_only, delistings_only, both] {
        assert!(
            block.contains("must not be described"),
            "every block must refuse the word conservative"
        );
    }
}

/// E11i. The engine refuses a run whose delisting labelling would be wrong.
///
/// The same guard the dividend wiring carries, one dataset over, across three
/// states rather than two. The configuration records what rule was applied and
/// which file it was applied to, the panel records whether anything was
/// classified at all, and any pair of those disagreeing is a mislabelled
/// number rather than a result.
///
/// The two half-wired shapes are the ones worth naming. A convention with no
/// file hash is a run whose result cannot be reproduced from the log, because
/// nothing records which securities the rule reached. A file hash with no
/// convention is a hash that moves for a file the arithmetic never consulted.
#[test]
fn e11i_the_engine_refuses_mismatched_delisting_wiring() {
    let bare = two_asset_panel();
    let attached = with_delistings(two_asset_panel(), &[]);

    let refusals = [
        (
            "a configuration naming a convention ran against a panel with no delistings",
            run::backtest(&bare, &imputing(test_config(2))),
        ),
        (
            "a panel carrying delistings ran under a configuration recording neither the \
             convention nor the file",
            run::backtest(&attached, &test_config(2)),
        ),
        (
            "a run imputed under a configuration that does not record which file it \
             classified from, so the result cannot be reproduced from the log",
            run::backtest(
                &attached,
                &BacktestConfig {
                    delisting_convention: Some(DELISTING_CONVENTION.to_string()),
                    ..test_config(2)
                },
            ),
        ),
        (
            "a configuration recorded a delistings file hash while naming no convention, so \
             the hash moves for a file the arithmetic never consulted",
            run::backtest(
                &attached,
                &BacktestConfig {
                    delistings_sha256: Some("d".repeat(64)),
                    ..test_config(2)
                },
            ),
        ),
    ];

    for (what, outcome) in refusals {
        assert!(
            matches!(
                outcome,
                Err(crate::error::EngineError::DelistingWiringMismatch { .. })
            ),
            "{what}, got {outcome:?}"
        );
    }

    // The fully wired shape runs, so the four refusals above are not passing
    // because the guard refuses everything.
    run::backtest(&attached, &imputing(test_config(2)))
        .expect("a configuration recording both the convention and the file runs");
}

/// A record carrying an observed delisting return is honoured rather than
/// overwritten by the convention, and it counts in its own column.
///
/// Nothing on the current vendor produces one, which is exactly why this is
/// worth holding. The three states exist so that an assumption can be told
/// apart from a measurement, and a census whose `observed` column could only
/// ever read zero would be decoration.
///
/// ```text
///   the record observes -0.92 rather than assuming -0.30
///   terminal mark = 18 * (1 - 0.92) = 18 * 0.08               = 1.44
///   (1.44 + 0.5) / 20 - 1 = 1.94 / 20 - 1                     = -0.903
/// ```
#[test]
fn an_observed_delisting_return_is_used_rather_than_the_convention() {
    let mut record = delisting(
        asset("GONE", 1),
        date(2020, 4, 15),
        DelistingReason::Bankruptcy,
    );
    record.terminal = ingest::actions::TerminalValue::Observed(dec("-0.92"));

    let advance = hold_over_the_exit(&with_delistings(one_exit_panel(), &[record]));

    assert_eq!(
        advance.gross_return,
        dec("-0.903"),
        "a measured delisting return was replaced by the published convention"
    );
    assert_eq!(advance.exit_census.observed, 1);
    assert_eq!(advance.exit_census.imputed, 0);
}

/// The report carries the flag the printer selects on, and it comes from the
/// panel rather than from anything a caller could set independently.
#[test]
fn the_report_reads_its_delisting_flag_off_the_panel() {
    let panel = with_delistings(two_asset_panel(), &[]);
    let report = run::backtest(&panel, &imputing(test_config(2))).expect("backtests");
    assert!(report.delistings_imputed);

    let bare = run::backtest(&two_asset_panel(), &test_config(2)).expect("backtests");
    assert!(!bare.delistings_imputed);
}

/// A configuration in the shape a real delisting run produces: the convention
/// it applied, and the hash of the file it classified from.
///
/// Both together, because the engine refuses either on its own. Mirrors
/// `with_actions` in the dividend tests.
fn imputing(config: BacktestConfig) -> BacktestConfig {
    BacktestConfig {
        delisting_convention: Some(DELISTING_CONVENTION.to_string()),
        delistings_sha256: Some("d".repeat(64)),
        ..config
    }
}
