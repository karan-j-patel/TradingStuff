//! The comparisons rule 5 requires beside every result.
//!
//! A strategy that beats zero has shown nothing. The question is whether it
//! beats holding the same universe, and whether it beats picking names at
//! random with the same turnover and the same holding period. Both run on the
//! same calendar, the same eligibility rules, and the same cost model, because
//! a baseline that is charged less than the strategy is not a baseline.
//!
//! The linear-model baseline is absent on purpose. See
//! [`LINEAR_BASELINE_NOTE`].

use std::collections::BTreeMap;

use rust_decimal::Decimal;

use crate::config::{BacktestConfig, Weighting};
use crate::error::EngineError;
use crate::momentum::Rebalance;
use crate::panel::Panel;
use crate::portfolio::{self, Weights};

/// What is printed where the linear-model baseline would go.
///
/// Rule 5 asks for a linear model on the same features. There is one feature
/// and the strategy ranks on it directly, so a linear model of it is a
/// monotone transformation of the signal and produces the identical ranking,
/// the identical portfolio, and the identical returns. Printing a duplicate
/// column would suggest a comparison had been made.
pub const LINEAR_BASELINE_NOTE: &str =
    "The linear-model baseline degenerates to the signal itself with one feature.";

/// What is printed in the same place for the conservative formula.
///
/// The composite is already a linear model: an equal-weight sum of two ranks
/// with nothing fitted and no free parameter to estimate. So a linear baseline
/// on those same two features is the strategy, and printing it as a comparison
/// would be printing the strategy twice.
///
/// What that does not cover is a *fitted* linear model, which is a different
/// question and a real gap. Ridge regression over the same features arrives at
/// milestone 6, and until it does the note names the gap rather than implying
/// rule 5 has been satisfied in full.
pub const LINEAR_BASELINE_NOTE_CONSERVATIVE: &str = "The strategy is itself a linear model of its features, an equal-weight average of \
     two ranks with nothing fitted, so an unfitted linear baseline is the strategy. A \
     FITTED linear baseline over the same features is not run here and joins at the ridge \
     milestone; until then this comparison is missing rather than satisfied.";

/// The note belonging to a strategy.
///
/// A function rather than a caller choosing, on the rule [`crate::run::caveats`]
/// follows: the two notes make different claims about what has and has not been
/// compared, and printing the wrong one beside a real figure would say rule 5
/// was satisfied when it was not.
pub fn linear_baseline_note(strategy: crate::config::Strategy) -> &'static str {
    match strategy {
        crate::config::Strategy::Momentum
        | crate::config::Strategy::LowVolatility
        | crate::config::Strategy::Value => LINEAR_BASELINE_NOTE,
        crate::config::Strategy::ConservativeFormula => LINEAR_BASELINE_NOTE_CONSERVATIVE,
    }
}

/// A baseline's monthly net series and what it came to.
#[derive(Debug, Clone)]
pub struct BaselineSeries {
    pub net_monthly: Vec<Decimal>,
    pub total_net_return: Decimal,
    pub annualised_sharpe: Option<Decimal>,
}

impl BaselineSeries {
    pub fn from_monthly(net_monthly: Vec<Decimal>) -> Result<Self, EngineError> {
        let total_net_return = portfolio::cumulative(&net_monthly)?;
        let annualised_sharpe = match rigor::moments(&net_monthly) {
            Some(moments) => Some(
                moments
                    .sharpe
                    .checked_mul(portfolio::annualisation_factor()?)
                    .ok_or_else(|| EngineError::math("annualising a baseline Sharpe"))?,
            ),
            None => None,
        };
        Ok(BaselineSeries {
            net_monthly,
            total_net_return,
            annualised_sharpe,
        })
    }
}

