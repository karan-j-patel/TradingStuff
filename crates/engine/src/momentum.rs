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

use crate::config::{BacktestConfig, Strategy};
use crate::error::EngineError;
use crate::lowvol;
use crate::panel::Panel;

/// What one rebalance date decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebalance {
    pub date: Date,
    /// Indices into `Panel::securities`, ascending, which is identity order.
    pub eligible: Vec<usize>,
    /// The held quintile, best first, where "best" is whichever end of the
    /// ranking the configured strategy takes.
    pub chosen: Vec<usize>,
    /// Signal per eligible name, in `eligible` order. Kept so a test can assert
    /// on the ranking rather than only on what came out the other end. Under
    /// [`Strategy::LowVolatility`] the value is the trailing volatility rather
    /// than a return, and a smaller one is better.
    pub signals: Vec<(usize, Decimal)>,
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
pub fn rebalance_at(
    panel: &Panel,
    config: &BacktestConfig,
    index: usize,
) -> Result<Rebalance, EngineError> {
    let month_ends = panel.month_ends();
    let date = month_ends[index];
    let window_start = month_ends[index - config.signal_lookback_months];
    let window_end = month_ends[index - config.signal_skip_months];

    // Denominator of the coverage rule: days the market was open across the
    // lead-in window, not days this particular name traded.
    let open_days = panel.trading_days_in(window_start, window_end);
    let required_days = config
        .min_coverage
        .checked_mul(Decimal::from(open_days))
        .ok_or_else(|| EngineError::math("sizing the coverage requirement"))?
        .ceil();

    let mut eligible = Vec::new();
    let mut signals = Vec::new();
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
        let value = match config.strategy {
            Strategy::Momentum => signal(panel, position, window_start, window_end),
            Strategy::LowVolatility => {
                lowvol::volatility(panel, position, window_start, window_end)
            }
        };
        let Some(value) = value else {
            continue;
        };
        eligible.push(position);
        signals.push((position, value));
    }

    // Best signal first, where momentum wants the largest and low volatility
    // wants the smallest. Ties break on the security's position, which is
    // identity order, so a tie resolves the same way on every run rather than
    // however the sort happened to land. The tie-break is ascending in both
    // arms deliberately: it is an identity order, not a second signal, and
    // flipping it with the strategy would make the two strategies disagree
    // about which of two identical names to prefer.
    let mut ranked = signals.clone();
    match config.strategy {
        Strategy::Momentum => {
            ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
        }
        Strategy::LowVolatility => {
            ranked.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
        }
    }

    // Integer division floors, so 9 eligible names give a quintile of 1 rather
    // than 2. The floor of 1 keeps a thin month holding something rather than
    // silently sitting in cash, and thin months are counted and reported.
    let quintile = (ranked.len() / config.quintile_divisor).max(1);
    let chosen = ranked
        .iter()
        .take(quintile)
        .map(|(position, _)| *position)
        .collect();

    Ok(Rebalance {
        date,
        eligible,
        chosen,
        signals,
    })
}
