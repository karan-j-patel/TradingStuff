//! Formation replay: how much of a book's turnover is its size rather than its
//! signal.
//!
//! # The question
//!
//! The conservative formula's first trial measured 41.8% turnover a quarter
//! against the source paper's ~32%. The candidate explanation is book size: we
//! hold ten to fifteen names where the paper holds a hundred. Names within a
//! short rank distance of the selection boundary churn whatever the book size
//! is, so the churned *fraction* of a small book is larger. That gives a model
//! with one estimable constant:
//!
//! ```text
//! turnover(K) = a + b / K
//! ```
//!
//! `b / K` is the boundary churn, which vanishes as the book grows. `a` is what
//! is left, the turnover the signal itself demands, and it is the number worth
//! comparing against a paper that held a hundred names.
//!
//! Fitting it needs turnover measured at several book sizes on this data, which
//! is what [`replay`] over the deterministic sub-universes provides: half the
//! universe holds roughly half the names, a quarter holds roughly a quarter.
//!
//! # Why this is not a backtest, and cannot become one by accident
//!
//! Rule 2 counts backtests because a backtest produces a return series and a
//! Sharpe, and a Sharpe is a thing a search can be run over. This replays
//! formations only. [`Replay`] has fields for book size, turnover and formation
//! count, and no field for a return, so nothing this module emits can be
//! presented as performance or selected on as one. The trial log is not opened
//! for writing anywhere on this path.
//!
//! That is a structural claim rather than a promise, and it has a maintenance
//! condition: a change that adds any return computation to this path has to
//! convert it into a counted backtest first. `x_t1` pins the log-untouched half.

use rust_decimal::Decimal;

use crate::config::BacktestConfig;
use crate::error::EngineError;
use crate::panel::Panel;
use crate::portfolio;
use crate::run;

/// Printed with every diagnostic, so a transcript explains itself.
pub const NOT_A_TRIAL: &str = "\
This is a formation replay, not a backtest, and no trial is recorded. It
  computes no return series and no Sharpe: the type it reports carries book
  sizes, turnover and formation counts, and has no field a return could go in.
  Adding one converts this into a backtest and it must be counted as one.";

/// The `(modulus, residue)` splits every diagnostic replays.
///
/// The full universe, both halves, and all four quarters. Seven points is what
/// the two-constant fit has to work with, and the three distinct book sizes are
/// what give `1 / K` any leverage at all.
///
/// `permaticker % modulus` is the same discriminator the universe fetch itself
/// used to draw the sample, so these splits are deterministic across runs,
/// disjoint, and between them exhaustive.
pub const SUB_UNIVERSES: &[(u64, u64)] = &[(1, 0), (2, 0), (2, 1), (4, 0), (4, 1), (4, 2), (4, 3)];

/// How one sub-universe behaved. No return field, by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    /// `permaticker % modulus == residue` is the membership rule.
    pub modulus: u64,
    pub residue: u64,
    /// Securities the sub-universe contained, before any eligibility rule.
    pub securities: usize,
    /// Formation dates the schedule produced.
    pub formations: usize,
    /// Mean number of names actually held, which is the `K` of the model.
    pub mean_book: Decimal,
    /// Mean single-counted one-way turnover per formation.
    pub mean_turnover: Decimal,
}

impl Replay {
    /// How the membership rule reads in a report.
    pub fn label(&self) -> String {
        if self.modulus == 1 {
            "full universe".to_string()
        } else {
            format!("permaticker % {} == {}", self.modulus, self.residue)
        }
    }
}

/// Whether a permaticker belongs to one sub-universe.
pub fn in_sub_universe(permaticker: u64, modulus: u64, residue: u64) -> bool {
    permaticker % modulus == residue
}

/// The vendor permanent identifier, when the security carries one.
fn permaticker(asset: &ingest::schema::AssetKey) -> Option<u64> {
    match asset.permanent {
        Some(ingest::schema::PermanentId::Sharadar(id)) => Some(id),
        _ => None,
    }
}

