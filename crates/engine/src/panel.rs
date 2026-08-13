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

use ingest::actions::{
    ActionRecord, Convention, CorporateAction, Delisting, DividendKind, TerminalValue,
    flat_convention_for,
};
use ingest::adjusted::AdjustedBar;
use ingest::schema::{AssetKey, PermanentId};
use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::error::EngineError;

/// One cash dividend: the date it went ex, and the amount per share.
///
/// Private to this module. Nothing outside needs the pair, because the only
/// question anyone asks of them is [`Series::dividend_cash_in`].
#[derive(Debug, Clone, Copy)]
struct CashDividend {
    ex_date: Date,
    amount: Decimal,
}

/// What the delistings dataset says about a holding that stopped trading.
///
/// Three states rather than an `Option<Decimal>`, mirroring
/// [`ingest::TerminalValue`], because an assumed delisting return that cannot
/// be told apart from a measured one is the same class of problem as a vendor's
/// adjusted close. The report prints the three counts separately and any figure
/// resting on the middle one has to be able to say so.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExitMark {
    /// A delisting return that was measured. Nothing on the current vendor
    /// produces one, and the state exists because a hand-curated file or a
    /// second provider can.
    Observed(Decimal),
    /// A delisting return assumed from a published convention.
    Imputed {
        value: Decimal,
        convention: Convention,
    },
    /// The delistings dataset has nothing that explains this exit, so the
    /// holding keeps the pre-imputation treatment and is counted as
    /// unexplained. Never silently treated as a merger, which would be an
    /// assumption dressed up as an absence.
    Unexplained,
}

impl ExitMark {
    /// What the terminal mark is multiplied by.
    ///
    /// One for an unexplained exit and for the merger convention, so both leave
    /// the last close exactly as it was. `Decimal` multiplication by one is
    /// exact and changes no digit, which is what keeps a run with no delistings
    /// attached identical to one from before this existed.
    pub fn terminal_factor(&self) -> Result<Decimal, EngineError> {
        let delisting_return = match self {
            ExitMark::Observed(value) => *value,
            ExitMark::Imputed { value, .. } => *value,
            ExitMark::Unexplained => Decimal::ZERO,
        };
        Decimal::ONE
            .checked_add(delisting_return)
            .ok_or_else(|| EngineError::math("applying a delisting return to a terminal mark"))
    }
}

/// One security's classified exit, resolved when the dataset is attached.
///
/// The convention is chosen here rather than at exit time because it is a
/// property of the run, recorded in the configuration hash, and not a property
/// of any one holding.
#[derive(Debug, Clone, Copy)]
struct ClassifiedExit {
    date: Date,
    mark: ExitMark,
}

/// Resolve one stored delisting into the mark the engine applies.
///
/// A record carrying an observed return is honoured, because that is a
/// measurement and the whole point of keeping the three states apart. Anything
/// else is imputed here under [`flat_convention_for`], including a record that
/// already carries an imputation of its own. The stored one was chosen by
/// whoever curated the file; this one is chosen by the configuration the trial
/// log records, and only the second is described by the recorded hash.
fn mark_of(delisting: &Delisting) -> ExitMark {
    match delisting.terminal {
        TerminalValue::Observed(value) => ExitMark::Observed(value),
        TerminalValue::Imputed { .. } | TerminalValue::Unknown => {
            let convention = flat_convention_for(delisting.reason);
            ExitMark::Imputed {
                value: convention.value(),
                convention,
            }
        }
    }
}

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
    /// Cash dividends, ascending by ex-date. Empty until
    /// [`Panel::with_dividends`] attaches any, which is what keeps a run with
    /// no actions file byte-identical to one from before this existed.
    dividends: Vec<CashDividend>,
    /// Classified exits, ascending by date. Empty until
    /// [`Panel::with_delistings`] attaches any, on the same rule.
    exits: Vec<ClassifiedExit>,
}

