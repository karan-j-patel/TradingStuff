//! The conservative formula: van Vliet and Blitz (2018), SSRN 3145152.
//!
//! # What the source says, and what this does with it
//!
//! The paper sorts the largest 1,000 US stocks each quarter into two halves on
//! thirty-six-month return volatility, ranks the calm half on 12-1 price
//! momentum and on net payout yield, averages the two ranks, and holds the best
//! hundred equally weighted. Every parameter here comes from that paragraph or
//! from a ruling fixed before any result existed, and none of them is swept:
//! rule 2 counts every configuration looked at, so a sweep of the volatility
//! window would be one trial per point.
//!
//! Three terms, defined because this file is read by engineers rather than by
//! portfolio managers.
//!
//! *Net payout yield* is what a shareholder receives net of what the company
//! takes back by issuing new shares. Cash dividends are the payout; shares
//! issued dilute the existing holders and so count against it, and shares
//! bought back count for it. Boudoukh and co-authors (2007) is the source the
//! paper cites for the measure.
//!
//! *12-1 momentum* is the price return over the twelve months ending one month
//! before the formation, with that most recent month dropped because short-term
//! reversal contaminates it. It is the existing [`crate::momentum::signal`],
//! reused unchanged.
//!
//! *A rank* here is ordinal and starts at zero. Two names with the same value
//! take consecutive ranks broken by identity order, which is the engine's
//! existing convention and the only one that makes a run reproducible.
//!
//! # Why the composite does not fit the sort-then-slice shape
//!
//! Momentum and low volatility each score a name with one number, so the
//! selection is a sort and a slice. This one averages two *ranks*, and a rank
//! only exists relative to the cross-section it was taken in. There is no
//! per-name scalar to hand the shared path, so the composite owns its
//! selection instead of the shared path being bent into a shape that fits
//! neither.
//!
//! # Where the share count went
//!
//! The share-count leg divides a market capitalisation by a price. The market
//! cap arrives in the vendor's units, which are millions of dollars for the
//! first provider, and the price is the panel's split-adjusted close. Neither
//! the vendor's scale factor nor the split adjustment survives the ratio of two
//! such quantities, and the ratio is the only thing anything here reads. So no
//! absolute share count is ever formed, and a units mistake is not expressible.
//!
//! Dividing by the *adjusted* close rather than the raw one is the whole point.
//! A two-for-one split doubles the raw share count overnight while the company
//! has issued nothing, and a leg reading the raw count would score that as a
//! hundred percent dilution. The adjusted basis is fixed at fetch time, so
//! every month-end's quantity is restated onto one share basis and only real
//! issuance and buybacks move it.

use std::collections::BTreeMap;

use jiff::civil::Date;
use rust_decimal::{Decimal, MathematicalOps};

use crate::config::BacktestConfig;
use crate::error::EngineError;
use crate::liquidity;
use crate::momentum::{self, FormationCensus, Rebalance};
use crate::panel::{Panel, Series};

/// A window length the conservative formula cannot run without.
///
/// Refused rather than defaulted. A missing window is a configuration mistake,
/// and a default would produce a number under a hash that says nothing about
/// which window produced it.
fn required(value: Option<usize>, field: &'static str) -> Result<usize, EngineError> {
    value.ok_or(EngineError::ConservativeWindowMissing { field })
}

