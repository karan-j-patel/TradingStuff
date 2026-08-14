//! Weights, turnover, costs, and what a month of holding does to them.
//!
//! # Why weights rather than share counts
//!
//! A long-only, fully-invested portfolio is completely described by its
//! weights, and weights avoid a rounding trap that share counts walk straight
//! into. Dividing a portfolio value by three names gives a target notional that
//! does not divide exactly, so a share-count implementation has to decide where
//! the leftover cent goes and every test then asserts on that decision rather
//! than on the accounting. Weights sum to one by construction and the single
//! division lands in one place.
//!
//! # How a cost becomes a return
//!
//! Traded notional is measured as a fraction of portfolio value, so selling
//! everything and buying a fresh set moves `2.0` of notional and a full
//! rotation is charged twice the per-side rate. The charge is multiplicative on
//! value, `V -> V * (1 - rate * traded)`, because a cost is a fraction of the
//! portfolio lost at the instant of trading rather than a number subtracted
//! from a return computed elsewhere.
//!
//! Rule 3 has no gross mode. A gross series is computed here and it exists only
//! to be printed on a line labelled diagnostic beside the net one.

use std::collections::{BTreeMap, BTreeSet};

use jiff::civil::Date;
use rust_decimal::{Decimal, MathematicalOps};

use crate::config::Weighting;
use crate::error::EngineError;
use crate::panel::{ExitMark, Panel, Series};

/// How many exits rested on a measurement, on an assumption, and on nothing.
///
/// Three counts rather than one, because a report that says only "four names
/// stopped trading" cannot tell a reader how much of the result is assumed.
/// `unexplained` is the one that matters most: those holdings still exit at
/// their last close with no imputation, which is the treatment this round
/// exists to stop being the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExitCensus {
    pub imputed: usize,
    pub observed: usize,
    pub unexplained: usize,
}

impl ExitCensus {
    pub fn total(&self) -> usize {
        self.imputed + self.observed + self.unexplained
    }

    /// This census with one more exit in it. Returns a new value rather than
    /// mutating, so no caller can end up holding a count that moved underneath
    /// it.
    fn counting(self, mark: ExitMark) -> Self {
        match mark {
            ExitMark::Imputed { .. } => ExitCensus {
                imputed: self.imputed + 1,
                ..self
            },
            ExitMark::Observed(_) => ExitCensus {
                observed: self.observed + 1,
                ..self
            },
            ExitMark::Unexplained => ExitCensus {
                unexplained: self.unexplained + 1,
                ..self
            },
        }
    }

    /// Two censuses summed, for a loop accumulating over months.
    pub fn plus(self, other: Self) -> Self {
        ExitCensus {
            imputed: self.imputed + other.imputed,
            observed: self.observed + other.observed,
            unexplained: self.unexplained + other.unexplained,
        }
    }
}

/// Portfolio weights by security index. Sums to one while invested, empty when
/// flat.
pub type Weights = BTreeMap<usize, Decimal>;

/// Equal weight across `indices`.
pub fn equal_weight(indices: &[usize]) -> Result<Weights, EngineError> {
    if indices.is_empty() {
        return Ok(Weights::new());
    }
    let each = Decimal::ONE
        .checked_div(Decimal::from(indices.len()))
        .ok_or_else(|| EngineError::math("splitting the portfolio equally"))?;
    Ok(indices.iter().map(|index| (*index, each)).collect())
}

