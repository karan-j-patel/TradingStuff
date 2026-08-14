//! Index-deletion probe: do names dropping out of the large-cap set rebound?
//!
//! # Where this idea came from, and why that is written down
//!
//! The user, not the literature. He queued it last for exactly that reason: it
//! is a guess, it carries the weakest evidence of anything tested in this
//! project, and his own power statement is recorded below. The probe exists to
//! look, not to conclude.
//!
//! # The hypothesis
//!
//! Index funds sell a deleted name mechanically, regardless of merit, so the
//! months after a deletion may carry a rebound.
//!
//! # The proxy, and its two defects stated before any number exists
//!
//! This platform holds NO index-membership data and no new vendor requests were
//! authorised. So the set is synthetic: the top [`TOP_N`] of our own sample by
//! market cap at each month-end, and an exit from it stands in for a deletion.
//! That is honest only alongside what it cannot be:
//!
//! 1. **Nobody sells anything because of our set.** The proxy correlates with
//!    real deletions only insofar as our sample's largest names are genuinely
//!    index members and a sharp cap decline coincides with a real removal. In a
//!    1-in-17 sample the top fifty is the region where that is plausible, which
//!    is a reason to look there and not a reason to believe the answer.
//! 2. **The counts will be small.** [`POWER_CEILING`] is not a caveat softening
//!    a conclusion, it is the conclusion's ceiling. A null here is nearly
//!    uninformative. Only a large and consistent positive would justify
//!    spending anything further, and the default outcome is that nothing else
//!    happens.
//!
//! # Why this is not a backtest, and cannot become one by accident
//!
//! It computes returns, because measuring a rebound is measuring a return. What
//! it does not compute, anywhere, is a Sharpe: [`Probe`] and [`EventRow`] carry
//! event counts, per-window returns and excesses over a control, and no field a
//! Sharpe could go in. Nothing here can be presented as performance or selected
//! on as one, and the trial log is not opened for writing on this path.
//!
//! The hypothesis is still counted. It is declared through the CLI's
//! `--config` door under its own program before the probe runs, because a
//! return hypothesis tested off the books is exactly how a trial count rots.
//! That declaration is a separate deliberate step; this tool stays read-only on
//! the log and `x_d5` pins it.

use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::error::EngineError;
use crate::panel::{Panel, Series};

/// Printed with every probe, so a transcript explains itself.
pub const NOT_A_TRIAL: &str = "\
This is an event probe, not a backtest, and no trial is recorded here. It
  computes no Sharpe: the types it reports carry event counts and per-window
  returns against a control, and have no field a Sharpe could go in. The
  hypothesis itself IS counted, declared through the --config door under its
  own program before this ran.";

/// The power statement, printed beside every figure this produces.
///
/// Written before any number existed and printed whatever the numbers say, so a
/// reader cannot meet the result without meeting its ceiling.
pub const POWER_CEILING: &str = "\
Power ceiling, fixed before any figure existed. The synthetic set is a PROXY
  for index membership and nobody sells a stock for leaving it. Event counts are
  small by construction. A null here is nearly uninformative and must not be
  read as evidence against the hypothesis; only a large, consistent positive
  would justify spending anything further.";

/// How many names the synthetic large-cap set holds, recomputed monthly.
pub const TOP_N: usize = 50;

/// A name must fall past this rank to count as deleted.
///
/// Ten ranks of buffer below [`TOP_N`]. Without it a name oscillating around
/// rank fifty would register as a deletion and a readmission every month, and
/// the probe would measure rank jitter rather than deletions.
pub const BUFFER_RANK: usize = 60;

/// The rank band at `m-1` a control is drawn from, inclusive both ends.
///
/// Straddles the boundary, so a control is a name of comparable size that did
/// not leave the set.
pub const CONTROL_RANKS: std::ops::RangeInclusive<usize> = 40..=70;

/// Months of trailing return the control is matched on.
pub const TRAILING_MONTHS: usize = 3;

/// The measurement windows, in months after the event month.
pub const WINDOWS: &[usize] = &[3, 12];

/// One deletion event and what happened after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    /// The month-end the name left the set on.
    pub date: Date,
    pub ticker: String,
    pub rank_before: usize,
    pub rank_after: usize,
    /// The control's ticker, or `None` when the band held no eligible name.
    pub control: Option<String>,
    /// One entry per [`WINDOWS`] length, in that order.
    pub windows: Vec<WindowReturn>,
}

/// What the event and its control returned over one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowReturn {
    pub months: usize,
    pub event: Option<Decimal>,
    pub control: Option<Decimal>,
    /// Whether the event's name stopped trading inside this window.
    ///
    /// Its return is still computed, marked at the delisting convention the run
    /// is configured with, exactly as the backtest marks an exit. The flag is
    /// what lets the aggregate be reported both with and without them, which is
    /// the sensitivity split.
    pub delisted_in_window: bool,
}

impl WindowReturn {
    /// Event minus control, or `None` when either side has no return.
    pub fn excess(&self) -> Option<Decimal> {
        self.event?.checked_sub(self.control?)
    }
}

