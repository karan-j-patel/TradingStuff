//! The pluggable data-source interface.
//!
//! Sharadar and SEC XBRL sit behind these traits, and neither name appears
//! downstream. This exists because the product ships bring-your-own-key: a
//! buyer may hold a subscription to a vendor that does not exist yet.
//!
//! # Notes for a Rust newcomer
//!
//! A `trait` is an interface — a set of methods a type promises to provide.
//! Unlike a Python base class it carries no data and there is no inheritance;
//! a type declares `impl PriceSource for Sharadar` and the compiler checks it
//! implemented everything. Traits can be used two ways: as a generic bound
//! (`fn run<S: PriceSource>(source: S)`), resolved at compile time with no
//! runtime cost, or as `&dyn PriceSource`, resolved at runtime like a Python
//! method call. Prefer the first; reach for the second when you need a
//! collection of differently-typed sources.

use jiff::civil::Date;

use crate::schema::{AssetKey, PriceBar};

/// An inclusive span of calendar dates.
///
/// A struct rather than a bare tuple so `start` and `end` cannot be passed in
/// the wrong order without it being visible at the call site — and so the
/// invariant `start <= end` has one place to live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    start: Date,
    end: Date,
}

impl DateRange {
    /// Build a range, rejecting an inverted one.
    ///
    /// Fields are private and this is the only constructor, so an invalid
    /// `DateRange` cannot exist anywhere in the program. Validating once at the
    /// boundary beats re-checking at every use — a pattern worth internalising,
    /// since it is how Rust replaces defensive assertions scattered through a
    /// codebase.
    pub fn new(start: Date, end: Date) -> Result<Self, SourceError> {
        if start > end {
            return Err(SourceError::InvalidRange { start, end });
        }
        Ok(Self { start, end })
    }

    pub fn start(&self) -> Date {
        self.start
    }

    pub fn end(&self) -> Date {
        self.end
    }

    pub fn contains(&self, date: Date) -> bool {
        self.start <= date && date <= self.end
    }
}

/// Why a fetch failed.
///
/// `#[from]` on the transport variant lets `?` convert an underlying error
/// automatically, so call sites stay readable.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("date range starts at {start} which is after its end {end}")]
    InvalidRange { start: Date, end: Date },

    #[error("{provider} does not cover {ticker}")]
    NotCovered { provider: String, ticker: String },

    #[error("{provider} rejected the credentials")]
    Unauthorized { provider: String },

    #[error("{provider} declined: {detail}")]
    Refused { provider: String, detail: String },

    #[error("could not reach {provider}: {source}")]
    Transport {
        provider: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("{provider} returned data this crate could not parse: {detail}")]
    Malformed { provider: String, detail: String },
}

/// A source of daily price history.
pub trait PriceSource {
    /// Human-readable name, used in errors and in the provenance recorded
    /// alongside every stored row.
    fn name(&self) -> &str;

    /// The earliest date this source can serve.
    ///
    /// Free tiers are often windowed rather than ticker-limited — Sharadar's,
    /// measured directly, serves a rolling five years. Callers need this to
    /// fail loudly at startup instead of silently training a model on a
    /// truncated history and wondering why the results are thin.
    fn earliest_available(&self) -> Option<Date> {
        None
    }

    fn fetch_prices(
        &self,
        assets: &[AssetKey],
        range: DateRange,
    ) -> Result<Vec<PriceBar>, SourceError>;
}

/// One fundamental observation, carrying the two dates that must never be
/// confused.
///
/// This distinction is the entire point-in-time guarantee, and getting it wrong
/// is the most common way a backtest invents alpha that never existed.
#[derive(Debug, Clone, PartialEq)]
pub struct FundamentalRecord {
    pub asset: AssetKey,

    /// The day this filing became public. Every research join uses this.
    ///
    /// Sharadar's direct API calls it `date`; the Nasdaq channel called it
    /// `datekey`; SEC XBRL derives it from the submission. Whatever the vendor
    /// names it, it answers "when could a trader have known this."
    pub as_reported: Date,

    /// The fiscal period the figures describe. Useful for aligning companies
    /// with different year ends, and dangerous for anything else — it precedes
    /// `as_reported` by weeks or months.
    pub period_end: Date,

    /// When *we* pulled these values from the vendor.
    ///
    /// Distinct from `as_reported`, and the distinction matters. A vendor can
    /// hand back a 2024 filing date attached to values it silently corrected in
    /// 2025. `as_reported` describes the filing; `observed_at` describes the
    /// snapshot. Without the second, a revision is undetectable — the row looks
    /// identical either way.
    ///
    /// This does not prevent contamination on its own. It makes it *findable*,
    /// by letting two observations of the same filing be compared. See
    /// [`detect_revisions`].
    pub observed_at: Date,

    /// Which provider produced this row. Recorded because vendors disagree, and
    /// a disagreement is only diagnosable if you know who said what.
    pub source: String,

    /// Whether these figures are as originally filed or later restated.
    pub basis: ReportBasis,

