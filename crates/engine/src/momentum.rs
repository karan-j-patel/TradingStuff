//! The signal and the eligibility rules, which together decide what is held.
//!
//! # Which two prices the signal spans, and why that reading was chosen
//!
//! The spec describes the signal three ways: "cumulative return over months
//! t-12 through t-2", "skip the most recent month", and "the literature-
//! standard 12-1 momentum". Two of the three pin the same pair of prices and
//! the third is the same statement under the convention that `t` indexes the
//! month the portfolio is *held* rather than the month it is formed in.
//!
//! Twelve months of lookback ending at the rebalance month, with the most
//! recent one dropped, leaves eleven months of return running from the end of
//! month `t-12` to the end of month `t-1`. That is the pair used:
//!
//! ```text
//! signal(t) = close(month_end[t-1]) / close(month_end[t-12]) - 1
//! ```
//!
//! Reading "months t-12 through t-2" literally with `t` as the *formation*
//! month instead would span `month_end[t-13]` to `month_end[t-2]`, which skips
//! two months rather than one and so contradicts the other two descriptions.
//! That reading is not used.
//!
//! # Why nothing here can see the future
//!
//! Both endpoints are month-ends strictly at or before the rebalance date, and
//! [`crate::panel::Series`] has no accessor that returns a bar without being
//! told the latest date it may look at. The rebalance month's own return never
//! enters the signal, which is the property `e1_lookahead` holds.
//!
//! # Why [`rebalance_at`] lives in the momentum module and serves both
//! strategies
//!
//! Eligibility, the formation window, the quintile, and the tie-break are the
//! same for momentum and for low volatility. Only the per-name number and the
//! end of the ranking that is held differ, so [`rebalance_at`] computes the
//! window once and dispatches on [`crate::config::Strategy`]. Splitting it into
//! a selection module of its own would be the tidier home for it, and it is
//! deliberately not done in this round: moving it would rewrite the imports of
//! the momentum regression tests, and those are the evidence that adding a
//! second strategy left the first one alone.

use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::config::{BacktestConfig, Strategy, Weighting};
use crate::error::EngineError;
use crate::liquidity;
use crate::lowvol;
use crate::panel::Panel;

/// How many names each stage of a formation removed.
///
/// Reported per formation rather than summed, because the question a reader has
/// is where the cross-section went, and a total cannot answer it. Stages a
/// strategy does not have are zero: momentum runs no size screen and forms no
/// volatility half, so those counts stay at zero for it rather than being made
/// up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FormationCensus {
    /// Names clearing every rule that does not depend on a ranking.
    pub eligible: usize,
    /// Names the size screen removed from that set.
    pub size_screened: usize,
    /// Names kept by the low-volatility half.
    pub vol_half: usize,
    /// Names inside that half whose net payout yield or momentum could not be
    /// computed, and which were therefore dropped before the ranking.
    pub npy_ineligible: usize,
    /// Names actually held.
    pub held: usize,
}

/// What one rebalance date decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebalance {
    pub date: Date,
    /// Where `date` sits in `Panel::month_ends`. Carried so the holding loop
    /// can walk the month-ends between two formations, which is what keeps the
    /// marks monthly when the formations are quarterly.
    pub index: usize,
    /// Indices into `Panel::securities`, ascending, which is identity order.
    /// This is the set after any screen that removes names from the field the
    /// portfolio is chosen out of, which is what the buy-and-hold baseline
    /// holds.
    pub eligible: Vec<usize>,
    /// The held quintile, best first, where "best" is whichever end of the
    /// ranking the configured strategy takes.
    pub chosen: Vec<usize>,
    /// Signal per ranked name. For the two scalar strategies this is in
    /// `eligible` order and holds the signal itself, and under
    /// [`Strategy::LowVolatility`] the value is the trailing volatility rather
    /// than a return, where a smaller one is better. Under
    /// [`Strategy::ConservativeFormula`] it holds the averaged rank for the
    /// names that reached the ranking, which is a subset of `eligible`, and a
    /// smaller one is better there too.
    pub signals: Vec<(usize, Decimal)>,
    /// Where the cross-section went at this formation.
    pub census: FormationCensus,
}