/// Replay the formations of one sub-universe.
///
/// Every number here comes from code the real backtest runs. The schedule is
/// [`run::schedule`], the selection is `momentum::rebalance_at` inside it, the
/// weights and the drift are `run::run_schedule`, and the turnover average is
/// [`portfolio::mean_one_way_turnover`]. Nothing in this function computes a
/// position, a weight or a traded notional itself, which is what makes `x_t2`'s
/// exact-parity claim hold by construction rather than by two implementations
/// happening to agree.
///
/// The sub-universe is carved out HERE, from the full panel, rather than by
/// the caller. `Panel::retaining` hands back a panel wearing its parent's
/// attachment digests, which is correct for this diagnostic and a mislabelled
/// trial waiting to happen anywhere else, so the filtered panel never leaves
/// this function. A security with no permanent identifier is refused rather
/// than skipped: it belongs to no sub-universe, so the splits would silently
/// stop being exhaustive and the full-universe point would not be the union
/// of the parts.
pub fn replay(
    full_panel: &Panel,
    config: &BacktestConfig,
    modulus: u64,
    residue: u64,
) -> Result<Replay, EngineError> {
    for series in full_panel.securities() {
        if permaticker(&series.asset).is_none() {
            return Err(EngineError::SubUniverseUnplaceable {
                ticker: series.asset.ticker.clone(),
            });
        }
    }
    let panel = &full_panel.retaining(|asset| {
        permaticker(asset).is_some_and(|id| in_sub_universe(id, modulus, residue))
    });
    let rebalances = run::schedule(panel, config)?;
    let series = run::run_schedule(panel, config, &rebalances, |_, rebalance| {
        rebalance.chosen.clone()
    })?;

    // The last formation sells and buys nothing, so its `chosen` is computed and
    // never held. Averaging it in would report a book the run never carried.
    let held = &rebalances[..rebalances.len() - 1];
    let names: usize = held.iter().map(|rebalance| rebalance.chosen.len()).sum();
    let mean_book = Decimal::from(names as u64)
        .checked_div(Decimal::from(held.len() as u64))
        .ok_or_else(|| EngineError::math("averaging the held book size"))?;

    Ok(Replay {
        modulus,
        residue,
        securities: panel.securities().len(),
        formations: rebalances.len(),
        mean_book,
        mean_turnover: portfolio::mean_one_way_turnover(&series.traded)?,
    })
}

/// Least squares of turnover on the reciprocal of book size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fit {
    /// The intercept: turnover as book size grows without bound, which is the
    /// book-size-free estimate the whole exercise is for.
    pub a: Decimal,
    /// The slope on `1 / K`, which is the boundary churn a book of size one
    /// would pay.
    pub b: Decimal,
    /// Observed minus fitted, one per input point and in input order.
    ///
    /// Carried in the same struct as the estimate rather than computed by the
    /// printer, so there is no value of this type that holds an estimate and
    /// not its residuals.
    pub residuals: Vec<Decimal>,
}