/// Weight across `indices` in proportion to market capitalisation at
/// `formation`, normalised to sum to one.
///
/// The normalisation is what makes this a portfolio rather than a basket. The
/// caps arrive in the vendor's units, so an unnormalised vector would express
/// the holding in millions of dollars and every downstream figure, the return
/// and the traded notional alike, would be scaled by the size of the universe.
///
/// # Lookahead
///
/// `formation` is the only date read. [`crate::panel::Series::marketcap_at_month_end`]
/// takes the last figure dated on or before it and bounded to the same calendar
/// month, so nothing published after the formation can reach a weight. Rule 4.
///
/// A name with no positive cap at `formation` is refused rather than defaulted
/// or dropped. Selection already excludes those before the ranking, so an error
/// here means the two rules disagree, and inventing a weight would hide the
/// disagreement behind a number.
pub fn value_weight(
    panel: &Panel,
    indices: &[usize],
    formation: Date,
) -> Result<Weights, EngineError> {
    if indices.is_empty() {
        return Ok(Weights::new());
    }

    let mut caps: Vec<(usize, Decimal)> = Vec::with_capacity(indices.len());
    let mut total = Decimal::ZERO;
    for index in indices {
        let series = panel
            .securities()
            .get(*index)
            .ok_or_else(|| EngineError::math("looking up a held security's market cap"))?;
        let cap = series
            .marketcap_at_month_end(formation)
            .filter(|cap| *cap > Decimal::ZERO)
            .ok_or(EngineError::UnweightableHolding {
                security: *index,
                date: formation,
            })?;
        total = total
            .checked_add(cap)
            .ok_or_else(|| EngineError::math("summing the held market caps"))?;
        caps.push((*index, cap));
    }
    if total <= Decimal::ZERO {
        return Err(EngineError::math("normalising by a non-positive total cap"));
    }

    caps.into_iter()
        .map(|(index, cap)| {
            cap.checked_div(total)
                .map(|weight| (index, weight))
                .ok_or_else(|| EngineError::math("normalising a market cap weight"))
        })
        .collect()
}

/// The weights `indices` are held at, under whichever rule the run is
/// configured with.
///
/// One chokepoint rather than a match at each call site. The strategy loop and
/// the buy-and-hold baseline both form a portfolio, and a weighting applied to
/// one and not the other would leave the baseline beating the strategy on
/// construction rather than on selection.
pub fn weights_at(
    panel: &Panel,
    weighting: Weighting,
    indices: &[usize],
    formation: Date,
) -> Result<Weights, EngineError> {
    match weighting {
        Weighting::Equal => equal_weight(indices),
        Weighting::ValueByMarketcap => value_weight(panel, indices, formation),
    }
}

/// Notional traded to move from one set of weights to another, as a fraction of
/// portfolio value.
///
/// One for an initial purchase, one for a full liquidation, two for a complete
/// rotation, zero when nothing changes.
pub fn traded_notional(from: &Weights, to: &Weights) -> Result<Decimal, EngineError> {
    let names: BTreeSet<usize> = from.keys().chain(to.keys()).copied().collect();
    let mut total = Decimal::ZERO;
    for name in names {
        let before = from.get(&name).copied().unwrap_or(Decimal::ZERO);
        let after = to.get(&name).copied().unwrap_or(Decimal::ZERO);
        let change = after
            .checked_sub(before)
            .ok_or_else(|| EngineError::math("differencing two weights"))?;
        total = total
            .checked_add(change.abs())
            .ok_or_else(|| EngineError::math("accumulating traded notional"))?;
    }
    Ok(total)
}

/// What one month of holding did.
#[derive(Debug, Clone)]
pub struct Advance {
    /// Portfolio return over the month, before costs.
    pub gross_return: Decimal,
    /// Weights at the end of the month, after prices moved them.
    pub drifted: Weights,
    /// Held names that had no bar on the closing date and were marked at their
    /// last available close instead. These are delistings, or a data gap that
    /// is indistinguishable from one, and both are counted as caveats.
    pub exits: Vec<usize>,
    /// How those exits were classified by the delistings dataset.
    pub exit_census: ExitCensus,
}

