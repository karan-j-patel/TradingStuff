//! The monthly loop, and the report it produces.
//!
//! # Shape of the loop
//!
//! At each rebalance date the target weights are formed, the difference from
//! what is already held is charged, the portfolio is held to the next rebalance
//! date, and the drifted weights carry forward. The last rebalance date sells
//! rather than buys, and its cost lands on the final month's return, so the
//! series is charged for getting in and for getting out.
//!
//! The strategy and the random-ranking baseline share this loop and differ only
//! in which names they pick. That is deliberate: a baseline that ran through a
//! second implementation of the accounting would be comparing two code paths
//! rather than two hypotheses.

use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::baseline::{self, BaselineSeries, RandomBaseline, SplitMix64};
use crate::config::BacktestConfig;
use crate::error::EngineError;
use crate::momentum::{self, Rebalance};
use crate::panel::Panel;
use crate::portfolio::{self, ExitCensus, Weights};

/// The caveat block for a run with no actions file behind it.
///
/// The directions are not interchangeable and were corrected once already.
/// Excluded cash dividends leave a long-only total return too low. A delisting
/// marked at the last close rather than at a realised delisting return leaves
/// it too high. They push opposite ways, they do not cancel by construction,
/// and which one wins is not known from here.
pub const CAVEATS: &str = "\
Returns are price returns from split and stock-dividend adjusted closes.
  Cash dividends are absent from the data, which UNDERSTATES a long-only
  total return, biasing it DOWN.
  Delisted names exit at their last available close with no delisting-return
  imputation, which OVERSTATES it, biasing it UP.
  The two partially offset and the net direction is UNKNOWN. This is not a
  conservative estimate and must not be described as one.";

/// The caveat block for a run that applied cash dividends.
///
/// One bias is gone and the other is not, so the closing line stops saying the
/// direction is unknown and says which way it goes. That is a stronger claim
/// than the block above makes and it is only true when the dividends are
/// actually in the arithmetic, which is why the literals are selected by
/// [`caveats`] rather than edited into one.
pub const CAVEATS_WITH_DIVIDENDS: &str = "\
Returns include cash dividends on their ex-dates, on top of split and
  stock-dividend adjusted closes. Cash paid mid-month is held uninvested until
  the next rebalance rather than reinvested on the day it arrives.
  Delisted names exit at their last available close with no delisting-return
  imputation, which OVERSTATES a long-only total return, biasing it UP.
  The remaining known bias is UP. This is not a conservative
  estimate and must not be described as one.";

/// The caveat block for a run that imputed delisting returns but applied no
/// cash dividends.
///
/// The delisting bias is no longer stated as a direction, because it is no
/// longer omitted. What replaces it is not a claim of accuracy, and the text
/// says so. A convention is an assumption, and the dividend bias that survives
/// this run still runs DOWN.
///
/// Written out in full rather than assembled from a shared fragment, on the
/// rule the two blocks above already follow. Each block is one statement made
/// as a whole, and a reader checking whether a published figure carried the
/// right caveat should be able to read the literal that was published rather
/// than reconstruct it from parts.
pub const CAVEATS_WITH_DELISTINGS: &str = "\
Returns are price returns from split and stock-dividend adjusted closes.
  Cash dividends are absent from the data, which UNDERSTATES a long-only
  total return, biasing it DOWN.
  Performance-related exits are imputed at -30 percent applied to the last
  close, following Shumway (1997) as Jensen, Kelly and Pedersen (2023) apply it
  where a dataset publishes no delisting returns at all. Merger and acquisition
  exits take the last close unchanged.
  Exits the delistings dataset cannot explain still take the last close with no
  imputation. Their count is printed above and is not zero by assumption.
  The remaining known bias is DOWN. The delisting figure is an assumption
  rather than a measurement, so these results are not assumption-free and
  must not be described as conservative.";

/// The caveat block for a run with both corrections applied.
///
/// This is the state the delisting round exists to reach, and the closing line
/// is deliberately not a boast. No bias with a known direction is left, which
/// is a different statement from the result being right. A published convention
/// stands where a measurement would go, so the block refuses both
/// "conservative" and "assumption-free".
pub const CAVEATS_WITH_DIVIDENDS_AND_DELISTINGS: &str = "\
Returns include cash dividends on their ex-dates, on top of split and
  stock-dividend adjusted closes. Cash paid mid-month is held uninvested until
  the next rebalance rather than reinvested on the day it arrives.
  Performance-related exits are imputed at -30 percent applied to the last
  close, following Shumway (1997) as Jensen, Kelly and Pedersen (2023) apply it
  where a dataset publishes no delisting returns at all. Merger and acquisition
  exits take the last close unchanged.
  Exits the delistings dataset cannot explain still take the last close with no
  imputation. Their count is printed above and is not zero by assumption.
  No bias with a known direction remains. The delisting figure is an assumption
  rather than a measurement, so these results are not assumption-free and
  must not be described as conservative.";

