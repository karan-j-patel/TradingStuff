//! The price panel: bars grouped by security, plus the trading calendar.
//!
//! # Why the grouping goes through a `BTreeMap` keyed by a string
//!
//! `HashMap` iteration order is deliberately randomised in Rust, so grouping
//! through one and then iterating would make the security order differ between
//! runs. Order leaks into the answer through tie-breaking, so that alone would
//! make the backtest non-reproducible while every test still passed.
//!
//! The key is [`identity`], which spells out the same rule `AssetKey`'s own
//! equality uses: the permanent identifier when the provider supplies one, the
//! ticker otherwise. Writing it out rather than relying on `AssetKey: Hash`
//! gets a *sorted* order for free, which is what determinism needs.
//!
//! # What "lookahead" means concretely here
//!
//! Every accessor on [`Series`] takes the date it is allowed to see and refuses
//! to look past it. There is no accessor that returns "the latest bar" without
//! an as-of date, because that is the shape of the bug rule 4 exists to
//! prevent, and an API that cannot express it cannot commit it.

use std::collections::BTreeMap;

use ingest::adjusted::AdjustedBar;
use ingest::schema::{AssetKey, PermanentId};
use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::error::EngineError;

/// The deterministic sort key for a security.
///
/// Matches `AssetKey`'s equality rule: permanent identifier when present,
/// ticker otherwise. The namespace prefix keeps two providers' integer
/// identifiers from colliding, which is the same reason `PermanentId` is an
/// enum rather than a bare `u64`.
pub fn identity(asset: &AssetKey) -> String {
    match &asset.permanent {
        Some(PermanentId::Sharadar(id)) => format!("sharadar:{id}"),
        Some(PermanentId::SecCik(id)) => format!("cik:{id}"),
        Some(PermanentId::Alpaca(id)) => format!("alpaca:{id}"),
        None => format!("ticker:{}", asset.ticker),
    }
}

/// One security's bars, ascending by date, at most one per date.
#[derive(Debug, Clone)]
pub struct Series {
    pub asset: AssetKey,
    pub identity: String,
    bars: Vec<AdjustedBar>,
}

impl Series {
    /// The split-adjusted close on exactly `date`, if the security traded then.
    pub fn close_on(&self, date: Date) -> Option<Decimal> {
        self.bar_on(date).map(|bar| bar.close)
    }

    /// The unadjusted close on exactly `date`. This is the price the eligibility
    /// floor is applied to, because a $5 floor is about what the share actually
    /// costs, not about what it costs after twenty years of split adjustment.
    pub fn close_unadjusted_on(&self, date: Date) -> Option<Decimal> {
        self.bar_on(date).map(|bar| bar.close_unadjusted)
    }

    fn bar_on(&self, date: Date) -> Option<&AdjustedBar> {
        self.bars
            .binary_search_by(|bar| bar.date.cmp(&date))
            .ok()
            .map(|index| &self.bars[index])
    }

    /// The last bar dated on or before `as_of`.
    ///
    /// This is the only accessor that can return a bar from a date other than
    /// the one asked for, and it is what a delisted name exits at. It still
    /// cannot see past `as_of`.
    pub fn last_close_on_or_before(&self, as_of: Date) -> Option<(Date, Decimal)> {
        let index = match self.bars.binary_search_by(|bar| bar.date.cmp(&as_of)) {
            Ok(index) => index,
            // `Err(i)` is the insertion point, so `i - 1` is the last bar
            // strictly before `as_of`, and `i == 0` means there is none.
            Err(0) => return None,
            Err(index) => index - 1,
        };
        Some((self.bars[index].date, self.bars[index].close))
    }

    /// The last close within the calendar month `month_end` falls in, dated on
    /// or before `month_end`.
    ///
    /// The signal needs a price at a month boundary, and a name that simply did
    /// not trade on the panel's month-end date should not be dropped for it.
    /// Reaching back is bounded to the same calendar month so that a name with
    /// a long gap produces no signal rather than a stale one computed off a
    /// price from an arbitrary distance in the past.
    pub fn close_at_month_end(&self, month_end: Date) -> Option<Decimal> {
        let (found, close) = self.last_close_on_or_before(month_end)?;
        (found.year() == month_end.year() && found.month() == month_end.month()).then_some(close)
    }