/// Hold `weights` from `from` to `to` and report what happened.
///
/// A name with no bar on `to` is marked at its last close on or before `to`.
/// Marking it at anything else, in particular at the price it was bought at,
/// fabricates the very number this run is honest about not having.
///
/// # What the delistings dataset changes, and what it does not
///
/// The trigger is unchanged: an absent bar, and nothing else, decides that a
/// holding exited. The dataset only *classifies* what the absent bar already
/// found. A performance-related exit then has the published convention applied
/// to the terminal mark, and a merger or an unexplained exit keeps the last
/// close exactly.
///
/// The composition with cash is fixed and is the reason this arithmetic is
/// written as one expression:
///
/// ```text
/// security_return = (close * (1 + delisting_return) + dividend_cash) / open - 1
/// ```
///
/// The haircut applies to the terminal mark alone. Cash that reached the holder
/// during the month was received and is not clawed back by the company's later
/// fate, so it is added after the multiplication rather than inside it.
pub fn advance(
    panel: &Panel,
    weights: &Weights,
    from: Date,
    to: Date,
) -> Result<Advance, EngineError> {
    let mut gross_return = Decimal::ZERO;
    let mut returns: Vec<(usize, Decimal)> = Vec::with_capacity(weights.len());
    let mut exits = Vec::new();
    let mut exit_census = ExitCensus::default();

    for (index, weight) in weights {
        let series = panel
            .securities()
            .get(*index)
            .ok_or_else(|| EngineError::math("looking up a held security"))?;

        // No bar on the month this mark opens means the holding already stopped
        // trading earlier in this holding period and was marked out then. It is
        // carried flat at that mark until the next formation sells it, which is
        // what happens to a holder of a delisted name: there is nothing to sell
        // it into, so the position sits at the last print. Its return is zero
        // and its exit is not counted again, having been counted the month the
        // bar first went missing.
        //
        // Only reachable when formations are further apart than the marks. On a
        // monthly stride every holding was chosen at `from` and therefore has a
        // bar there, which is what keeps those runs byte-identical to the code
        // from before the stride existed.
        let Some(open) = series.close_on(from) else {
            if series.last_close_on_or_before(from).is_none() {
                return Err(EngineError::math(
                    "pricing a holding at the rebalance it was bought",
                ));
            }
            returns.push((*index, Decimal::ZERO));
            continue;
        };

        let close = match series.close_on(to) {
            Some(close) => close,
            None => {
                exits.push(*index);
                let (last_bar, last) = series
                    .last_close_on_or_before(to)
                    .ok_or_else(|| EngineError::math("pricing a delisted holding at exit"))?;
                let mark = series.exit_mark_in(last_bar, to);
                exit_census = exit_census.counting(mark);
                last.checked_mul(mark.terminal_factor()?)
                    .ok_or_else(|| EngineError::math("marking a delisted holding at exit"))?
            }
        };

        if open <= Decimal::ZERO {
            return Err(EngineError::math("dividing by a non-positive entry price"));
        }

        // Cash collected over the month, added to the closing mark. A holding
        // that stopped trading still collects: the fallback close above is the
        // last print, and cash that went ex before it reached the holder just
        // as it would have on a live name. Added after the delisting factor
        // rather than inside it, for the reason stated on this function.
        let cash = series.dividend_cash_in(from, to)?;
        let security_return = close
            .checked_add(cash)
            .and_then(|marked| marked.checked_div(open))
            .and_then(|ratio| ratio.checked_sub(Decimal::ONE))
            .ok_or_else(|| EngineError::math("computing a security return"))?;

        gross_return = weight
            .checked_mul(security_return)
            .and_then(|contribution| gross_return.checked_add(contribution))
            .ok_or_else(|| EngineError::math("accumulating the portfolio return"))?;
        returns.push((*index, security_return));
    }

    let growth = Decimal::ONE
        .checked_add(gross_return)
        .ok_or_else(|| EngineError::math("growing the portfolio over a month"))?;
    if growth <= Decimal::ZERO {
        return Err(EngineError::TotalLoss { date: to });
    }

    // Weights drift with prices. A name that outperformed is a larger share of
    // the portfolio at the end of the month than at the start, and the next
    // rebalance only trades the difference.
    let mut drifted = Weights::new();
    for (index, security_return) in returns {
        let weight = weights[&index];
        let grown = Decimal::ONE
            .checked_add(security_return)
            .and_then(|factor| weight.checked_mul(factor))
            .and_then(|value| value.checked_div(growth))
            .ok_or_else(|| EngineError::math("drifting a weight"))?;
        drifted.insert(index, grown);
    }

    Ok(Advance {
        gross_return,
        drifted,
        exits,
        exit_census,
    })
}

/// Mean one-way turnover per formation, single-counted.
///
/// `traded` holds the two-way notional moved at each formation, so a complete
/// rotation is 2.0 there and reads as 1.0 here. That is the convention a source
/// paper's own turnover figure is quoted in, which is the only reason to halve
/// it at all.
///
/// One body rather than one per caller. The backtest report and the turnover
/// diagnostic both quote this number and a divisor that drifted between two
/// copies would make the diagnostic disagree with the run it is describing,
/// which is the one thing it exists not to do.
pub fn mean_one_way_turnover(traded: &[Decimal]) -> Result<Decimal, EngineError> {
    if traded.is_empty() {
        return Err(EngineError::math("averaging turnover over no formations"));
    }
    traded
        .iter()
        .try_fold(Decimal::ZERO, |running, value| running.checked_add(*value))
        .and_then(|total| total.checked_div(Decimal::from(traded.len())))
        .and_then(|two_way| two_way.checked_div(Decimal::from(2u64)))
        .ok_or_else(|| EngineError::math("averaging turnover"))
}

