//! X-D1 through X-D4 and X-D6, the engine half of the deletion probe.
//!
//! X-D5, the log-untouched claim, is the CLI's: the trial log is only reachable
//! there.
//!
//! # Why the fixture has seventy names
//!
//! The set is the top fifty and the buffer sits at rank sixty, so a deletion
//! cannot even be expressed with fewer than sixty-one ranked names. The fixture
//! is generated rather than written out: name `k` carries cap `(71 - k) * 1000`
//! at every month-end, so its rank IS `k` until something moves it, and prices
//! are flat at 100 unless a test says otherwise.
//!
//! Caps and prices are independent in this fixture, which is what lets a test
//! set a trailing return without disturbing a single rank.

use ingest::actions::{Delisting, DelistingReason};
use ingest::marketcap::MarketCapRecord;
use jiff::civil::{Date, date};
use rust_decimal::Decimal;

use super::{ACTIONS_SHA256, DELISTINGS_SHA256, MARKETCAP_SHA256, asset, cash_dividend, dec};
use crate::deletions::{self, BUFFER_RANK, TOP_N};
use crate::panel::Panel;

/// Enough month-ends for a twelve-month window after an event at index 3.
const MONTHS: usize = 18;

/// Seventy, so rank sixty-one exists.
const NAMES: u64 = 70;

fn months() -> Vec<Date> {
    (0..MONTHS as i16)
        .map(|step| {
            date(
                2020 + step / 12,
                i8::try_from(step % 12).expect("month fits") + 1,
                28,
            )
        })
        .collect()
}

fn ticker(name: u64) -> String {
    format!("N{name:03}")
}

/// The base panel with overrides applied.
///
/// `caps` and `prices` are `(name, month index, value)`. `stops` is `(name,
/// month index)` and removes every bar from that month on, which is what a name
/// that stopped trading looks like. `dividends` is `(name, month index,
/// amount)`.
fn deletion_panel(
    caps: &[(u64, usize, &str)],
    prices: &[(u64, usize, &str)],
    stops: &[(u64, usize)],
    dividends: &[(u64, usize, &str)],
) -> Panel {
    let days = months();
    let mut bars = Vec::new();
    let mut cap_rows = Vec::new();

    for name in 1..=NAMES {
        let key = asset(&ticker(name), name);
        let stop = stops
            .iter()
            .find(|(who, _)| *who == name)
            .map(|(_, index)| *index);

        for (index, day) in days.iter().enumerate() {
            if stop.is_some_and(|first_absent| index >= first_absent) {
                continue;
            }
            let close = prices
                .iter()
                .find(|(who, when, _)| *who == name && *when == index)
                .map_or("100", |(_, _, value)| value);
            bars.push(super::bar(&key, *day, dec(close), dec(close)));

            // Rank k by construction, until a cap override moves it.
            let cap = caps
                .iter()
                .find(|(who, when, _)| *who == name && *when == index)
                .map(|(_, _, value)| (*value).to_string())
                .unwrap_or_else(|| ((NAMES + 1 - name) * 1000).to_string());
            cap_rows.push(MarketCapRecord {
                asset: key.clone(),
                date: *day,
                marketcap: dec(&cap),
                source: "synthetic".to_string(),
            });
        }
    }

    let dividend_rows: Vec<_> = dividends
        .iter()
        .map(|(name, index, amount)| {
            cash_dividend(&asset(&ticker(*name), *name), days[*index], dec(amount))
        })
        .collect();
    // A name that stopped trading is classified, so the sensitivity split has
    // something to split on.
    let exits: Vec<Delisting> = stops
        .iter()
        .map(|(name, index)| {
            super::delisting(
                asset(&ticker(*name), *name),
                days[*index],
                DelistingReason::Bankruptcy,
            )
        })
        .collect();

    Panel::from_bars(bars)
        .expect("fixture panel builds")
        .with_dividends(&dividend_rows, ACTIONS_SHA256)
        .expect("fixture dividends attach")
        .with_delistings(&exits, DELISTINGS_SHA256)
        .expect("fixture delistings attach")
        .with_marketcaps(&cap_rows, MARKETCAP_SHA256)
        .expect("fixture market caps attach")
}