/// The sample standard deviation of monthly total returns across `window`.
///
/// `window` is the month-ends the estimate spans, ascending and inclusive at
/// both ends, so `n` returns need `n + 1` of them. `None` when any month-end
/// has no close, when a denominator is not positive, or when fewer than two
/// returns exist, because the `n - 1` denominator has nothing to divide by at
/// one return. A missing estimate excludes the name and is never defaulted to
/// zero, which for this strategy is the most attractive value there is.
///
/// # Total returns, not price returns
///
/// The return is `(close + cash gone ex in the month) / previous close - 1`.
/// The paper's own return data takes dividends into account, and the half-open
/// dividend window is the same one the portfolio accounting uses, so the
/// volatility is measured on the series a holder would actually have
/// experienced. Note that this is the opposite choice from
/// [`crate::lowvol::volatility`], which is deliberately price-only to stay
/// comparable to the published low-volatility index measures. The two verbs
/// answer to two different sources and are kept apart for that reason.
pub fn monthly_total_return_volatility(
    series: &Series,
    window: &[Date],
) -> Result<Option<Decimal>, EngineError> {
    if window.len() < 3 {
        return Ok(None);
    }

    let mut returns = Vec::with_capacity(window.len() - 1);
    for pair in window.windows(2) {
        let (previous, current) = (pair[0], pair[1]);
        let (Some(open), Some(close)) = (
            series.close_at_month_end(previous),
            series.close_at_month_end(current),
        ) else {
            return Ok(None);
        };
        if open <= Decimal::ZERO {
            return Ok(None);
        }
        let cash = series.dividend_cash_in(previous, current)?;
        let Some(monthly) = close
            .checked_add(cash)
            .and_then(|marked| marked.checked_div(open))
            .and_then(|ratio| ratio.checked_sub(Decimal::ONE))
        else {
            return Ok(None);
        };
        returns.push(monthly);
    }

    let count = Decimal::from(returns.len());
    let Some(mean) = returns
        .iter()
        .try_fold(Decimal::ZERO, |running, value| running.checked_add(*value))
        .and_then(|total| total.checked_div(count))
    else {
        return Ok(None);
    };
    let Some(sum_of_squares) = returns.iter().try_fold(Decimal::ZERO, |running, value| {
        let deviation = value.checked_sub(mean)?;
        running.checked_add(deviation.checked_mul(deviation)?)
    }) else {
        return Ok(None);
    };
    Ok(count
        .checked_sub(Decimal::ONE)
        .and_then(|denominator| sum_of_squares.checked_div(denominator))
        .and_then(|variance| variance.sqrt()))
}

/// Trailing cash dividends per share over `(window_start, formation]`, divided
/// by the close at the formation.
///
/// Both halves are on the panel's adjusted basis, which is what makes the
/// division meaningful: the vendor restates dividend amounts onto the same
/// basis as its adjusted close, so numerator and denominator agree about what a
/// share is.
///
/// `None` only when there is no close to divide by. A name with no dividend
/// rows genuinely paid nothing, and that is a yield of zero rather than missing
/// data, which is why the numerator is never the thing that makes this absent.
pub fn dividend_yield(
    series: &Series,
    window_start: Date,
    formation: Date,
) -> Result<Option<Decimal>, EngineError> {
    let Some(close) = series.close_at_month_end(formation) else {
        return Ok(None);
    };
    if close <= Decimal::ZERO {
        return Ok(None);
    }
    let cash = series.dividend_cash_in(window_start, formation)?;
    Ok(cash.checked_div(close))
}

/// Market cap over adjusted close at one month-end: a quantity proportional to
/// shares outstanding, on the panel's single share basis.
///
/// Never read on its own, only as the numerator or denominator of a ratio of
/// two of these, which is what cancels the vendor's units and the split
/// adjustment together. `None` when either figure is missing or not positive.
///
/// Zero is the case worth stating. The provider quantises market cap to one
/// decimal place of a million dollars, so a company below that quantum arrives
/// as exactly zero on a row it deliberately shipped. That is a real value and
/// the panel keeps it, but it carries no information about the share count, so
/// a name with one anywhere in its window has no share ratio at all rather than
/// a ratio computed off a zero. No tolerance constant is invented to soften it.
fn scaled_shares(series: &Series, month_end: Date) -> Option<Decimal> {
    let marketcap = series.marketcap_at_month_end(month_end)?;
    if marketcap <= Decimal::ZERO {
        return None;
    }
    let close = series.close_at_month_end(month_end)?;
    if close <= Decimal::ZERO {
        return None;
    }
    marketcap.checked_div(close)
}

/// The change in shares outstanding as a fraction: the latest month-end's level
/// over the mean of the `average_months` month-ends before it, minus one.
///
/// Positive is issuance and negative is a buyback. The current month is
/// excluded from its own average, which is how "the latest level divided by the
/// 24-month average" reads once it has to be written down.
///
/// `None` unless every one of the `average_months + 1` observations exists,
/// mirroring the paper's own full-history eligibility rule for volatility. A
/// partial average would compare a level against a different number of months
/// for different names, which is not one measure.
pub fn share_change(
    series: &Series,
    month_ends: &[Date],
    index: usize,
    average_months: usize,
) -> Option<Decimal> {
    let latest = scaled_shares(series, *month_ends.get(index)?)?;

    let mut total = Decimal::ZERO;
    for step in 1..=average_months {
        let earlier = scaled_shares(series, *month_ends.get(index.checked_sub(step)?)?)?;
        total = total.checked_add(earlier)?;
    }
    let mean = total.checked_div(Decimal::from(average_months))?;
    if mean <= Decimal::ZERO {
        return None;
    }
    latest.checked_div(mean)?.checked_sub(Decimal::ONE)
}