/// Everything the probe found.
///
/// No Sharpe field, by construction. See the module documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub events: Vec<EventRow>,
    /// Names in the set at `m-1` that stopped trading at `m`.
    ///
    /// Not deletions. A name that leaves because it was acquired or went
    /// bankrupt was not sold by an index fund tracking a rule, and counting it
    /// would put the very event the hypothesis is about in the same bucket as
    /// the thing it must be distinguished from. Counted rather than dropped, so
    /// a reader sees how many exits the rule refused.
    pub delisting_exits: usize,
}

/// The aggregate over one window, for one side of the sensitivity split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub events: usize,
    pub mean: Option<Decimal>,
    pub median: Option<Decimal>,
    /// Population standard deviation, matching `rigor::moments`, so the two
    /// dispersion figures in this codebase mean the same thing.
    pub spread: Option<Decimal>,
}

impl Probe {
    /// Mean, median and spread of the excess return over one window.
    ///
    /// `include_delisted` selects the sensitivity split: `false` drops events
    /// whose name stopped trading inside the window, `true` keeps them at their
    /// imputed exit mark.
    pub fn summarise(&self, months: usize, include_delisted: bool) -> Summary {
        let mut excesses: Vec<Decimal> = self
            .events
            .iter()
            .filter_map(|event| event.windows.iter().find(|window| window.months == months))
            .filter(|window| include_delisted || !window.delisted_in_window)
            .filter_map(WindowReturn::excess)
            .collect();
        excesses.sort();

        let count = excesses.len();
        if count == 0 {
            return Summary {
                events: 0,
                mean: None,
                median: None,
                spread: None,
            };
        }

        let total = excesses
            .iter()
            .try_fold(Decimal::ZERO, |running, value| running.checked_add(*value));
        let mean = total.and_then(|total| total.checked_div(Decimal::from(count as u64)));

        // Even counts average the middle pair, which is the ordinary
        // definition and matters here because event counts are small enough
        // that one observation moves the answer.
        let median = if count % 2 == 1 {
            Some(excesses[count / 2])
        } else {
            excesses[count / 2 - 1]
                .checked_add(excesses[count / 2])
                .and_then(|pair| pair.checked_div(Decimal::TWO))
        };

        let spread = mean.filter(|_| count >= 2).and_then(|mean| {
            excesses
                .iter()
                .try_fold(Decimal::ZERO, |running, value| {
                    let deviation = value.checked_sub(mean)?;
                    running.checked_add(deviation.checked_mul(deviation)?)
                })
                .and_then(|sum| sum.checked_div(Decimal::from(count as u64)))
                .and_then(|variance| rust_decimal::MathematicalOps::sqrt(&variance))
        });

        Summary {
            events: count,
            mean,
            median,
            spread,
        }
    }
}

/// Securities ranked by market cap at `month_end`, largest first.
///
/// Rank one is the largest. A name with no cap at that month-end has no rank
/// and is absent from the result: it cannot be in the set, and it cannot be
/// shown to have left one.
///
/// Ties break on identity order, which is the panel's own ordering, so a run is
/// reproducible rather than depending on how the sort landed.
fn ranks_at(panel: &Panel, month_end: Date) -> Vec<(usize, usize)> {
    let mut by_cap: Vec<(Decimal, usize)> = panel
        .securities()
        .iter()
        .enumerate()
        .filter_map(|(index, series)| {
            series
                .marketcap_at_month_end(month_end)
                .map(|cap| (cap, index))
        })
        .collect();
    // Descending by cap, ascending by identity within a tie.
    by_cap.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    by_cap
        .into_iter()
        .enumerate()
        .map(|(position, (_, index))| (index, position + 1))
        .collect()
}

/// The rank of one security in a ranking, if it has one.
fn rank_of(ranks: &[(usize, usize)], security: usize) -> Option<usize> {
    ranks
        .iter()
        .find(|(index, _)| *index == security)
        .map(|(_, rank)| *rank)
}

/// Total return over `(from, to]`, dividends included.
///
/// The window is half-open at the start, so the month the event happened
/// contributes its closing price as the denominator and NOT its own return. The
/// event month is the selling-pressure month; folding it into the measurement
/// is the classic way a study like this flatters itself, and `x_d4` is the pin.
///
/// A name that stops trading inside the window is marked at its last close
/// times the delisting factor the run is configured with, which is what
/// `crate::portfolio::advance` does for a held position. Returning `None`
/// instead would silently drop exactly the events the sensitivity split exists
/// to show.
fn total_return(
    series: &Series,
    from: Date,
    to: Date,
) -> Result<Option<(Decimal, bool)>, EngineError> {
    let Some(open) = series.close_at_month_end(from) else {
        return Ok(None);
    };
    if open <= Decimal::ZERO {
        return Ok(None);
    }

    let (close, delisted) = match series.close_at_month_end(to) {
        Some(close) => (close, false),
        None => {
            let Some((last_bar, last)) = series.last_close_on_or_before(to) else {
                return Ok(None);
            };
            let mark = series.exit_mark_in(last_bar, to);
            match last.checked_mul(mark.terminal_factor()?) {
                Some(marked) => (marked, true),
                None => return Ok(None),
            }
        }
    };

    let cash = series.dividend_cash_in(from, to)?;
    Ok(close
        .checked_add(cash)
        .and_then(|marked| marked.checked_div(open))
        .and_then(|ratio| ratio.checked_sub(Decimal::ONE))
        .map(|value| (value, delisted)))
}

