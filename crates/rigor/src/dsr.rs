//! The Deflated Sharpe Ratio (Bailey & López de Prado, 2014).
//!
//! # What it answers
//!
//! Given a strategy's Sharpe ratio, how likely is it that the edge is real
//! rather than the best of everything that was tried? The answer is a
//! probability between zero and one, and `CLAUDE.md` rule 1 requires it beside
//! every Sharpe this platform reports.
//!
//! # How it differs from [`crate::threshold`]
//!
//! [`crate::threshold::expected_max_sharpe`] answers a narrower question with
//! less input: the leading term of the expected best Sharpe under the null,
//! `sigma_SR * sqrt(2 * ln(N))`. It is a bar to clear and it is not a
//! probability. This module computes the real thing, which needs three inputs
//! that a bar does not:
//!
//! - `T`, the number of returns, because a Sharpe measured over forty months
//!   is weaker evidence than the same Sharpe over four hundred.
//! - The skewness and kurtosis of those returns, because the Sharpe's own
//!   sampling distribution is only the familiar one when returns are normal,
//!   and strategy returns are reliably not. Negative skew and fat tails both
//!   inflate a Sharpe relative to what it is worth.
//!
//! Both stay. The threshold is what `status` prints about the log as a whole,
//! where no return series exists to take moments of.
//!
//! # The formulae, as implemented
//!
//! ```text
//! DSR  = Z[ (SR - SR*) * sqrt(T - 1) / sqrt(1 - g3*SR + (g4 - 1)/4 * SR^2) ]
//!
//! SR*  = sigma_SR * [ (1 - gamma) * Z^-1(1 - 1/N) + gamma * Z^-1(1 - 1/(N e)) ]
//! ```
//!
//! `Z` is the standard normal CDF, `gamma` is the Euler-Mascheroni constant,
//! `g3` and `g4` are skewness and kurtosis, and `SR` is at the frequency the
//! returns are sampled at rather than annualised. That last point is the
//! easiest thing here to get wrong: feeding an annualised Sharpe in produces a
//! confident and badly wrong probability, so [`DeflatedSharpe::observed`] says
//! so at the field.
//!
//! # Accuracy, stated rather than implied
//!
//! `Decimal` carries no error function, so [`normal_cdf`] uses the
//! Abramowitz and Stegun 7.1.26 rational approximation, whose published
//! absolute error bound is `1.5e-7`. [`normal_quantile`] bisects on that, so it
//! inherits the same bound rather than adding one of its own. A probability
//! reported to four decimal places is therefore honest and a fifteenth decimal
//! place would not be. Nothing here routes through `f64`.

use std::str::FromStr as _;

use rust_decimal::{Decimal, MathematicalOps};

/// Euler-Mascheroni, the constant weighting the two quantiles in `SR*`.
fn euler_mascheroni() -> Decimal {
    constant("0.5772156649015329")
}

fn e() -> Decimal {
    constant("2.718281828459045235360287471")
}

/// Parse a literal that is known good, so the call sites stay readable.
fn constant(text: &str) -> Decimal {
    Decimal::from_str(text).expect("a literal in this module is not a valid Decimal")
}

/// Beyond this the approximation is saturated and `exp(-x^2)` would underflow.
///
/// `erf(6)` differs from one by about `2e-17`, which is far inside the
/// approximation's own error, so returning exactly one past here loses nothing
/// and avoids asking `Decimal` for `exp(-36)`.
const ERF_SATURATION: i64 = 6;

/// The error function, via Abramowitz and Stegun 7.1.26.
///
/// Absolute error bounded by `1.5e-7`, which is the published figure for this
/// approximation and the accuracy limit of everything downstream of it.
fn erf(x: Decimal) -> Option<Decimal> {
    let negative = x.is_sign_negative();
    let x = x.abs();

    if x > Decimal::from(ERF_SATURATION) {
        return Some(if negative {
            -Decimal::ONE
        } else {
            Decimal::ONE
        });
    }

    let t = Decimal::ONE.checked_div(Decimal::ONE.checked_add(constant("0.3275911") * x)?)?;

    // Horner's method, which evaluates a polynomial with one multiply per term
    // and no powers, so it is both shorter and less lossy than the written-out
    // form.
    let poly = ((((constant("1.061405429") * t - constant("1.453152027")) * t
        + constant("1.421413741"))
        * t
        - constant("0.284496736"))
        * t
        + constant("0.254829592"))
        * t;

    let decay = Decimal::ZERO
        .checked_sub(x.checked_mul(x)?)?
        .checked_exp()?;
    let value = Decimal::ONE.checked_sub(poly.checked_mul(decay)?)?;

    Some(if negative { -value } else { value })
}

