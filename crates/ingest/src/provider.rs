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

/// A source of daily prices on a vendor's adjusted basis.
///
/// Separate from [`PriceSource`] rather than a variant of it, because the two
/// promise different numbers and no amount of care at a call site keeps that
/// straight. `PriceSource` promises prices as traded. A vendor that ships
/// split-adjusted open, high and low cannot honestly implement it, so it
/// implements this instead and the difference is visible in the type a caller
/// receives. See [`crate::adjusted::AdjustedBar`] for the barrier itself.
pub trait AdjustedPriceSource {
    fn name(&self) -> &str;

    /// The earliest date this source will actually serve.
    ///
    /// # Measured, never claimed
    ///
    /// This returns a `Result` where [`PriceSource::earliest_available`]
    /// returns a bare `Option`, and the difference is the point. A boundary
    /// taken from vendor metadata is a claim, and the claim can be wrong:
    /// Sharadar's TICKERS table reports a first price date of 2010 for TSLA
    /// while the key in use serves nothing before 2021-08-10. An implementation
    /// here measures the boundary by asking for data, so it can fail, so it
    /// says so.
    ///
    /// `None` means the source served nothing at all, which is different from
    /// an unknown boundary and different again from an error.
    fn earliest_available(&self) -> Result<Option<Date>, SourceError>;

    /// Fetch adjusted bars, refusing a range the source cannot honestly cover.
    ///
    /// A request reaching before [`AdjustedPriceSource::earliest_available`] is
    /// an error naming the boundary, never a quietly shortened series. Silent
    /// truncation is the failure mode that trains a model on less history than
    /// anyone thinks it has.
    fn fetch_adjusted(
        &self,
        assets: &[AssetKey],
        range: DateRange,
    ) -> Result<Vec<crate::adjusted::AdjustedBar>, SourceError>;
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

    /// How much time these figures cover. Quarterly, annual, and trailing
    /// twelve-month reports for one company can share a period end.
    pub scope: ReportScope,

    /// Vendor filing identifier when the source exposes one.
    pub filing_id: Option<String>,

    /// Field name to value. Deliberately untyped: fundamental fields number in
    /// the hundreds and differ per vendor, so the panel layer maps them rather
    /// than this crate hardcoding a schema it cannot keep current.
    pub fields: std::collections::BTreeMap<String, rust_decimal::Decimal>,
}

impl FundamentalRecord {
    /// The identity of the filing this record describes.
    pub fn key(&self) -> FilingKey {
        FilingKey {
            asset: self.asset.clone(),
            period_end: self.period_end,
            basis: self.basis,
            scope: self.scope,
            as_reported: self.as_reported,
            filing_id: self.filing_id.clone(),
        }
    }
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

/// How much time a report covers.
///
/// Independent of [`ReportBasis`], and conflating the two loses information.
/// Sharadar's six dimensions are exactly the product of these two axes:
///
/// | dimension | basis       | scope      |
/// |-----------|-------------|------------|
/// | `ARQ`     | AsReported  | Quarterly  |
/// | `ART`     | AsReported  | TrailingTwelveMonths |
/// | `ARY`     | AsReported  | Annual     |
/// | `MRQ`     | Restated    | Quarterly  |
/// | `MRT`     | Restated    | TrailingTwelveMonths |
/// | `MRY`     | Restated    | Annual     |
///
/// This matters for identity, not just labelling. A company's Q4 and its full
/// year both end on 31 December, so an annual and a quarterly record share an
/// asset, a period end, and a basis. Without scope they are indistinguishable,
/// and revenue figures differing by a factor of four look like a vendor
/// rewriting history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportScope {
    Quarterly,
    Annual,
    TrailingTwelveMonths,
}

/// What makes one filing distinct from another.
///
/// Every field is load-bearing. Dropping any one of them merges records that
/// describe genuinely different things, and the merge is silent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FilingKey {
    pub asset: AssetKey,
    pub period_end: Date,
    pub basis: ReportBasis,
    pub scope: ReportScope,

    /// The day this filing became public.
    ///
    /// Included because an amendment is a **new public event**, not a change to
    /// an existing one. An original 10-Q and a later 10-Q/A share an asset, a
    /// period end, a basis, and a scope; only the publication date separates
    /// them. Without it, a company amending its own filing is indistinguishable
    /// from a vendor silently rewriting history, which inverts the exact
    /// judgement [`detect_revisions`] exists to make.
    ///
    /// The cost is one weaker signal: a vendor that *corrects a filing date*
    /// shows up as one filing disappearing and another appearing, rather than
    /// as a revision. Amendments are far more common and far more damaging to
    /// misread, so the trade is worth taking.
    pub as_reported: Date,

    /// Vendor filing identifier when available. An SEC accession number, or a
    /// vendor's own id. An additional discriminator rather than a replacement
    /// for the publication date, since some vendors reuse ids across amendments
    /// and many expose none at all.
    pub filing_id: Option<String>,
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

