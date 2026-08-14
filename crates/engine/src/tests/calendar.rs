//! The panel calendar's one invariant: month-ends are calendar-consecutive.
//!
//! # Why this is worth its own module
//!
//! Every window in this crate is an index offset into `Panel::month_ends`.
//! Momentum reads `index - 12`, the conservative formula `index - 36`, the
//! export's forward label `index + 1`, and each is written and documented as a
//! span of calendar months. `month_ends_of` builds that vector from the months
//! PRESENT in the data and proves nothing about the ones that are not, so
//! without a guard every one of those offsets is really "however many
//! month-ends happen to exist" and the two only coincide when the data is
//! complete.
//!
//! The guard is at construction rather than at each reader, so the property is
//! established once and inherited by every consumer, including ones not written
//! yet. These tests are what say it can fail.

use super::{asset, bar, dec, month_ends, panel_of};
use crate::error::EngineError;
use crate::panel::Panel;
use crate::portfolio;
use jiff::civil::date;

/// A panel missing an entire calendar month is refused at construction.
///
/// # What the fixture is
///
/// Two securities, both trading in January and March 2020 and neither in
/// February. This is a market-wide hole rather than one name going quiet: the
/// calendar is the union of every security's trading days, so February is
/// absent from the panel entirely.
///
/// # What it would cost to tolerate
///
/// The month-ends would be `[2020-01-31, 2020-03-31]` and every consumer's
/// index arithmetic would read them as adjacent months. The export's forward
/// label at index 0 would be `total_return(Jan 31, Mar 31)`, a two-month return
/// written into a column named `label_return_1m`, and the fit that consumes it
/// would train on a target that is sometimes one month and sometimes two with
/// nothing distinguishing them. The same slip applies to every momentum and
/// volatility window over the gap.
///
/// The error names both ends, because a reader meeting this on a real fetch
/// needs to know which month to go and get rather than only that something is
/// wrong.
#[test]
fn a_panel_missing_a_calendar_month_is_refused() {
    let january = date(2020, 1, 31);
    let march = date(2020, 3, 31);
    let bars = vec![
        bar(&asset("AAA", 1), january, dec("100"), dec("100")),
        bar(&asset("AAA", 1), march, dec("120"), dec("120")),
        bar(&asset("BBB", 2), january, dec("50"), dec("50")),
        bar(&asset("BBB", 2), march, dec("60"), dec("60")),
    ];

    let error = Panel::from_bars(bars).expect_err("a panel skipping February must be refused");
    assert!(
        matches!(
            error,
            EngineError::PanelMonthGap { before, after } if before == january && after == march
        ),
        "got {error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("2020-01-31"), "got {message}");
    assert!(message.contains("2020-03-31"), "got {message}");
}

/// The guard refuses a gap and nothing else.
///
/// Without this the test above would pass against a constructor that refused
/// every panel. The six consecutive month-ends every other fixture in this
/// crate is built on still build, and so does a single-month panel, which has
/// no pair to compare and is trivially consecutive rather than a special case.
#[test]
fn a_consecutive_panel_still_builds() {
    let m = month_ends();
    let panel = panel_of(&[(
        asset("AAA", 1),
        m.iter().map(|day| (*day, dec("100"))).collect(),
    )]);
    assert_eq!(panel.month_ends().len(), m.len());

    let one_month = Panel::from_bars(vec![bar(
        &asset("AAA", 1),
        date(2020, 1, 31),
        dec("100"),
        dec("100"),
    )])
    .expect("a single month has no pair to be inconsistent about");
    assert_eq!(one_month.month_ends().len(), 1);
}

/// A security that goes quiet for a month is not a gap in the calendar.
///
/// The distinction the guard rests on, asserted rather than assumed. One name
/// missing a month is an ordinary event that the delisting and coverage rules
/// already handle; a month in which nothing traded anywhere is a missing fetch.
/// A guard that could not tell them apart would refuse most real panels.
#[test]
fn one_security_going_quiet_is_not_a_calendar_gap() {
    let m = month_ends();
    let quiet = asset("QUIET", 1);
    let panel = panel_of(&[
        // No bar at m[1] at all.
        (
            quiet.clone(),
            vec![(m[0], dec("100")), (m[2], dec("110")), (m[3], dec("120"))],
        ),
        (
            asset("KEEP", 2),
            m.iter().map(|day| (*day, dec("50"))).collect(),
        ),
    ]);

    assert_eq!(panel.month_ends().len(), m.len());
    // And the quiet name really is quiet at m[1], so the fixture is exercising
    // the case it claims to.
    assert_eq!(
        portfolio::total_return(&panel.securities()[0], m[0], m[1])
            .expect("the arithmetic holds")
            .map(|(_, delisted)| delisted),
        Some(true),
        "the fixture's quiet name has a bar at m1 after all"
    );
}