/// The standard normal CDF, written `Z` in the paper.
pub fn normal_cdf(x: Decimal) -> Option<Decimal> {
    let root_two = Decimal::TWO.sqrt()?;
    let scaled = erf(x.checked_div(root_two)?)?;
    Decimal::ONE.checked_add(scaled)?.checked_div(Decimal::TWO)
}

/// How many halvings the quantile search takes.
///
/// The bracket is 18 wide and `Decimal` holds 28 significant digits, so a
/// hundred halvings takes the interval to about `1.4e-29` and the search has
/// stopped moving long before that. It is not a tuning parameter.
const QUANTILE_STEPS: usize = 100;

/// The inverse of [`normal_cdf`], by bisection.
///
/// # Why bisection rather than a rational approximation
///
/// The published closed forms for this are tables of six or more hand-entered
/// constants, and a single mistyped digit in one of them produces a function
/// that is smooth, plausible, and wrong in the tail, which is exactly where
/// this is used. Bisection has no constants to mistype: it is correct as long
/// as [`normal_cdf`] is monotone, which it is. The cost is a hundred cheap
/// evaluations, on a path that runs once per report.
///
/// `None` outside the open interval `(0, 1)`, where no quantile exists.
pub fn normal_quantile(probability: Decimal) -> Option<Decimal> {
    if probability <= Decimal::ZERO || probability >= Decimal::ONE {
        return None;
    }

    let (mut low, mut high) = (Decimal::from(-9), Decimal::from(9));
    for _ in 0..QUANTILE_STEPS {
        let middle = low.checked_add(high)?.checked_div(Decimal::TWO)?;
        if normal_cdf(middle)? < probability {
            low = middle;
        } else {
            high = middle;
        }
    }

    low.checked_add(high)?.checked_div(Decimal::TWO)
}

/// `SR*`, the Sharpe the luckiest of `N` trials is expected to reach with no
/// edge at all.
///
/// This is the exact expression from the paper, where
/// [`crate::threshold::expected_max_sharpe`] is its leading term. `None` below
/// two trials, where there is no selection to correct for and `1 - 1/N` is not
/// a probability the quantile is defined at.
pub fn benchmark_sharpe(sigma_sr: Decimal, trials: usize) -> Option<Decimal> {
    if trials < 2 {
        return None;
    }

    let count = Decimal::from(trials);
    let gamma = euler_mascheroni();

    let first = normal_quantile(Decimal::ONE.checked_sub(Decimal::ONE.checked_div(count)?)?)?;
    let second = normal_quantile(
        Decimal::ONE.checked_sub(Decimal::ONE.checked_div(count.checked_mul(e())?)?)?,
    )?;

    let weighted = Decimal::ONE
        .checked_sub(gamma)?
        .checked_mul(first)?
        .checked_add(gamma.checked_mul(second)?)?;

    sigma_sr.checked_mul(weighted)
}

/// Everything the Deflated Sharpe Ratio needs.
///
/// A struct rather than six positional arguments, because four of them are
/// `Decimal` and transposing two at a call site would compile and produce a
/// number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeflatedSharpe {
    /// The strategy's Sharpe **at the frequency the returns are sampled at**,
    /// never annualised.
    ///
    /// Annualising multiplies by the square root of the periods per year, and
    /// feeding that in here overstates the result badly while failing in no
    /// visible way. Monthly returns mean a monthly Sharpe.
    pub observed: Decimal,
    /// `T`, how many returns the Sharpe was measured over.
    pub periods: usize,
    /// `g3`, the skewness of those returns.
    pub skewness: Decimal,
    /// `g4`, the kurtosis of those returns, **not** excess kurtosis. A normal
    /// distribution scores 3 here, not 0.
    pub kurtosis: Decimal,
    /// `sigma_SR`, the spread of Sharpes across the trials, from
    /// [`crate::threshold::sigma_sr`].
    pub sigma_sr: Decimal,
    /// `N`, the trial count this is being deflated against.
    pub trials: usize,
}