    /// Field name to value. Deliberately untyped: fundamental fields number in
    /// the hundreds and differ per vendor, so the panel layer maps them rather
    /// than this crate hardcoding a schema it cannot keep current.
    pub fields: std::collections::BTreeMap<String, rust_decimal::Decimal>,
}

/// Whether a figure is what the company originally said, or what it says now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportBasis {
    /// As originally filed. The only basis valid for backtesting — it is what a
    /// trader could actually have seen on the day.
    AsReported,
    /// Restated to incorporate later corrections. Correct for studying a
    /// company's true financials; catastrophic in a backtest, because it trains
    /// the model on corrections that had not happened yet.
    Restated,
}

impl ReportBasis {
    /// True when this basis is safe to train on.
    pub fn is_backtest_safe(&self) -> bool {
        matches!(self, ReportBasis::AsReported)
    }
}

/// A source of fundamental data.
pub trait FundamentalSource {
    fn name(&self) -> &str;

    /// Fetch fundamentals as they stood on `as_of`.
    ///
    /// `as_of` is part of the contract rather than a post-filter concern, so a
    /// source that can answer historical queries natively (SEC XBRL can — every
    /// submission carries its own filing date) does the filtering where it is
    /// actually reliable. A source that cannot must still honour it, and must
    /// document what it cannot guarantee.
    fn fetch_fundamentals(
        &self,
        assets: &[AssetKey],
        range: DateRange,
        as_of: Date,
        basis: ReportBasis,
    ) -> Result<Vec<FundamentalRecord>, SourceError>;
}

/// Drop any record that would not have been public by `as_of`.
///
/// # What this catches, and what it cannot
///
/// Catches: a filing published after `as_of`, and anything explicitly marked
/// restated. Both are the obvious forms of lookahead and both are real.
///
/// **Does not catch** the subtle form: a vendor returning a 2024 filing date
/// attached to values it quietly corrected in 2025. Such a row satisfies both
/// conditions here — the date is old enough, the basis says as-reported — while
/// carrying information nobody had in 2024. No date filter can detect that,
/// because the contamination is in the *values*, not the metadata.
///
/// Defence against that form is threefold and lives elsewhere:
///   1. `as_of` on [`FundamentalSource::fetch_fundamentals`], pushing the
///      guarantee to the only layer that can honour it;
///   2. `observed_at` on each record, so two snapshots are comparable;
///   3. [`detect_revisions`], which finds values that changed between them.
///
/// This function stays because it is cheap, total, and independently testable.
/// It is a backstop, not the guarantee.
pub fn visible_as_of(
    records: impl IntoIterator<Item = FundamentalRecord>,
    as_of: Date,
) -> Vec<FundamentalRecord> {
    records
        .into_iter()
        .filter(|record| record.as_reported <= as_of)
        .filter(|record| record.basis.is_backtest_safe())
        .collect()
}

/// A value that changed between two observations of the same filing.
///
/// An as-reported figure is supposed to be immutable — it is a historical fact
/// about what a company published on a given day. If it moves, either the
/// vendor restated data it labelled as-reported, or one of the two pulls is
/// corrupt. Both invalidate any backtest already run on the earlier snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Revision {
    pub asset: AssetKey,
    pub period_end: Date,
    pub field: String,
    pub was: rust_decimal::Decimal,
    pub now: rust_decimal::Decimal,
    pub first_observed: Date,
    pub then_observed: Date,
}

