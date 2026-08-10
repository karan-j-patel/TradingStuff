//! Prices on a vendor's adjusted basis, and the barrier that keeps them there.
//!
//! # Why this type exists rather than a flag on [`PriceBar`]
//!
//! The probe on 2026-08-09 measured what Sharadar actually ships. `open`,
//! `high`, `low` and `close` are adjusted for splits and stock dividends, with
//! the vendor's own rounding already baked in; only `closeunadj` is the price a
//! trade happened at. On a 3-for-1 split the adjusted close missed the traded
//! close by a residual of `0.001`, and the implied factor `close/closeunadj`
//! came out different on every row, so the traded open cannot be recovered by
//! dividing. The verbatim record is at
//! `ContinuationDocs/2026-08-09-probe-output.md`, which is gitignored, and the
//! regimes it measured are preserved as a synthetic twin in the tests.
//!
//! A boolean field saying "these are adjusted" would be correct and useless. It
//! is checked by whoever remembers to check it, and the failure when nobody does
//! is a plausible wrong number rather than a crash. So adjustment status lives in
//! the type instead, the same way [`marketdata::replay::Observed`] puts lookahead
//! safety in the type rather than in a convention.
//!
//! [`marketdata::replay::Observed`]: https://docs.rs/marketdata

use jiff::civil::Date;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::schema::{AssetKey, CloseKind, Reject, SessionScope, ValidationReport};

/// One security's prices for one day, on the vendor's adjusted basis.
///
/// # The barrier
///
/// There is no accessor here returning an as-traded open, high, or low, and
/// there is not going to be one, because no such number exists on this type.
/// The vendor did not ship them and they cannot be recovered from what it did
/// ship. A function that computes an as-traded return therefore cannot be
/// written against an `AdjustedBar`: the mistake shows up as a missing method
/// at compile time rather than as a return series that is quietly wrong.
///
/// ```compile_fail
/// use ingest::AdjustedBar;
///
/// fn traded_open(bar: &AdjustedBar) -> rust_decimal::Decimal {
///     // No such method. That absence is the entire purpose of the type.
///     bar.open_as_traded()
/// }
/// ```
///
/// Exactly one missing method, deliberately. A doctest naming two of them
/// would still fail to compile after one was added, and would therefore go on
/// passing while the barrier it guards had a hole in it.
///
/// The separation is by type and not by discipline, so an `AdjustedBar` also
/// cannot be handed to code written for genuinely as-traded prices.
///
/// ```compile_fail
/// use ingest::{AdjustedBar, PriceBar};
///
/// fn as_traded_return(bar: &PriceBar) -> rust_decimal::Decimal {
///     bar.close - bar.open
/// }
///
/// fn call(adjusted: &AdjustedBar) -> rust_decimal::Decimal {
///     // An adjusted bar is not a PriceBar and never converts to one.
///     as_traded_return(adjusted)
/// }
/// ```
///
/// # What [`close_unadjusted`] is for
///
/// It is the one exact, as-traded number on the row. It is a named field rather
/// than an accessor called `close_as_traded` so that the asymmetry is visible:
/// the close has an as-traded form and the other three do not.
///
/// [`close_unadjusted`]: AdjustedBar::close_unadjusted
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdjustedBar {
    pub asset: AssetKey,
    pub date: Date,

    /// Adjusted for splits and stock dividends, exactly as the vendor shipped
    /// them, rounding included. Not adjusted for cash dividends or spinoffs.
    ///
    /// Stored as shipped rather than recomputed. Undoing the vendor's rounding
    /// is not possible and inventing a cleaner number would be a fabrication
    /// that reads like data.
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,

    /// The official closing price with no adjustment of any kind. As traded,
    /// and exact.
    pub close_unadjusted: Decimal,

    pub session: SessionScope,
    pub close_kind: CloseKind,
}

impl AdjustedBar {
    /// Check this bar against every rule, returning the first violation.
    ///
    /// The rules are [`PriceBar::validate`]'s, plus one for `close_unadjusted`.
    /// Strict on `open` as well as `close`: both carry the vendor's rounding
    /// equally, so forgiving one and not the other was an asymmetry with
    /// nothing behind it. If rounding ever does push a last-digit open outside
    /// the band, that is a finding worth surfacing loudly rather than one worth
    /// pre-forgiving.
    ///
    /// [`PriceBar::validate`]: crate::schema::PriceBar::validate
    pub fn validate(&self) -> Result<(), Reject> {
        if self.asset.ticker.trim().is_empty() {
            return Err(Reject::EmptyTicker);
        }

        for (field, value) in [
            ("open", self.open),
            ("high", self.high),
            ("low", self.low),
            ("close", self.close),
            ("close_unadjusted", self.close_unadjusted),
        ] {
            if value <= Decimal::ZERO {
                return Err(Reject::NonPositivePrice { field, value });
            }
        }

        if self.volume < Decimal::ZERO {
            return Err(Reject::NegativeVolume { value: self.volume });
        }

        if self.high < self.low {
            return Err(Reject::HighBelowLow {
                high: self.high,
                low: self.low,
            });
        }

        for (field, value) in [("open", self.open), ("close", self.close)] {
            if value > self.high || value < self.low {
                return Err(Reject::OutsideRange {
                    field,
                    value,
                    low: self.low,
                    high: self.high,
                });
            }
        }

        Ok(())
    }
}

