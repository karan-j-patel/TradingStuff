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

/// A security leaving the market.
///
/// # Why `final_return` exists and is usually `None`
///
/// The most consequential known bias in equity research comes from omitting the
/// return earned between the last quoted price and whatever holders actually
/// received on delisting. Shumway (1997) found this bias in Nasdaq data to be
/// 4.7 times the already-documented bias in NYSE and AMEX data, and that
/// correcting it left no evidence the Nasdaq size effect had ever existed.
///
/// CRSP publishes a delisting return. Retail data APIs do not. The row simply
/// stops, and a backtest that reads that as "position closed at the last price"
/// systematically overstates returns on exactly the securities that did worst.
///
/// This field is `Option` so the absence is explicit rather than invisible. Any
/// result computed over a period containing delistings with `None` here must
/// say so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delisting {
    pub asset: AssetKey,
    pub date: Date,
    pub reason: DelistingReason,
    pub final_return: Option<Decimal>,
    pub source: String,
}

impl Delisting {
    /// True when this delisting can be modelled without assuming a return.
    pub fn is_return_known(&self) -> bool {
        self.final_return.is_some()
    }
}

/// How many delistings in a set lack a final return.
///
/// Intended to be reported alongside any result covering a period with
/// delistings, so the reader knows how much of the outcome rests on an
/// assumption rather than on data.
pub fn unknown_return_count(delistings: &[Delisting]) -> usize {
    delistings.iter().filter(|d| !d.is_return_known()).count()
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
    fn delistings_without_a_final_return_are_countable() {
        let known = Delisting {
            asset: AssetKey::ticker_only("GONE"),
            date: date(2026, 6, 1),
            reason: DelistingReason::Bankruptcy,
            final_return: Some(dec(-0.55)),
            source: "test".into(),
        };
        let mut unknown = known.clone();
        unknown.final_return = None;

        assert!(known.is_return_known());
        assert!(!unknown.is_return_known());
        assert_eq!(unknown_return_count(&[known, unknown]), 1);
    }
}
