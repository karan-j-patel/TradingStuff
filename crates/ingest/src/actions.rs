//! Corporate actions and delistings, stored as records rather than folded into
//! prices by a vendor.
//!
//! # Why these are separate from price bars
//!
//! Vendors disagree about adjusted closes in ways that are invisible until they
//! change a result. Measured on SPY for 2021-05-05, one vendor reported an
//! adjusted close of 410.347, another 410.3496, against a correct value of
//! 410.3167. One of them reinvests dividends at "close minus dividend", which is
//! not a price anyone could transact at. Another never applied a documented
//! spin-off factor at all. A third silently dropped two dividends.
//!
//! Raw prices and corporate action records are things vendors broadly agree on.
//! Adjusted closes are where they diverge. So this crate takes the former and
//! computes the latter itself, once, under an explicit mode.

use jiff::civil::Date;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::schema::AssetKey;

/// What kind of distribution a dividend is.
///
/// Cash and stock dividends adjust prices differently, and specials are worth
/// distinguishing because they are often large enough to dominate a return
/// series for one day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DividendKind {
    Cash,
    Stock,
    Special,
}

/// An event that changes the relationship between a price today and the same
/// company's price yesterday.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CorporateAction {
    /// A share split or reverse split. `ratio` is new shares per old share, so
    /// a 2-for-1 split is 2 and a 1-for-10 reverse split is 0.1.
    Split { ratio: Decimal },

    /// A cash or stock distribution. `amount` is per share, in the security's
    /// own currency, on the ex-dividend date.
    Dividend { amount: Decimal, kind: DividendKind },

    /// A spin-off, expressed as the price adjustment factor the parent's
    /// history requires. Frequently missing from vendor adjusted closes, which
    /// is one of the reasons this crate does not trust them.
    SpinOff { factor: Decimal },
}

/// A corporate action attached to a security and a date.
///
/// `effective` is the date from which the action changes the price
/// relationship. For dividends that is the ex-dividend date, not the pay date,
/// because the price drops when the buyer stops being entitled to the payment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionRecord {
    pub asset: AssetKey,
    pub effective: Date,
    pub action: CorporateAction,
    /// Which provider reported this. Vendors disagree about corporate actions
    /// more than they disagree about prices, so provenance is not optional.
    pub source: String,
}

impl ActionRecord {
    /// The multiplicative factor this action applies to prices before it.
    ///
    /// Returns `None` for actions that do not adjust the price series on their
    /// own, such as stock dividends, which are handled as splits by most
    /// vendors and need the share-count change rather than a price factor.
    pub fn price_factor(&self, reference_close: Decimal) -> Option<Decimal> {
        match &self.action {
            CorporateAction::Split { ratio } if *ratio > Decimal::ZERO => {
                Some(Decimal::ONE / ratio)
            }
            CorporateAction::Split { .. } => None,

            // A cash dividend reduces the prior series by the proportion of the
            // price it represents. Needs a reference price, which is why this
            // takes one rather than being a pure property of the action.
            CorporateAction::Dividend {
                amount,
                kind: DividendKind::Cash | DividendKind::Special,
            } if reference_close > Decimal::ZERO => {
                Some((reference_close - amount) / reference_close)
            }
            CorporateAction::Dividend { .. } => None,

            CorporateAction::SpinOff { factor } if *factor > Decimal::ZERO => Some(*factor),
            CorporateAction::SpinOff { .. } => None,
        }
    }
}

/// Why a security stopped trading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DelistingReason {
    Merger,
    Acquired,
    Bankruptcy,
    /// Removed for failing an exchange listing requirement. Usually bad.
    ExchangeRule,
    /// Voluntarily withdrawn by the issuer.
    Voluntary,
    Unknown,
}

/// Which market a security was listed on when it delisted.
///
/// Only the distinction that changes the imputation is modelled. The published
/// corrections differ by roughly 25 percentage points between these two groups,
/// which is larger than most effects anyone is trying to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Listing {
    NyseAmex,
    Nasdaq,
    Other,
}

/// A published convention for a delisting return that was never observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Convention {
    /// -30%. Shumway, T. (1997), "The Delisting Bias in CRSP Data",
    /// Journal of Finance 52(1):327-340. Performance delistings, NYSE/AMEX.
    Shumway1997NyseAmex,
    /// -55%. Shumway, T. and Warther, V. (1999), "The Delisting Bias in CRSP's
    /// Nasdaq Data and Its Implications for the Size Effect", Journal of
    /// Finance 54(6):2361-2379. Performance delistings, Nasdaq.
    ///
    /// Larger than the NYSE/AMEX figure because the Nasdaq bias is 4.7 times
    /// as severe. Applying this correction eliminates the Nasdaq size effect.
    ShumwayWarther1999Nasdaq,
    /// Zero. CRSP's own convention when a merger's final consideration is
    /// unknown or paid through distributions, on the reasoning that the last
    /// traded price already reflected the announced deal.
    CrspMergerUnknown,
}