/// Partition a batch of adjusted bars into accepted and rejected.
///
/// Sibling of [`crate::schema::validate_batch`]. Rejections are counted by
/// cause rather than dropped, for the same reason: a pipeline that silently
/// discards rows produces a result that looks fine and is quietly wrong.
pub fn validate_adjusted(
    bars: impl IntoIterator<Item = AdjustedBar>,
) -> ValidationReport<AdjustedBar> {
    let mut report = ValidationReport::default();
    for bar in bars {
        match bar.validate() {
            Ok(()) => report.accepted.push(bar),
            Err(reason) => report.rejected.push((bar, reason)),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{AssetKey, CloseKind, SessionScope};
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("test literal is an exact decimal")
    }

    /// A well-formed bar. Every P3 case starts here and breaks one rule, so a
    /// failure names the rule rather than the fixture.
    fn bar() -> AdjustedBar {
        AdjustedBar {
            asset: AssetKey::ticker_only("TEST"),
            date: Date::from_str("2022-08-22").expect("valid date"),
            open: dec("10.10"),
            high: dec("10.90"),
            low: dec("9.90"),
            close: dec("10.75"),
            volume: dec("1000"),
            close_unadjusted: dec("43.00"),
            session: SessionScope::RegularHours,
            close_kind: CloseKind::ClosingAuction,
        }
    }

    #[test]
    fn p3_a_well_formed_bar_is_accepted() {
        assert_eq!(bar().validate(), Ok(()));
    }

    #[test]
    fn p3_every_price_must_be_positive() {
        for field in ["open", "high", "low", "close", "close_unadjusted"] {
            let mut subject = bar();
            match field {
                "open" => subject.open = Decimal::ZERO,
                "high" => subject.high = Decimal::ZERO,
                "low" => subject.low = Decimal::ZERO,
                "close" => subject.close = Decimal::ZERO,
                _ => subject.close_unadjusted = Decimal::ZERO,
            }
            assert!(
                matches!(subject.validate(), Err(Reject::NonPositivePrice { .. })),
                "{field} at zero should have been refused"
            );
        }
    }

    /// `close_unadjusted` gets its own case because it is the field this type
    /// added, and the one a validator written from `PriceBar` would forget.
    #[test]
    fn p3_a_missing_as_traded_close_is_refused() {
        let mut subject = bar();
        subject.close_unadjusted = Decimal::ZERO;
        assert_eq!(
            subject.validate(),
            Err(Reject::NonPositivePrice {
                field: "close_unadjusted",
                value: Decimal::ZERO
            })
        );
    }

    #[test]
    fn p3_high_below_low_is_refused() {
        let mut subject = bar();
        subject.high = dec("9.00");
        subject.low = dec("11.00");
        assert!(matches!(
            subject.validate(),
            Err(Reject::HighBelowLow { .. })
        ));
    }

    #[test]
    fn p3_a_close_outside_the_range_is_refused() {
        let mut subject = bar();
        subject.close = dec("12.00");
        assert!(matches!(
            subject.validate(),
            Err(Reject::OutsideRange { field: "close", .. })
        ));
    }

    /// Strict both ways. The vendor's rounding argument applies to `close` as
    /// much as to `open`, and `close` is checked, so pre-forgiving `open` was
    /// an asymmetry with nothing behind it. If rounding ever does push a
    /// last-digit open outside the band, a refusal surfaces it as a finding.
    #[test]
    fn p3_an_open_outside_the_range_is_refused() {
        let mut subject = bar();
        subject.open = dec("12.00");
        assert_eq!(
            subject.validate(),
            Err(Reject::OutsideRange {
                field: "open",
                value: dec("12.00"),
                low: dec("9.90"),
                high: dec("10.90"),
            })
        );
    }

    #[test]
    fn p3_negative_volume_is_refused_and_zero_is_not() {
        let mut subject = bar();
        subject.volume = dec("-1");
        assert!(matches!(
            subject.validate(),
            Err(Reject::NegativeVolume { .. })
        ));

        subject.volume = Decimal::ZERO;
        assert_eq!(subject.validate(), Ok(()), "a halted day is a real day");
    }

    #[test]
    fn p3_an_empty_ticker_is_refused() {
        let mut subject = bar();
        subject.asset = AssetKey::ticker_only("   ");
        assert_eq!(subject.validate(), Err(Reject::EmptyTicker));
    }

    #[test]
    fn p3_a_batch_partitions_by_cause() {
        let mut bad = bar();
        bad.close_unadjusted = Decimal::ZERO;
        let report = validate_adjusted([bar(), bad]);
        assert_eq!(report.accepted.len(), 1);
        assert_eq!(report.rejected.len(), 1);
    }
}