/// Buy-and-hold of the eligible universe at the first rebalance.
///
/// Bought once, held to the end, costs at entry and exit only. A name that
/// stops trading is marked at its last close and its value sits in cash from
/// then on, which is what actually happens to a buy-and-hold investor holding a
/// name that delists: they are left with whatever the last print was and no
/// position.
///
/// # Why the weighting is an argument rather than read off the configuration
///
/// A baseline compared against a value-weighted strategy has to be
/// value-weighted, or the comparison confounds the weighting change with the
/// strategy change and the confounded number is the one that looks like a
/// result. So the matched call passes `config.weighting`.
///
/// The equal-weighted reading is still wanted beside it as a cross-variant
/// reference, because every other research program reports against equal-weight
/// buy-and-hold and `DECISION_CRITERIA.md` gate G2.4 names it. That second call
/// deliberately disagrees with the configuration, which is exactly why the
/// disagreement is written at the call site instead of hidden inside here.
///
/// # Why the marks come from the panel rather than from the formation dates
///
/// The baseline is bought once, so the formation schedule tells it nothing. Its
/// series has to be sampled at the same frequency as the strategy's or the two
/// annualised Sharpes beside each other would be computed from returns of
/// different lengths. Under a monthly stride the month-ends between the first
/// and last formation are exactly the formation dates, so this is unchanged for
/// every program that had one.
pub fn buy_and_hold(
    panel: &Panel,
    config: &BacktestConfig,
    rebalances: &[Rebalance],
    weighting: Weighting,
) -> Result<BaselineSeries, EngineError> {
    let marks = &panel.month_ends()[rebalances[0].index..=rebalances[rebalances.len() - 1].index];

    // Entered at the weight each name's market cap commands on the entry date,
    // which is the first formation, and the book drifts from there.
    let mut alive: Weights = portfolio::weights_at(
        panel,
        weighting,
        &rebalances[0].eligible,
        rebalances[0].date,
    )?;
    let mut cash = Decimal::ZERO;
    let mut net_monthly = Vec::with_capacity(marks.len().saturating_sub(1));

    let entry_cost = config
        .cost_per_side
        .checked_mul(portfolio::traded_notional(&Weights::new(), &alive)?)
        .ok_or_else(|| EngineError::math("costing the buy-and-hold entry"))?;

    for (position, pair) in marks.windows(2).enumerate() {
        let (from, to) = (pair[0], pair[1]);
        let mut gross = Decimal::ZERO;
        let mut returns: BTreeMap<usize, (Decimal, bool)> = BTreeMap::new();

        for (index, weight) in &alive {
            let series = panel
                .securities()
                .get(*index)
                .ok_or_else(|| EngineError::math("looking up a buy-and-hold holding"))?;
            let open = series.close_on(from).ok_or_else(|| {
                EngineError::math("pricing a buy-and-hold holding at month start")
            })?;
            // The same delisting treatment the strategy loop applies, through
            // the same two helpers. This loop is a second copy of the return
            // arithmetic and always has been, so a haircut wired into
            // `portfolio::advance` alone would leave the baseline exiting at a
            // price the strategy no longer uses, and it would beat the strategy
            // on treatment rather than on selection.
            let (close, exited) = match series.close_on(to) {
                Some(close) => (close, false),
                None => {
                    let (last_bar, last) = series.last_close_on_or_before(to).ok_or_else(|| {
                        EngineError::math("pricing a delisted buy-and-hold holding")
                    })?;
                    let mark = series.exit_mark_in(last_bar, to);
                    let marked = last.checked_mul(mark.terminal_factor()?).ok_or_else(|| {
                        EngineError::math("marking a delisted buy-and-hold holding at exit")
                    })?;
                    (marked, true)
                }
            };
            if open <= Decimal::ZERO {
                return Err(EngineError::math("dividing by a non-positive entry price"));
            }
            // The same cash the strategy loop collects, through the same
            // helper. A baseline that missed dividends the strategy received
            // would be losing to it on treatment rather than on selection.
            // Named for what it is, because `cash` a few lines down is the
            // portfolio's cash *weight* and shadowing one with the other would
            // let a future edit corrupt the weight silently.
            let dividend_cash = series.dividend_cash_in(from, to)?;
            let security_return = close
                .checked_add(dividend_cash)
                .and_then(|marked| marked.checked_div(open))
                .and_then(|ratio| ratio.checked_sub(Decimal::ONE))
                .ok_or_else(|| EngineError::math("computing a buy-and-hold security return"))?;
            gross = weight
                .checked_mul(security_return)
                .and_then(|contribution| gross.checked_add(contribution))
                .ok_or_else(|| EngineError::math("accumulating the buy-and-hold return"))?;
            returns.insert(*index, (security_return, exited));
        }

        let growth = Decimal::ONE
            .checked_add(gross)
            .ok_or_else(|| EngineError::math("growing the buy-and-hold portfolio"))?;
        if growth <= Decimal::ZERO {
            return Err(EngineError::TotalLoss { date: to });
        }

        let mut next: Weights = Weights::new();
        let mut next_cash = cash
            .checked_div(growth)
            .ok_or_else(|| EngineError::math("drifting the buy-and-hold cash weight"))?;
        let mut exited_notional = Decimal::ZERO;
        for (index, (security_return, exited)) in returns {
            let grown = Decimal::ONE
                .checked_add(security_return)
                .and_then(|factor| alive[&index].checked_mul(factor))
                .and_then(|value| value.checked_div(growth))
                .ok_or_else(|| EngineError::math("drifting a buy-and-hold weight"))?;
            if exited {
                exited_notional = exited_notional
                    .checked_add(grown)
                    .ok_or_else(|| EngineError::math("sizing a delisted holding's exit"))?;
                next_cash = next_cash
                    .checked_add(grown)
                    .ok_or_else(|| EngineError::math("retiring a delisted holding to cash"))?;
            } else {
                next.insert(index, grown);
            }
        }
        alive = next;
        cash = next_cash;

        let cost = if position == 0 {
            entry_cost
        } else {
            Decimal::ZERO
        };
        let month = portfolio::net_of_cost(gross, cost)?;

        // A delisted holding is sold at its last close, so it pays to leave
        // exactly as a live holding pays to be sold at a rebalance. Without
        // this, value moved to cash for free and the baseline was charged less
        // than the strategy it exists to be compared against.
        //
        // Charged at the end of the month rather than folded into `cost`,
        // because that is when the sale happens: the notional is the exited
        // name's share of the portfolio *after* the month's returns.
        let exit_charge = config
            .cost_per_side
            .checked_mul(exited_notional)
            .ok_or_else(|| EngineError::math("costing a delisted holding's exit"))?;
        net_monthly.push(portfolio::net_of_cost(month, exit_charge)?);
    }

    // The exit sells whatever is still held. Cash needs no selling, so a
    // portfolio that has entirely delisted pays nothing to close.
    let exit_cost = config
        .cost_per_side
        .checked_mul(portfolio::traded_notional(&alive, &Weights::new())?)
        .ok_or_else(|| EngineError::math("costing the buy-and-hold exit"))?;
    if let Some(last) = net_monthly.last_mut() {
        *last = portfolio::net_of_cost(*last, exit_cost)?;
    }

    BaselineSeries::from_monthly(net_monthly)
}

