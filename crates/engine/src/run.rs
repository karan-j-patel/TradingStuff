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
use crate::config::{BacktestConfig, Strategy, Weighting};
use crate::error::EngineError;
use crate::momentum::{self, FormationCensus, Rebalance};
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
/// # Why the marks are monthly even when the formations are not
///
/// `rebalances` holds the formation dates, which are every month-end under a
/// stride of one and every third under a stride of three. The portfolio is
/// marked at every month-end in between regardless, so `net_monthly` is a
/// monthly series whatever the stride and the `sqrt(12)` annualisation stays
/// correct. Coarsening the marks with the stride would leave a quarterly series
/// annualised as though it were monthly, which inflates the Sharpe by `sqrt(3)`
/// with nothing visibly failing.
///
/// The rebalance cost lands on the first month of the stretch, because that is
/// the month the trade happened in.
pub fn run_schedule(
    panel: &Panel,
    config: &BacktestConfig,
    rebalances: &[Rebalance],
    mut pick: impl FnMut(usize, &Rebalance) -> Vec<usize>,
) -> Result<Series, EngineError> {
    let month_ends = panel.month_ends();
    let mut held: Weights = Weights::new();
    let mut net_monthly = Vec::new();
    let mut gross_monthly = Vec::new();
    let mut traded = Vec::with_capacity(rebalances.len());
    let mut exits = ExitCensus::default();

    for (position, rebalance) in rebalances.iter().enumerate() {
        let last = position + 1 == rebalances.len();
        let target = if last {
            // Nothing is bought on the final date. The portfolio is sold and
            // the cost of selling it lands on the month just finished.
            Weights::new()
        } else {
            // The weights are fixed at the formation date, so nothing dated
            // after it can reach a holding. Rule 4.
            portfolio::weights_at(
                panel,
                config.weighting,
                &pick(position, rebalance),
                rebalance.date,
            )?
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

        let mut weights = target;
        let mut charge = cost;
        for step in rebalance.index..rebalances[position + 1].index {
            let advance =
                portfolio::advance(panel, &weights, month_ends[step], month_ends[step + 1])?;
            exits = exits.plus(advance.exit_census);
            gross_monthly.push(advance.gross_return);
            net_monthly.push(portfolio::net_of_cost(advance.gross_return, charge)?);
            charge = Decimal::ZERO;
            weights = advance.drifted;
        }
        held = weights;
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
    /// Mean one-way turnover per formation, single-counted, so a complete
    /// rotation reads as 1.0 rather than 2.0 and a quarterly schedule reports a
    /// quarterly figure rather than a monthly one. This is the figure the
    /// source paper's own turnover is comparable with.
    pub mean_one_way_turnover: Decimal,
    pub eligible_min: usize,
    pub eligible_max: usize,
    /// Where the cross-section went at each formation, in formation order.
    /// Carried whole rather than summarised, so a caller that wants every row
    /// has it and the printer can decide how much of it to show.
    pub formations: Vec<FormationCensus>,
    /// Buy-and-hold weighted the way the strategy is, which is the comparison
    /// rule 5 asks for. Comparing a value-weighted strategy against an
    /// equal-weighted baseline confounds the weighting change with the strategy
    /// change.
    pub buy_and_hold: BaselineSeries,
    /// The same baseline weighted equally, carried only when that is not what
    /// [`Report::buy_and_hold`] already is.
    ///
    /// Every other research program reports against equal-weight buy-and-hold
    /// and `DECISION_CRITERIA.md` gate G2.4 names it, so a variant that dropped
    /// the figure would be incomparable with everything beside it. `None` under
    /// equal weighting, where printing it would be printing the matched
    /// baseline twice and would suggest a comparison had been made.
    pub equal_weighted_buy_and_hold: Option<BaselineSeries>,
    pub random: RandomBaseline,
    /// Whether cash dividends are in these returns, which decides which caveat
    /// block the figures are published under.
    pub dividends_applied: bool,
    /// Whether delisting returns were imputed in these returns, on the same
    /// rule and for the same reason.
    pub delistings_imputed: bool,
}

/// Every check that the configuration and the panel describe the same run.
///
/// One function rather than three blocks inline, so that no path through
/// [`backtest`] can reach the arithmetic without going past all of them. The
/// configuration is what the trial log records and the panel is what the
/// arithmetic sees, so a disagreement between them is a number labelled as
/// something it is not. Catching it at the boundary costs three comparisons;
/// catching it later means noticing that a published figure was wrong.
fn check_wiring(panel: &Panel, config: &BacktestConfig) -> Result<(), EngineError> {
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
    // Once more, one dataset over again. Market caps decide both the size
    // screen and the share-count leg, so a run that ranked on them without
    // recording which file they came from is not reproducible from the log.
    if config.marketcap_sha256.is_some() != panel.marketcaps_attached() {
        return Err(EngineError::MarketcapWiringMismatch {
            config_has_marketcap: config.marketcap_sha256.is_some(),
            panel_has_marketcaps: panel.marketcaps_attached(),
        });
    }

    // Presence agreeing is not identity agreeing. Every check above asks only
    // whether a dataset was named and attached, and a configuration recording
    // the digest of file A while the panel holds file B passes all three. That
    // run publishes numbers under a hash describing data it never read, and the
    // digest is the only description of the input the trial log keeps, so
    // nothing downstream can notice.
    //
    // One loop over all three rather than a check beside each presence test.
    // The hazard is identical for every dataset, and the near-miss worth
    // avoiding is a fourth attachment arriving later with a presence check and
    // no equality check.
    for (dataset, recorded, attached) in [
        (
            "actions",
            config.actions_sha256.as_deref(),
            panel.dividends_sha256(),
        ),
        (
            "delistings",
            config.delistings_sha256.as_deref(),
            panel.delistings_sha256(),
        ),
        (
            "market cap",
            config.marketcap_sha256.as_deref(),
            panel.marketcaps_sha256(),
        ),
    ] {
        if let (Some(recorded), Some(attached)) = (recorded, attached)
            && recorded != attached
        {
            return Err(EngineError::DatasetDigestMismatch {
                dataset,
                recorded: recorded.to_owned(),
                attached: attached.to_owned(),
            });
        }
    }

    // The conservative formula refuses the degraded run rather than producing
    // one. Without dividends its payout leg is a share-count change with no
    // payout in it, without market caps it has neither that leg nor its size
    // screen, and without delistings its exits are unimputed. Each of those is
    // a different strategy wearing this one's name, and a figure published
    // under it would be a mislabelled trial rather than a weaker result.
    if config.strategy == Strategy::ConservativeFormula
        && !(panel.dividends_attached()
            && panel.delistings_attached()
            && panel.marketcaps_attached())
    {
        return Err(EngineError::ConservativeFormulaMissingInputs {
            dividends: panel.dividends_attached(),
            delistings: panel.delistings_attached(),
            marketcaps: panel.marketcaps_attached(),
        });
    }

    // Value weighting needs the market caps themselves, which is a stronger
    // requirement than the hash agreement checked above. A run asked for
    // `--variant value-weighted` with no `--marketcap` argument satisfies that
    // check, because the configuration names no file and the panel carries no
    // caps, so nothing before this point refuses it.
    //
    // # What this guard does and does not prevent, measured
    //
    // It does not prevent a silent equal-weighted run. Removing it does not
    // produce one: selection already excludes every name that cannot be
    // weighted, so with no caps at all the field empties and the run fails with
    // `EligibilityCollapse` across every rebalance date. That was measured
    // rather than assumed, by returning early ahead of this check.
    //
    // What it prevents is that failure naming the wrong cause. "Every rebalance
    // date had an empty eligible set" points a reader at the universe, the
    // price floor and the coverage rule, none of which is what went wrong. The
    // cause is a missing `--marketcap` argument, and this says so at the
    // boundary where the configuration and the panel are compared.
    if config.weighting == Weighting::ValueByMarketcap && !panel.marketcaps_attached() {
        return Err(EngineError::ValueWeightingMissingMarketcaps);
    }
    Ok(())
}

/// The formation dates a configuration implies over a panel, each already
/// resolved into the names it would hold.
///
/// Extracted from [`backtest`] so the turnover diagnostic replays the identical
/// schedule rather than a second copy of this arithmetic. A stride, a lead-in or
/// a collapse rule that drifted between two copies would make the diagnostic
/// describe a run nobody could reproduce.
pub fn schedule(panel: &Panel, config: &BacktestConfig) -> Result<Vec<Rebalance>, EngineError> {
    // Refused rather than clamped to one. A schedule that forms a portfolio
    // every zero months is a configuration mistake, and the loop below would
    // never advance past its first formation.
    if config.rebalance_every_months == 0 {
        return Err(EngineError::RebalanceStrideZero);
    }
    let stride = config.rebalance_every_months;

    let month_ends = panel.month_ends();
    let lead_in = config.required_lead_in();
    // Two formation dates are needed after the lead-in, one to buy on and one
    // to sell on, and they are a stride apart rather than a month apart.
    if month_ends.len() <= lead_in + stride {
        return Err(EngineError::InsufficientHistory {
            months: month_ends.len(),
            required: lead_in + stride + 1,
        });
    }

    // The run ends at its last formation rather than at the panel's last
    // month-end, so under a stride of three up to two trailing months go
    // unused. That is deterministic and stated rather than papered over with a
    // final partial period, which would be a holding period of a different
    // length from every other one in the series.
    let rebalances: Vec<Rebalance> = (lead_in..month_ends.len())
        .step_by(stride)
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
    Ok(rebalances)
}

/// Run the configured strategy and both baselines over a panel.
pub fn backtest(panel: &Panel, config: &BacktestConfig) -> Result<Report, EngineError> {
    check_wiring(panel, config)?;

    let rebalances = schedule(panel, config)?;

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

    let mean_one_way_turnover = portfolio::mean_one_way_turnover(&strategy.traded)?;

    let buy_and_hold = baseline::buy_and_hold(panel, config, &rebalances, config.weighting)?;
    let equal_weighted_buy_and_hold = match config.weighting {
        // Already computed above, and a second copy of one number is not a
        // second comparison.
        Weighting::Equal => None,
        _ => Some(baseline::buy_and_hold(
            panel,
            config,
            &rebalances,
            Weighting::Equal,
        )?),
    };

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
        formations: rebalances
            .iter()
            .map(|rebalance| rebalance.census)
            .collect(),
        strategy,
        buy_and_hold,
        equal_weighted_buy_and_hold,
        random,
        // Equal to `config.actions_sha256.is_some()` by the guard at the top of
        // this function, and read off the panel because the panel is what the
        // arithmetic actually used.
        dividends_applied: panel.dividends_attached(),
        delistings_imputed: panel.delistings_attached(),
        config: config.clone(),
    })
}