/// Which caveat block belongs to a run.
///
/// A function rather than a caller deciding, because the blocks make different
/// claims about what bias is left and picking the wrong one publishes a
/// statement that is not true of the figure beside it.
pub fn caveats(dividends_applied: bool, delistings_imputed: bool) -> &'static str {
    match (dividends_applied, delistings_imputed) {
        (false, false) => CAVEATS,
        (true, false) => CAVEATS_WITH_DIVIDENDS,
        (false, true) => CAVEATS_WITH_DELISTINGS,
        (true, true) => CAVEATS_WITH_DIVIDENDS_AND_DELISTINGS,
    }
}

/// One strategy's monthly series and the accounting behind it.
#[derive(Debug, Clone)]
pub struct Series {
    pub net_monthly: Vec<Decimal>,
    /// Diagnostic only. Rule 3 forbids presenting this as performance.
    pub gross_monthly: Vec<Decimal>,
    /// Notional traded at each rebalance, as a fraction of portfolio value.
    pub traded: Vec<Decimal>,
    /// Held names marked at a last close because they stopped trading. Equal to
    /// `exits.total()`, and computed from it rather than counted twice.
    pub delisting_exits: usize,
    /// How those exits were classified, which is what says how much of the
    /// result rests on a convention rather than on data.
    pub exits: ExitCensus,
}

/// Run the monthly loop with a caller-supplied choice of names.
///
/// `pick` receives the rebalance's index and its decisions and returns the
/// names to hold. `FnMut` rather than `Fn` because the random baseline carries
/// a generator state across rebalances, and a stream that restarted each month
/// would not be the draw it claims to be.
pub fn run_schedule(
    panel: &Panel,
    config: &BacktestConfig,
    rebalances: &[Rebalance],
    mut pick: impl FnMut(usize, &Rebalance) -> Vec<usize>,
) -> Result<Series, EngineError> {
    let mut held: Weights = Weights::new();
    let mut net_monthly = Vec::with_capacity(rebalances.len());
    let mut gross_monthly = Vec::with_capacity(rebalances.len());
    let mut traded = Vec::with_capacity(rebalances.len());
    let mut exits = ExitCensus::default();

    for (index, rebalance) in rebalances.iter().enumerate() {
        let last = index + 1 == rebalances.len();
        let target = if last {
            // Nothing is bought on the final date. The portfolio is sold and
            // the cost of selling it lands on the month just finished.
            Weights::new()
        } else {
            portfolio::equal_weight(&pick(index, rebalance))?
        };

        let moved = portfolio::traded_notional(&held, &target)?;
        traded.push(moved);
        let cost = config
            .cost_per_side
            .checked_mul(moved)
            .ok_or_else(|| EngineError::math("costing a rebalance"))?;

        if last {
            if let Some(final_month) = net_monthly.last_mut() {
                *final_month = portfolio::net_of_cost(*final_month, cost)?;
            }
            break;
        }

        let advance =
            portfolio::advance(panel, &target, rebalance.date, rebalances[index + 1].date)?;
        exits = exits.plus(advance.exit_census);
        gross_monthly.push(advance.gross_return);
        net_monthly.push(portfolio::net_of_cost(advance.gross_return, cost)?);
        held = advance.drifted;
    }

    Ok(Series {
        net_monthly,
        gross_monthly,
        traded,
        delisting_exits: exits.total(),
        exits,
    })
}

/// Everything one backtest produced.
#[derive(Debug, Clone)]
pub struct Report {
    pub config: BacktestConfig,
    pub first_rebalance: Date,
    pub last_rebalance: Date,
    pub months: usize,
    pub strategy: Series,
    /// Sharpe at the monthly frequency. This is what `rigor::DeflatedSharpe`
    /// takes; handing it the annualised figure produces a confident wrong
    /// probability with nothing visibly failing.
    pub sharpe_monthly: Decimal,
    /// The figure the trial log records, and it is a diagnostic.
    pub sharpe_annualised: Decimal,
    pub skewness: Decimal,
    pub kurtosis: Decimal,
    pub max_drawdown: Decimal,
    pub total_net_return: Decimal,
    pub total_gross_return: Decimal,
    /// Mean one-way turnover per rebalance. One-way, so a complete rotation
    /// reads as 1.0 rather than 2.0.
    pub mean_one_way_turnover: Decimal,
    pub eligible_min: usize,
    pub eligible_max: usize,
    pub buy_and_hold: BaselineSeries,
    pub random: RandomBaseline,
    /// Whether cash dividends are in these returns, which decides which caveat
    /// block the figures are published under.
    pub dividends_applied: bool,
    /// Whether delisting returns were imputed in these returns, on the same
    /// rule and for the same reason.
    pub delistings_imputed: bool,
}

