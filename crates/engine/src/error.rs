//! What the engine refuses to do.
//!
//! Every variant here is a refusal rather than a degraded answer. `CLAUDE.md`
//! is explicit that malformed input errors rather than being normalised,
//! clamped, or no-oped, because a backtest that quietly repairs its own input
//! produces a number nobody can trace back to data.

use jiff::civil::Date;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("the price panel is empty, so there is nothing to rebalance on")]
    EmptyPanel,

    #[error(
        "the panel spans {months} month-ends and a rebalance needs {required} of lead-in \
         before the first one, so no rebalance is possible"
    )]
    InsufficientHistory { months: usize, required: usize },

    #[error(
        "every rebalance date had an empty eligible set, so no portfolio was ever formed \
         across {rebalances} rebalance dates"
    )]
    EligibilityCollapse { rebalances: usize },

    #[error(
        "fewer than two monthly observations ({months}), so no Sharpe exists; a single \
         return has no dispersion to divide by"
    )]
    TooFewMonths { months: usize },

    #[error(
        "the return series has no dispersion, so a Sharpe would be a division by zero \
         dressed up as a number"
    )]
    NoDispersion,

    #[error(
        "arithmetic on Decimal overflowed or divided by zero while {context}; this is a \
         data surprise rather than a rounding matter and is not being papered over"
    )]
    Arithmetic { context: &'static str },

    #[error(
        "the portfolio lost its entire value over the month ending {date}, so the drifted \
         weights are undefined"
    )]
    TotalLoss { date: Date },

    #[error(
        "a cash dividend for {ticker} on {date} carries the non-positive amount {amount}, and \
         nothing per-share is distributed at zero or below"
    )]
    NonPositiveDividend {
        ticker: String,
        date: Date,
        amount: rust_decimal::Decimal,
    },

    #[error(
        "the configuration names an actions file ({config_has_actions}) but the panel carries \
         attached dividends ({panel_has_dividends}), and those must agree. Otherwise the run \
         reports a dividend treatment it did not apply"
    )]
    DividendWiringMismatch {
        config_has_actions: bool,
        panel_has_dividends: bool,
    },

    #[error(
        "the liquidity floor fraction {fraction} is outside [0, 1), and a screen that \
         removes every eligible name or a negative number of them is a configuration \
         mistake rather than a screen"
    )]
    LiquidityFractionOutOfRange { fraction: rust_decimal::Decimal },

    #[error("encoding the configuration for hashing failed")]
    ConfigEncode(#[source] serde_json::Error),
}

impl EngineError {
    /// Shorthand for the `Arithmetic` variant, which is raised in enough places
    /// that spelling the struct out each time buries the code that matters.
    pub(crate) fn math(context: &'static str) -> Self {
        EngineError::Arithmetic { context }
    }
}
