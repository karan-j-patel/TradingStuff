//! The low-volatility signal.
//!
//! # What is measured, and why this window
//!
//! Volatility is the sample standard deviation of daily price returns over the
//! twelve months ending at the last trading day of the month before the
//! rebalance month. The choice is taken from the literature and is deliberately
//! not swept, because every extra formation window is another trial and rule 2
//! counts them all.
//!
//! Twelve months of daily bars matches the trailing 252 trading days the S&P
//! 500 Low Volatility Index uses, and sits inside the range the academic
//! literature spans. Blitz and van Vliet (2007) form on three years of weekly
//! returns and Ang, Hodrick, Xing and Zhang (2006) on one month of daily ones,
//! so twelve months of daily bars is neither the shortest nor the longest
//! reasonable reading. It is also momentum's own window, which is what lets the
//! two strategies share every constant in [`crate::config::BacktestConfig`] and
//! differ only in the signal.
//!
//! # Why the window ends a month early
//!
//! The literature computes through the rebalance reference date. Ending at the
//! month before instead reuses momentum's lookahead barrier unchanged, so the
//! property E1 holds for momentum holds here for free: nothing in the formation
//! window can see the month the portfolio is formed in. The cost is that the
//! estimate is one month more stale than the literature's. Stale is never less
//! honest, and a signal that can see its own rebalance month is not a signal.
//!
//! # Daily returns, not total returns
//!
//! Returns here are close-over-close price returns from adjusted closes. Cash
//! dividends are deliberately not added, even on a run that applies them to
//! holding returns, because the raw volatility measures the cited literature
//! uses are computed on price returns too. The known consequence is that the
//! mechanical price drop on an ex-dividend date slightly inflates measured
//! volatility for a heavy payer. That bias is shared with the published
//! measures this is meant to be comparable to, so removing it here would make
//! the number less comparable rather than more correct.

use jiff::civil::Date;
use rust_decimal::{Decimal, MathematicalOps};

use crate::panel::Panel;

/// Trailing volatility for one security over one formation window.
///
/// `None` when the window holds fewer than two daily returns, or when any
/// starting price is not positive, or when the arithmetic overflows. A missing
/// value excludes the name, exactly as a missing momentum signal does. It is
/// never defaulted to zero, because zero is the most attractive possible value
/// for this strategy and substituting it would put every name with no data
/// straight into the portfolio.
///
/// Two returns is the minimum because the denominator is `n - 1`. At one return
/// the sample variance is a division by zero, which is a name with no estimate
/// rather than a name with no risk.
///
/// # The denominator, and the square root
///
/// `n - 1` is the sample standard deviation, which is what the index
/// methodologies and the papers report. A constant factor cannot reorder a
/// ranking, so this choice is invisible to any test that asserts on which names
/// were held; `e9c` pins it as a value for that reason.
///
/// `Decimal::sqrt` comes from `rust_decimal`'s `maths` feature and iterates to
/// the type's full 28 significant digits. It is deterministic for a pinned
/// version of the crate, which the workspace manifest fixes, and it is the same
/// call the annualisation factor and the deflated Sharpe already make. Ranking
/// needs strict determinism rather than many digits, and this gives both.
pub fn volatility(
    panel: &Panel,
    security: usize,
    window_start: Date,
    window_end: Date,
) -> Option<Decimal> {
    let series = panel.securities().get(security)?;
    let closes = series.closes_spanning(window_start, window_end);

    // `windows(2)` walks overlapping pairs, so each bar is both one return's
    // close and the next one's open. Bars the security did not trade on are
    // simply absent from `closes`, which is what "between consecutive available
    // bars" means: a gap produces one longer return rather than a fabricated
    // flat day. The coverage rule is what stops a name with too many gaps from
    // reaching this at all.
    let mut returns = Vec::with_capacity(closes.len().saturating_sub(1));
    for pair in closes.windows(2) {
        let (previous, current) = (pair[0], pair[1]);
        if previous <= Decimal::ZERO {
            return None;
        }
        returns.push(current.checked_div(previous)?.checked_sub(Decimal::ONE)?);
    }
    if returns.len() < 2 {
        return None;
    }

    let count = Decimal::from(returns.len());
    let mean = returns
        .iter()
        .try_fold(Decimal::ZERO, |running, value| running.checked_add(*value))?
        .checked_div(count)?;
    let sum_of_squares = returns.iter().try_fold(Decimal::ZERO, |running, value| {
        let deviation = value.checked_sub(mean)?;
        running.checked_add(deviation.checked_mul(deviation)?)
    })?;
    let variance = sum_of_squares.checked_div(count.checked_sub(Decimal::ONE)?)?;
    variance.sqrt()
}
