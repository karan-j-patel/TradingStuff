//! Price adjustment, computed here rather than taken from a vendor.
//!
//! # Why there is a mode rather than one answer
//!
//! "Adjusted close" names at least four different numbers, and vendors ship
//! whichever they chose without saying which. QuantConnect's Lean exposes four
//! separate normalization modes for exactly this reason. Collapsing them into a
//! single field means every consumer silently inherits a convention nobody
//! chose, and return series computed from it cannot be compared across sources.
//!
//! Adjustment here is backward-looking, which is the convention research
//! expects. Today's price is unchanged and history is restated so that a return
//! series is continuous across splits and distributions. This means the whole
//! series changes whenever a new action occurs, which is correct and is also
//! why an adjusted series must never be cached without its action set.

use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::actions::ActionRecord;
use crate::schema::PriceBar;

/// Which convention to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdjustmentMode {
    /// Prices exactly as traded. What execution uses, always, because it is the
    /// only series containing prices anyone could actually transact at.
    Raw,

    /// Splits only. Keeps the series continuous through share-count changes
    /// without treating distributions as return.
    SplitOnly,

    /// Splits and distributions, giving a price-return series. Distributions
    /// are treated as reducing the prior price rather than as reinvested.
    SplitAndDividend,

    /// Splits and distributions with dividends reinvested at the ex-date close,
    /// giving a total-return series. What most factor research means by
    /// "returns", and what a buy-and-hold benchmark must use to be honest.
    TotalReturn,
}

impl AdjustmentMode {
    fn applies_distributions(&self) -> bool {
        matches!(
            self,
            AdjustmentMode::SplitAndDividend | AdjustmentMode::TotalReturn
        )
    }

    fn applies_splits(&self) -> bool {
        !matches!(self, AdjustmentMode::Raw)
    }
}