/// Run the probe over a panel.
///
/// Needs market caps for the ranking and prices for the returns; dividends and
/// delistings change what the returns mean and the caller is expected to attach
/// all four, which the CLI enforces.
pub fn probe(panel: &Panel) -> Result<Probe, EngineError> {
    let month_ends = panel.month_ends();
    let longest = WINDOWS.iter().copied().max().unwrap_or(0);

    let mut events = Vec::new();
    let mut delisting_exits = 0usize;

    // From the second month-end, since an event needs a previous ranking, and
    // stopping `longest` short of the end so every event has a full window. A
    // truncated final window would put fewer months of return beside the same
    // number of events and read as a result.
    for index in 1..month_ends.len().saturating_sub(longest) {
        let before = month_ends[index - 1];
        let month = month_ends[index];
        let ranked_before = ranks_at(panel, before);
        let ranked_now = ranks_at(panel, month);

        for (security, rank_before) in ranked_before.iter().copied() {
            if rank_before > TOP_N {
                continue;
            }
            let series = &panel.securities()[security];

            // An exit by delisting or merger is not a deletion. Nobody rebalanced
            // out of it on a rule; it stopped existing.
            if series.close_at_month_end(month).is_none() {
                delisting_exits += 1;
                continue;
            }

            let Some(rank_after) = rank_of(&ranked_now, security) else {
                continue;
            };
            if rank_after <= BUFFER_RANK {
                continue;
            }

            let control = control_for(panel, &ranked_before, &ranked_now, security, month, index)?;
            let mut windows = Vec::with_capacity(WINDOWS.len());
            for months in WINDOWS.iter().copied() {
                let end = month_ends[index + months];
                let event = total_return(series, month, end)?;
                let control_return = match control {
                    None => None,
                    Some(other) => total_return(&panel.securities()[other], month, end)?
                        .map(|(value, _)| value),
                };
                windows.push(WindowReturn {
                    months,
                    event: event.map(|(value, _)| value),
                    control: control_return,
                    delisted_in_window: event.is_some_and(|(_, delisted)| delisted),
                });
            }

            events.push(EventRow {
                date: month,
                ticker: series.asset.ticker.clone(),
                rank_before,
                rank_after,
                control: control.map(|index| panel.securities()[index].asset.ticker.clone()),
                windows,
            });
        }
    }

    Ok(Probe {
        events,
        delisting_exits,
    })
}

/// The control for one event: the closest trailing return in the rank band.
///
/// Matching on the trailing return is the whole point of the control. Without
/// it the comparison is "a name that fell hard and left the set" against "a name
/// that did not fall", and any rebound measured is just mean reversion after a
/// fall. With it, both sides fell about as far and only one of them left.
///
/// Ties break on identity order, which is the panel's own ordering, so the same
/// data gives the same control on every run.
fn control_for(
    panel: &Panel,
    ranked_before: &[(usize, usize)],
    ranked_now: &[(usize, usize)],
    event: usize,
    month: Date,
    index: usize,
) -> Result<Option<usize>, EngineError> {
    let month_ends = panel.month_ends();
    let Some(trailing_start) = index
        .checked_sub(TRAILING_MONTHS)
        .map(|start| month_ends[start])
    else {
        return Ok(None);
    };
    let Some((event_trailing, _)) =
        total_return(&panel.securities()[event], trailing_start, month)?
    else {
        return Ok(None);
    };

    let mut best: Option<(Decimal, usize)> = None;
    for (candidate, rank_before) in ranked_before.iter().copied() {
        if candidate == event || !CONTROL_RANKS.contains(&rank_before) {
            continue;
        }
        // Non-event only. A candidate that was in the set and left past the
        // buffer is another deletion event, not a control for one. A candidate
        // that fell past the buffer WITHOUT ever being in the set is exactly
        // the comparison wanted: it fell like the event did and was not
        // deleted, so rejecting it hands the match to a worse name. The first
        // shipped version rejected every faller; cross-model review caught it.
        match rank_of(ranked_now, candidate) {
            None => continue,
            Some(rank_after) if rank_before <= TOP_N && rank_after > BUFFER_RANK => continue,
            Some(_) => {}
        }
        let Some((trailing, _)) =
            total_return(&panel.securities()[candidate], trailing_start, month)?
        else {
            continue;
        };
        let Some(distance) = trailing.checked_sub(event_trailing).map(|gap| gap.abs()) else {
            continue;
        };
        // Strictly closer, so a tie keeps the earlier identity.
        if best.is_none_or(|(closest, _)| distance < closest) {
            best = Some((distance, candidate));
        }
    }

    Ok(best.map(|(_, candidate)| candidate))
}
