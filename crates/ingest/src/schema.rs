//! Vendor-neutral market data records and their validation rules.
//!
//! Every data provider (Sharadar, SEC XBRL, Alpaca, ...) parses into the types
//! here. Nothing downstream should ever see a vendor-specific row, because the
//! product ships bring-your-own-key and a buyer may hold a different vendor's
//! subscription entirely.
//!
//! # Notes for a Rust newcomer coming from Python
//!
//! - A `struct` is roughly a Python class with no methods and no inheritance.
//! - An `enum` is the thing Python lacks. It is not a list of constants — each
//!   variant can carry its own differently-shaped data, and `match` will not
//!   compile unless every variant is handled. Adding a variant later turns
//!   "silent bug" into "build error", which is the whole reason to use one.
//! - `#[derive(...)]` writes boilerplate implementations at compile time. It is
//!   closest to a class decorator, except it runs before the program exists.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use jiff::civil::Date;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// A stable identifier for a security, issued by whoever supplied the data.
///
/// Tickers are display labels, not identities: `FB` became `META`, and retired
/// tickers get reassigned to unrelated companies years later. Keyed on the
/// ticker string alone, a reassignment silently welds two companies' price
/// histories into one series — a bug that is invisible in aggregate and
/// unfixable after the fact.
///
/// Each provider has its own permanent identifier, hence an enum rather than a
/// single integer: the variants are genuinely different namespaces and must not
/// be compared across providers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PermanentId {
    /// Sharadar's `permaticker` — stable across ticker changes.
    Sharadar(u64),
    /// SEC Central Index Key, stable for the life of the filer.
    SecCik(u64),
    /// Alpaca's asset UUID, stored as a string to avoid a uuid dependency here.
    Alpaca(String),
}

/// How a security is referred to, combining the unstable label with the stable
/// identity when the provider offers one.
///
/// `permanent` is an `Option` because not every source supplies one. `Option<T>`
/// is Rust's replacement for `None`-as-a-sentinel: the compiler forces you to
/// handle the missing case before you can touch the value, so there is no
/// equivalent of `AttributeError: 'NoneType' object has no attribute ...`.
///
/// # Why equality is hand-written
///
/// `#[derive(PartialEq, Hash)]` would compare *both* fields, which defeats the
/// entire purpose of carrying a permanent ID: after Facebook renamed to Meta,
/// `{ticker: "FB", permanent: Sharadar(199059)}` and
/// `{ticker: "META", permanent: Sharadar(199059)}` would be unequal and hash
/// differently, splitting one company's history in every map, join, and ledger
/// they touch. The permanent ID exists precisely so the ticker can change.
///
/// So identity is: the permanent ID when present, the ticker otherwise. A key
/// that has an ID and one that does not are **never** equal, even with matching
/// tickers — that pairing cannot be established as the same security without
/// assuming the ticker was never reassigned, which is the assumption this whole
/// type exists to avoid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetKey {
    /// Display label. Time-varying — never use it as a join key.
    pub ticker: String,
    /// Stable identity, when the provider supplies one.
    pub permanent: Option<PermanentId>,
}

impl PartialEq for AssetKey {
    fn eq(&self, other: &Self) -> bool {
        match (&self.permanent, &other.permanent) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.ticker == other.ticker,
            // One side is identified and the other is not. Refusing to equate
            // them is the conservative choice: a wrong merge corrupts history
            // silently, while a wrong split is visible as missing data.
            _ => false,
        }
    }
}

impl Eq for AssetKey {}

impl Hash for AssetKey {
    /// Must agree with `PartialEq`: equal keys must hash equally, or `HashMap`
    /// lookups miss entries that are present.
    ///
    /// The leading discriminant byte keeps identified and unidentified keys in
    /// separate hash spaces, so a ticker that happens to match a permanent ID's
    /// string form cannot collide.
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.permanent {
            Some(id) => {
                1u8.hash(state);
                id.hash(state);
            }
            None => {
                0u8.hash(state);
                self.ticker.hash(state);
            }
        }
    }
}

impl AssetKey {
    /// Construct a key with no permanent identifier.
    ///
    /// `impl AsRef<str>` accepts `&str`, `String`, or anything else that can
    /// hand out a string slice, which saves callers writing `.to_string()`.
    pub fn ticker_only(ticker: impl AsRef<str>) -> Self {
        Self {
            ticker: ticker.as_ref().to_owned(),
            permanent: None,
        }
    }