/// The cap that drops a name to a chosen rank at one month.
///
/// Base caps are `(71 - k) * 1000`, so a value strictly between the caps of the
/// names at `rank` and `rank + 1` lands there once the displaced names shift up
/// by one. Written as a function so a test states the rank it wants rather than
/// a magic number.
fn cap_for_rank(rank: usize) -> String {
    let above = (NAMES + 1 - rank as u64) * 1000;
    (above - 500).to_string()
}

/// X-D1. The event boundary, on both sides.
///
/// ```text
///   rank 50 at m-1, rank 61 at m   -> a deletion
///   rank 50 at m-1, rank 60 at m   -> not one, the buffer absorbs it
///   rank 51 at m-1, rank 62 at m   -> not one, it was never in the set
/// ```
///
/// The buffer exists because a name oscillating either side of rank fifty would
/// otherwise register a deletion and a readmission every month, and the probe
/// would be measuring rank jitter.
#[test]
fn x_d1_the_event_boundary_holds_on_both_sides() {
    let past_buffer = cap_for_rank(61);
    let inside_buffer = cap_for_rank(60);
    let never_in_set = cap_for_rank(62);

    // In the set, falls past the buffer: an event.
    let deleted = deletion_panel(&[(50, 3, &past_buffer)], &[], &[], &[]);
    let probe = deletions::probe(&deleted).expect("the fixture probes");
    assert_eq!(
        probe
            .events
            .iter()
            .map(|event| (event.ticker.as_str(), event.rank_before, event.rank_after))
            .collect::<Vec<_>>(),
        vec![("N050", 50, 61)],
        "a name at the edge of the set falling past the buffer is not the one \
         event this fixture contains"
    );

    // In the set, falls only to the buffer: not an event.
    let jittered = deletion_panel(&[(50, 3, &inside_buffer)], &[], &[], &[]);
    assert!(
        deletions::probe(&jittered)
            .expect("probes")
            .events
            .is_empty(),
        "a fall to rank {BUFFER_RANK} counted as a deletion, so rank jitter at \
         the boundary is being measured as index activity"
    );

    // Never in the set, falls past the buffer: not an event.
    let outsider = deletion_panel(&[(51, 3, &never_in_set)], &[], &[], &[]);
    assert!(
        deletions::probe(&outsider)
            .expect("probes")
            .events
            .is_empty(),
        "a name that was never in the top {TOP_N} was deleted from it"
    );
}

/// X-D2. A name that stops trading is not a deletion.
///
/// Nobody rebalanced out of it on a rule; it stopped existing. Counting it would
/// put the very event the hypothesis is about in the same bucket as the thing it
/// has to be distinguished from, and since a bankruptcy is followed by no
/// rebound at all it would drag every aggregate down for a reason that has
/// nothing to do with index selling.
#[test]
fn x_d2_a_name_that_stops_trading_is_not_a_deletion() {
    let panel = deletion_panel(&[], &[], &[(50, 3)], &[]);
    let probe = deletions::probe(&panel).expect("the fixture probes");

    assert!(
        probe.events.is_empty(),
        "a name whose bars stop at the event month was counted as a deletion"
    );
    assert_eq!(
        probe.delisting_exits, 1,
        "the exit was neither an event nor counted in the split, so it left the \
         probe without a word"
    );
}