impl Series {
    /// Cash dividends whose ex-date falls in `(from, to]`, summed.
    ///
    /// One helper rather than the arithmetic written twice, because the two
    /// return loops in this crate are duplicated already and a boundary rule
    /// that drifts between them would be invisible.
    ///
    /// # Which holder the cash belongs to
    ///
    /// The *ex-date* is the first day a share trades without the right to the
    /// declared dividend, which is why the price drops by roughly the dividend
    /// that morning. So the window is half-open at the start and closed at the
    /// end. A dividend going ex exactly on `from` belongs to whoever held the
    /// share the day before, and the close of `from` that this holder paid has
    /// already given the cash up. A dividend going ex exactly on `to` is
    /// collected, because the drop it causes is inside the return being
    /// measured.
    ///
    /// # Why the amount is added as it arrives
    ///
    /// The vendor restates dividend amounts onto the same basis as its adjusted
    /// close, so the figure shipped against a date before a later split is
    /// already the split-adjusted per-share amount and adds straight to
    /// `close`. Measured 2026-08-10 on Walmart's dividend of 2023-03-16,
    /// publicly declared at 0.57 per share ahead of the 3-for-1 split of
    /// 2024-02-26 and shipped as the split-adjusted 0.57 / 3: adding it
    /// unscaled to the adjusted close reproduces the vendor's own
    /// dividend-adjusted return to 1.1e-5, which is the rounding of that
    /// series itself, while first scaling it by the bar's
    /// `close / close_unadjusted` lands 2.7e-3 away. Scaling here would be a
    /// bug that reads like care.
    ///
    /// # What this deliberately does not model
    ///
    /// Cash received mid-month sits uninvested until `to`. Reinvesting it on
    /// the ex-date needs a daily loop, and at rebalance granularity holding it
    /// is the standard approximation and the honest one available.
    pub fn dividend_cash_in(&self, from: Date, to: Date) -> Result<Decimal, EngineError> {
        let mut total = Decimal::ZERO;
        for dividend in &self.dividends {
            if dividend.ex_date > from && dividend.ex_date <= to {
                total = total.checked_add(dividend.amount).ok_or_else(|| {
                    EngineError::math("summing cash dividends over a holding period")
                })?;
            }
        }
        Ok(total)
    }

    /// How the delistings dataset classifies an exit whose last bar is
    /// `last_bar` and which was noticed on `as_of`.
    ///
    /// # Why the window has both ends
    ///
    /// The lower end is the probe's measurement. On five of five names checked
    /// across 1999 and 2026 the last bar this vendor holds is dated exactly on
    /// the exit action, so there is no post-exit price tail and an event is
    /// never dated before the series stops.
    ///
    /// The upper end is what makes a temporary gap different from an exit, and
    /// it is not decoration. A name that stops trading for one month and
    /// resumes triggers the absent-bar rule at the gap, and its real delisting
    /// years later is still an event dated after the last bar. Without the
    /// upper bound that event would classify the gap, and a month of missing
    /// data would be charged a delisting return. The bound is also what rule 4
    /// requires of any accessor: nothing here may read a record dated after the
    /// date being decided on.
    ///
    /// The first matching event wins. Events are ascending by date, and a
    /// security with two exits inside one holding month is a data fault rather
    /// than a case to arbitrate.
    pub fn exit_mark_in(&self, last_bar: Date, as_of: Date) -> ExitMark {
        self.exits
            .iter()
            .find(|exit| exit.date >= last_bar && exit.date <= as_of)
            .map(|exit| exit.mark)
            .unwrap_or(ExitMark::Unexplained)
    }

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