/// Total return over `(from, to]`, dividends included, marked at the delisting
/// convention when the name stops trading inside the window.
///
/// The window is half-open at the start: `from`'s close is the denominator and
/// `from`'s own return is NOT in it. That is the property a forward label needs
/// and the one an event study gets wrong by folding the event month in.
///
/// # Why this lives here and what has not been consolidated
///
/// Promoted from `crate::deletions`, which now calls it, because the panel
/// export's forward label needs the identical arithmetic and a second copy of a
/// return is how two figures that must agree stop agreeing.
///
/// Two other copies remain and are deliberately NOT touched this round:
///
/// - [`advance`]'s inline form, which additionally drifts weights and
///   accumulates a portfolio return, so lifting it would mean either splitting
///   that loop or giving this function outputs no other caller wants.
/// - `crate::conservative::monthly_total_return_volatility`'s loop, which walks
///   a window of month-ends pairwise and needs each step's return rather than
///   one span's.
///
/// Both are named here so the next person consolidating them starts from a list
/// rather than a grep, and so the deferral is a decision on the record rather
/// than an oversight.
///
/// `None` when either end has no price, when the opening price is not positive,
/// or when a delisted name has no last close to mark. The `bool` is whether the
/// name stopped trading inside the window.
pub fn total_return(
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

/// Apply a cost to a return, both expressed as fractions.
///
/// `(1 - cost) * (1 + gross) - 1`, which is what a portfolio that pays the cost
/// at the start of the month and then earns the return ends up with.
pub fn net_of_cost(gross_return: Decimal, cost: Decimal) -> Result<Decimal, EngineError> {
    Decimal::ONE
        .checked_sub(cost)
        .and_then(|after_cost| {
            Decimal::ONE
                .checked_add(gross_return)
                .and_then(|growth| after_cost.checked_mul(growth))
        })
        .and_then(|value| value.checked_sub(Decimal::ONE))
        .ok_or_else(|| EngineError::math("applying a cost to a monthly return"))
}

/// The largest peak-to-trough fall in cumulative value, as a positive fraction.
pub fn max_drawdown(returns: &[Decimal]) -> Result<Decimal, EngineError> {
    let mut value = Decimal::ONE;
    let mut peak = Decimal::ONE;
    let mut worst = Decimal::ZERO;
    for monthly in returns {
        value = Decimal::ONE
            .checked_add(*monthly)
            .and_then(|growth| value.checked_mul(growth))
            .ok_or_else(|| EngineError::math("compounding the value series"))?;
        if value > peak {
            peak = value;
        }
        if peak > Decimal::ZERO {
            let fall = peak
                .checked_sub(value)
                .and_then(|drop| drop.checked_div(peak))
                .ok_or_else(|| EngineError::math("measuring a drawdown"))?;
            if fall > worst {
                worst = fall;
            }
        }
    }
    Ok(worst)
}

/// Total compounded return of a monthly series.
pub fn cumulative(returns: &[Decimal]) -> Result<Decimal, EngineError> {
    let mut value = Decimal::ONE;
    for monthly in returns {
        value = Decimal::ONE
            .checked_add(*monthly)
            .and_then(|growth| value.checked_mul(growth))
            .ok_or_else(|| EngineError::math("compounding the value series"))?;
    }
    value
        .checked_sub(Decimal::ONE)
        .ok_or_else(|| EngineError::math("converting value to a total return"))
}

/// Twelve, rooted, which turns a monthly Sharpe into an annual one.
///
/// Computed rather than pinned as a literal so the precision is `Decimal`'s
/// rather than however many digits someone typed.
pub fn annualisation_factor() -> Result<Decimal, EngineError> {
    Decimal::from(12u64)
        .sqrt()
        .ok_or_else(|| EngineError::math("taking the square root of twelve"))
}