/// X-D3. The control is the closest trailing return, not the closest rank.
///
/// # The fixture
///
/// ```text
///   name   rank at m-1   trailing 3m return   return over (m, m+3]
///   N050   50 -> 61      -0.5   the event     0
///   N045   45            -0.45  closest       0
///   N070   70            -0.10  farther      +5.0
/// ```
///
/// Both candidates sit in the control band. Matching on rank distance picks
/// N045 too, so that alone would not discriminate; the fixture makes the
/// SECOND-closest trailing return also the rank-farthest name, and gives it a
/// post-window return five hundred points away. A matcher that reaches for it
/// moves the excess from 0 to -5.
#[test]
fn a_control_that_fell_without_ever_being_in_the_set_is_still_eligible() {
    // N055 was rank 55 at m-1: never in the top 50, so its fall past the
    // buffer is not a deletion event, and it is the closest name by trailing
    // return. A matcher that rejects every candidate ranked past the buffer at
    // m confuses "fell like the event did" with "is an event", and quietly
    // hands the comparison to a worse match. Found by cross-model review.
    // N055's own departure shifts every rank above it up one, so the event's
    // cap must land clearly past the buffer AFTER that shift: 5500 sits below
    // N065's 6000 and puts N050 at rank 65.
    let panel = deletion_panel(
        &[(50, 3, "5500"), (55, 3, "6100")],
        &[
            // The event: 100 at m-3, 50 at m. Trailing -50%.
            (50, 3, "50"),
            (50, 4, "50"),
            (50, 5, "50"),
            (50, 6, "50"),
            // Closest trailing return (-48%), itself fallen past the buffer.
            (55, 3, "52"),
            (55, 4, "52"),
            (55, 5, "52"),
            (55, 6, "52"),
            // Second closest (-45%), the wrong answer.
            (45, 3, "55"),
            (45, 4, "55"),
            (45, 5, "55"),
            (45, 6, "55"),
        ],
        &[],
        &[],
    );

    let probe = deletions::probe(&panel).expect("the fixture probes");
    let event = probe
        .events
        .iter()
        .find(|event| event.ticker == "N050")
        .expect("the deletion is detected");
    assert_eq!(
        event.control.as_deref(),
        Some("N055"),
        "the closest-return control was rejected for falling in rank while \
         never having been in the set, so a non-event was treated as an event"
    );
}

#[test]
fn x_d3_the_control_is_matched_on_trailing_return() {
    let past_buffer = cap_for_rank(61);
    let panel = deletion_panel(
        &[(50, 3, &past_buffer)],
        &[
            // The event: 100 at m-3, 50 at m, flat after.
            (50, 3, "50"),
            (50, 4, "50"),
            (50, 5, "50"),
            (50, 6, "50"),
            // Closest trailing return, flat afterwards.
            (45, 3, "55"),
            (45, 4, "55"),
            (45, 5, "55"),
            (45, 6, "55"),
            // Farther trailing return, and a huge post-window rebound.
            (70, 3, "90"),
            (70, 4, "540"),
            (70, 5, "540"),
            (70, 6, "540"),
        ],
        &[],
        &[],
    );

    let probe = deletions::probe(&panel).expect("the fixture probes");
    let event = probe
        .events
        .iter()
        .find(|event| event.ticker == "N050")
        .expect("the deletion is detected");

    assert_eq!(
        event.control.as_deref(),
        Some("N045"),
        "the control is not the closest name by trailing return, so the \
         comparison is a name that fell against one that did not"
    );

    let three = event
        .windows
        .iter()
        .find(|window| window.months == 3)
        .expect("the three-month window");
    assert_eq!(three.event, Some(dec("0")));
    assert_eq!(three.control, Some(dec("0")));
    assert_eq!(
        three.excess(),
        Some(dec("0")),
        "the excess is not event minus control"
    );

    // The fixture's discriminating property, asserted rather than assumed: the
    // second-closest candidate returns five hundred points over the same
    // window, so a matcher that reaches for it moves the excess from 0 to -5
    // rather than landing on the same answer by luck.
    let days = months();
    let rebounder = panel
        .securities()
        .iter()
        .find(|series| series.asset.ticker == "N070")
        .expect("the fixture holds N070");
    assert_eq!(rebounder.close_at_month_end(days[3]), Some(dec("90")));
    assert_eq!(rebounder.close_at_month_end(days[6]), Some(dec("540")));
}

/// X-D4. The window excludes the event month's own return.
///
/// The month a name leaves the set is the selling-pressure month, and folding it
/// into the measurement is the classic way a study like this flatters itself: it
/// books the fall and the rebound together and reports the rebound.
///
/// The fixture halves the price into the event month and holds it flat after, so
/// the correct answer is exactly zero and a closed-open window reads -0.5.
#[test]
fn x_d4_the_window_excludes_the_event_months_own_return() {
    let past_buffer = cap_for_rank(61);
    let panel = deletion_panel(
        &[(50, 3, &past_buffer)],
        &[(50, 3, "50"), (50, 4, "50"), (50, 5, "50"), (50, 6, "50")],
        &[],
        &[],
    );

    let probe = deletions::probe(&panel).expect("the fixture probes");
    let event = &probe.events[0];
    let three = event
        .windows
        .iter()
        .find(|window| window.months == 3)
        .expect("the three-month window");

    assert_eq!(
        three.event,
        Some(dec("0")),
        "the window carries the event month's own fall, so the probe measures \
         the selling pressure and the rebound as one number"
    );
}