/// Compare two snapshots of the same filings and report values that moved.
///
/// This is the mechanism that makes point-in-time claims falsifiable rather
/// than merely asserted. Re-fetch history periodically, diff against what was
/// stored, and a vendor silently rewriting the past becomes a visible event
/// instead of a slow corruption of every downstream result.
///
/// Records are matched on (asset, period_end, basis). Fields present in one
/// snapshot but absent from the other are ignored — a vendor adding a column is
/// not a revision of existing data.
pub fn detect_revisions(
    earlier: &[FundamentalRecord],
    later: &[FundamentalRecord],
) -> Vec<Revision> {
    use std::collections::HashMap;

    let index: HashMap<(&AssetKey, Date, ReportBasis), &FundamentalRecord> = earlier
        .iter()
        .map(|record| ((&record.asset, record.period_end, record.basis), record))
        .collect();

    let mut revisions = Vec::new();
    for current in later {
        let Some(previous) = index.get(&(&current.asset, current.period_end, current.basis)) else {
            continue; // newly appeared filing, not a revision
        };
        for (field, now) in &current.fields {
            let Some(was) = previous.fields.get(field) else {
                continue; // new column
            };
            if was != now {
                revisions.push(Revision {
                    asset: current.asset.clone(),
                    period_end: current.period_end,
                    field: field.clone(),
                    was: *was,
                    now: *now,
                    first_observed: previous.observed_at,
                    then_observed: current.observed_at,
                });
            }
        }
    }
    revisions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn date(y: i16, m: i8, d: i8) -> Date {
        Date::new(y, m, d).expect("valid test date")
    }

    fn record(as_reported: Date, basis: ReportBasis) -> FundamentalRecord {
        FundamentalRecord {
            asset: AssetKey::ticker_only("TEST"),
            as_reported,
            period_end: date(2026, 6, 30),
            observed_at: as_reported,
            source: "test".into(),
            basis,
            fields: BTreeMap::new(),
        }
    }

    /// A filing observed on a given day, carrying one numeric field.
    fn snapshot(observed_at: Date, revenue: i64) -> FundamentalRecord {
        let mut fields = BTreeMap::new();
        fields.insert("revenue".to_string(), rust_decimal::Decimal::from(revenue));
        FundamentalRecord {
            asset: AssetKey::ticker_only("TEST"),
            as_reported: date(2024, 7, 31),
            period_end: date(2024, 6, 30),
            observed_at,
            source: "test".into(),
            basis: ReportBasis::AsReported,
            fields,
        }
    }

    #[test]
    fn rejects_an_inverted_range() {
        let result = DateRange::new(date(2026, 6, 1), date(2026, 1, 1));
        assert!(matches!(result, Err(SourceError::InvalidRange { .. })));
    }

    #[test]
    fn accepts_a_single_day_range() {
        let day = date(2026, 6, 1);
        let range = DateRange::new(day, day).expect("same start and end is valid");
        assert!(range.contains(day));
    }

    #[test]
    fn range_boundaries_are_inclusive() {
        let range = DateRange::new(date(2026, 1, 1), date(2026, 12, 31)).expect("valid");
        assert!(range.contains(date(2026, 1, 1)));
        assert!(range.contains(date(2026, 12, 31)));
        assert!(!range.contains(date(2025, 12, 31)));
        assert!(!range.contains(date(2027, 1, 1)));
    }

    #[test]
    fn hides_filings_published_after_the_as_of_date() {
        let visible = visible_as_of(
            [
                record(date(2026, 7, 31), ReportBasis::AsReported),
                record(date(2026, 8, 15), ReportBasis::AsReported),
            ],
            date(2026, 8, 1),
        );
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].as_reported, date(2026, 7, 31));
    }

    #[test]
    fn a_filing_published_exactly_on_the_as_of_date_is_visible() {
        let visible = visible_as_of(
            [record(date(2026, 8, 1), ReportBasis::AsReported)],
            date(2026, 8, 1),
        );
        assert_eq!(visible.len(), 1);
    }

    #[test]
    fn restated_figures_are_dropped_even_when_old_enough() {
        let visible = visible_as_of(
            [record(date(2021, 1, 1), ReportBasis::Restated)],
            date(2026, 8, 1),
        );
        assert!(
            visible.is_empty(),
            "restated data must never reach a backtest"
        );
    }

    #[test]
    fn only_as_reported_is_backtest_safe() {
        assert!(ReportBasis::AsReported.is_backtest_safe());
        assert!(!ReportBasis::Restated.is_backtest_safe());
    }

    // --- the lookahead visible_as_of cannot see ---------------------------

    #[test]
    fn a_silently_revised_figure_slips_past_the_date_filter() {
        // The exact scenario from review: a 2024 filing date on values the
        // vendor corrected in 2025. Old enough, marked as-reported, so the
        // date filter accepts it — while carrying 2025 information.
        let contaminated = snapshot(date(2025, 3, 1), 999);
        let survivors = visible_as_of([contaminated], date(2024, 12, 31));
        assert_eq!(
            survivors.len(),
            1,
            "documents the gap: visible_as_of cannot detect value contamination"
        );
    }

    #[test]
    fn comparing_snapshots_does_detect_the_revision() {
        let revisions = detect_revisions(
            &[snapshot(date(2024, 8, 1), 100)],
            &[snapshot(date(2025, 3, 1), 999)],
        );
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].field, "revenue");
        assert_eq!(revisions[0].was, rust_decimal::Decimal::from(100));
        assert_eq!(revisions[0].now, rust_decimal::Decimal::from(999));
        assert_eq!(revisions[0].first_observed, date(2024, 8, 1));
    }

    #[test]
    fn an_unchanged_figure_is_not_a_revision() {
        let revisions = detect_revisions(
            &[snapshot(date(2024, 8, 1), 100)],
            &[snapshot(date(2025, 3, 1), 100)],
        );
        assert!(
            revisions.is_empty(),
            "same value re-observed is not a revision"
        );
    }

    #[test]
    fn a_newly_appeared_filing_is_not_a_revision() {
        let revisions = detect_revisions(&[], &[snapshot(date(2025, 3, 1), 100)]);
        assert!(revisions.is_empty());
    }

    #[test]
    fn a_new_column_is_not_a_revision() {
        let earlier = snapshot(date(2024, 8, 1), 100);
        let mut later = snapshot(date(2025, 3, 1), 100);
        later
            .fields
            .insert("ebitda".into(), rust_decimal::Decimal::from(42));
        assert!(detect_revisions(&[earlier], &[later]).is_empty());
    }
}