/// The momentum signal for one security at one rebalance.
///
/// `None` when either endpoint price is missing or the starting price is not
/// positive. A missing signal excludes the name; it is never defaulted to zero,
/// because zero is a perfectly ordinary signal value and substituting it would
/// silently rank a name with no data in the middle of the field.
pub fn signal(
    panel: &Panel,
    security: usize,
    window_start: Date,
    window_end: Date,
) -> Option<Decimal> {
    let series = panel.securities().get(security)?;
    let start = series.close_at_month_end(window_start)?;
    let end = series.close_at_month_end(window_end)?;
    if start <= Decimal::ZERO {
        return None;
    }
    end.checked_div(start)?.checked_sub(Decimal::ONE)
}

/// Decide the portfolio at one rebalance date.
///
/// `index` is a position in `panel.month_ends()`. It must be at least
/// `config.required_lead_in()`, which the caller guarantees by where it starts
/// the loop.
/// The names that clear every rule which does not depend on a ranking, in
/// identity order.
///
/// Extracted rather than written twice because the conservative formula applies
/// the identical three rules over a longer window, and a coverage denominator
/// or a price floor that drifted between two copies would be invisible from
/// either one.
pub(crate) fn tradable(
    panel: &Panel,
    config: &BacktestConfig,
    date: Date,
    window_start: Date,
    window_end: Date,
) -> Result<Vec<usize>, EngineError> {
    // Denominator of the coverage rule: days the market was open across the
    // lead-in window, not days this particular name traded.
    let open_days = panel.trading_days_in(window_start, window_end);
    let required_days = config
        .min_coverage
        .checked_mul(Decimal::from(open_days))
        .ok_or_else(|| EngineError::math("sizing the coverage requirement"))?
        .ceil();

    let mut kept = Vec::new();
    for (position, series) in panel.securities().iter().enumerate() {
        // A name must have traded on the rebalance date. Without that there is
        // no price to buy at, and imputing one is inventing a fill.
        let Some(unadjusted) = series.close_unadjusted_on(date) else {
            continue;
        };
        if unadjusted < config.price_floor {
            continue;
        }
        if Decimal::from(series.bars_in(window_start, window_end)) < required_days {
            continue;
        }
        kept.push(position);
    }
    Ok(kept)
}