/// X-D6. A dividend inside the window reaches the return.
///
/// The measurement is a total return, so a holder's dividend is part of the
/// rebound being claimed. Dropping it understates every event by whatever the
/// name paid, which is a bias with a direction rather than noise.
///
/// Price flat at 50 across the window and a dividend of 5 inside it, so the
/// return is exactly the yield: `(50 + 5) / 50 - 1 = 0.1`.
#[test]
fn x_d6_a_dividend_inside_the_window_reaches_the_return() {
    let past_buffer = cap_for_rank(61);
    let panel = deletion_panel(
        &[(50, 3, &past_buffer)],
        &[(50, 3, "50"), (50, 4, "50"), (50, 5, "50"), (50, 6, "50")],
        &[],
        // Paid at m+2, inside (m, m+3].
        &[(50, 5, "5")],
    );

    let probe = deletions::probe(&panel).expect("the fixture probes");
    let three = probe.events[0]
        .windows
        .iter()
        .find(|window| window.months == 3)
        .expect("the three-month window");

    assert_eq!(
        three.event,
        Some(dec("0.1")),
        "the dividend paid inside the window is missing from the return, so \
         every event is understated by whatever the name paid"
    );

    // The control's window carries no dividend, so the excess is the yield and
    // a dropped dividend term moves it rather than cancelling.
    assert_eq!(three.control, Some(dec("0")));
    assert_eq!(three.excess(), Some(dec("0.1")));
}

/// The sensitivity split reports both sides, and they differ when one event
/// delists inside the window.
///
/// A probe that only ever reported one aggregate would let a reader treat the
/// convenient side as the answer. Both are printed and neither is the headline.
#[test]
fn the_sensitivity_split_separates_events_that_delist_inside_the_window() {
    let past_buffer = cap_for_rank(61);
    let panel = deletion_panel(
        &[(50, 3, &past_buffer)],
        &[(50, 3, "50")],
        // Trading stops two months after the event, inside both windows.
        &[(50, 5)],
        &[],
    );

    let probe = deletions::probe(&panel).expect("the fixture probes");
    assert_eq!(probe.events.len(), 1, "the deletion is still an event");
    let three = probe.events[0]
        .windows
        .iter()
        .find(|window| window.months == 3)
        .expect("the three-month window");
    assert!(
        three.delisted_in_window,
        "an event whose name stopped trading inside the window is not flagged, \
         so the split cannot separate it"
    );

    assert_eq!(
        probe.summarise(3, false).events,
        0,
        "excluding in-window delistings did not exclude this one"
    );
    assert_eq!(
        probe.summarise(3, true).events,
        1,
        "including in-window delistings did not include this one"
    );
}

/// No event means no invented aggregate.
///
/// On a probe whose counts are small by construction, the empty case is the one
/// a reader is most likely to meet, and a zero printed where a mean does not
/// exist reads as a measured null.
#[test]
fn an_empty_probe_reports_absence_rather_than_zero() {
    let probe = deletions::probe(&deletion_panel(&[], &[], &[], &[])).expect("probes");
    assert!(probe.events.is_empty());

    let summary = probe.summarise(3, true);
    assert_eq!(summary.events, 0);
    assert_eq!(summary.mean, None, "a mean was invented for no events");
    assert_eq!(summary.median, None);
    assert_eq!(summary.spread, None);
}

/// The probe type carries no Sharpe, and cannot.
///
/// A structural claim rather than a promise: this is the assertion that fails if
/// somebody adds one, and the module documentation says why adding one converts
/// the probe into a counted backtest.
#[test]
fn the_probe_reports_no_sharpe() {
    let probe = deletions::probe(&deletion_panel(&[], &[], &[], &[])).expect("probes");
    let rendered = format!("{probe:?}");
    assert!(
        !rendered.to_lowercase().contains("sharpe"),
        "the probe's own debug output names a Sharpe, so one reached a type that \
         must not carry it: {rendered}"
    );
    assert!(!Decimal::is_sign_negative(&Decimal::ZERO));
}