    /// Closes on every bar dated in `[from, through]`, ascending.
    ///
    /// # Why this one is closed at both ends
    ///
    /// [`Series::bars_in`] counts `(after, through]`, which is the right shape
    /// for a coverage denominator: it counts the days that elapsed between two
    /// endpoint month-ends. A series of daily *returns* over that same span
    /// needs one more price than that, because the first return's denominator
    /// is the close at `from` itself. Closed at both ends, this returns exactly
    /// the prices those returns are computed from, and the number of returns it
    /// yields equals the number [`Series::bars_in`] reports.
    ///
    /// Bounded by `through`, so like every other accessor here it cannot see
    /// past the date it is told.
    pub fn closes_spanning(&self, from: Date, through: Date) -> Vec<Decimal> {
        let start = self.bars.partition_point(|bar| bar.date < from);
        let end = self.bars.partition_point(|bar| bar.date <= through);
        self.bars[start..end.max(start)]
            .iter()
            .map(|bar| bar.close)
            .collect()
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

    /// Dollar volume, `close * volume`, for every bar in `(after, through]`.
    ///
    /// # Why the product is on a consistent basis
    ///
    /// The vendor documents `volume` as "adjusted for stock splits and stock
    /// dividends", the same adjustment its `close` carries, recorded verbatim
    /// at `ContinuationDocs/2026-08-09-sharadar-research.md` line 174. A split
    /// therefore divides the price and multiplies the share count by the same
    /// factor, so the product is unchanged by it and equals as-traded dollar
    /// volume up to that identity. Mixing an adjusted price with an unadjusted
    /// count would put a 3-for-1 split straight into the number and make a
    /// name look three times as thin the day it split.
    ///
    /// # Why the window is half-open at the start
    ///
    /// The same convention as [`Series::bars_in`], because the coverage rule is
    /// what guarantees this window is populated at all. A screen counting a
    /// different set of days from the rule that admitted the name would make
    /// the guarantee mean nothing.
    pub fn dollar_volumes_in(
        &self,
        after: Date,
        through: Date,
    ) -> Result<Vec<Decimal>, EngineError> {
        let start = self.bars.partition_point(|bar| bar.date <= after);
        let end = self.bars.partition_point(|bar| bar.date <= through);
        self.bars[start..end.max(start)]
            .iter()
            .map(|bar| {
                bar.close
                    .checked_mul(bar.volume)
                    .ok_or_else(|| EngineError::math("computing a daily dollar volume"))
            })
            .collect()
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
    /// Whether dividends have been attached at all, which is a different
    /// question from whether any arrived. True after an attachment that matched
    /// nothing, because the run still read an actions file and still reports
    /// dividend-inclusive returns.
    dividends_attached: bool,
    /// Records naming a security this panel does not hold.
    unmatched_dividends: usize,
    /// Records that were not cash dividends, and so were not consumed.
    non_cash_actions: usize,
    /// Whether a delistings dataset was attached at all, which is a different
    /// question from whether any exit was classified. True after an attachment
    /// that matched nothing, because the run still read a delistings file and
    /// still reports imputed exits.
    delistings_attached: bool,
    /// Delisting records naming a security this panel does not hold.
    unmatched_delistings: usize,
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
                dividends: Vec::new(),
                exits: Vec::new(),
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
            dividends_attached: false,
            unmatched_dividends: 0,
            non_cash_actions: 0,
            delistings_attached: false,
            unmatched_delistings: 0,
        })
    }

    /// A new panel carrying the cash dividends in `records`.
    ///
    /// Consumes the panel and returns a new one rather than mutating in place,
    /// so a caller cannot end up holding a panel whose dividend state changed
    /// underneath it.
    ///
    /// Only [`DividendKind::Cash`] is consumed. Splits and spin-offs already
    /// live in the adjusted close, and a stock dividend does too, so adding any
    /// of them as cash would count the same event twice and bias returns up.
    /// Everything not consumed is counted rather than dropped quietly, because
    /// a census of what was ignored is the only way anyone notices that a file
    /// was mostly ignored.
    pub fn with_dividends(self, records: &[ActionRecord]) -> Result<Panel, EngineError> {
        let mut by_identity: BTreeMap<String, Vec<CashDividend>> = BTreeMap::new();
        let mut non_cash_actions = 0usize;

        for record in records {
            let CorporateAction::Dividend {
                amount,
                kind: DividendKind::Cash,
            } = record.action
            else {
                non_cash_actions += 1;
                continue;
            };

            // `validate_action` holds this at the writer boundary, and a record
            // built in memory has never been past it. A non-positive amount
            // reaching the sum would move a return without anyone having
            // distributed anything.
            if amount <= Decimal::ZERO {
                return Err(EngineError::NonPositiveDividend {
                    ticker: record.asset.ticker.clone(),
                    date: record.effective,
                    amount,
                });
            }

            by_identity
                .entry(identity(&record.asset))
                .or_default()
                .push(CashDividend {
                    ex_date: record.effective,
                    amount,
                });
        }

        // Destructured rather than updated field by field: `..self` cannot
        // follow a move out of `self.securities`, and naming the carried-over
        // fields here means a future field cannot be forgotten silently.
        let Panel {
            securities,
            dates,
            month_ends,
            delistings_attached,
            unmatched_delistings,
            ..
        } = self;

        let mut unmatched_dividends = 0usize;
        let securities: Vec<Series> = securities
            .into_iter()
            .map(|series| {
                let mut dividends = by_identity.remove(&series.identity).unwrap_or_default();
                // Ascending by ex-date, because the sum walks them in order and
                // a fixture that happened to supply them sorted would otherwise
                // be the only reason this held.
                dividends.sort_by_key(|dividend| dividend.ex_date);
                Series {
                    dividends,
                    ..series
                }
            })
            .collect();

        // Whatever is left named a security the panel does not hold. Counted
        // and surfaced rather than treated as an error: a universe file is a
        // sample of the master, so an actions file covering the master is
        // expected to overhang it.
        for leftover in by_identity.values() {
            unmatched_dividends += leftover.len();
        }

        Ok(Panel {
            securities,
            dates,
            month_ends,
            dividends_attached: true,
            unmatched_dividends,
            non_cash_actions,
            delistings_attached,
            unmatched_delistings,
        })
    }

    /// A new panel carrying the classified exits in `delistings`.
    ///
    /// Consumes and returns, exactly as [`Panel::with_dividends`] does and for
    /// the same reason. Attaching classifies only: nothing here decides that a
    /// security stopped trading. That trigger is the absent bar in
    /// [`crate::portfolio::advance`] and it is unchanged by this round, so a
    /// delisting record for a name that kept trading changes nothing at all.
    pub fn with_delistings(self, delistings: &[Delisting]) -> Result<Panel, EngineError> {
        let mut by_identity: BTreeMap<String, Vec<ClassifiedExit>> = BTreeMap::new();
        for record in delistings {
            by_identity
                .entry(identity(&record.asset))
                .or_default()
                .push(ClassifiedExit {
                    date: record.date,
                    mark: mark_of(record),
                });
        }

        // Destructured for the same reason `with_dividends` is: `..self` cannot
        // follow a move out of `self.securities`, and naming the carried-over
        // fields means a future field cannot be forgotten silently.
        let Panel {
            securities,
            dates,
            month_ends,
            dividends_attached,
            unmatched_dividends,
            non_cash_actions,
            ..
        } = self;

        let mut unmatched_delistings = 0usize;
        let securities: Vec<Series> = securities
            .into_iter()
            .map(|series| {
                let mut exits = by_identity.remove(&series.identity).unwrap_or_default();
                // Ascending by date, because the lookup takes the first match
                // in the window and a fixture that happened to supply them
                // sorted would otherwise be the only reason that held.
                exits.sort_by_key(|exit| exit.date);
                Series { exits, ..series }
            })
            .collect();

        // Whatever is left named a security the panel does not hold. Counted
        // rather than refused, on the same rule the dividend attachment
        // follows: a universe file is a sample of the master, so a market-wide
        // delistings file is expected to overhang it.
        for leftover in by_identity.values() {
            unmatched_delistings += leftover.len();
        }

        Ok(Panel {
            securities,
            dates,
            month_ends,
            dividends_attached,
            unmatched_dividends,
            non_cash_actions,
            delistings_attached: true,
            unmatched_delistings,
        })
    }

    pub fn securities(&self) -> &[Series] {
        &self.securities
    }

    /// Whether an actions file was attached, however little of it applied.
    pub fn dividends_attached(&self) -> bool {
        self.dividends_attached
    }

    /// Dividend records naming a security outside this panel.
    pub fn unmatched_dividends(&self) -> usize {
        self.unmatched_dividends
    }

    /// Action records that were not cash dividends, and so changed nothing.
    pub fn non_cash_actions(&self) -> usize {
        self.non_cash_actions
    }

    /// Whether a delistings file was attached, however little of it applied.
    pub fn delistings_attached(&self) -> bool {
        self.delistings_attached
    }

    /// Delisting records naming a security outside this panel.
    pub fn unmatched_delistings(&self) -> usize {
        self.unmatched_delistings
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
