//! Book-to-market: the value signal, and the units trap inside it.
//!
//! *Book-to-market* is the accounting value of a company's equity divided by
//! what the market says that equity is worth. A high ratio means the market
//! prices the company below its books, which is what "value" names. Fama and
//! French (1992) is the source; the deviations this project takes from their
//! construction are recorded on [`crate::config::BacktestConfig::value_v0`].
//!
//! # The units do not cancel here
//!
//! This is the one ratio in the codebase where the vendor's scale factor
//! matters, and it is worth stating plainly because the neighbouring one is the
//! opposite. `crate::conservative`'s share-count leg divides a market cap by a
//! price and then divides one such quantity by another, so any constant factor
//! cancels twice over and a units mistake is not expressible.
//!
//! Book-to-market divides two figures from two different datasets:
//!
//! - `equityusd` from the filings table, in WHOLE US DOLLARS.
//! - `marketcap` from the daily table, in MILLIONS of US dollars.
//!
//! Nothing cancels. Forgetting [`MILLIONS`] multiplies every ratio by a million
//! and, because it multiplies every one of them by the same million, leaves the
//! ranking and therefore the portfolio completely unchanged. A test that
//! asserted only on which names were held would pass against it. `x_f3` pins the
//! ratio's value for that reason.

use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::panel::Panel;

/// What the market cap dataset's figure is multiplied by to reach whole dollars.
///
/// The daily table ships millions. `equityusd` ships whole dollars. See the
/// module documentation for why this constant cannot be dropped without a test
/// that reads the ratio itself.
pub const MILLIONS: Decimal = Decimal::from_parts(1_000_000, 0, 0, false, 0);

/// Book-to-market for one security at one formation date.
///
/// `None` excludes the name from the ranking, and every branch below is a real
/// state of the world rather than an error:
///
/// - no filing visible yet, or the latest visible one is stale. Both come from
///   [`crate::panel::Series::book_equity_usd_as_of`], which owns both bounds.
/// - book equity at or below zero. Fama and French (1993) exclude negative-book
///   firms from both the breakpoints and the portfolios; the boundary is fixed
///   at `<= 0` here because the French library's own factor construction
///   requires strictly positive book equity. Asness and Frazzini (2013) admit
///   zero, the two conflict, and this project takes positive-only. Fixed before
///   any result existed.
/// - no market cap at the formation. Nothing to divide by.
///
/// A name excluded here never reaches the ranking, so the quintile is a
/// quintile of names that can actually be valued.
pub fn book_to_market(
    panel: &Panel,
    security: usize,
    as_of: Date,
    staleness_days: usize,
) -> Option<Decimal> {
    let series = panel.securities().get(security)?;

    let book = series.book_equity_usd_as_of(as_of, staleness_days)?;
    if book <= Decimal::ZERO {
        return None;
    }

    let market = series
        .marketcap_at_month_end(as_of)?
        .checked_mul(MILLIONS)?;
    if market <= Decimal::ZERO {
        return None;
    }

    book.checked_div(market)
}
