//! How high a Sharpe has to be before it means anything, given how many things
//! were tried.
//!
//! # The problem this quantifies
//!
//! Try enough strategies on the same data and one of them looks good by luck
//! alone. The **Sharpe ratio** is return divided by volatility, and the best
//! Sharpe out of many trials is biased upward by exactly that selection. The
//! bias grows with the number of trials `N` and with `sigma_SR`, the spread of
//! Sharpes across those trials.
//!
//! Bailey and López de Prado (2014) give the expected maximum Sharpe under the
//! null hypothesis that no strategy has any edge. Its usual short form is
//!
//! ```text
//! E[max SR] ≈ sigma_SR * sqrt(2 * ln(N))
//! ```
//!
//! That is the leading term of the exact expression, which also carries a small
//! Euler-Mascheroni correction. The short form is what is implemented here, and
//! calling the result approximate rather than exact is the honest description.
//!
//! Read the output as a floor rather than a target. A strategy scoring below it
//! has not distinguished itself from the luckiest of `N` coin flips. Scoring
//! above it is necessary and nowhere near sufficient, which is why
//! `DECISION_CRITERIA.md` gates on the Deflated Sharpe Ratio instead.
//!
//! # Why every function returns Option
//!
//! Because "not computable yet" is a real answer and inventing a fallback would
//! be worse than useless. With fewer than two recorded Sharpes there is no
//! spread to measure, and with fewer than two trials there is no selection to
//! correct for. In both cases these return `None` and the caller is expected to
//! say so out loud rather than substitute a number.

use rust_decimal::{Decimal, MathematicalOps};

/// Sample standard deviation of the recorded Sharpes, which is `sigma_SR`.
///
/// Sample rather than population, so the divisor is `n - 1`. The recorded
/// Sharpes are a sample of the trials that could have been run, not the whole
/// population of them.
///
/// `None` when fewer than two Sharpes were recorded, or when the arithmetic
/// overflows Decimal's range, which for plausible Sharpes it will not.
pub fn sigma_sr(sharpes: &[Decimal]) -> Option<Decimal> {
    if sharpes.len() < 2 {
        return None;
    }

    let count = Decimal::from(sharpes.len());

    // try_fold over Option short circuits on the first None, which is how the
    // checked arithmetic propagates without a pile of nested matches. It is the
    // same idea as `?` inside a loop.
    let total = sharpes.iter().try_fold(Decimal::ZERO, |running, sharpe| {
        running.checked_add(*sharpe)
    })?;
    let mean = total.checked_div(count)?;

    let sum_of_squares = sharpes.iter().try_fold(Decimal::ZERO, |running, sharpe| {
        let deviation = sharpe.checked_sub(mean)?;
        running.checked_add(deviation.checked_mul(deviation)?)
    })?;

    // count is at least 2, so the divisor is at least 1 and never zero.
    let variance = sum_of_squares.checked_div(count - Decimal::ONE)?;
    variance.sqrt()
}

/// The Sharpe a result has to clear to be unremarkable, given `sigma_SR` and
/// the number of trials.
///
/// `None` when `trials` is below two, since one trial involves no selection and
/// the formula's `ln(N)` is zero there, which would claim any positive Sharpe
/// is meaningful.
pub fn expected_max_sharpe(sigma_sr: Decimal, trials: usize) -> Option<Decimal> {
    if trials < 2 {
        return None;
    }

    let log_trials = Decimal::from(trials).checked_ln()?;
    let root = (Decimal::TWO.checked_mul(log_trials)?).sqrt()?;
    sigma_sr.checked_mul(root)
}

/// [`sigma_sr`] and [`expected_max_sharpe`] together, which is what a caller
/// reporting on a log actually wants.
///
/// Returns `None` for the threshold whenever either input is missing, so a
/// caller cannot accidentally print a threshold derived from a `sigma_SR` that
/// does not exist.
pub fn threshold_for(sharpes: &[Decimal], trials: usize) -> (Option<Decimal>, Option<Decimal>) {
    let sigma = sigma_sr(sharpes);
    let threshold = sigma.and_then(|sigma| expected_max_sharpe(sigma, trials));
    (sigma, threshold)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn decimals(values: &[&str]) -> Vec<Decimal> {
        values
            .iter()
            .map(|value| Decimal::from_str(value).expect("valid decimal"))
            .collect()
    }

    fn assert_close(actual: Decimal, expected: &str) {
        let expected = Decimal::from_str(expected).expect("valid decimal");
        let tolerance = Decimal::from_str("0.000001").expect("valid decimal");
        assert!(
            (actual - expected).abs() < tolerance,
            "expected about {expected}, got {actual}"
        );
    }

    #[test]
    fn sigma_sr_needs_at_least_two_sharpes() {
        assert_eq!(sigma_sr(&[]), None);
        assert_eq!(sigma_sr(&decimals(&["1.5"])), None);
    }

    #[test]
    fn sigma_sr_is_the_sample_standard_deviation() {
        // Hand checked. Sharpes 1, 3, 5. Mean 3. Deviations -2, 0, 2.
        // Sum of squares 8. Sample variance 8 / (3 - 1) = 4. Root 2.
        let sigma = sigma_sr(&decimals(&["1", "3", "5"])).expect("three Sharpes give a spread");
        assert_eq!(sigma, Decimal::from(2));
    }

    #[test]
    fn identical_sharpes_have_no_spread() {
        assert_eq!(
            sigma_sr(&decimals(&["1.4", "1.4", "1.4"])).expect("computable"),
            Decimal::ZERO
        );
    }

    #[test]
    fn the_threshold_is_not_computable_below_two_trials() {
        let sigma = Decimal::from(2);
        assert_eq!(expected_max_sharpe(sigma, 0), None);
        assert_eq!(expected_max_sharpe(sigma, 1), None);
    }

    #[test]
    fn the_threshold_matches_a_hand_checked_value() {
        // sigma_SR 2, N 4. ln(4) = 1.3862943611198906.
        // 2 * ln(4) = 2.7725887222397812. Its root is 1.6651092223153954.
        // Times sigma_SR 2 gives 3.3302184446307908.
        let threshold =
            expected_max_sharpe(Decimal::from(2), 4).expect("computable at four trials");
        assert_close(threshold, "3.3302184446");
    }

    #[test]
    fn the_threshold_grows_with_the_number_of_trials() {
        let sigma = Decimal::ONE;
        let few = expected_max_sharpe(sigma, 10).expect("computable");
        let many = expected_max_sharpe(sigma, 1000).expect("computable");
        assert!(
            many > few,
            "more trials must demand a higher Sharpe, got {few} then {many}"
        );
    }

    #[test]
    fn threshold_for_withholds_the_threshold_when_sigma_is_unknown() {
        // Two trials but only one recorded Sharpe, so there is no spread and
        // therefore no honest threshold, even though N is large enough.
        let (sigma, threshold) = threshold_for(&decimals(&["1.5"]), 2);
        assert_eq!(sigma, None);
        assert_eq!(threshold, None);
    }

    #[test]
    fn threshold_for_reports_both_when_both_are_computable() {
        let (sigma, threshold) = threshold_for(&decimals(&["1", "3", "5"]), 4);
        assert_eq!(sigma, Some(Decimal::from(2)));
        assert_close(threshold.expect("computable"), "3.3302184446");
    }
}