impl DeflatedSharpe {
    /// The probability that the edge is real rather than the best of `N` tries.
    ///
    /// `None` when the inputs cannot support an answer: fewer than two returns,
    /// fewer than two trials, or a variance term that is not positive. In every
    /// one of those cases the honest output is that no figure exists, and a
    /// caller is expected to print that rather than substitute one.
    pub fn probability(&self) -> Option<Decimal> {
        if self.periods < 2 {
            return None;
        }

        let benchmark = benchmark_sharpe(self.sigma_sr, self.trials)?;
        let excess = self.observed.checked_sub(benchmark)?;
        let span = Decimal::from(self.periods)
            .checked_sub(Decimal::ONE)?
            .sqrt()?;

        // The variance of the Sharpe's own estimator under non-normal returns.
        // Negative skew makes this larger, which shrinks the statistic, which
        // is the correction the whole paper is about.
        let variance = Decimal::ONE
            .checked_sub(self.skewness.checked_mul(self.observed)?)?
            .checked_add(
                self.kurtosis
                    .checked_sub(Decimal::ONE)?
                    .checked_div(Decimal::from(4))?
                    .checked_mul(self.observed.checked_mul(self.observed)?)?,
            )?;
        if variance <= Decimal::ZERO {
            return None;
        }

        normal_cdf(excess.checked_mul(span)?.checked_div(variance.sqrt()?)?)
    }
}

/// The Sharpe, skewness, and kurtosis of one return series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moments {
    /// Mean over standard deviation, at the frequency the returns are in.
    pub sharpe: Decimal,
    pub skewness: Decimal,
    /// Not excess. A normal distribution scores 3.
    pub kurtosis: Decimal,
}