    /// True when this key can survive a ticker rename.
    ///
    /// `&self` borrows: the caller keeps ownership and nothing is copied. A
    /// bare `self` would consume the value and the caller could not use it
    /// again — the distinction Python has no equivalent for.
    pub fn is_stable(&self) -> bool {
        self.permanent.is_some()
    }
}

/// One security's prices for one trading day.
///
/// Money and quantities are `Decimal`, never `f64`. Binary floating point
/// cannot represent 0.10 exactly, so `0.1 + 0.2 != 0.3`; accumulated across
/// thousands of fractional-share fills that drift resurfaces as reconciliation
/// mismatches against the broker. `Decimal` is base-10 and exact for these
/// magnitudes. It also has no NaN or infinity, which removes an entire class of
/// validation check that a float-based version would need.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceBar {
    pub asset: AssetKey,
    pub date: Date,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    /// Close adjusted for splits and dividends. Research uses this; execution
    /// uses the raw `close`. Conflating them is a classic source of fake alpha.
    pub close_adj: Decimal,
    pub volume: Decimal,
}

/// Why a row was rejected.
///
/// Each variant carries the evidence needed to investigate without re-reading
/// the source file. `&'static str` is a string slice that lives for the whole
/// program — true of literals like `"high"`, and it costs nothing to store.
/// This is a lifetime annotation, and it is the least painful one you will meet.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Reject {
    #[error("{field} must be positive, got {value}")]
    NonPositivePrice { field: &'static str, value: Decimal },

    #[error("high {high} is below low {low}")]
    HighBelowLow { high: Decimal, low: Decimal },

    #[error("{field} {value} lies outside the high-low range [{low}, {high}]")]
    OutsideRange {
        field: &'static str,
        value: Decimal,
        low: Decimal,
        high: Decimal,
    },

    #[error("volume must not be negative, got {value}")]
    NegativeVolume { value: Decimal },

    #[error("ticker is empty")]
    EmptyTicker,
}

