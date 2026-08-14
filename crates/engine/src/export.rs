//! The characteristic panel export: what the engine knows, as one wide table.
//!
//! # What this is for
//!
//! Milestone 4's panel and milestone 6's first input. Every column here is
//! already computed somewhere in this crate at every formation, and until now
//! none of it left the process. The fit script needs the same numbers the
//! strategies traded on, so the export calls the strategies' own functions
//! rather than reimplementing them, and `x_r4` pins the result by equality
//! against the rebalances themselves.
//!
//! # Why this records no trial
//!
//! It ranks nothing, holds nothing, computes no return series and no Sharpe.
//! The table is an input to a model that has not been fitted, and whatever is
//! fitted on it is counted where that model's predictions are backtested. See
//! [`NOT_A_TRIAL`], which is printed with every export so a transcript explains
//! itself, and `x_r6`, which holds that the log is byte-identical afterwards.
//!
//! # The two rules the columns follow
//!
//! A characteristic that cannot be computed is NULL. Never zero, never a
//! median, never the previous month's value carried forward. Imputation is a
//! modelling choice and belongs in the fit script where a reader can see it;
//! baked in here it would be inherited silently by everything downstream, and
//! zero is an ordinary value for a momentum signal, a share change and a
//! dividend yield alike.
//!
//! Nothing reads a date after the month-end it is written against, except the
//! forward label, which is labelled as forward in its own column name. Every
//! accessor this module calls takes the date it may see, and the windows come
//! from the configuration rather than from a literal here, which is what makes
//! `x_r4`'s equality against the rebalances meaningful rather than circular.
//!
//! # Which month-ends get rows
//!
//! From `signal_lookback_months` to the end of the panel. That is the shortest
//! of the configured windows, not the deepest, and the difference is a real
//! amount of data: on the shipped configuration the deepest window is
//! thirty-six months, so holding the start back to it would discard two years
//! of rows in which eight of the eleven columns are perfectly computable.
//!
//! The shortest window is the floor because it is the one the ELIGIBILITY flag
//! needs. `momentum::tradable` reads the month-end a lookback before the one it
//! is deciding on, so before that point the flag has no window at all and a row
//! would carry a boolean somebody had to invent. Every other column simply goes
//! null until its own window fits, which is the rule the paragraph above
//! already states for a name with no filing. A partial window is never used: an
//! eleven-month slice of a thirty-six month volatility is a different measure
//! wearing this one's name, not a shorter estimate of it.

use ingest::PanelRow;
use rust_decimal::{Decimal, MathematicalOps};

use crate::config::BacktestConfig;
use crate::error::EngineError;
use crate::panel::Panel;
use crate::{conservative, liquidity, lowvol, momentum, portfolio, value};

/// Printed with every export, so a transcript explains itself.
pub const NOT_A_TRIAL: &str = "\
This is a data export, not a backtest, and no trial is recorded here. It
  ranks nothing, holds nothing, and computes no return series and no Sharpe.
  The characteristics and the forward label it writes are inputs to a model
  that has not been fitted yet, and whatever is fitted on them is counted where
  its predictions are backtested.";

/// A window the export cannot write a column without.
///
/// Refused rather than defaulted, on the rule `crate::conservative::required`
/// follows one strategy over. A column headed `vol_monthly_36m` filled from a
/// window of some other length is a mislabelled number, and the configuration
/// hash the file records would not say which length produced it.
fn required(value: Option<usize>, field: &'static str) -> Result<usize, EngineError> {
    value.ok_or(EngineError::ExportWindowMissing { field })
}