/// Take the moments the Deflated Sharpe needs from a return series.
///
/// Population moments, dividing by `T`, which is what the paper's expression
/// assumes. This is deliberately not the sample convention used by
/// [`crate::threshold::sigma_sr`], which measures a different thing: the spread
/// of Sharpes across trials is a sample of the trials that might have been run,
/// while these describe the return series in hand.
///
/// `None` for fewer than two returns or a series with no variation, where a
/// Sharpe would be a division by zero.
pub fn moments(returns: &[Decimal]) -> Option<Moments> {
    if returns.len() < 2 {
        return None;
    }

    let count = Decimal::from(returns.len());
    let total = returns
        .iter()
        .try_fold(Decimal::ZERO, |running, value| running.checked_add(*value))?;
    let mean = total.checked_div(count)?;

    let mut second = Decimal::ZERO;
    let mut third = Decimal::ZERO;
    let mut fourth = Decimal::ZERO;
    for value in returns {
        let deviation = value.checked_sub(mean)?;
        let squared = deviation.checked_mul(deviation)?;
        second = second.checked_add(squared)?;
        third = third.checked_add(squared.checked_mul(deviation)?)?;
        fourth = fourth.checked_add(squared.checked_mul(squared)?)?;
    }

    let variance = second.checked_div(count)?;
    if variance <= Decimal::ZERO {
        return None;
    }
    let deviation = variance.sqrt()?;

    Some(Moments {
        sharpe: mean.checked_div(deviation)?,
        skewness: third
            .checked_div(count)?
            .checked_div(deviation.checked_mul(variance)?)?,
        kurtosis: fourth
            .checked_div(count)?
            .checked_div(variance.checked_mul(variance)?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The accuracy the A&S 7.1.26 approximation is published at, which is the
    /// tightest any assertion on a CDF value may honestly be.
    fn close(actual: Decimal, expected: &str) {
        close_within(actual, expected, "0.0000005");
    }

    /// For values that come back through [`normal_quantile`].
    ///
    /// Inverting amplifies the CDF's error by one over the normal density at
    /// the answer, because `dz = dp / phi(z)`. At `z = 1.96` the density is
    /// about `0.0584`, so a `1.5e-7` error in the CDF becomes roughly `2.6e-6`
    /// in the quantile, and deeper in the tail it grows further. That is a
    /// property of inverting an approximation rather than a defect, and the
    /// tolerance says so instead of the assertion being quietly relaxed.
    fn close_within(actual: Decimal, expected: &str, tolerance: &str) {
        let expected = constant(expected);
        let tolerance = constant(tolerance);
        assert!(
            (actual - expected).abs() < tolerance,
            "expected about {expected}, got {actual}, difference {}",
            (actual - expected).abs()
        );
    }

    fn d(text: &str) -> Decimal {
        constant(text)
    }

    // --- the contract, against an independent implementation ----------------
    //
    // Every expected value below was produced by SciPy, not by this code:
    //
    //   from scipy.stats import norm
    //   norm.cdf(1.96)   ->  0.9750021048517795
    //   norm.ppf(0.975)  ->  1.959963984540054
    //
    // That is the point of them. A literal this file computed for itself would
    // agree with this file forever and prove nothing, which is the same reason
    // `entry.rs` pins the genesis hash against a figure Python produced.
    //
    // --- why SciPy is the oracle and not the paper -------------------------
    //
    // The module header cites Bailey and López de Prado (2014) because that is
    // where the formulae come from. None of the numbers below appear in it.
    //
    // The paper's own worked example would have been the better oracle, and it
    // was not used for one reason: its figures could not be verified from the
    // sources available when this was written. Transcribing numbers from memory
    // and attributing them to a citation would produce a test that looks
    // authoritative, cannot be checked against its stated source, and is wrong
    // in a way nobody would think to question. Citing the oracle actually used
    // is worse advertising and better evidence.
    //
    // This note exists so that a reader who does have the paper, finds these
    // values absent from it, and reaches for the obvious conclusion — that
    // somebody fat-fingered a transcription — stops here instead.
    //
    // Adding the paper's example alongside these, once its figures are
    // verified against the paper itself, would be a strict improvement: two
    // independent oracles agreeing is a stronger contract than one, and they
    // would catch different classes of error. The formulae are unchanged
    // either way, so it is additive work rather than a correction.

    #[test]
    fn d1_the_normal_cdf_matches_scipy() {
        close(normal_cdf(Decimal::ZERO).expect("computable"), "0.5");
        close(normal_cdf(d("1.96")).expect("computable"), "0.9750021049");
        close(normal_cdf(d("-1")).expect("computable"), "0.1586552539");
        close(normal_cdf(d("2.5")).expect("computable"), "0.9937903347");
    }

    #[test]
    fn d1_the_cdf_is_symmetric_and_saturates() {
        // Symmetry is structural rather than approximated, so it holds tighter
        // than the error bound and is worth asserting separately.
        let left = normal_cdf(d("-1.3")).expect("computable");
        let right = normal_cdf(d("1.3")).expect("computable");
        close(left + right, "1");
        assert_eq!(normal_cdf(d("20")).expect("computable"), Decimal::ONE);
    }

    #[test]
    fn d2_the_quantile_matches_scipy() {
        close_within(
            normal_quantile(d("0.975")).expect("computable"),
            "1.9599639845",
            "0.00001",
        );
        close_within(
            normal_quantile(d("0.99")).expect("computable"),
            "2.326347874",
            "0.00001",
        );
    }

    #[test]
    fn d2_the_quantile_is_undefined_outside_zero_to_one() {
        assert_eq!(normal_quantile(Decimal::ZERO), None);
        assert_eq!(normal_quantile(Decimal::ONE), None);
        assert_eq!(normal_quantile(d("-0.5")), None);
    }

    #[test]
    fn d3_the_benchmark_sharpe_matches_scipy() {
        // sigma_SR 0.1, N 50. SciPy: 0.22763030934203485
        close_within(
            benchmark_sharpe(d("0.1"), 50).expect("computable"),
            "0.2276303093",
            "0.00001",
        );
    }

    #[test]
    fn d3_the_benchmark_needs_at_least_two_trials() {
        assert_eq!(benchmark_sharpe(d("0.1"), 0), None);
        assert_eq!(benchmark_sharpe(d("0.1"), 1), None);
    }

    /// The whole formula end to end, against SciPy.
    ///
    /// SR 0.2 over 100 periods, skew -0.5, kurtosis 4.0, sigma_SR 0.1, N 50.
    /// SciPy gives SR* 0.22763030934203485, z -0.2586212001509879, and a
    /// Deflated Sharpe of 0.3979637621306946.
    ///
    /// Worth reading rather than only checking: an observed Sharpe of 0.2 sits
    /// *below* the 0.2276 the best of fifty coin flips would be expected to
    /// reach, so the probability lands under a half. That is the correction
    /// doing its job.
    #[test]
    fn d4_the_deflated_sharpe_matches_scipy_end_to_end() {
        let inputs = DeflatedSharpe {
            observed: d("0.2"),
            periods: 100,
            skewness: d("-0.5"),
            kurtosis: d("4.0"),
            sigma_sr: d("0.1"),
            trials: 50,
        };

        close_within(
            inputs.probability().expect("computable"),
            "0.3979637621",
            "0.00001",
        );
    }

    /// More trials must make the same result less impressive.
    #[test]
    fn d4_more_trials_deflate_the_same_sharpe_further() {
        let inputs = DeflatedSharpe {
            observed: d("0.2"),
            periods: 100,
            skewness: d("-0.5"),
            kurtosis: d("4.0"),
            sigma_sr: d("0.1"),
            trials: 50,
        };
        let searched = DeflatedSharpe {
            trials: 1000,
            ..inputs
        };

        // SciPy for N = 1000: 0.1200372556271952. Looser than the N = 50 case
        // above by design: a larger N puts the benchmark quantile further into
        // the tail, where inverting the CDF amplifies its error more.
        close_within(
            searched.probability().expect("computable"),
            "0.1200372556",
            "0.00005",
        );
        assert!(
            searched.probability().expect("computable") < inputs.probability().expect("computable"),
            "searching harder must not raise the probability that the edge is real"
        );
    }

    #[test]
    fn d4_no_figure_exists_below_two_periods_or_two_trials() {
        let inputs = DeflatedSharpe {
            observed: d("0.2"),
            periods: 1,
            skewness: Decimal::ZERO,
            kurtosis: Decimal::from(3),
            sigma_sr: d("0.1"),
            trials: 50,
        };
        assert_eq!(inputs.probability(), None);
        assert_eq!(
            DeflatedSharpe {
                periods: 100,
                trials: 1,
                ..inputs
            }
            .probability(),
            None
        );
    }

    #[test]
    fn d5_moments_match_hand_computation() {
        // Returns 1, 2, 3, 4. Mean 2.5. Deviations -1.5, -0.5, 0.5, 1.5.
        // Squares 2.25, 0.25, 0.25, 2.25, summing to 5. Population variance
        // 5/4 = 1.25, deviation sqrt(1.25) = 1.1180339887.
        // Sharpe 2.5 / 1.1180339887 = 2.2360679775.
        // Third powers cancel in pairs, so skewness is exactly 0.
        // Fourth powers 5.0625, 0.0625, 0.0625, 5.0625 sum to 10.25.
        // Kurtosis (10.25 / 4) / 1.25^2 = 2.5625 / 1.5625 = 1.64.
        let taken = moments(&[
            Decimal::ONE,
            Decimal::TWO,
            Decimal::from(3),
            Decimal::from(4),
        ])
        .expect("computable");

        close(taken.sharpe, "2.2360679775");
        assert_eq!(taken.skewness, Decimal::ZERO);
        close(taken.kurtosis, "1.64");
    }

    #[test]
    fn d5_moments_need_variation_and_two_returns() {
        assert_eq!(moments(&[Decimal::ONE]), None);
        assert_eq!(moments(&[Decimal::ONE, Decimal::ONE]), None);
    }
}