/// Net payout yield: the trailing dividend yield less the change in shares
/// outstanding.
///
/// The sign convention is the load-bearing part. The source names the measure
/// "net payout" without writing the arithmetic down, and the words settle it:
/// issuing shares is the opposite of paying shareholders, so issuance lowers
/// the yield and a buyback raises it. Walkshausl (2020), adapting the same
/// formula, states it outright.
/// # Why this is crate-private
///
/// Found by the same sweep as the two rebalancers and shrunk on the same rule.
/// `month_ends[index]` on the first line of the body is a raw index with no
/// bound of its own, so an out-of-range `index` from outside the crate panics
/// rather than returning `None`. Every in-crate caller reaches it from a
/// formation loop that derived the index from the panel it is walking.
///
/// Note what is NOT shrunk beside it: [`share_change`] takes an index too and
/// reads it through `month_ends.get(index)?` and `checked_sub`, so it is total
/// for any input and stays public.
///
/// ```compile_fail
/// fn cannot_read_the_payout_from_outside(
///     series: &engine::panel::Series,
///     month_ends: &[jiff::civil::Date],
/// ) {
///     let _ = engine::conservative::net_payout_yield(series, month_ends, 24, 24, 12);
/// }
/// ```
pub(crate) fn net_payout_yield(
    series: &Series,
    month_ends: &[Date],
    index: usize,
    average_months: usize,
    dividend_months: usize,
) -> Result<Option<Decimal>, EngineError> {
    let formation = month_ends[index];
    let Some(window_start) = index
        .checked_sub(dividend_months)
        .and_then(|start| month_ends.get(start))
    else {
        return Ok(None);
    };
    let Some(yield_) = dividend_yield(series, *window_start, formation)? else {
        return Ok(None);
    };
    let Some(change) = share_change(series, month_ends, index, average_months) else {
        return Ok(None);
    };
    Ok(yield_.checked_sub(change))
}

/// Ordinal ranks over `values`, best first, where best is the largest value.
///
/// Ties break on the security's position, which is identity order, so a tie
/// resolves the same way on every run rather than however the sort happened to
/// land. Ranks start at zero and are consecutive, so two tied names take two
/// adjacent ranks rather than one shared rank; averaging shared ranks would
/// make the composite depend on how many names happened to tie.
fn descending_ranks(values: &[(usize, Decimal)]) -> BTreeMap<usize, usize> {
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    sorted
        .into_iter()
        .enumerate()
        .map(|(rank, (position, _))| (position, rank))
        .collect()
}

