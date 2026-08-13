//! The tradability screen: drop the thinnest names before ranking the rest.
//!
//! # What this is for
//!
//! Li, Sullivan and Garcia-Feijoo (2014) report that the low-volatility
//! anomaly's abnormal return is concentrated in low-liquidity and smaller
//! names, which is the shape of a limits-to-arbitrage artifact rather than a
//! risk premium: a pattern that survives only where it cannot be traded is not
//! one anybody can collect. Removing the thin tail before the ranking is the
//! test of that, and a result that survives it is worth more than one that has
//! never been asked.
//!
//! Note what this screen is not. It is not a cost model. Costs are charged on
//! every run by rule 3 whatever this does, and a name that clears the screen is
//! charged exactly as much as one that would not have.
//!
//! # Why median rather than mean dollar volume
//!
//! Dollar volume is heavily right-skewed within a name's own history. A single
//! index-rebalance day or an earnings print can carry many times an ordinary
//! day's turnover, and a mean over a twelve-month window carries that one day
//! into the estimate for the whole year. The question the screen asks is what a
//! typical day looks like, which is the median's question and not the mean's.

use jiff::civil::Date;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use crate::error::EngineError;
use crate::panel::Panel;

/// The median of a set of values.
///
/// `None` for an empty set, which is a question with no answer rather than an
/// answer of zero. An even count takes the mean of the two middle values, which
/// is the ordinary definition and the one the fixture in `e10b` pins.
///
/// Takes a slice and copies it rather than sorting the caller's data in place.
/// The copy is a handful of values per security per rebalance, and a function
/// that reorders its argument is the kind of surprise that costs an afternoon
/// when someone later reads the same slice expecting date order.
fn median(values: &[Decimal]) -> Result<Option<Decimal>, EngineError> {
    if values.is_empty() {
        return Ok(None);
    }
    let mut sorted = values.to_vec();
    // `Decimal` implements `Ord`, comparing by value rather than by scale, so
    // 0.20 and 0.2 sort as equal here exactly as they compare equal elsewhere.
    sorted.sort();

    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        return Ok(Some(sorted[middle]));
    }
    sorted[middle - 1]
        .checked_add(sorted[middle])
        .and_then(|sum| sum.checked_div(Decimal::TWO))
        .map(Some)
        .ok_or_else(|| EngineError::math("averaging the two middle dollar volumes"))
}

/// Median daily dollar volume for one security over one formation window.
///
/// `Ok(None)` when the security has no bar in the window at all, which makes it
/// unrankable and so excludes it, exactly as a missing signal does. `Err` is an
/// arithmetic surprise rather than a data gap, and is not papered over.
pub fn median_dollar_volume(
    panel: &Panel,
    security: usize,
    window_start: Date,
    window_end: Date,
) -> Result<Option<Decimal>, EngineError> {
    let Some(series) = panel.securities().get(security) else {
        return Ok(None);
    };
    // Over the bars the name actually has, not over the days the market was
    // open. The coverage rule has already refused anything with fewer than 80
    // percent of them, so this is a median over a populated window rather than
    // over whatever a sparse name happened to print.
    median(&series.dollar_volumes_in(window_start, window_end)?)
}

/// The eligible names that survive the screen, in identity order.
///
/// `eligible` is the set that passed every other rule, ascending. The return is
/// a subset of it in the same order.
///
/// # How many names go
///
/// With `E` names ranked and a fraction `f`, exactly `floor(E * f)` are
/// excluded. The floor is what keeps a thin month from emptying itself: at four
/// eligible names and a fifth removed, `floor(0.8)` is zero and the month keeps
/// everything rather than losing a name it could not afford to lose. Rounding
/// up instead would remove one name from a month that has four, which is a
/// quarter of the cross-section rather than a fifth.
///
/// # Ties
///
/// Two names with the same median dollar volume break on their position, which
/// is identity order, so the same fixture excludes the same name on every run.
/// That matches the tie-break the signal ranking already uses and for the same
/// reason: an order that depended on how the sort happened to land would make
/// the backtest non-reproducible while every test still passed.
pub fn screened(
    panel: &Panel,
    fraction: Decimal,
    eligible: &[usize],
    window_start: Date,
    window_end: Date,
) -> Result<Vec<usize>, EngineError> {
    // Refused rather than clamped. At 1.0 the screen would empty the
    // cross-section and the run would report a flat series as a result; below
    // zero it would exclude a negative number of names, which is not a
    // question. `CLAUDE.md` is explicit that malformed input errors.
    if fraction < Decimal::ZERO || fraction >= Decimal::ONE {
        return Err(EngineError::LiquidityFractionOutOfRange { fraction });
    }

    // A name with no median cannot be ranked, so it is excluded here rather
    // than given a value. Zero would be the thinnest possible reading and would
    // put it in the removed tail; the largest possible reading would keep it
    // whatever its liquidity. Neither is a measurement.
    let mut ranked: Vec<(Decimal, usize)> = Vec::with_capacity(eligible.len());
    for position in eligible {
        if let Some(volume) = median_dollar_volume(panel, *position, window_start, window_end)? {
            ranked.push((volume, *position));
        }
    }

    drop_smallest(fraction, ranked, "sizing the liquidity exclusion")
}

/// The names left after `floor(len * fraction)` of them are dropped from the
/// small end of `ranked`, returned in identity order.
///
/// Shared by the liquidity screen and by the conservative formula's size
/// screen, because the two differ only in what they rank on. The floor is the
/// part worth having in one place: at four names and a fifth removed,
/// `floor(0.8)` is zero and the month keeps everything rather than losing a
/// name it could not afford to lose. Rounding up instead would remove a quarter
/// of a four-name cross-section rather than a fifth, and a rule that drifted
/// between two copies would be invisible.
///
/// `context` names the arithmetic in an overflow message, since a reader
/// chasing one down needs to know which screen produced it.
///
/// The fraction is validated by the caller, which is what lets each screen
/// refuse with an error naming its own field rather than the other one's.
pub(crate) fn drop_smallest(
    fraction: Decimal,
    mut ranked: Vec<(Decimal, usize)>,
    context: &'static str,
) -> Result<Vec<usize>, EngineError> {
    // Smallest first, ties on identity order. `sort` on a tuple compares the
    // first element and falls through to the second, which is exactly the
    // tie-break wanted, so no comparator is needed.
    ranked.sort();

    let excluded = Decimal::from(ranked.len())
        .checked_mul(fraction)
        .ok_or(EngineError::Arithmetic { context })?
        .floor()
        .to_usize()
        .ok_or(EngineError::Arithmetic { context })?;

    // Back to identity order. The caller filters parallel vectors against this,
    // and all of them are in identity order.
    let mut kept: Vec<usize> = ranked
        .into_iter()
        .skip(excluded)
        .map(|(_, position)| position)
        .collect();
    kept.sort_unstable();
    Ok(kept)
}