/// A price series with its adjustment convention attached.
///
/// The mode travels with the data deliberately. A bare `Vec<Decimal>` of
/// adjusted prices is exactly the artefact that caused the vendor divergence
/// this module exists to avoid.
#[derive(Debug, Clone, PartialEq)]
pub struct AdjustedSeries {
    pub mode: AdjustmentMode,
    /// One entry per input bar, in ascending date order.
    pub points: Vec<AdjustedPoint>,
    /// Actions that were applied. Retained so a series can be audited against
    /// the events that produced it.
    pub applied: Vec<ActionRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdjustedPoint {
    pub date: Date,
    /// Price as traded on the day. Never modified.
    pub raw_close: Decimal,
    /// Price restated onto today's basis.
    pub adjusted_close: Decimal,
    /// Cumulative factor applied to this bar. 1.0 for the most recent bar,
    /// smaller going back through splits, and useful for auditing.
    pub factor: Decimal,
}

/// Restate a single asset's history onto the basis of its most recent bar.
///
/// `bars` and `actions` must describe one asset. Bars are sorted here so
/// callers cannot pass an unordered series and get a meaningless result.
///
/// Actions are applied to every bar strictly before their effective date, which
/// is the correct boundary. A split effective on the 5th changes the 4th and
/// everything prior, and leaves the 5th alone, because the 5th already trades
/// on the new basis.
pub fn adjust(bars: &[PriceBar], actions: &[ActionRecord], mode: AdjustmentMode) -> AdjustedSeries {
    let mut sorted: Vec<&PriceBar> = bars.iter().collect();
    sorted.sort_by_key(|bar| bar.date);

    let relevant: Vec<ActionRecord> = actions
        .iter()
        .filter(|record| match &record.action {
            crate::actions::CorporateAction::Split { .. } => mode.applies_splits(),
            crate::actions::CorporateAction::Dividend { .. } => mode.applies_distributions(),
            crate::actions::CorporateAction::SpinOff { .. } => mode.applies_distributions(),
        })
        .cloned()
        .collect();

    // Walk backwards accumulating factors. Going forwards would require knowing
    // the final factor in advance, since adjustment is relative to the end.
    let mut factor = Decimal::ONE;
    let mut points: Vec<AdjustedPoint> = Vec::with_capacity(sorted.len());

    for bar in sorted.iter().rev() {
        points.push(AdjustedPoint {
            date: bar.date,
            raw_close: bar.close,
            adjusted_close: bar.close * factor,
            factor,
        });

        // Any action effective on or before the NEXT bar back, but after this
        // one, applies to everything from that point backwards. Evaluated after
        // pushing so the current bar keeps the factor it was quoted under.
        for record in &relevant {
            if record.effective == bar.date
                && let Some(step) = record.price_factor(bar.close)
            {
                factor *= step;
            }
        }
    }

    points.reverse();
    AdjustedSeries {
        mode,
        points,
        applied: relevant,
    }
}

/// A day where the adjustment factor moved without an action to explain it.
///
/// This is the invariant that replaces trusting a vendor's adjusted close. Our
/// factor should step only where our action set says it should. A step without
/// an action means the actions are incomplete, and a missing action is exactly
/// how a spin-off or a dropped dividend corrupts a return series silently.
#[derive(Debug, Clone, PartialEq)]
pub struct UnexplainedStep {
    pub date: Date,
    pub previous_factor: Decimal,
    pub factor: Decimal,
}

/// Verify that every change in the adjustment factor has a corresponding action.
///
/// Returns the steps that do not. An empty result means the series and the
/// action set agree.
pub fn unexplained_steps(series: &AdjustedSeries) -> Vec<UnexplainedStep> {
    let mut unexplained = Vec::new();
    for pair in series.points.windows(2) {
        let (earlier, later) = (&pair[0], &pair[1]);
        if earlier.factor == later.factor {
            continue;
        }
        // The action sits on the LATER bar, not the earlier one. A split
        // effective on the 2nd is what makes the 1st's factor differ from the
        // 2nd's, because the 2nd already trades on the new basis.
        let explained = series
            .applied
            .iter()
            .any(|record| record.effective == later.date);
        if !explained {
            unexplained.push(UnexplainedStep {
                date: later.date,
                previous_factor: earlier.factor,
                factor: later.factor,
            });
        }
    }
    unexplained
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{CorporateAction, DividendKind};
    use crate::schema::{AssetKey, CloseKind, SessionScope};
    use rust_decimal::prelude::FromPrimitive;

    fn dec(v: f64) -> Decimal {
        Decimal::from_f64(v).expect("test literal is representable")
    }

    fn date(y: i16, m: i8, d: i8) -> Date {
        Date::new(y, m, d).expect("valid test date")
    }

    fn bar(day: i8, close: f64) -> PriceBar {
        PriceBar {
            asset: AssetKey::ticker_only("TEST"),
            date: date(2026, 6, day),
            open: dec(close),
            high: dec(close),
            low: dec(close),
            close: dec(close),
            volume: dec(1_000.0),
            session: SessionScope::RegularHours,
            close_kind: CloseKind::ClosingAuction,
        }
    }

    fn split(day: i8, ratio: f64) -> ActionRecord {
        ActionRecord {
            asset: AssetKey::ticker_only("TEST"),
            effective: date(2026, 6, day),
            action: CorporateAction::Split { ratio: dec(ratio) },
            source: "test".into(),
        }
    }

    #[test]
    fn raw_mode_changes_nothing() {
        let bars = [bar(1, 100.0), bar(2, 50.0)];
        let series = adjust(&bars, &[split(2, 2.0)], AdjustmentMode::Raw);
        for point in &series.points {
            assert_eq!(point.adjusted_close, point.raw_close);
            assert_eq!(point.factor, Decimal::ONE);
        }
    }

    #[test]
    fn a_two_for_one_split_halves_prior_history() {
        // Traded at 100 on the 1st, splits 2:1 effective the 2nd, trades at 50.
        // Restated onto today's basis the 1st becomes 50, so the return across
        // the split is zero rather than minus fifty percent.
        let bars = [bar(1, 100.0), bar(2, 50.0)];
        let series = adjust(&bars, &[split(2, 2.0)], AdjustmentMode::SplitOnly);

        assert_eq!(series.points[0].adjusted_close, dec(50.0));
        assert_eq!(series.points[1].adjusted_close, dec(50.0));
        assert_eq!(
            series.points[1].factor,
            Decimal::ONE,
            "latest bar is the basis"
        );
    }

    #[test]
    fn the_bar_on_the_effective_date_is_not_adjusted() {
        // It already trades on the new basis. Adjusting it would double-count.
        let bars = [bar(1, 100.0), bar(2, 50.0), bar(3, 51.0)];
        let series = adjust(&bars, &[split(2, 2.0)], AdjustmentMode::SplitOnly);
        assert_eq!(series.points[1].adjusted_close, dec(50.0));
        assert_eq!(series.points[2].adjusted_close, dec(51.0));
    }

    #[test]
    fn split_only_mode_ignores_dividends() {
        let dividend = ActionRecord {
            asset: AssetKey::ticker_only("TEST"),
            effective: date(2026, 6, 2),
            action: CorporateAction::Dividend {
                amount: dec(1.0),
                kind: DividendKind::Cash,
            },
            source: "test".into(),
        };
        let bars = [bar(1, 100.0), bar(2, 100.0)];
        let series = adjust(&bars, &[dividend], AdjustmentMode::SplitOnly);
        assert_eq!(series.points[0].adjusted_close, dec(100.0));
    }

    #[test]
    fn price_return_mode_applies_dividends() {
        let dividend = ActionRecord {
            asset: AssetKey::ticker_only("TEST"),
            effective: date(2026, 6, 2),
            action: CorporateAction::Dividend {
                amount: dec(1.0),
                kind: DividendKind::Cash,
            },
            source: "test".into(),
        };
        let bars = [bar(1, 100.0), bar(2, 100.0)];
        let series = adjust(&bars, &[dividend], AdjustmentMode::SplitAndDividend);
        assert_eq!(series.points[0].adjusted_close, dec(99.0));
    }

    #[test]
    fn successive_splits_compound() {
        // 2:1 then 2:1 leaves the earliest bar at a quarter.
        let bars = [bar(1, 100.0), bar(2, 50.0), bar(3, 25.0)];
        let series = adjust(
            &bars,
            &[split(2, 2.0), split(3, 2.0)],
            AdjustmentMode::SplitOnly,
        );
        assert_eq!(series.points[0].adjusted_close, dec(25.0));
        assert_eq!(series.points[1].adjusted_close, dec(25.0));
        assert_eq!(series.points[2].adjusted_close, dec(25.0));
    }

    #[test]
    fn unsorted_bars_are_sorted_before_adjustment() {
        let ordered = [bar(1, 100.0), bar(2, 50.0)];
        let shuffled = [bar(2, 50.0), bar(1, 100.0)];
        let actions = [split(2, 2.0)];
        assert_eq!(
            adjust(&ordered, &actions, AdjustmentMode::SplitOnly),
            adjust(&shuffled, &actions, AdjustmentMode::SplitOnly)
        );
    }

    #[test]
    fn an_explained_series_has_no_unexplained_steps() {
        let bars = [bar(1, 100.0), bar(2, 50.0)];
        let series = adjust(&bars, &[split(2, 2.0)], AdjustmentMode::SplitOnly);
        assert!(unexplained_steps(&series).is_empty());
    }

    #[test]
    fn a_missing_action_is_the_thing_this_catches() {
        // The failure mode from the vendor audit: a spin-off factor that was
        // never applied. Here we simulate the inverse, a series adjusted by a
        // factor with no action in the set to justify it.
        let bars = [bar(1, 100.0), bar(2, 50.0)];
        let mut series = adjust(&bars, &[split(2, 2.0)], AdjustmentMode::SplitOnly);
        series.applied.clear(); // pretend the action set was incomplete

        let steps = unexplained_steps(&series);
        assert_eq!(
            steps.len(),
            1,
            "a factor step with no action must be visible"
        );
        assert_eq!(steps[0].date, date(2026, 6, 2));
    }
}