/// Fit `turnover = a + b / K` over the replayed points.
///
/// Refused rather than approximated when the points cannot determine a line:
/// fewer than two of them, or every point at the same book size, leaves the
/// slope undefined and a fabricated one would be the single most flattering
/// number this module could emit.
pub fn fit(points: &[Replay]) -> Result<Fit, EngineError> {
    if points.len() < 2 {
        return Err(EngineError::TurnoverFitUnderdetermined {
            points: points.len(),
        });
    }

    let n = Decimal::from(points.len() as u64);
    let mut reciprocals = Vec::with_capacity(points.len());
    for point in points {
        if point.mean_book <= Decimal::ZERO {
            return Err(EngineError::math("taking the reciprocal of an empty book"));
        }
        reciprocals.push(
            Decimal::ONE
                .checked_div(point.mean_book)
                .ok_or_else(|| EngineError::math("taking the reciprocal of a book size"))?,
        );
    }

    let sum = |values: &[Decimal]| -> Result<Decimal, EngineError> {
        values
            .iter()
            .try_fold(Decimal::ZERO, |running, value| running.checked_add(*value))
            .ok_or_else(|| EngineError::math("summing a fit term"))
    };
    let turnovers: Vec<Decimal> = points.iter().map(|point| point.mean_turnover).collect();

    let sum_x = sum(&reciprocals)?;
    let sum_y = sum(&turnovers)?;
    let mut sum_xy = Decimal::ZERO;
    let mut sum_xx = Decimal::ZERO;
    for (x, y) in reciprocals.iter().zip(&turnovers) {
        sum_xy = x
            .checked_mul(*y)
            .and_then(|term| sum_xy.checked_add(term))
            .ok_or_else(|| EngineError::math("accumulating the cross term"))?;
        sum_xx = x
            .checked_mul(*x)
            .and_then(|term| sum_xx.checked_add(term))
            .ok_or_else(|| EngineError::math("accumulating the square term"))?;
    }

    // n * Sxx - Sx^2. Zero exactly when every point shares one book size, which
    // is a degenerate design rather than a rounding matter.
    let denominator = n
        .checked_mul(sum_xx)
        .and_then(|scaled| sum_x.checked_mul(sum_x).map(|square| scaled - square))
        .ok_or_else(|| EngineError::math("forming the least-squares denominator"))?;
    if denominator == Decimal::ZERO {
        return Err(EngineError::TurnoverFitUnderdetermined {
            points: points.len(),
        });
    }

    let b = n
        .checked_mul(sum_xy)
        .and_then(|scaled| sum_x.checked_mul(sum_y).map(|product| scaled - product))
        .and_then(|numerator| numerator.checked_div(denominator))
        .ok_or_else(|| EngineError::math("solving for the slope"))?;
    let a = b
        .checked_mul(sum_x)
        .and_then(|fitted| sum_y.checked_sub(fitted))
        .and_then(|residual| residual.checked_div(n))
        .ok_or_else(|| EngineError::math("solving for the intercept"))?;

    let residuals = reciprocals
        .iter()
        .zip(&turnovers)
        .map(|(x, y)| {
            b.checked_mul(*x)
                .and_then(|slope| a.checked_add(slope))
                .and_then(|fitted| y.checked_sub(fitted))
                .ok_or_else(|| EngineError::math("computing a residual"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Fit { a, b, residuals })
}

/// Six decimal places, matching what the backtest report prints.
fn show(value: Decimal) -> String {
    value.round_dp(6).normalize().to_string()
}

/// The whole fit as one block of text: the estimate, and every residual.
///
/// # Why this is one function returning one string
///
/// The spec's requirement is that no code path prints the book-size-free
/// estimate without the residuals beside it. A printer that emitted the estimate
/// and then looped would satisfy that only until somebody returned early. Here
/// the estimate and the residuals are assembled into a single value before any
/// of it can be printed, so the caller has no way to take one without the other,
/// and `x_t5` fails on a version that tries.
///
/// The residuals are what tell a reader whether the estimate means anything. A
/// clean fit and a fit that missed every point by ten points of turnover produce
/// the same `a`, and only the residuals distinguish them.
pub fn block(points: &[Replay], fit: &Fit) -> String {
    let mut lines = vec![
        "Book-size-free turnover estimate, from turnover(K) = a + b/K".to_string(),
        format!("  a, turnover as K grows:  {}   the estimate", show(fit.a)),
        format!("  b, boundary churn:       {}", show(fit.b)),
        String::new(),
        "  Residuals, observed minus fitted. The estimate means nothing without".to_string(),
        "  these: a fit that missed every point produces an `a` too.".to_string(),
    ];
    for (point, residual) in points.iter().zip(&fit.residuals) {
        lines.push(format!(
            "    {:<24}  K {:>8}   turnover {:>10}   residual {}",
            point.label(),
            show(point.mean_book),
            show(point.mean_turnover),
            show(*residual)
        ));
    }
    lines.join("\n")
}