impl Convention {
    /// The return this convention assumes, as a fraction.
    pub fn value(self) -> Decimal {
        match self {
            Convention::Shumway1997NyseAmex => Decimal::new(-30, 2),
            Convention::ShumwayWarther1999Nasdaq => Decimal::new(-55, 2),
            Convention::CrspMergerUnknown => Decimal::ZERO,
        }
    }
}

/// What a holder ended up with, and how confidently we know it.
///
/// Three states rather than an `Option`, because an imputed value that cannot
/// be distinguished from an observed one is the same class of problem as a
/// vendor's adjusted close. Any result built on imputed values must be able to
/// say so and to vary the assumption.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TerminalValue {
    /// Known from deal terms or a final settlement.
    Observed(Decimal),
    /// Assumed from a published convention.
    Imputed {
        value: Decimal,
        convention: Convention,
    },
    /// Neither observed nor imputable. Must be reported, never silently
    /// treated as zero.
    Unknown,
}

impl TerminalValue {
    pub fn value(&self) -> Option<Decimal> {
        match self {
            TerminalValue::Observed(v) => Some(*v),
            TerminalValue::Imputed { value, .. } => Some(*value),
            TerminalValue::Unknown => None,
        }
    }

    pub fn is_observed(&self) -> bool {
        matches!(self, TerminalValue::Observed(_))
    }
}

impl DelistingReason {
    /// Whether this delisting is the tractable kind.
    ///
    /// Mergers and acquisitions have a terminal value knowable from deal terms,
    /// and CRSP records one for over 99% of them. Performance delistings do
    /// not: 99.8% of Nasdaq performance-delisting returns are missing. The
    /// missing data is concentrated almost entirely in the disasters, which is
    /// why the two classes cannot share a convention.
    pub fn is_performance_related(&self) -> bool {
        matches!(
            self,
            DelistingReason::Bankruptcy
                | DelistingReason::ExchangeRule
                // Conservative. An unclassified delisting treated as a merger
                // would bias results optimistic, which is the direction this
                // whole correction exists to avoid.
                | DelistingReason::Unknown
        )
    }
}

/// Choose the published convention for an unobserved delisting return.
pub fn convention_for(reason: DelistingReason, listing: Listing) -> Convention {
    if !reason.is_performance_related() {
        return Convention::CrspMergerUnknown;
    }
    match listing {
        Listing::Nasdaq => Convention::ShumwayWarther1999Nasdaq,
        // `Other` takes the milder correction rather than the harsher one,
        // since applying the Nasdaq figure to an unidentified venue would
        // overstate a correction as readily as omitting it understates one.
        Listing::NyseAmex | Listing::Other => Convention::Shumway1997NyseAmex,
    }
}

/// A security leaving the market.
///
/// # Why the terminal value matters more than it looks
///
/// The most consequential known bias in equity research comes from omitting the
/// return between the last quoted price and whatever holders actually received.
/// Shumway (1997) found this bias in Nasdaq data to be 4.7 times the documented
/// NYSE/AMEX bias, and correcting it left no evidence the Nasdaq size effect had
/// ever existed.
///
/// Retail data providers do not publish a delisting return. The row simply
/// stops, and a backtest reading that as "position closed at the last price"
/// systematically overstates returns on exactly the securities that did worst.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delisting {
    pub asset: AssetKey,
    pub date: Date,
    pub reason: DelistingReason,
    pub listing: Listing,
    pub terminal: TerminalValue,
    pub source: String,
}

impl Delisting {
    /// Fill an unknown terminal value with the applicable published convention.
    ///
    /// Leaves observed and already-imputed values untouched.
    pub fn imputed(mut self) -> Self {
        if matches!(self.terminal, TerminalValue::Unknown) {
            let convention = convention_for(self.reason, self.listing);
            self.terminal = TerminalValue::Imputed {
                value: convention.value(),
                convention,
            };
        }
        self
    }
}