/// A deterministic 64-bit generator, SplitMix64.
///
/// Written out rather than pulled in as a dependency, because a backtest that
/// cannot be reproduced from its seed is not a backtest, and that guarantee is
/// eleven lines of arithmetic. The constants are the published ones.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        SplitMix64(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform value in `0..bound`, by rejection so the low bits are not
    /// skewed the way a plain modulo skews them.
    pub fn below(&mut self, bound: usize) -> usize {
        assert!(bound > 0, "a bound of zero has no values to draw from");
        let bound = bound as u64;
        let limit = u64::MAX - (u64::MAX % bound) - 1;
        loop {
            let draw = self.next_u64();
            if draw <= limit {
                return (draw % bound) as usize;
            }
        }
    }

    /// `count` distinct entries of `pool`, drawn without replacement.
    ///
    /// A partial Fisher-Yates: swap a random remaining entry to the front, one
    /// position at a time, and stop after `count` of them.
    pub fn sample(&mut self, pool: &[usize], count: usize) -> Vec<usize> {
        let mut remaining: Vec<usize> = pool.to_vec();
        let count = count.min(remaining.len());
        for position in 0..count {
            let pick = position + self.below(remaining.len() - position);
            remaining.swap(position, pick);
        }
        remaining.truncate(count);
        remaining
    }
}

/// What the random-ranking baseline found across its draws.
#[derive(Debug, Clone)]
pub struct RandomBaseline {
    /// Annualised net Sharpe per draw, in seed order. `None` for a draw whose
    /// series had no dispersion, which is reported rather than dropped.
    pub sharpes: Vec<Option<Decimal>>,
    pub mean_sharpe: Option<Decimal>,
    pub min_sharpe: Option<Decimal>,
    pub max_sharpe: Option<Decimal>,
    pub spread: Option<Decimal>,
}

impl RandomBaseline {
    pub fn summarise(sharpes: Vec<Option<Decimal>>) -> Result<Self, EngineError> {
        let present: Vec<Decimal> = sharpes.iter().flatten().copied().collect();
        if present.is_empty() {
            return Ok(RandomBaseline {
                sharpes,
                mean_sharpe: None,
                min_sharpe: None,
                max_sharpe: None,
                spread: None,
            });
        }
        let total = present
            .iter()
            .try_fold(Decimal::ZERO, |running, value| running.checked_add(*value))
            .ok_or_else(|| EngineError::math("summing the random baseline Sharpes"))?;
        let mean = total
            .checked_div(Decimal::from(present.len()))
            .ok_or_else(|| EngineError::math("averaging the random baseline Sharpes"))?;
        let min = present.iter().copied().min();
        let max = present.iter().copied().max();
        // Population standard deviation, matching `rigor::moments`, so the two
        // dispersion figures in the output mean the same thing.
        let spread = if present.len() < 2 {
            None
        } else {
            let variance = present
                .iter()
                .try_fold(Decimal::ZERO, |running, value| {
                    let deviation = value.checked_sub(mean)?;
                    running.checked_add(deviation.checked_mul(deviation)?)
                })
                .and_then(|total| total.checked_div(Decimal::from(present.len())))
                .ok_or_else(|| EngineError::math("measuring the random baseline spread"))?;
            rust_decimal::MathematicalOps::sqrt(&variance)
        };
        Ok(RandomBaseline {
            sharpes,
            mean_sharpe: Some(mean),
            min_sharpe: min,
            max_sharpe: max,
            spread,
        })
    }
}