    /// How many bars fall in `(after, through]`.
    ///
    /// Half-open at the start so that a window described by its two endpoint
    /// month-ends counts the days that elapsed between them without counting
    /// the opening day twice.
    pub fn bars_in(&self, after: Date, through: Date) -> usize {
        // `partition_point` returns the first index whose predicate is false,
        // which on a sorted slice is a binary search for a boundary.
        let start = self.bars.partition_point(|bar| bar.date <= after);
        let end = self.bars.partition_point(|bar| bar.date <= through);
        end.saturating_sub(start)
    }

    /// Every date this security has a bar on, for tests and diagnostics.
    pub fn dates(&self) -> impl Iterator<Item = Date> + '_ {
        self.bars.iter().map(|bar| bar.date)
    }
}

/// Every security's bars plus the calendar they share.
#[derive(Debug, Clone)]
pub struct Panel {
    securities: Vec<Series>,
    dates: Vec<Date>,
    month_ends: Vec<Date>,
}

impl Panel {
    /// Group loose bars into a panel.
    ///
    /// Duplicate `(security, date)` pairs are collapsed to the last one seen,
    /// which is the only sane reading of a vendor shipping a row twice, and the
    /// input is sorted here rather than assumed sorted.
    pub fn from_bars(bars: Vec<AdjustedBar>) -> Result<Self, EngineError> {
        if bars.is_empty() {
            return Err(EngineError::EmptyPanel);
        }

        let mut grouped: BTreeMap<String, (AssetKey, BTreeMap<Date, AdjustedBar>)> =
            BTreeMap::new();
        for bar in bars {
            let key = identity(&bar.asset);
            grouped
                .entry(key)
                .or_insert_with(|| (bar.asset.clone(), BTreeMap::new()))
                .1
                .insert(bar.date, bar);
        }

        let securities: Vec<Series> = grouped
            .into_iter()
            .map(|(identity, (asset, by_date))| Series {
                asset,
                identity,
                bars: by_date.into_values().collect(),
            })
            .collect();

        // The calendar is the union of every security's trading days. A
        // `BTreeMap` key set is already sorted and deduplicated, so this is one
        // pass rather than a sort and a dedup.
        let dates: Vec<Date> = securities
            .iter()
            .flat_map(Series::dates)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();

        let month_ends = month_ends_of(&dates);

        Ok(Panel {
            securities,
            dates,
            month_ends,
        })
    }

    pub fn securities(&self) -> &[Series] {
        &self.securities
    }

    /// Last trading day of each calendar month present, ascending.
    ///
    /// These are the rebalance dates. Note what they are not: they are not
    /// calendar month-ends. A month whose last trading day is the 29th has its
    /// rebalance on the 29th, because that is when a trade could have happened.
    pub fn month_ends(&self) -> &[Date] {
        &self.month_ends
    }

    /// Number of trading days in `(after, through]` across the whole panel.
    ///
    /// The denominator of the coverage test. Using the panel's calendar rather
    /// than a security's own means a name that stopped trading is measured
    /// against the days the market was open, which is the question the rule
    /// asks.
    pub fn trading_days_in(&self, after: Date, through: Date) -> usize {
        let start = self.dates.partition_point(|date| *date <= after);
        let end = self.dates.partition_point(|date| *date <= through);
        end.saturating_sub(start)
    }
}

/// Pick the last date of each calendar month from an ascending, deduplicated
/// list of dates.
fn month_ends_of(dates: &[Date]) -> Vec<Date> {
    let mut ends: Vec<Date> = Vec::new();
    for date in dates {
        match ends.last_mut() {
            Some(last) if last.year() == date.year() && last.month() == date.month() => {
                *last = *date;
            }
            _ => ends.push(*date),
        }
    }
    ends
}