/// Decide the portfolio at one formation date.
///
/// The order of operations is fixed and each step is numbered against the round
/// specification, because the order is itself a decision: a size screen applied
/// after the volatility split would take half of a different set, and a
/// volatility split applied before the size screen would rank names the screen
/// was going to remove.
/// # Why this is crate-private
///
/// The same reason [`crate::momentum::rebalance_at`] is, and this one indexes
/// `month_ends` four ways rather than three: the volatility window, the two
/// momentum endpoints, and the formation date itself. None is checked here,
/// because both doors that reach a formation check the relation first, and a
/// caller outside this crate would have passed neither.
///
/// ```compile_fail
/// fn cannot_form_a_conservative_rebalance_from_outside(
///     panel: &engine::Panel,
///     config: &engine::BacktestConfig,
/// ) {
///     let _ = engine::conservative::rebalance_at(panel, config, 36);
/// }
/// ```
pub(crate) fn rebalance_at(
    panel: &Panel,
    config: &BacktestConfig,
    index: usize,
) -> Result<Rebalance, EngineError> {
    let month_ends = panel.month_ends();
    let date = month_ends[index];

    let volatility_months = required(
        config.volatility_lookback_months,
        "volatility_lookback_months",
    )?;
    let average_months = required(
        config.payout_share_average_months,
        "payout_share_average_months",
    )?;
    let dividend_months = required(
        config.payout_dividend_trailing_months,
        "payout_dividend_trailing_months",
    )?;
    let window = &month_ends[index - volatility_months..=index];

    // 1 and 2. Traded on the formation date, above the price floor, and covered
    // across the volatility window. The coverage window is half-open at its
    // start for the reason `Series::bars_in` documents.
    let candidates = momentum::tradable(panel, config, date, window[0], date)?;

    // 2 and 3. Every month-end in the window has a close, which is the paper's
    // own eligibility rule and also what the volatility needs to exist, and a
    // market cap is present and positive at the formation so the name can be
    // size-ranked at all.
    //
    // The volatility is computed here rather than at the split below. A name
    // whose window carries a non-positive close has no volatility and therefore
    // no business in the size ranking either, and on data where every close is
    // positive the two orders admit exactly the same set.
    let mut eligible = Vec::new();
    let mut by_marketcap: Vec<(Decimal, usize)> = Vec::new();
    let mut volatilities: BTreeMap<usize, Decimal> = BTreeMap::new();
    for position in candidates {
        let series = &panel.securities()[position];
        if window
            .iter()
            .any(|month_end| series.close_at_month_end(*month_end).is_none())
        {
            continue;
        }
        let Some(marketcap) = series.marketcap_at_month_end(date) else {
            continue;
        };
        if marketcap <= Decimal::ZERO {
            continue;
        }
        let Some(volatility) = monthly_total_return_volatility(series, window)? else {
            continue;
        };
        eligible.push(position);
        by_marketcap.push((marketcap, position));
        volatilities.insert(position, volatility);
    }

    // 4. The size screen, standing in for the source's "the 1,000 largest
    // stocks". Refused rather than clamped outside `[0, 1)`, same as the
    // liquidity screen and for the same reason.
    let screened = match config.size_floor_fraction {
        None => eligible.clone(),
        Some(fraction) => {
            if fraction < Decimal::ZERO || fraction >= Decimal::ONE {
                return Err(EngineError::SizeFloorFractionOutOfRange { fraction });
            }
            liquidity::drop_smallest(fraction, by_marketcap, "sizing the market cap exclusion")?
        }
    };

    // 5. Two halves on volatility, keeping the calm one. The inclusive side
    // goes to the conservative half on an odd count, which is what
    // `div_ceil` says: at 1,000 names the split is 500 exactly, and at 999 the
    // calm half takes 500.
    let mut by_volatility: Vec<(Decimal, usize)> = screened
        .iter()
        .map(|position| (volatilities[position], *position))
        .collect();
    by_volatility.sort();
    let half = by_volatility.len().div_ceil(2);
    let calm: Vec<usize> = by_volatility
        .iter()
        .take(half)
        .map(|(_, position)| *position)
        .collect();

    // 6. Both legs, inside the calm half only. A name missing either one is
    // dropped rather than given a value, because zero is an ordinary yield and
    // an ordinary momentum and substituting it would rank a name with no data
    // in the middle of the field.
    let momentum_window_start = month_ends[index - config.signal_lookback_months];
    let momentum_window_end = month_ends[index - config.signal_skip_months];
    let mut momentums: Vec<(usize, Decimal)> = Vec::with_capacity(calm.len());
    let mut payouts: Vec<(usize, Decimal)> = Vec::with_capacity(calm.len());
    for position in &calm {
        let series = &panel.securities()[*position];
        let (Some(trend), Some(payout)) = (
            momentum::signal(panel, *position, momentum_window_start, momentum_window_end),
            net_payout_yield(series, month_ends, index, average_months, dividend_months)?,
        ) else {
            continue;
        };
        momentums.push((*position, trend));
        payouts.push((*position, payout));
    }

    // 7. The two ranks averaged, lowest average held. Halved rather than left
    // as a sum so that the recorded value is the average the source describes;
    // the division is exact and cannot reorder anything.
    let momentum_ranks = descending_ranks(&momentums);
    let payout_ranks = descending_ranks(&payouts);
    let mut signals: Vec<(usize, Decimal)> = Vec::with_capacity(momentums.len());
    for (position, _) in &momentums {
        let combined = Decimal::from(momentum_ranks[position] + payout_ranks[position])
            .checked_div(Decimal::TWO)
            .ok_or_else(|| EngineError::math("averaging the two conservative ranks"))?;
        signals.push((*position, combined));
    }

    let mut ranked = signals.clone();
    ranked.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
    let quintile = (ranked.len() / config.quintile_divisor).max(1);
    let chosen: Vec<usize> = ranked
        .iter()
        .take(quintile)
        .map(|(position, _)| *position)
        .collect();

    let census = FormationCensus {
        eligible: eligible.len(),
        size_screened: eligible.len() - screened.len(),
        vol_half: calm.len(),
        npy_ineligible: calm.len() - momentums.len(),
        held: chosen.len(),
    };
    Ok(Rebalance {
        date,
        index,
        eligible: screened,
        chosen,
        signals,
        census,
    })
}