/// One row per (security, month-end) where the security has a bar at that
/// month-end.
///
/// # Why the missing bar removes the row rather than nulling the columns
///
/// A name with no bar at the month-end was not a thing anyone could have bought
/// that day. Several of its characteristics would still compute off history, so
/// a row would exist carrying real-looking numbers for a name that was not
/// there, and the consumer would have to know to filter it. Absence is the
/// honest shape and `x_r7` pins it.
pub fn characteristics(
    panel: &Panel,
    config: &BacktestConfig,
) -> Result<Vec<PanelRow>, EngineError> {
    // The same door a backtest goes through. The export carries the recorded
    // digests into the file's own metadata, so a configuration naming file A
    // while the panel holds file B would publish an artifact under a provenance
    // block describing data it never read.
    crate::run::check_wiring(panel, config)?;
    // The same relation the backtest path checks, before any index arithmetic.
    // The loop below starts at the lookback, so a skip at or past it underflows
    // on the very first row.
    crate::run::check_signal_window(config)?;

    // Presence agreeing with the configuration is not presence. `check_wiring`
    // passes a run that names no filings over a panel carrying none, which is a
    // legitimate momentum backtest and an export whose book-to-market column is
    // null from top to bottom. A whole absent column is not a characteristic
    // that could not be computed, it is a dataset nobody attached, and the two
    // are indistinguishable downstream once written.
    if !(panel.dividends_attached()
        && panel.delistings_attached()
        && panel.marketcaps_attached()
        && panel.filings_attached())
    {
        return Err(EngineError::ExportMissingInputs {
            dividends: panel.dividends_attached(),
            delistings: panel.delistings_attached(),
            marketcaps: panel.marketcaps_attached(),
            filings: panel.filings_attached(),
        });
    }

    let volatility_months = required(
        config.volatility_lookback_months,
        "volatility_lookback_months",
    )?;
    let average_months = required(
        config.payout_share_average_months,
        "payout_share_average_months",
    )?;
    let dividend_months = required(
        config.payout_dividend_trailing_months,
        "payout_dividend_trailing_months",
    )?;
    let staleness = config
        .book_staleness_days
        .ok_or(EngineError::ValueStalenessMissing)?;

    let month_ends = panel.month_ends();
    // The ELIGIBILITY lead-in, which is momentum's window and the shortest of
    // the four, rather than the deepest of them. See the note on which
    // month-ends get rows: past this point every column that cannot be computed
    // yet is null, which is the rule the export already follows for a name with
    // no filing, so the deeper windows do not need to hold the start date back.
    let lead_in = config.signal_lookback_months;
    if month_ends.len() <= lead_in {
        return Err(EngineError::InsufficientHistory {
            months: month_ends.len(),
            required: lead_in + 1,
        });
    }

    let mut rows = Vec::new();
    for index in lead_in..month_ends.len() {
        let date = month_ends[index];
        // Momentum's window, and the window every 12/1 column spans. Taken from
        // the configuration rather than written as 12 and 1, so a column here
        // and the strategy that ships it cannot disagree about what they span.
        // Safe to index without a guard: the loop starts at exactly this
        // lookback, which is what makes it the eligibility lead-in.
        let window_start = month_ends[index - config.signal_lookback_months];
        let window_end = month_ends[index - config.signal_skip_months];
        // The two deeper windows, absent until they fit. `None` here becomes a
        // null column rather than a panic or a truncated window: a volatility
        // over eleven of its thirty-six months is not a shorter estimate of the
        // same thing, it is a different measure under this one's name.
        let volatility_window = index
            .checked_sub(volatility_months)
            .map(|start| &month_ends[start..=index]);
        let dividend_window_start = index
            .checked_sub(dividend_months)
            .map(|start| month_ends[start]);
        // The label's closing month-end. `None` at the last one, which is what
        // makes the final month's label null rather than a return over no time.
        //
        // `index + 1` is one CALENDAR month, not merely the next observation.
        // `Panel::from_bars` refuses a panel whose month-ends skip a calendar
        // month, so index arithmetic here is calendar arithmetic. Without that
        // guard this label would silently span two months across a data gap and
        // still look like a monthly return.
        let label_end = month_ends.get(index + 1).copied();

        // Computed once for the whole cross-section rather than per name,
        // because the coverage denominator is a property of the calendar and
        // this is the function the strategies call.
        let tradable = momentum::tradable(panel, config, date, window_start, window_end)?;

        for (position, series) in panel.securities().iter().enumerate() {
            if series.close_on(date).is_none() {
                continue;
            }

            let (label_return_1m, label_delisted_in_window) = match label_end {
                None => (None, None),
                Some(label_end) => match portfolio::total_return(series, date, label_end)? {
                    None => (None, None),
                    Some((value, delisted)) => (Some(value), Some(delisted)),
                },
            };

            rows.push(PanelRow {
                asset: series.asset.clone(),
                month_end: date,
                momentum_12_1: momentum::signal(panel, position, window_start, window_end),
                vol_daily_12m: lowvol::volatility(panel, position, window_start, window_end),
                vol_monthly_36m: match volatility_window {
                    None => None,
                    Some(window) => conservative::monthly_total_return_volatility(series, window)?,
                },
                // The log, not the level. A change of vendor units multiplies
                // every market cap by one constant and so adds one constant to
                // every log, which the consumer's cross-sectional rank
                // transform erases exactly. Non-positive has no logarithm and
                // is absent rather than clamped: the provider quantises to a
                // tenth of a million, so a company below that quantum arrives
                // as a real zero, and log of zero is not a small number.
                log_marketcap: series
                    .marketcap_at_month_end(date)
                    .filter(|marketcap| *marketcap > Decimal::ZERO)
                    .and_then(|marketcap| marketcap.checked_ln()),
                book_to_market: value::book_to_market(panel, position, date, staleness),
                dividend_yield_12m: match dividend_window_start {
                    None => None,
                    Some(start) => conservative::dividend_yield(series, start, date)?,
                },
                // `share_change` already returns `None` when the window runs
                // off the start of the panel, so the guard is stated rather
                // than needed. It is here because the rule is uniform across
                // the three windowed columns and a reader should not have to
                // open a second module to find out that this one is the
                // exception.
                share_change_24m: (index >= average_months)
                    .then(|| conservative::share_change(series, month_ends, index, average_months))
                    .flatten(),
                median_dollar_volume_12m: liquidity::median_dollar_volume(
                    panel,
                    position,
                    window_start,
                    window_end,
                )?,
                // `tradable` is ascending, so membership is a binary search
                // rather than a scan inside a loop over the same securities.
                eligible: tradable.binary_search(&position).is_ok(),
                label_return_1m,
                label_delisted_in_window,
            });
        }
    }

    Ok(rows)
}