/// The outcome of comparing two snapshots.
#[derive(Debug, Default, PartialEq)]
pub struct RevisionReport {
    /// Values that changed between the two observations.
    pub revisions: Vec<Revision>,
    /// Filings whose key appeared more than once within a single snapshot.
    ///
    /// Surfaced rather than silently deduplicated. A repeated key means the
    /// identity is not fine enough for this source, and any comparison against
    /// it is arbitrary — whichever duplicate happened to be indexed last would
    /// win. Callers should treat this as a data-modelling defect to fix, not a
    /// warning to ignore.
    pub ambiguous: Vec<FilingKey>,
}

impl RevisionReport {
    pub fn is_clean(&self) -> bool {
        self.revisions.is_empty() && self.ambiguous.is_empty()
    }
}

/// Compare two snapshots of the same filings and report values that moved.
///
/// Re-fetch history periodically, diff against what was stored, and a vendor
/// silently rewriting the past becomes a visible event rather than a slow
/// corruption of every downstream result.
///
/// Records are matched on [`FilingKey`]. Fields present in one snapshot but
/// absent from the other are ignored, since a vendor adding a column is not a
/// revision of existing data.
///
/// # What this cannot catch
///
/// **First-pull contamination.** If the first time you ever fetch 2024 data is
/// in 2026, and the vendor already rewrote it, there is no earlier snapshot to
/// diff against and this function reports nothing. No client-side mechanism can
/// detect that, because the evidence never existed locally.
///
/// The only real defence is a source whose history is immutable by
/// construction. SEC XBRL submissions qualify: each is a dated document that
/// cannot be edited after filing, so "as reported" is a property of the archive
/// rather than a promise from a vendor. Where such a source is available it
/// should be preferred over any amount of diffing.
///
/// **Unfetched history** is the same problem in a different shape. This only
/// covers filings actually present in both snapshots.
pub fn detect_revisions(
    earlier: &[FundamentalRecord],
    later: &[FundamentalRecord],
) -> RevisionReport {
    use std::collections::HashMap;

    // Group rather than index, so a repeated key is visible instead of being
    // resolved by whichever record happened to be inserted last. Comparing
    // against an arbitrarily-chosen duplicate produces a revision row that
    // looks authoritative and is not, and a false alarm in an audit is worse
    // than a gap because it teaches operators to ignore the alarm.
    let mut by_key: HashMap<FilingKey, Vec<&FundamentalRecord>> = HashMap::new();
    for record in earlier {
        by_key.entry(record.key()).or_default().push(record);
    }
    let mut later_by_key: HashMap<FilingKey, Vec<&FundamentalRecord>> = HashMap::new();
    for record in later {
        later_by_key.entry(record.key()).or_default().push(record);
    }

    let mut ambiguous: Vec<FilingKey> = by_key
        .iter()
        .chain(later_by_key.iter())
        .filter(|(_, records)| records.len() > 1)
        .map(|(key, _)| key.clone())
        .collect();

    let mut revisions = Vec::new();
    for (key, current_records) in &later_by_key {
        // Only compare where exactly one record exists on each side. Anything
        // else is not uniquely identified, and is reported as ambiguous above
        // rather than guessed at.
        let [current] = current_records.as_slice() else {
            continue;
        };
        let Some(previous_records) = by_key.get(key) else {
            continue; // newly appeared filing, not a revision
        };
        let [previous] = previous_records.as_slice() else {
            continue;
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

    // HashMap iteration is randomised per process, so both outputs are sorted
    // for a result that is stable across runs.
    revisions.sort_by(|a, b| {
        a.asset
            .ticker
            .cmp(&b.asset.ticker)
            .then(a.period_end.cmp(&b.period_end))
            .then(a.field.cmp(&b.field))
    });

    ambiguous.sort_by_key(|key| (key.asset.ticker.clone(), key.period_end));
    ambiguous.dedup();
    RevisionReport {
        revisions,
        ambiguous,
    }
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
            scope: ReportScope::Quarterly,
            filing_id: None,
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
            scope: ReportScope::Quarterly,
            filing_id: None,
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
        let report = detect_revisions(
            &[snapshot(date(2024, 8, 1), 100)],
            &[snapshot(date(2025, 3, 1), 999)],
        );
        assert_eq!(report.revisions.len(), 1);
        assert_eq!(report.revisions[0].field, "revenue");
        assert_eq!(report.revisions[0].was, rust_decimal::Decimal::from(100));
        assert_eq!(report.revisions[0].now, rust_decimal::Decimal::from(999));
        assert_eq!(report.revisions[0].first_observed, date(2024, 8, 1));
    }

    #[test]
    fn an_unchanged_figure_is_not_a_revision() {
        let report = detect_revisions(
            &[snapshot(date(2024, 8, 1), 100)],
            &[snapshot(date(2025, 3, 1), 100)],
        );
        assert!(
            report.is_clean(),
            "same value re-observed is not a revision"
        );
    }

    #[test]
    fn a_newly_appeared_filing_is_not_a_revision() {
        let report = detect_revisions(&[], &[snapshot(date(2025, 3, 1), 100)]);
        assert!(report.is_clean());
    }

    /// Same asset, same period end, same basis — but an annual report and a
    /// quarterly one. Round 2 of review caught these keying identically.
    fn scoped(scope: ReportScope, revenue: i64) -> FundamentalRecord {
        let mut record = snapshot(date(2026, 3, 1), revenue);
        record.period_end = date(2025, 12, 31);
        record.scope = scope;
        record
    }

    #[test]
    fn annual_and_quarterly_sharing_a_period_end_are_different_filings() {
        let report = detect_revisions(
            &[scoped(ReportScope::Annual, 4_000)],
            &[scoped(ReportScope::Quarterly, 1_000)],
        );
        assert!(
            report.is_clean(),
            "a full year and its Q4 are different filings, not a 4x revision"
        );
    }

    #[test]
    fn a_revision_within_one_scope_is_still_caught() {
        let report = detect_revisions(
            &[scoped(ReportScope::Annual, 4_000)],
            &[scoped(ReportScope::Annual, 4_200)],
        );
        assert_eq!(report.revisions.len(), 1, "same scope, changed value");
    }

    #[test]
    fn a_duplicate_key_within_one_snapshot_is_reported_not_swallowed() {
        // Two records that key identically cannot be compared meaningfully;
        // whichever indexed last would arbitrarily win.
        let report = detect_revisions(
            &[
                scoped(ReportScope::Annual, 100),
                scoped(ReportScope::Annual, 200),
            ],
            &[],
        );
        assert_eq!(
            report.ambiguous.len(),
            1,
            "ambiguity surfaced, not silently deduped"
        );
        assert!(!report.is_clean());
    }

    #[test]
    fn a_filing_id_separates_otherwise_identical_records() {
        let mut original = scoped(ReportScope::Annual, 100);
        original.filing_id = Some("0000320193-26-000001".into());
        let mut amended = scoped(ReportScope::Annual, 200);
        amended.filing_id = Some("0000320193-26-000002".into());

        let report = detect_revisions(&[original], &[amended]);
        assert!(
            report.is_clean(),
            "an amended filing is a new document, not a revision of the original"
        );
    }

    /// Round 3 of review: an amendment is a new public event, not a rewrite.
    #[test]
    fn an_amended_filing_is_not_a_revision_of_the_original() {
        let original = scoped(ReportScope::Quarterly, 1_000);
        let mut amended = scoped(ReportScope::Quarterly, 1_100);
        amended.as_reported = date(2026, 5, 15); // 10-Q/A filed later
        // No filing_id on either, which is the case the previous key missed.
        assert_eq!(original.filing_id, None);

        let report = detect_revisions(&[original], &[amended]);
        assert!(
            report.is_clean(),
            "a company amending its own filing is public information, \
             not a vendor silently rewriting history"
        );
    }

    #[test]
    fn the_same_filing_re_observed_is_still_compared() {
        // Guards against over-correcting: adding as_reported to the key must
        // not stop the primary case from working.
        let first_pull = scoped(ReportScope::Quarterly, 1_000);
        let mut second_pull = scoped(ReportScope::Quarterly, 1_500);
        second_pull.observed_at = date(2026, 6, 1); // same filing, later snapshot
        assert_eq!(first_pull.as_reported, second_pull.as_reported);

        let report = detect_revisions(&[first_pull], &[second_pull]);
        assert_eq!(report.revisions.len(), 1, "same filing, changed value");
    }

    #[test]
    fn an_ambiguous_key_produces_no_revision_rows() {
        // Two identical keys on the earlier side. Any comparison would be
        // against an arbitrarily chosen duplicate.
        let report = detect_revisions(
            &[
                scoped(ReportScope::Annual, 100),
                scoped(ReportScope::Annual, 200),
            ],
            &[scoped(ReportScope::Annual, 300)],
        );
        assert_eq!(report.ambiguous.len(), 1, "ambiguity is reported");
        assert!(
            report.revisions.is_empty(),
            "a knowingly-arbitrary comparison is worse than none"
        );
    }

    #[test]
    fn a_new_column_is_not_a_revision() {
        let earlier = snapshot(date(2024, 8, 1), 100);
        let mut later = snapshot(date(2025, 3, 1), 100);
        later
            .fields
            .insert("ebitda".into(), rust_decimal::Decimal::from(42));
        assert!(detect_revisions(&[earlier], &[later]).is_clean());
    }
}