pub fn rebalance_at(
    panel: &Panel,
    config: &BacktestConfig,
    index: usize,
) -> Result<Rebalance, EngineError> {
    // The composite owns its selection, so it leaves here before anything below
    // runs. It is not a sort on one scalar, and the two matches further down
    // have nothing to put in a conservative arm for that reason.
    if config.strategy == Strategy::ConservativeFormula {
        return crate::conservative::rebalance_at(panel, config, index);
    }

    let month_ends = panel.month_ends();
    let date = month_ends[index];
    let window_start = month_ends[index - config.signal_lookback_months];
    let window_end = month_ends[index - config.signal_skip_months];

    // Read once, before the loop, and refused rather than defaulted. A default
    // staleness would produce a number under a hash that says nothing about
    // which bound produced it.
    let staleness = match config.strategy {
        Strategy::Value => config
            .book_staleness_days
            .ok_or(EngineError::ValueStalenessMissing)?,
        _ => 0,
    };

    let mut eligible = Vec::new();
    let mut signals = Vec::new();
    for position in tradable(panel, config, date, window_start, window_end)? {
        let value = match config.strategy {
            Strategy::Momentum => signal(panel, position, window_start, window_end),
            Strategy::LowVolatility => {
                lowvol::volatility(panel, position, window_start, window_end)
            }
            // The one signal that reads a date rather than a window. A name with
            // no visible filing, a stale one, non-positive book equity, or no
            // market cap at the formation returns `None` here and is dropped
            // just below, which is what puts the exclusion BEFORE the ranking.
            Strategy::Value => crate::value::book_to_market(panel, position, date, staleness),
            // Returned at the top of this function. The arm exists because the
            // compiler cannot see that, and it never runs.
            Strategy::ConservativeFormula => {
                unreachable!("the conservative formula selects through its own module")
            }
        };
        let Some(value) = value else {
            continue;
        };
        // A name that cannot be weighted cannot be held, so it leaves the field
        // here rather than being dropped out of the portfolio afterwards. The
        // order is the decision: excluding after the ranking would take the
        // quintile of a larger set and then remove names from the portfolio,
        // which is a portfolio of a different size from the one the quintile
        // describes. It is the same rule the conservative formula applies at
        // its step 3, one weighting over.
        //
        // Zero is excluded alongside absent. The provider quantises market cap
        // to a tenth of a million dollars, so a company below that quantum
        // arrives as exactly zero on a row it deliberately shipped. That is a
        // real value and the panel keeps it, but it divides into no weight.
        if config.weighting == Weighting::ValueByMarketcap {
            let holdable = panel.securities()[position]
                .marketcap_at_month_end(date)
                .is_some_and(|marketcap| marketcap > Decimal::ZERO);
            if !holdable {
                continue;
            }
        }
        eligible.push(position);
        signals.push((position, value));
    }

    // The tradability screen, after every other eligibility rule and before the
    // ranking. Order matters and this is the order the spec fixes: the fraction
    // is a fraction of the names that were otherwise going to be ranked, so
    // screening first would take a fifth of a larger set, and screening after
    // the quintile was chosen would remove names from the portfolio rather than
    // from the field it was chosen out of.
    //
    // `None` leaves both vectors exactly as they were, which is what keeps
    // every program except one byte-identical to the code from before this
    // existed.
    let (eligible, signals) = match config.liquidity_floor_fraction {
        None => (eligible, signals),
        Some(fraction) => {
            let kept = liquidity::screened(panel, fraction, &eligible, window_start, window_end)?;
            // `kept` is ascending, so a binary search decides membership in
            // logarithmic time. `Vec::contains` would be a linear scan inside a
            // loop over the same vector, which is quadratic in the number of
            // eligible names and this runs at every rebalance.
            let survives = |position: &usize| kept.binary_search(position).is_ok();
            (
                eligible.iter().copied().filter(survives).collect(),
                signals
                    .into_iter()
                    .filter(|(position, _)| survives(position))
                    .collect::<Vec<_>>(),
            )
        }
    };

    // Best signal first, where momentum wants the largest and low volatility
    // wants the smallest. Ties break on the security's position, which is
    // identity order, so a tie resolves the same way on every run rather than
    // however the sort happened to land. The tie-break is ascending in both
    // arms deliberately: it is an identity order, not a second signal, and
    // flipping it with the strategy would make the two strategies disagree
    // about which of two identical names to prefer.
    let mut ranked = signals.clone();
    match config.strategy {
        // Both want the largest signal first: the strongest trend, and the
        // cheapest book. Grouped rather than duplicated so the tie-break cannot
        // drift between them.
        Strategy::Momentum | Strategy::Value => {
            ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
        }
        Strategy::LowVolatility => {
            ranked.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
        }
        // Returned at the top of this function, same as the match above.
        Strategy::ConservativeFormula => {
            unreachable!("the conservative formula selects through its own module")
        }
    }

    // Integer division floors, so 9 eligible names give a quintile of 1 rather
    // than 2. The floor of 1 keeps a thin month holding something rather than
    // silently sitting in cash, and thin months are counted and reported.
    let quintile = (ranked.len() / config.quintile_divisor).max(1);
    let chosen: Vec<usize> = ranked
        .iter()
        .take(quintile)
        .map(|(position, _)| *position)
        .collect();

    let census = FormationCensus {
        eligible: eligible.len(),
        held: chosen.len(),
        ..FormationCensus::default()
    };
    Ok(Rebalance {
        date,
        index,
        eligible,
        chosen,
        signals,
        census,
    })
}
