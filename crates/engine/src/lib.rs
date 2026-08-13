//! Portfolio accounting and the backtest loop.
//!
//! # Why this is Rust rather than Python
//!
//! `CLAUDE.md` says it plainly: portfolio accounting is where a silent error
//! becomes wrong profit and loss, and compile time is a better place to catch a
//! forgotten dividend than reconciliation is. Nothing in here is here for
//! speed.
//!
//! # The rules this crate is bound by
//!
//! Every one of these is a mandatory-test path, meaning a test is written first
//! and shown to fail against a deliberately broken implementation before the
//! implementation is trusted.
//!
//! - Rule 3, the cost model is always applied. There is no gross-returns mode
//!   and there will not be one. A gross figure may be computed and printed as
//!   an explicitly labelled diagnostic, never as performance.
//! - Rule 4, nothing may read a bar dated after the date it is deciding on.
//! - Rule 5, every result carries its baselines.
//!
//! Sync throughout, `Decimal` for every number that touches money, and no
//! wall-clock anywhere: a backtest that gives a different answer on a different
//! day is not a backtest.

//! # Layout
//!
//! - [`config`] holds every constant the run is defined by, and hashes them.
//! - [`panel`] groups bars by security and owns the trading calendar.
//! - [`momentum`] computes the momentum signal, applies the eligibility rules,
//!   and forms the rebalance for whichever strategy is configured.
//! - [`lowvol`] computes the low-volatility signal.
//! - [`conservative`] computes the conservative formula's two legs and owns its
//!   selection, which averages two ranks rather than sorting one scalar.
//! - [`liquidity`] computes the tradability screen one variant applies.
//! - [`portfolio`] turns weights and prices into returns, with costs.
//! - [`baseline`] runs the comparisons rule 5 requires.
//! - [`run`] is the monthly loop and the report it produces.

pub mod baseline;
pub mod config;
pub mod conservative;
pub mod error;
pub mod liquidity;
pub mod lowvol;
pub mod momentum;
pub mod panel;
pub mod portfolio;
pub mod run;

#[cfg(test)]
mod tests;

pub use config::{
    BacktestConfig, CONSERVATIVE_PROGRAM, DELISTING_CONVENTION, LOWVOL_PROGRAM, PROGRAM, RUNNABLE,
    Strategy, VARIANT_LIQUIDITY_SCREENED, VARIANT_PRICE_FLOOR_10,
};
pub use error::EngineError;
pub use momentum::FormationCensus;
pub use panel::{ExitMark, Panel};
pub use portfolio::ExitCensus;
pub use run::{
    CAVEATS, CAVEATS_WITH_DELISTINGS, CAVEATS_WITH_DIVIDENDS,
    CAVEATS_WITH_DIVIDENDS_AND_DELISTINGS, Report, backtest, caveats,
};