impl PriceBar {
    /// Check this bar against every rule, returning the first violation.
    ///
    /// The return type `Result<(), Reject>` says: success carries no value —
    /// `()` is the empty tuple — and failure carries a structured reason. It is
    /// deliberately not a `bool`, because "invalid" without "why" forces the
    /// caller to guess, and the plan requires counting rejections by cause
    /// rather than silently dropping rows.
    pub fn validate(&self) -> Result<(), Reject> {
        if self.asset.ticker.trim().is_empty() {
            return Err(Reject::EmptyTicker);
        }

        // An array of (name, value) pairs iterated with a `for` loop, rather
        // than four near-identical `if` blocks. Adding a price field later means
        // adding one tuple.
        for (field, value) in [
            ("open", self.open),
            ("high", self.high),
            ("low", self.low),
            ("close", self.close),
            ("close_adj", self.close_adj),
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

        // `open` and `close` must sit within the day's range. A violation means
        // the vendor's adjustment pipeline disagreed with itself, which has
        // happened often enough to be worth catching at the boundary.
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

/// The outcome of validating a batch: what survived, and why the rest did not.
///
/// Rejections are counted by cause rather than discarded, because a pipeline
/// that silently drops 3% of its rows produces a backtest that looks fine and
/// is quietly wrong.
#[derive(Debug, Default)]
pub struct ValidationReport {
    pub accepted: Vec<PriceBar>,
    pub rejected: Vec<(PriceBar, Reject)>,
}

impl ValidationReport {
    pub fn rejection_rate(&self) -> f64 {
        let total = self.accepted.len() + self.rejected.len();
        if total == 0 {
            return 0.0;
        }
        self.rejected.len() as f64 / total as f64
    }
}

/// Partition a batch of bars into accepted and rejected.
///
/// `impl IntoIterator<Item = PriceBar>` accepts a `Vec`, an array, or any lazy
/// iterator, so callers are not forced to allocate a `Vec` just to call this.
pub fn validate_batch(bars: impl IntoIterator<Item = PriceBar>) -> ValidationReport {
    let mut report = ValidationReport::default();
    for bar in bars {
        match bar.validate() {
            Ok(()) => report.accepted.push(bar),
            Err(reason) => report.rejected.push((bar, reason)),
        }
    }
    report
}

/// A day where the split/dividend adjustment factor moved unexpectedly.
///
/// Not an error on its own — a real split produces exactly this signature. It
/// becomes an error only when no corporate action explains it, which is a
/// judgement this crate cannot make until an actions table exists.
#[derive(Debug, Clone, PartialEq)]
pub struct AdjustmentJump {
    pub asset: AssetKey,
    pub date: Date,
    pub previous_factor: Decimal,
    pub factor: Decimal,
    /// factor / previous_factor. A 2:1 split shows roughly 0.5 or 2.0
    /// depending on adjustment direction.
    pub ratio: Decimal,
}

/// Flag days where `close_adj / close` moved more than `tolerance`.
///
/// Per-row validation cannot catch a corrupted adjusted close. A row with
/// `close = 10` and `close_adj = 1000` is individually plausible — deeply split
/// stocks legitimately have adjustment factors spanning orders of magnitude, so
/// no fixed bound separates "heavily adjusted" from "vendor error".
///
/// What *is* detectable is discontinuity. The factor should hold steady between
/// corporate actions and step exactly once when one occurs. An unexplained step
/// is either a real action or vendor corruption, and both deserve a human
/// glance before the data reaches a return series.
///
/// `bars` may contain any mix of assets and any ordering. Grouping and sorting
/// happen here rather than being a documented precondition, because a
/// precondition a caller can violate silently is not a safeguard. A mixed batch
/// compared as one series would diff one company's adjustment factor against
/// another's and report fake splits, which is worse than no check at all —
/// operators learn to ignore an alarm that cries wolf.
pub fn find_adjustment_jumps(bars: &[PriceBar], tolerance: Decimal) -> Vec<AdjustmentJump> {
    let mut by_asset: HashMap<&AssetKey, Vec<&PriceBar>> = HashMap::new();
    for bar in bars {
        by_asset.entry(&bar.asset).or_default().push(bar);
    }

    let mut jumps = Vec::new();
    for series in by_asset.values_mut() {
        series.sort_by_key(|bar| bar.date);

        let mut previous: Option<Decimal> = None;
        for bar in series.iter() {
            if bar.close <= Decimal::ZERO {
                continue; // already rejected by validate(); nothing to compare
            }
            let factor = bar.close_adj / bar.close;

            if let Some(prior_factor) = previous
                && prior_factor > Decimal::ZERO
            {
                let ratio = factor / prior_factor;
                if (ratio - Decimal::ONE).abs() > tolerance {
                    jumps.push(AdjustmentJump {
                        asset: bar.asset.clone(),
                        date: bar.date,
                        previous_factor: prior_factor,
                        factor,
                        ratio,
                    });
                }
            }
            previous = Some(factor);
        }
    }

    // HashMap iteration order is randomised per process, so sort for a stable
    // result. A check whose output reorders between runs is unusable in a diff
    // or a regression test.
    jumps.sort_by(|a, b| {
        a.asset
            .ticker
            .cmp(&b.asset.ticker)
            .then(a.date.cmp(&b.date))
    });
    jumps
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromPrimitive;

    /// All test data is synthetic. Committing real vendor rows — even a handful
    /// in a fixture — is redistribution under Sharadar's licence terms.
    fn bar(open: f64, high: f64, low: f64, close: f64, volume: f64) -> PriceBar {
        let d = |v: f64| Decimal::from_f64(v).expect("test literal is representable");
        PriceBar {
            asset: AssetKey::ticker_only("TEST"),
            date: Date::new(2020, 1, 2).expect("valid date"),
            open: d(open),
            high: d(high),
            low: d(low),
            close: d(close),
            close_adj: d(close),
            volume: d(volume),
        }
    }

    #[test]
    fn accepts_a_well_formed_bar() {
        assert_eq!(bar(10.0, 11.0, 9.5, 10.5, 1_000.0).validate(), Ok(()));
    }

    #[test]
    fn rejects_zero_and_negative_prices() {
        let zero = bar(0.0, 11.0, 9.5, 10.5, 1_000.0).validate();
        assert!(matches!(
            zero,
            Err(Reject::NonPositivePrice { field: "open", .. })
        ));

        let negative = bar(10.0, 11.0, -1.0, 10.5, 1_000.0).validate();
        assert!(matches!(
            negative,
            Err(Reject::NonPositivePrice { field: "low", .. })
        ));
    }

    #[test]
    fn rejects_inverted_high_low() {
        let result = bar(10.0, 9.0, 11.0, 10.5, 1_000.0).validate();
        assert!(matches!(result, Err(Reject::HighBelowLow { .. })));
    }

    #[test]
    fn rejects_close_outside_the_days_range() {
        let result = bar(10.0, 11.0, 9.5, 12.0, 1_000.0).validate();
        assert!(matches!(
            result,
            Err(Reject::OutsideRange { field: "close", .. })
        ));
    }

    #[test]
    fn rejects_negative_volume() {
        let result = bar(10.0, 11.0, 9.5, 10.5, -5.0).validate();
        assert!(matches!(result, Err(Reject::NegativeVolume { .. })));
    }

    #[test]
    fn accepts_zero_volume_because_halted_days_are_real() {
        assert_eq!(bar(10.0, 10.0, 10.0, 10.0, 0.0).validate(), Ok(()));
    }

    #[test]
    fn rejects_empty_ticker() {
        let mut b = bar(10.0, 11.0, 9.5, 10.5, 1_000.0);
        b.asset = AssetKey::ticker_only("   ");
        assert_eq!(b.validate(), Err(Reject::EmptyTicker));
    }

    #[test]
    fn batch_partitions_and_counts() {
        let report = validate_batch([
            bar(10.0, 11.0, 9.5, 10.5, 1_000.0),
            bar(10.0, 9.0, 11.0, 10.5, 1_000.0), // inverted
            bar(0.0, 11.0, 9.5, 10.5, 1_000.0),  // zero open
        ]);
        assert_eq!(report.accepted.len(), 1);
        assert_eq!(report.rejected.len(), 2);
        assert!((report.rejection_rate() - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn ticker_only_keys_are_not_stable() {
        assert!(!AssetKey::ticker_only("AAPL").is_stable());
        let keyed = AssetKey {
            ticker: "AAPL".into(),
            permanent: Some(PermanentId::Sharadar(199_059)),
        };
        assert!(keyed.is_stable());
    }

    // --- identity: the FB -> META problem -------------------------------

    fn keyed(ticker: &str, id: u64) -> AssetKey {
        AssetKey {
            ticker: ticker.into(),
            permanent: Some(PermanentId::Sharadar(id)),
        }
    }

    fn hash_of(key: &AssetKey) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn a_rename_does_not_split_one_company() {
        let before = keyed("FB", 199_059);
        let after = keyed("META", 199_059);
        assert_eq!(before, after, "same permaticker is the same company");
        assert_eq!(
            hash_of(&before),
            hash_of(&after),
            "equal keys must hash equally"
        );
    }

    #[test]
    fn a_reused_ticker_does_not_merge_two_companies() {
        assert_ne!(keyed("XYZ", 111), keyed("XYZ", 222));
    }

    #[test]
    fn identified_and_unidentified_keys_never_equate() {
        assert_ne!(keyed("AAPL", 199_059), AssetKey::ticker_only("AAPL"));
    }

    #[test]
    fn ids_from_different_providers_are_different_namespaces() {
        let sharadar = AssetKey {
            ticker: "AAPL".into(),
            permanent: Some(PermanentId::Sharadar(320_193)),
        };
        let sec = AssetKey {
            ticker: "AAPL".into(),
            permanent: Some(PermanentId::SecCik(320_193)),
        };
        assert_ne!(sharadar, sec, "same number, unrelated namespaces");
    }

    #[test]
    fn renamed_keys_resolve_to_one_entry_in_a_map() {
        use std::collections::HashMap;
        let mut prices: HashMap<AssetKey, i32> = HashMap::new();
        prices.insert(keyed("FB", 199_059), 1);
        prices.insert(keyed("META", 199_059), 2);
        assert_eq!(prices.len(), 1, "a rename must not create a second entry");
        assert_eq!(prices[&keyed("FB", 199_059)], 2);
    }

    // --- adjustment continuity ------------------------------------------

    fn bar_with_adj(day: i8, close: f64, close_adj: f64) -> PriceBar {
        let d = |v: f64| Decimal::from_f64(v).expect("test literal is representable");
        PriceBar {
            asset: AssetKey::ticker_only("TEST"),
            date: Date::new(2020, 1, day).expect("valid date"),
            open: d(close),
            high: d(close),
            low: d(close),
            close: d(close),
            close_adj: d(close_adj),
            volume: d(1_000.0),
        }
    }

    #[test]
    fn a_stable_adjustment_factor_produces_no_jumps() {
        let tol = Decimal::from_f64(0.01).expect("representable");
        let series = [
            bar_with_adj(1, 100.0, 50.0),
            bar_with_adj(2, 102.0, 51.0),
            bar_with_adj(3, 104.0, 52.0),
        ];
        assert!(find_adjustment_jumps(&series, tol).is_empty());
    }

    #[test]
    fn a_corrupted_adjusted_close_is_flagged() {
        // The reviewer's scenario: raw OHLC is sane, close_adj is nonsense.
        // Individually this row passes validate(); only continuity catches it.
        let tol = Decimal::from_f64(0.01).expect("representable");
        let series = [
            bar_with_adj(1, 10.0, 5.0),
            bar_with_adj(2, 10.0, 1000.0), // factor 0.5 -> 100.0
            bar_with_adj(3, 10.0, 5.0),
        ];
        let jumps = find_adjustment_jumps(&series, tol);
        assert_eq!(jumps.len(), 2, "one jump into the bad row, one back out");
        assert_eq!(jumps[0].date, Date::new(2020, 1, 2).expect("valid"));

        // And confirm the corrupt row really does pass per-row validation,
        // which is exactly why this batch-level check has to exist.
        assert_eq!(series[1].validate(), Ok(()));
    }

    #[test]
    fn unsorted_input_is_sorted_before_comparison() {
        let tol = Decimal::from_f64(0.01).expect("representable");
        let ordered = [
            bar_with_adj(1, 100.0, 50.0),
            bar_with_adj(2, 100.0, 50.0),
            bar_with_adj(3, 100.0, 50.0),
        ];
        let shuffled = [
            bar_with_adj(3, 100.0, 50.0),
            bar_with_adj(1, 100.0, 50.0),
            bar_with_adj(2, 100.0, 50.0),
        ];
        assert_eq!(
            find_adjustment_jumps(&ordered, tol),
            find_adjustment_jumps(&shuffled, tol)
        );
    }

    /// Round 2 of review caught this: a mixed batch was compared as one series,
    /// diffing one company's adjustment factor against another's.
    #[test]
    fn a_mixed_batch_does_not_produce_cross_asset_false_positives() {
        let tol = Decimal::from_f64(0.01).expect("representable");
        let mut alpha = bar_with_adj(1, 100.0, 50.0); // factor 0.5
        let mut beta = bar_with_adj(2, 100.0, 10.0); // factor 0.1, different company
        alpha.asset = AssetKey::ticker_only("ALPHA");
        beta.asset = AssetKey::ticker_only("BETA");

        let mut alpha2 = bar_with_adj(3, 100.0, 50.0);
        alpha2.asset = AssetKey::ticker_only("ALPHA");

        // Interleaved by date, which is what a real multi-asset batch looks like.
        let jumps = find_adjustment_jumps(&[alpha, beta, alpha2], tol);
        assert!(
            jumps.is_empty(),
            "each asset's factor is stable within itself; only cross-asset \
             comparison would report a jump"
        );
    }

    #[test]
    fn output_order_is_stable_across_runs() {
        // HashMap iteration is randomised per process, so an unsorted result
        // would reorder between runs and be useless in a diff or a snapshot test.
        let tol = Decimal::from_f64(0.01).expect("representable");
        let mut batch = Vec::new();
        for (ticker, adj) in [("ZETA", 1000.0), ("ALPHA", 1000.0), ("MU", 1000.0)] {
            let mut first = bar_with_adj(1, 10.0, 5.0);
            let mut second = bar_with_adj(2, 10.0, adj);
            first.asset = AssetKey::ticker_only(ticker);
            second.asset = AssetKey::ticker_only(ticker);
            batch.push(first);
            batch.push(second);
        }
        let tickers: Vec<String> = find_adjustment_jumps(&batch, tol)
            .iter()
            .map(|jump| jump.asset.ticker.clone())
            .collect();
        assert_eq!(
            tickers,
            vec!["ALPHA", "MU", "ZETA"],
            "sorted by ticker then date"
        );
    }

    #[test]
    fn a_two_for_one_split_is_reported_for_a_human_to_confirm() {
        // A genuine split has the same signature as corruption. The function
        // reports rather than judges; only an actions table can tell them apart.
        let tol = Decimal::from_f64(0.01).expect("representable");
        let series = [bar_with_adj(1, 100.0, 100.0), bar_with_adj(2, 50.0, 50.0)];
        assert!(
            find_adjustment_jumps(&series, tol).is_empty(),
            "ratio unchanged"
        );
    }
}
