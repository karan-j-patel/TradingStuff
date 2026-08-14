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
        "the price panel's month-ends jump from {before} to {after}, so an entire calendar month \
         has no bar on any security. Every window in this engine is expressed as an index offset \
         into those month-ends, so a gap makes each of them span more calendar time than its name \
         claims: a one-month forward label would cover two months and a 12-1 momentum window \
         thirteen, with every figure still plausible. Refused here rather than tolerated, because \
         a month in which nothing traded anywhere is a missing fetch rather than a quiet market"
    )]
    PanelMonthGap { before: Date, after: Date },

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
        "security {ticker} carries no vendor permanent identifier, so it belongs to no \
         sub-universe and the splits would not be exhaustive"
    )]
    SubUniverseUnplaceable { ticker: String },

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
        "the configuration names a delisting convention ({config_has_convention}) and a \
         delistings file hash ({config_has_hash}) while the panel carries attached delistings \
         ({panel_has_delistings}), and all three must agree. Otherwise the run either reports a \
         delisting treatment it did not apply, or applies one under a hash that does not record \
         which file it came from"
    )]
    DelistingWiringMismatch {
        config_has_convention: bool,
        config_has_hash: bool,
        panel_has_delistings: bool,
    },

    #[error(
        "the configuration names a market cap file ({config_has_marketcap}) while the panel \
         carries attached market caps ({panel_has_marketcaps}), and those must agree. Otherwise \
         the run either ranks on a dataset the trial log does not record, or records one it \
         never read"
    )]
    MarketcapWiringMismatch {
        config_has_marketcap: bool,
        panel_has_marketcaps: bool,
    },

    #[error(
        "the conservative formula needs cash dividends (attached: {dividends}), classified \
         delistings (attached: {delistings}) and market caps (attached: {marketcaps}), and all \
         three are missing or partial here. A conservative formula without its payout leg is a \
         different strategy rather than a degraded one, so it is refused instead of run"
    )]
    ConservativeFormulaMissingInputs {
        dividends: bool,
        delistings: bool,
        marketcaps: bool,
    },

    #[error(
        "the conservative formula needs {field} and the configuration leaves it unset, so the \
         window it names has no length; a strategy missing one of its own windows is a \
         configuration mistake rather than a degraded run"
    )]
    ConservativeWindowMissing { field: &'static str },

    #[error(
        "a market cap for {ticker} on {date} carries the negative amount {marketcap}, and no \
         company is worth less than nothing. This table expresses a missing figure by omitting \
         the row, so a negative that arrives anyway is a corrupt row rather than an absent one"
    )]
    NegativeMarketcap {
        ticker: String,
        date: Date,
        marketcap: rust_decimal::Decimal,
    },

    #[error(
        "the rebalance stride is zero, so the formation loop would never advance; a run that \
         forms a portfolio every zero months is a configuration mistake rather than a schedule"
    )]
    RebalanceStrideZero,

    #[error(
        "the signal skips {skip_months} months out of a {lookback_months}-month lookback, so the \
         formation window ends at or before it starts. Measured rather than reasoned about: at a \
         skip past the lead-in the index arithmetic underflows and panics, and at a skip inside it \
         the window simply reverses, whereupon the momentum signal returns a BACKWARD return and a \
         full backtest completes with a Sharpe. A window with nothing in it is a configuration \
         mistake rather than a signal"
    )]
    SignalWindowInverted {
        lookback_months: usize,
        skip_months: usize,
    },

    #[error(
        "the book staleness bound of {days} days does not fit the signed day arithmetic that \
         ages filings, so it would wrap rather than compare; a bound wider than the calendar \
         is a configuration mistake rather than a tolerance"
    )]
    BookStalenessOutOfRange { days: usize },

    #[error(
        "filings for {ticker} carry two rows for the period ending {period_end}, both filed \
         {as_reported}. The curated writer refuses this shape, so a pair arriving here came \
         around it, and which row the accessor would serve depends on caller order, which is \
         a number nobody chose"
    )]
    DuplicateFiling {
        ticker: String,
        period_end: Date,
        as_reported: Date,
    },

    #[error(
        "the liquidity floor fraction {fraction} is outside [0, 1), and a screen that \
         removes every eligible name or a negative number of them is a configuration \
         mistake rather than a screen"
    )]
    LiquidityFractionOutOfRange { fraction: rust_decimal::Decimal },

    #[error(
        "the size floor fraction {fraction} is outside [0, 1), and a screen that removes every \
         eligible name or a negative number of them is a configuration mistake rather than a \
         screen"
    )]
    SizeFloorFractionOutOfRange { fraction: rust_decimal::Decimal },

    #[error(
        "the configuration records the {dataset} file {recorded} and the panel was built from \
         {attached}. A run whose recorded digest is not the digest of the data it read publishes \
         numbers that cannot be reproduced from the trial log, and the log holds no other \
         description of the input"
    )]
    DatasetDigestMismatch {
        dataset: &'static str,
        recorded: String,
        attached: String,
    },

    #[error(
        "the configuration weights holdings by market cap and the panel carries no attached \
         market caps, so no name can be weighted and none would survive selection. Refused here \
         rather than left to surface as an empty eligible set at every rebalance, which is the \
         same failure describing the wrong cause"
    )]
    ValueWeightingMissingMarketcaps,

    #[error(
        "security {security} is held at the formation on {date} and has no positive market cap \
         there, so it has no weight. Selection excludes an unweightable name before the ranking, \
         so reaching here means the exclusion rule and the weighting disagree about what is \
         holdable"
    )]
    UnweightableHolding { security: usize, date: Date },

    #[error(
        "the turnover fit has {points} point(s) at a single distinct book size, so the line \
         through them is undetermined. A slope invented here would be the most flattering \
         number this diagnostic could emit, so it is refused instead"
    )]
    TurnoverFitUnderdetermined { points: usize },

    #[error(
        "the filings file carries a restated row for {ticker}, period ending {period_end}. \
         A restated figure is dated by its period end while its values were corrected later, so \
         it hands a backtest information that did not exist on the day. It is refused at the \
         attachment rather than filtered downstream, because a filter is a thing a later caller \
         can forget"
    )]
    RestatedFilingRefused { ticker: String, period_end: Date },

    #[error(
        "the value strategy needs book_staleness_days and the configuration leaves it unset, so \
         no bound says how old a filing may be. A default invented here would value names off \
         reports of unknown age under a hash that records no bound at all"
    )]
    ValueStalenessMissing,

    #[error(
        "the configuration names a filings file ({config_has_filings}) while the panel carries \
         attached filings ({panel_has_filings}), and those must agree. Otherwise the run either \
         values names on a dataset the trial log does not record, or records one it never read"
    )]
    FilingsWiringMismatch {
        config_has_filings: bool,
        panel_has_filings: bool,
    },

    #[error(
        "the value strategy needs cash dividends (attached: {dividends}), classified delistings \
         (attached: {delistings}), market caps (attached: {marketcaps}) and filings (attached: \
         {filings}), and at least one is missing here. A book-to-market run without book values \
         has no signal, and one without the other three is a different strategy rather than a \
         degraded one, so it is refused instead of run"
    )]
    ValueMissingInputs {
        dividends: bool,
        delistings: bool,
        marketcaps: bool,
        filings: bool,
    },

    #[error(
        "the characteristic panel export needs cash dividends (attached: {dividends}), classified \
         delistings (attached: {delistings}), market caps (attached: {marketcaps}) and filings \
         (attached: {filings}), and at least one is missing here. The configuration and the panel \
         agree with each other in that state, so nothing before this point refuses it, and the \
         export would write a whole column of nulls rather than say a dataset was absent"
    )]
    ExportMissingInputs {
        dividends: bool,
        delistings: bool,
        marketcaps: bool,
        filings: bool,
    },

    #[error(
        "the characteristic panel export needs {field} and the configuration leaves it unset, so \
         the column it names has no window; a default invented here would write a characteristic \
         under a heading that says how long it is and a hash that does not"
    )]
    ExportWindowMissing { field: &'static str },

    #[error(
        "predictions for {ticker} carry two rows for the month-end {month_end}. The curated \
         writer refuses this shape, so a pair arriving here came around it, and which value the \
         ranking would use depends on caller order, which is a portfolio nobody chose"
    )]
    DuplicatePrediction { ticker: String, month_end: Date },

    #[error(
        "the configuration names a predictions file ({config_has_predictions}) while the panel \
         carries attached predictions ({panel_has_predictions}), and those must agree. Otherwise \
         the run either ranks on a dataset the trial log does not record, or records one it never \
         read"
    )]
    PredictionsWiringMismatch {
        config_has_predictions: bool,
        panel_has_predictions: bool,
    },

    #[error(
        "the ridge program ranks on fitted predictions and the panel carries none, so every \
         formation would rank an empty field. A ridge run without its predictions is not a \
         degraded run, it is a different strategy with no signal at all"
    )]
    RidgeMissingPredictions,

    #[error(
        "the panel's month-ends end at {last_month_end} and the earliest prediction is dated \
         {first_prediction}, so no formation can rank on one. A run whose test window does not \
         overlap its predictions produces no result rather than a short one"
    )]
    PredictionsOutsidePanel {
        last_month_end: Date,
        first_prediction: Date,
    },

    #[error(
        "the attached predictions were fitted against {dataset} {fitted} and this run reads \
         {running}. The model learned its coefficients from one snapshot and would be ranking a \
         backtest built from another, which is a result nobody can reproduce and nothing else \
         would notice: both files are valid, both digests are recorded, and the numbers look \
         ordinary"
    )]
    PredictionsProvenanceMismatch {
        dataset: &'static str,
        fitted: String,
        running: String,
    },

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