/// How many delistings rest on an assumption rather than an observation.
///
/// Reported alongside any result covering a period with delistings, so a reader
/// knows how much of the outcome is assumed. Pairs with a sensitivity check: if
/// a headline result flips when the assumed return moves from -30% to -100%,
/// the result was never real.
pub fn imputed_count(delistings: &[Delisting]) -> usize {
    delistings
        .iter()
        .filter(|d| !d.terminal.is_observed())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromPrimitive;

    fn dec(v: f64) -> Decimal {
        Decimal::from_f64(v).expect("test literal is representable")
    }

    fn on(effective: Date, action: CorporateAction) -> ActionRecord {
        ActionRecord {
            asset: AssetKey::ticker_only("TEST"),
            effective,
            action,
            source: "test".into(),
        }
    }

    fn date(y: i16, m: i8, d: i8) -> Date {
        Date::new(y, m, d).expect("valid test date")
    }

    #[test]
    fn a_two_for_one_split_halves_prior_prices() {
        let record = on(date(2026, 6, 1), CorporateAction::Split { ratio: dec(2.0) });
        assert_eq!(record.price_factor(dec(100.0)), Some(dec(0.5)));
    }

    #[test]
    fn a_reverse_split_raises_prior_prices() {
        let record = on(date(2026, 6, 1), CorporateAction::Split { ratio: dec(0.1) });
        assert_eq!(record.price_factor(dec(100.0)), Some(dec(10.0)));
    }

    #[test]
    fn a_zero_ratio_split_yields_no_factor_rather_than_dividing_by_zero() {
        let record = on(
            date(2026, 6, 1),
            CorporateAction::Split {
                ratio: Decimal::ZERO,
            },
        );
        assert_eq!(record.price_factor(dec(100.0)), None);
    }

    #[test]
    fn a_cash_dividend_scales_by_its_share_of_the_price() {
        // $1 dividend against a $100 close leaves 99% of the prior series.
        let record = on(
            date(2026, 6, 1),
            CorporateAction::Dividend {
                amount: dec(1.0),
                kind: DividendKind::Cash,
            },
        );
        assert_eq!(record.price_factor(dec(100.0)), Some(dec(0.99)));
    }

    #[test]
    fn a_dividend_against_a_zero_reference_price_yields_no_factor() {
        let record = on(
            date(2026, 6, 1),
            CorporateAction::Dividend {
                amount: dec(1.0),
                kind: DividendKind::Cash,
            },
        );
        assert_eq!(record.price_factor(Decimal::ZERO), None);
    }

    #[test]
    fn a_spin_off_carries_its_factor_directly() {
        // The Realty Income / Orion factor a documented vendor never applied.
        let record = on(
            date(2026, 6, 1),
            CorporateAction::SpinOff { factor: dec(1.032) },
        );
        assert_eq!(record.price_factor(dec(100.0)), Some(dec(1.032)));
    }

    #[test]
    fn delistings_resting_on_assumptions_are_countable() {
        let known = Delisting {
            asset: AssetKey::ticker_only("GONE"),
            date: date(2026, 6, 1),
            reason: DelistingReason::Bankruptcy,
            listing: Listing::Nasdaq,
            terminal: TerminalValue::Observed(dec(-0.92)),
            source: "test".into(),
        };
        let mut unknown = known.clone();
        unknown.terminal = TerminalValue::Unknown;

        assert!(known.terminal.is_observed());
        assert!(!unknown.terminal.is_observed());
        assert_eq!(imputed_count(&[known, unknown]), 1);
    }

    fn delisting(reason: DelistingReason, listing: Listing) -> Delisting {
        Delisting {
            asset: AssetKey::ticker_only("GONE"),
            date: date(2026, 6, 1),
            reason,
            listing,
            terminal: TerminalValue::Unknown,
            source: "test".into(),
        }
    }

    #[test]
    fn a_nasdaq_performance_delisting_takes_the_harsher_correction() {
        let d = delisting(DelistingReason::Bankruptcy, Listing::Nasdaq).imputed();
        assert_eq!(d.terminal.value(), Some(dec(-0.55)));
        assert!(matches!(
            d.terminal,
            TerminalValue::Imputed {
                convention: Convention::ShumwayWarther1999Nasdaq,
                ..
            }
        ));
    }

    #[test]
    fn an_nyse_performance_delisting_takes_the_milder_one() {
        let d = delisting(DelistingReason::ExchangeRule, Listing::NyseAmex).imputed();
        assert_eq!(d.terminal.value(), Some(dec(-0.30)));
    }

    #[test]
    fn a_merger_with_unknown_terms_assumes_zero_not_a_loss() {
        // The last traded price already reflected the announced deal, so a
        // negative correction here would introduce a pessimistic bias while
        // correcting an optimistic one.
        let d = delisting(DelistingReason::Merger, Listing::Nasdaq).imputed();
        assert_eq!(d.terminal.value(), Some(Decimal::ZERO));
        assert!(matches!(
            d.terminal,
            TerminalValue::Imputed {
                convention: Convention::CrspMergerUnknown,
                ..
            }
        ));
    }

    #[test]
    fn an_unclassified_delisting_is_treated_as_performance_related() {
        // Conservative on purpose. Guessing "merger" would bias optimistic,
        // which is the direction this correction exists to remove.
        let d = delisting(DelistingReason::Unknown, Listing::Nasdaq).imputed();
        assert_eq!(d.terminal.value(), Some(dec(-0.55)));
    }

    #[test]
    fn an_observed_value_is_never_overwritten_by_a_convention() {
        let mut d = delisting(DelistingReason::Bankruptcy, Listing::Nasdaq);
        d.terminal = TerminalValue::Observed(dec(-0.92));
        let after = d.imputed();
        assert_eq!(after.terminal, TerminalValue::Observed(dec(-0.92)));
    }

    #[test]
    fn imputed_values_stay_distinguishable_from_observed_ones() {
        let imputed = delisting(DelistingReason::Bankruptcy, Listing::Nasdaq).imputed();
        assert!(!imputed.terminal.is_observed());
        assert_eq!(imputed_count(&[imputed]), 1);
    }
}