/// Run the momentum strategy and both baselines over a panel.
pub fn backtest(panel: &Panel, config: &BacktestConfig) -> Result<Report, EngineError> {
    // Checked before anything runs. The configuration is what the trial log
    // records and the panel is what the arithmetic sees, so a disagreement
    // between them is a number labelled as something it is not. Catching it at
    // the boundary costs one comparison; catching it later means noticing that
    // a published figure was wrong.
    if config.actions_sha256.is_some() != panel.dividends_attached() {
        return Err(EngineError::DividendWiringMismatch {
            config_has_actions: config.actions_sha256.is_some(),
            panel_has_dividends: panel.dividends_attached(),
        });
    }
    // The same guard for the same reason, one dataset over, and over two fields
    // rather than one. A configuration naming a convention it never applied
    // records a hash describing arithmetic that did not happen. A panel
    // carrying classified exits under a configuration that names no convention
    // publishes an imputation nothing in the trial log says was made. And a run
    // that imputed without recording which file it classified from is not
    // reproducible from the log, so the file hash has to travel with the
    // convention rather than beside it.
    let attached = panel.delistings_attached();
    if config.delisting_convention.is_some() != attached
        || config.delistings_sha256.is_some() != attached
    {
        return Err(EngineError::DelistingWiringMismatch {
            config_has_convention: config.delisting_convention.is_some(),
            config_has_hash: config.delistings_sha256.is_some(),
            panel_has_delistings: attached,
        });
    }

    let month_ends = panel.month_ends();
    let lead_in = config.required_lead_in();
    // Two dates are needed after the lead-in: one to buy on and one to sell on.
    if month_ends.len() <= lead_in + 1 {
        return Err(EngineError::InsufficientHistory {
            months: month_ends.len(),
            required: lead_in + 2,
        });
    }

    let rebalances: Vec<Rebalance> = (lead_in..month_ends.len())
        .map(|index| momentum::rebalance_at(panel, config, index))
        .collect::<Result<_, _>>()?;

    if rebalances
        .iter()
        .all(|rebalance| rebalance.eligible.is_empty())
    {
        return Err(EngineError::EligibilityCollapse {
            rebalances: rebalances.len(),
        });
    }

    let strategy = run_schedule(panel, config, &rebalances, |_, rebalance| {
        rebalance.chosen.clone()
    })?;

    let months = strategy.net_monthly.len();
    if months < 2 {
        return Err(EngineError::TooFewMonths { months });
    }
    let moments = rigor::moments(&strategy.net_monthly).ok_or(EngineError::NoDispersion)?;
    let sharpe_annualised = moments
        .sharpe
        .checked_mul(portfolio::annualisation_factor()?)
        .ok_or_else(|| EngineError::math("annualising the strategy Sharpe"))?;

    let traded_total = strategy
        .traded
        .iter()
        .try_fold(Decimal::ZERO, |running, value| running.checked_add(*value))
        .ok_or_else(|| EngineError::math("summing traded notional"))?;
    let mean_one_way_turnover = traded_total
        .checked_div(Decimal::from(strategy.traded.len()))
        .and_then(|two_way| two_way.checked_div(Decimal::from(2u64)))
        .ok_or_else(|| EngineError::math("averaging turnover"))?;

    let buy_and_hold = baseline::buy_and_hold(panel, config, &rebalances)?;

    let mut draws = Vec::with_capacity(config.random_draws);
    for draw in 0..config.random_draws {
        // Each draw gets its own stream, offset by the draw index, so the set
        // of draws is reproducible from the one seed in the config.
        let mut generator = SplitMix64::new(config.random_seed.wrapping_add(draw as u64));
        let series = run_schedule(panel, config, &rebalances, |_, rebalance| {
            let quintile = (rebalance.eligible.len() / config.quintile_divisor).max(1);
            generator.sample(&rebalance.eligible, quintile)
        })?;
        draws.push(BaselineSeries::from_monthly(series.net_monthly)?.annualised_sharpe);
    }
    let random = RandomBaseline::summarise(draws)?;

    let eligible_counts: Vec<usize> = rebalances
        .iter()
        .map(|rebalance| rebalance.eligible.len())
        .collect();

    Ok(Report {
        first_rebalance: rebalances[0].date,
        last_rebalance: rebalances[rebalances.len() - 1].date,
        months,
        sharpe_monthly: moments.sharpe,
        sharpe_annualised,
        skewness: moments.skewness,
        kurtosis: moments.kurtosis,
        max_drawdown: portfolio::max_drawdown(&strategy.net_monthly)?,
        total_net_return: portfolio::cumulative(&strategy.net_monthly)?,
        total_gross_return: portfolio::cumulative(&strategy.gross_monthly)?,
        mean_one_way_turnover,
        eligible_min: eligible_counts.iter().copied().min().unwrap_or(0),
        eligible_max: eligible_counts.iter().copied().max().unwrap_or(0),
        strategy,
        buy_and_hold,
        random,
        // Equal to `config.actions_sha256.is_some()` by the guard at the top of
        // this function, and read off the panel because the panel is what the
        // arithmetic actually used.
        dividends_applied: panel.dividends_attached(),
        delistings_imputed: panel.delistings_attached(),
        config: config.clone(),
    })
}
