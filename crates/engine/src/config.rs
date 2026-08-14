//! Every constant the run is defined by, in one struct that gets hashed.
//!
//! # Why the constants live in a struct rather than as `const` items
//!
//! Rule 2 requires the trial log to record a hash of the strategy
//! configuration, and a hash is only useful if it covers everything that could
//! change the answer. A scattering of `const` items cannot be hashed, so a
//! reader of the log would have to trust that the constants in the source
//! today are the constants that produced the entry. Gathering them into one
//! serialisable struct makes the hash mechanically complete: adding a field
//! that is left out of the canonical form is not possible, because the
//! canonical form is built from the serialisation rather than written by hand.
//!
//! The defaults are in [`BacktestConfig::momentum_v0`],
//! [`BacktestConfig::lowvol_v0`] and [`BacktestConfig::conservative_v0`], one
//! per research program.

use std::collections::BTreeMap;

use rigor::ConfigHash;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::error::EngineError;

/// The research program identifier the momentum configuration belongs to.
pub const PROGRAM: &str = "momentum-v0";

/// The research program identifier the low-volatility configuration belongs to.
///
/// A separate program, not a variant of momentum, because rule 2 scopes `N` to
/// a research program and these are two different hypotheses about what
/// predicts returns. Filing them together would make each one's scoped N look
/// like the other's search had been spent on it.
pub const LOWVOL_PROGRAM: &str = "lowvol-v0";

/// The research program identifier the conservative-formula configuration
/// belongs to.
///
/// A third program rather than a variant of either of the first two, on the
/// same rule [`LOWVOL_PROGRAM`] is filed under. Van Vliet and Blitz's
/// conservative formula is a composite of low volatility, momentum and net
/// payout yield, which is a different hypothesis about what predicts returns
/// from either leg on its own, so it carries its own scoped `N`.
pub const CONSERVATIVE_PROGRAM: &str = "conservative-v0";

/// The research program identifier the value configuration belongs to.
///
/// A fourth program on the rule the other three follow. Book-to-market is a
/// different hypothesis about what predicts returns from momentum, from low
/// volatility, and from the conservative composite, so it carries its own
/// scoped `N`.
pub const VALUE_PROGRAM: &str = "value-v0";

/// The research program identifier the ridge configuration belongs to.
///
/// A fifth program on the rule the other four follow. A fitted linear model over
/// every characteristic at once is a different hypothesis from any single
/// characteristic on its own, and rule 5 names the linear model as the baseline
/// the machine-learning families must beat, so it carries its own scoped `N`.
pub const RIDGE_PROGRAM: &str = "ridge-v0";

/// The variant of [`LOWVOL_PROGRAM`] that raises the price floor to $10.
pub const VARIANT_PRICE_FLOOR_10: &str = "price-floor-10";

/// The variant of [`LOWVOL_PROGRAM`] that drops the least liquid names.
pub const VARIANT_LIQUIDITY_SCREENED: &str = "liquidity-screened";

/// The variant of [`LOWVOL_PROGRAM`] that weights its holdings by market cap.
///
/// The strongest artifact check the low-volatility literature has. Equal
/// weighting overweights small names, so if the advantage lives
/// disproportionately in the small end, part of it is a size and illiquidity
/// effect rather than a property of calm stocks.
pub const VARIANT_VALUE_WEIGHTED: &str = "value-weighted";

/// The name recorded in [`BacktestConfig::delisting_convention`] when a run
/// imputes delisting returns.
///
/// One string rather than a per-reason set, because the rule is flat. A
/// performance-related exit takes Shumway (1997)'s -30 percent whatever the
/// venue, following Jensen, Kelly and Pedersen (2023) for a dataset that
/// publishes no delisting returns, and a merger takes zero. See
/// `ingest::flat_convention_for` for why the venue is not consulted.
///
/// A future round that changes the figure changes this string, which moves
/// every config hash under it. That is the point: a run at -30 and a run at
/// -100 are two hypotheses about what a delisting cost, not one measured twice.
pub const DELISTING_CONVENTION: &str = "shumway_1997_flat";

/// Every `(program, variant)` pair this engine has a configuration for.
///
/// # Why a registry rather than only a `match`
///
/// The near-miss this closes is a configuration that is reachable through the
/// command line without anything having asserted that it records as its own
/// trial. A `match` arm added on its own is runnable the moment it compiles and
/// nothing notices. Listing the pairs as data instead means one test can walk
/// every pair that exists, today and after the next one is added, and check
/// that each records a distinct configuration hash under the bare program name.
///
/// [`BacktestConfig::for_program`] refuses anything absent from here, so the
/// registry is the door rather than a description of it. A pair added to the
/// match without being listed is unrunnable; a pair listed without a match arm
/// resolves to nothing and the walk fails on it.
pub const RUNNABLE: &[(&str, Option<&str>)] = &[
    (PROGRAM, None),
    (LOWVOL_PROGRAM, None),
    (LOWVOL_PROGRAM, Some(VARIANT_PRICE_FLOOR_10)),
    (LOWVOL_PROGRAM, Some(VARIANT_LIQUIDITY_SCREENED)),
    (LOWVOL_PROGRAM, Some(VARIANT_VALUE_WEIGHTED)),
    (CONSERVATIVE_PROGRAM, None),
    (VALUE_PROGRAM, None),
    (RIDGE_PROGRAM, None),
];

/// How much of the portfolio each held name gets.
///
/// # Why this is in the hashed configuration rather than a second program
///
/// The signal, the eligibility rules, the cost model and the schedule are
/// identical between a run and its value-weighted rerun. What differs is one
/// vector. That makes it a robustness variant of one hypothesis rather than a
/// second hypothesis, so it is recorded under the bare program name and the
/// difference travels in the config hash, which is what keeps the scoped `N` of
/// rule 2 grouping a hypothesis with its own reruns.
///
/// Serialised as a snake_case string, matching [`Strategy`] and every other
/// token in the canonical form. `Copy` because it is a small tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Weighting {
    /// One over the number of held names, each.
    Equal,
    /// Proportional to market capitalisation at the formation date, with no
    /// cap, no floor and no smoothing. A name with no market cap figure at the
    /// formation, or one carrying zero, cannot be weighted and is excluded from
    /// selection before the ranking rather than after it, so the quintile is a
    /// quintile of names that can actually be held.
    ValueByMarketcap,
}

/// Which signal decides what is held.
///
/// # Why an enum in the hashed configuration rather than two binaries
///
/// The eligibility rules, the cost model, the accounting, and both baselines
/// are identical between the two strategies, and a second code path for them
/// would be comparing two implementations rather than two hypotheses. What
/// differs is one function and one sort direction. Putting the choice in the
/// configuration means it reaches the trial log's config hash automatically,
/// so a low-volatility run cannot be recorded as if it were a momentum run.
///
/// Serialised as a snake_case string, so the canonical form carries
/// `"strategy":"momentum"` or `"strategy":"low_volatility"` rather than an
/// object, matching the case of every other token in the canonical form.
/// `Copy` because it is a small tag and threading a borrow of it through the
/// rebalance loop would buy nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Hold the highest quintile by trailing return over the formation window.
    Momentum,
    /// Hold the LOWEST quintile by the sample standard deviation of daily
    /// returns over the formation window.
    LowVolatility,
    /// Van Vliet and Blitz's conservative formula: inside the low-volatility
    /// half of the large half of the universe, rank on 12-1 momentum and on net
    /// payout yield, average the two ranks, and hold the best.
    ///
    /// # Why this one does not fit the sort-then-slice shape
    ///
    /// The other two strategies score each name with one number and take an end
    /// of the ranking. This one has no single scalar: the composite is the mean
    /// of two *ranks*, and a rank is a property of the whole cross-section
    /// rather than of a name. So the conservative arm owns its selection path
    /// instead of the two being forced into one shape prematurely.
    ConservativeFormula,
    /// Hold the highest quintile by a fitted model's predicted next-month
    /// return, read from an attached predictions file.
    ///
    /// A scalar signal like momentum and value, so it takes the shared
    /// sort-then-slice path, descending, and holds the names the model likes
    /// most. What is new is where the number comes from: nothing computes it,
    /// it is read. That is why the predictions file's digest is in the hash and
    /// why the wiring guard refuses a ridge run with none attached — the
    /// signal is not a property of the price data and cannot be recomputed
    /// from it.
    ///
    /// A name with no prediction at a formation is not rankable and leaves the
    /// field before the quintile is taken, exactly as a name with no filing
    /// does under [`Strategy::Value`].
    Ridge,
    /// Hold the highest quintile by book-to-market: accounting equity over
    /// market equity, both as of the formation date.
    ///
    /// A scalar signal like the first two, so it takes the shared sort-then-slice
    /// path rather than owning its selection the way the composite does. What is
    /// new is where the number comes from: a filing, which is visible only after
    /// it was published and only for as long as it is not stale. Both bounds
    /// live in [`crate::panel::Series::book_equity_usd_as_of`].
    Value,
}

/// One configuration of the backtest.
///
/// `Decimal` fields carry `#[serde(with = "rust_decimal::serde::str")]` for the
/// same reason `rigor::TrialEntry::sharpe` does. The struct feeds a hash, and a
/// number that can be spelled more than one way would let two runs of the same
/// configuration hash differently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BacktestConfig {
    /// Which signal ranks the eligible names, and which end of the ranking is
    /// held.
    pub strategy: Strategy,

    /// How much of the portfolio each held name gets.
    ///
    /// Adding this field moves every previously recorded hash, `Equal` or not,
    /// because the canonical form gains a key. That is inherent to hashing the
    /// whole struct and is accepted, as it was when `strategy`,
    /// `liquidity_floor_fraction` and `delisting_convention` were added.
    pub weighting: Weighting,

    /// Months of price history the formation window spans, counting back from
    /// the rebalance month inclusive. Twelve, with one skipped, is the
    /// literature-standard 12-1 momentum, and the same twelve months are the
    /// volatility estimation window for the low-volatility strategy.
    pub signal_lookback_months: usize,

    /// Months immediately before the rebalance that the signal ignores. One,
    /// which drops the short-term reversal that contaminates a raw 12-month
    /// return, and which for either strategy keeps the rebalance month itself
    /// out of the formation window.
    pub signal_skip_months: usize,

    /// The portfolio holds `1 / quintile_divisor` of the eligible names, taken
    /// from whichever end of the ranking [`Strategy`] names. Five, hence
    /// "quintile".
    pub quintile_divisor: usize,

    /// Minimum unadjusted close on the rebalance date, in dollars. Sub-$5
    /// stocks carry spreads that a flat cost model does not describe, so they
    /// are excluded rather than modelled badly.
    #[serde(with = "rust_decimal::serde::str")]
    pub price_floor: Decimal,

    /// Fraction of the lead-in window's trading days a name must have a bar on.
    #[serde(with = "rust_decimal::serde::str")]
    pub min_coverage: Decimal,

    /// Fraction of the otherwise-eligible names to drop at each rebalance, from
    /// the thin end of a ranking on median daily dollar volume over the
    /// formation window.
    ///
    /// `None` applies no screen at all, which is what every program except the
    /// `liquidity-screened` variant carries, and that path is byte-identical to
    /// the code from before this field existed.
    ///
    /// # Why the field is optional rather than a fraction of zero
    ///
    /// A zero fraction would exclude nothing and would be the same run, so the
    /// two are not distinguishable by their results. They are distinguishable
    /// by their hashes, and `None` keeps two questions separate: whether a
    /// screen applies at all, and how much it removes. Adding the field still
    /// moves every previously recorded hash, `None` or not, because the
    /// canonical form gains a key; that is inherent to hashing the whole
    /// struct and is accepted, same as when `strategy` was added.
    #[serde(with = "rust_decimal::serde::str_option")]
    pub liquidity_floor_fraction: Option<Decimal>,

    /// Charged on traded notional, per side. Ten basis points.
    ///
    /// Source: a conservative all-in estimate for large-cap US equities,
    /// covering half-spread plus commission plus slippage. It is a placeholder
    /// with a known replacement: the execution layer measures implementation
    /// shortfall directly, and this constant goes when that number exists.
    #[serde(with = "rust_decimal::serde::str")]
    pub cost_per_side: Decimal,

    /// Seed the random-ranking baseline draws are derived from.
    pub random_seed: u64,

    /// How many independent random-ranking runs to average over.
    pub random_draws: usize,

    /// The universe sampling rule, recorded so the trial says which slice of
    /// the master it ran on. Mirrors `ingest::universe`.
    pub sample_modulus: u64,
    pub sample_residue: u64,

    /// SHA-256 of the universe file, lowercase hex. This is what makes the run
    /// reproducible against a specific list of securities rather than against
    /// whatever the sampling rule happens to produce today.
    pub universe_sha256: String,

    /// SHA-256 of the corporate actions file, when one was supplied.
    ///
    /// `None` means the run applied no cash dividends and its returns are price
    /// returns. That is a different strategy from the same signal run with
    /// dividends, not the same one measured better, so it hashes differently
    /// and counts as its own trial.
    ///
    /// Hashed over the file's bytes, so refetching an actions file that has not
    /// changed produces the same hash. The failure direction that leaves is
    /// over-counting trials when a refetch changes a byte without changing what
    /// the run sees, which is the safe way round.
    pub actions_sha256: Option<String>,

    /// The published convention delisting returns were imputed under, when a
    /// delistings file was supplied.
    ///
    /// `None` is the behaviour from before this existed. Every exit takes its
    /// last close with no imputation, which is the treatment the pre-delisting
    /// caveat block calls an upward bias.
    ///
    /// # Why this is the rule and not the data
    ///
    /// The convention is what changes the arithmetic. Two runs over the same
    /// prices, one imputing at -30 percent and one not, are two hypotheses
    /// about what a delisting cost a holder. Which securities that rule reached
    /// is a separate question and [`BacktestConfig::delistings_sha256`] carries
    /// it, because both have to be in the hash for a recorded trial to be
    /// reproducible.
    ///
    /// Adding this field moves every previously recorded hash, `None` or not,
    /// because the canonical form gains a key. That is inherent to hashing the
    /// whole struct and is accepted, as it was when `strategy` and
    /// `liquidity_floor_fraction` were added.
    pub delisting_convention: Option<String>,

    /// SHA-256 of the delistings file, when one was supplied.
    ///
    /// Same rules as [`BacktestConfig::actions_sha256`], and it exists for the
    /// same reason. `None` means no delistings file, so every exit took its
    /// last close.
    ///
    /// # Why the convention alone is not enough
    ///
    /// The convention says how a classified exit is marked. It says nothing
    /// about which exits got classified, and that is a property of the file. A
    /// refetch that explains one more name produces a different set of imputed
    /// exits and therefore a different return series under the identical rule.
    /// Without this field those two runs record as one trial and the second
    /// result is not reproducible from what the log holds.
    ///
    /// The unexplained count printed beside a result makes the difference
    /// visible to a reader. Visibility is not identity, and the trial log needs
    /// identity.
    ///
    /// Hashed over the file's bytes, so refetching a delistings file that has
    /// not changed produces the same hash. The failure direction that leaves is
    /// over-counting trials when a refetch changes a byte without changing what
    /// the run sees, which is the safe way round.
    pub delistings_sha256: Option<String>,

    /// SHA-256 of the market cap file, when one was supplied.
    ///
    /// Same rules as [`BacktestConfig::delistings_sha256`], and it exists for
    /// the same reason. The market caps decide both the size screen and the
    /// share-count leg of net payout yield, so a refetch that fills in one more
    /// month-end changes which names were held under an identical rule. Without
    /// this field the two runs record as one trial.
    pub marketcap_sha256: Option<String>,

    /// Months between formations. One re-forms the portfolio every month-end;
    /// three is the quarterly schedule the conservative formula's source
    /// specifies.
    ///
    /// # Why the marks stay monthly whatever this is
    ///
    /// The stride moves the *formation* dates only. The portfolio is still
    /// marked at every month-end in between, so [`crate::run::Series`]'s
    /// `net_monthly` really is monthly and the `sqrt(12)` in
    /// [`crate::portfolio::annualisation_factor`] stays correct. A stride that
    /// also coarsened the marks would leave a quarterly series annualised as
    /// though it were monthly, which inflates the Sharpe by `sqrt(3)` with
    /// nothing visibly failing.
    pub rebalance_every_months: usize,

    /// Months of month-end history the conservative formula's volatility
    /// estimate spans. Thirty-six, from the source paper.
    ///
    /// `None` for a strategy that has no such window, which is every program
    /// except the conservative one. Optional rather than zero for the reason
    /// [`BacktestConfig::liquidity_floor_fraction`] gives: whether a window
    /// exists at all and how long it is are two different questions.
    pub volatility_lookback_months: Option<usize>,

    /// Month-end observations the net payout yield's share ratio averages over,
    /// ending at the month before the formation. Twenty-four, from the source.
    pub payout_share_average_months: Option<usize>,

    /// Months of trailing cash dividends the net payout yield's dividend leg
    /// sums. Twelve, from the ruling that fixed the window the source leaves
    /// unsaid.
    pub payout_dividend_trailing_months: Option<usize>,

    /// Fraction of the otherwise-eligible names to drop at each formation, from
    /// the small end of a ranking on market capitalisation at the formation
    /// date.
    ///
    /// A half, standing in for the source's "the 1,000 largest stocks". The
    /// universe here is a deterministic sample rather than the whole market, so
    /// the count is not transferable and the proportion is preserved instead.
    #[serde(with = "rust_decimal::serde::str_option")]
    pub size_floor_fraction: Option<Decimal>,

    /// SHA-256 of the filings file, when one was supplied.
    ///
    /// Same rules as [`BacktestConfig::marketcap_sha256`], and it exists for the
    /// same reason. The filings decide which book value each name is valued on,
    /// so a refetch that fills in one more filing changes which names were held
    /// under an identical rule.
    pub filings_sha256: Option<String>,

    /// SHA-256 of the curated prices file.
    ///
    /// `Option` only because a configuration can be built before the file is
    /// read; every run through the command line records one. It reaches the
    /// hash like the others, and it is what a predictions file's embedded
    /// provenance is bound against at the engine door.
    pub prices_sha256: Option<String>,

    /// SHA-256 of the predictions file, when one was supplied.
    ///
    /// Same rules as [`BacktestConfig::filings_sha256`], and it matters more
    /// here than for any other dataset. The ridge signal is not computed from
    /// anything on disk, it IS the file, so two runs over two different fits of
    /// the same model are two hypotheses and the digest is the only thing that
    /// tells them apart.
    pub predictions_sha256: Option<String>,

    /// How many days after its publication a filing may still be used.
    ///
    /// `Some(548)` for the value program, `None` for every program that reads no
    /// filings. Eighteen months, which is what Fama and French's own
    /// construction tolerates: their June formation reads the fiscal year ending
    /// in the previous calendar year, so accounting data is up to eighteen
    /// months old by the time it stops being used.
    ///
    /// Optional rather than zero for the reason
    /// [`BacktestConfig::liquidity_floor_fraction`] gives. Whether a staleness
    /// bound exists at all and how long it is are two different questions, and a
    /// bound of zero would mean only same-day filings count, which is a real
    /// configuration rather than a way to spell "no bound".
    pub book_staleness_days: Option<usize>,
}

impl BacktestConfig {
    /// The configuration a `(program, variant)` pair resolves to.
    ///
    /// `None` for any pair absent from [`RUNNABLE`]. An unrecognised program or
    /// variant running the base configuration by default would record a trial
    /// under a name that does not describe what ran, which is the one thing the
    /// config hash exists to make impossible.
    ///
    /// A variant is a robustness rerun of one hypothesis rather than a new
    /// hypothesis, so it is recorded under the bare program string and the
    /// difference between it and the base run lives in the config hash. That is
    /// what keeps the scoped `N` of rule 2 grouping the variants together.
    pub fn for_program(
        program: &str,
        variant: Option<&str>,
        universe_sha256: impl Into<String>,
    ) -> Option<Self> {
        // The registry is the door, checked before anything is built. A pair
        // the match below could serve but the registry does not list is refused
        // here, so adding a configuration without listing it leaves it
        // unrunnable rather than quietly reachable and untested.
        if !RUNNABLE
            .iter()
            .any(|(runnable, listed)| *runnable == program && *listed == variant)
        {
            return None;
        }
        match (program, variant) {
            (PROGRAM, None) => Some(Self::momentum_v0(universe_sha256)),
            (LOWVOL_PROGRAM, None) => Some(Self::lowvol_v0(universe_sha256)),
            // Double the literature's $5, labelled as stricter-than-literature
            // robustness rather than as a replication of it. The $5 floor is
            // already in the base configuration, so rerunning at $5 would be
            // the identical config and would not be a second trial.
            (LOWVOL_PROGRAM, Some(VARIANT_PRICE_FLOOR_10)) => Some(Self {
                price_floor: Decimal::new(10, 0),
                ..Self::lowvol_v0(universe_sha256)
            }),
            // A fifth of the eligible names dropped from the thin end. One
            // value, not a sweep: every extra fraction is another trial and
            // rule 2 counts them all.
            (LOWVOL_PROGRAM, Some(VARIANT_LIQUIDITY_SCREENED)) => Some(Self {
                liquidity_floor_fraction: Some(Decimal::new(2, 1)),
                ..Self::lowvol_v0(universe_sha256)
            }),
            // The same selection weighted by size instead. One weighting, not a
            // sweep of tilts: every intermediate blend between equal and value
            // would be another trial and rule 2 counts them all.
            (LOWVOL_PROGRAM, Some(VARIANT_VALUE_WEIGHTED)) => Some(Self {
                weighting: Weighting::ValueByMarketcap,
                ..Self::lowvol_v0(universe_sha256)
            }),
            (CONSERVATIVE_PROGRAM, None) => Some(Self::conservative_v0(universe_sha256)),
            (VALUE_PROGRAM, None) => Some(Self::value_v0(universe_sha256)),
            (RIDGE_PROGRAM, None) => Some(Self::ridge_v0(universe_sha256)),
            // Listed in the registry with nothing to build. Unreachable while
            // the two agree, and the registry walk in the CLI tests is what
            // notices when they stop agreeing.
            _ => None,
        }
    }

    /// The momentum configuration.
    ///
    /// `Decimal::new(value, scale)` reads as `value * 10^-scale`, so
    /// `Decimal::new(10, 4)` is 0.0010, which is ten basis points.
    pub fn momentum_v0(universe_sha256: impl Into<String>) -> Self {
        Self {
            strategy: Strategy::Momentum,
            weighting: Weighting::Equal,
            signal_lookback_months: 12,
            signal_skip_months: 1,
            quintile_divisor: 5,
            price_floor: Decimal::new(5, 0),
            min_coverage: Decimal::new(80, 2),
            liquidity_floor_fraction: None,
            cost_per_side: Decimal::new(10, 4),
            random_seed: 20_260_810,
            random_draws: 20,
            sample_modulus: ingest::universe::SAMPLE_MODULUS,
            sample_residue: ingest::universe::SAMPLE_RESIDUE,
            universe_sha256: universe_sha256.into(),
            actions_sha256: None,
            delisting_convention: None,
            delistings_sha256: None,
            marketcap_sha256: None,
            rebalance_every_months: 1,
            volatility_lookback_months: None,
            payout_share_average_months: None,
            payout_dividend_trailing_months: None,
            size_floor_fraction: None,
            filings_sha256: None,
            prices_sha256: None,
            predictions_sha256: None,
            book_staleness_days: None,
        }
    }

    /// The low-volatility configuration.
    ///
    /// Every constant is momentum's, deliberately. The formation window, the
    /// eligibility rules, the quintile, the cost, and the baselines are held
    /// fixed so that the difference between the two trials is the signal and
    /// nothing else. `signal_lookback_months` of 12 with one skipped is the
    /// twelve months of daily returns ending at the month before the rebalance.
    pub fn lowvol_v0(universe_sha256: impl Into<String>) -> Self {
        Self {
            strategy: Strategy::LowVolatility,
            ..Self::momentum_v0(universe_sha256)
        }
    }

    /// The conservative-formula configuration.
    ///
    /// Every figure here comes from van Vliet and Blitz (2018) or from a ruling
    /// fixed before any result existed, and none of them is swept. The two that
    /// differ from the momentum defaults for a reason worth stating:
    ///
    /// `price_floor` is zero because the source has no price floor and its size
    /// screen does the liquidity work. `liquidity_floor_fraction` is `None` for
    /// the same reason. `min_coverage` stays at 0.80, because that is this
    /// project's data-quality rule rather than a strategy parameter, and it is
    /// what guards a thirty-six-month volatility estimate against sparse bars.
    pub fn conservative_v0(universe_sha256: impl Into<String>) -> Self {
        Self {
            strategy: Strategy::ConservativeFormula,
            price_floor: Decimal::ZERO,
            liquidity_floor_fraction: None,
            rebalance_every_months: 3,
            volatility_lookback_months: Some(36),
            payout_share_average_months: Some(24),
            payout_dividend_trailing_months: Some(12),
            // 0.5, written as `5 * 10^-1` so the scale is explicit.
            size_floor_fraction: Some(Decimal::new(5, 1)),
            ..Self::momentum_v0(universe_sha256)
        }
    }

    /// The value configuration.
    ///
    /// Book-to-market, highest quintile, equal weight, monthly. Every constant
    /// that is not about the signal is momentum's, deliberately, so the
    /// difference between this trial and the others is the signal and not the
    /// scaffolding.
    ///
    /// # Deviations from Fama and French, all recorded before any result existed
    ///
    /// *Book equity* is `equityusd` as shipped, plain shareholders' equity.
    /// FF1992 adds balance-sheet deferred taxes; FF1993 adds deferred taxes and
    /// the investment tax credit and subtracts the book value of preferred
    /// stock; the three canonical definitions conflict with each other. This
    /// vendor ships no balance-sheet deferred-tax or preferred-stock column at
    /// all, so plain equity is the vendor-constrained choice rather than a
    /// preference.
    ///
    /// *The visibility rule* uses actual filing dates. FF impose a blanket
    /// six-month minimum lag because they could not see filing dates; this rail
    /// can, so the deviation is an improvement rather than a shortcut, and the
    /// 548-day staleness bound is what replaces the upper half of their window.
    ///
    /// *Financials are included.* FF1992 excludes them. The French library's own
    /// live BE/ME portfolios include them, this vendor ships no sector column,
    /// and so exclusion is not implementable from anything on disk. Recorded as
    /// both forced and aligned with the library.
    pub fn value_v0(universe_sha256: impl Into<String>) -> Self {
        Self {
            strategy: Strategy::Value,
            book_staleness_days: Some(548),
            ..Self::momentum_v0(universe_sha256)
        }
    }

    /// The configuration the characteristic panel is exported under.
    ///
    /// # Why the export needs a configuration of its own
    ///
    /// Every window in [`BacktestConfig`] is optional because most programs have
    /// no use for most of them, and a program's configuration leaves the ones it
    /// does not read as `None`. The export reads all of them at once: it writes
    /// the momentum window, the two volatility windows, both payout windows and
    /// the book staleness bound as columns side by side. Run under momentum's
    /// configuration the three payout and volatility windows are `None`, the
    /// [`crate::export`] guard refuses, and run under a configuration that
    /// merely tolerated them the columns would be silently absent instead.
    ///
    /// So every optional window is `Some` here, at the value the program that
    /// owns it ships. The columns are the strategies' own numbers, which is what
    /// `x_r4` pins by equality against the rebalances themselves.
    ///
    /// # Why this is deliberately absent from [`RUNNABLE`]
    ///
    /// [`RUNNABLE`] is the backtest door and everything through it records a
    /// trial. This configuration ranks nothing and holds nothing, so a trial
    /// recorded under it would be a hypothesis nobody put forward. Keeping it
    /// out of the registry makes it unreachable from `--program`, which is what
    /// stops that happening by accident rather than by discipline.
    ///
    /// `strategy` is [`Strategy::Momentum`] because the export's eligibility
    /// flag is momentum's three rules and its formation window is momentum's.
    /// The field decides nothing else here: no ranking happens, so no end of a
    /// ranking is taken. The two screening fractions stay `None` because they
    /// are screens rather than windows, and the export writes the median dollar
    /// volume and the market cap as columns for the consumer to screen on
    /// rather than screening on them itself.
    pub fn panel_export(universe_sha256: impl Into<String>) -> Self {
        Self {
            volatility_lookback_months: Some(36),
            payout_share_average_months: Some(24),
            payout_dividend_trailing_months: Some(12),
            book_staleness_days: Some(548),
            ..Self::momentum_v0(universe_sha256)
        }
    }

    /// The ridge configuration.
    ///
    /// Every constant is momentum's, deliberately, so the difference between
    /// this trial and the single-signal ones is the signal and not the
    /// scaffolding: the same price floor, the same coverage rule, the same
    /// quintile, equal weight, monthly. That is what makes rule 5's real
    /// question here answerable, which is whether the combination beats its
    /// parts rather than whether it was screened differently.
    pub fn ridge_v0(universe_sha256: impl Into<String>) -> Self {
        Self {
            strategy: Strategy::Ridge,
            ..Self::momentum_v0(universe_sha256)
        }
    }

    /// How many month-ends of history a rebalance needs before it can happen.
    ///
    /// The momentum signal reads the close at the end of month `t - lookback`
    /// and the close at the end of month `t - skip`, so a rebalance at month
    /// index `i` needs `i >= lookback`. The conservative formula reads three
    /// windows further back than that, and the deepest of the four is what the
    /// loop has to wait for: a lead-in short enough for one leg and too short
    /// for another would silently drop every name at the early formations
    /// rather than refusing to form them.
    pub fn required_lead_in(&self) -> usize {
        self.signal_lookback_months
            .max(self.volatility_lookback_months.unwrap_or(0))
            .max(self.payout_share_average_months.unwrap_or(0))
            .max(self.payout_dividend_trailing_months.unwrap_or(0))
    }

    /// The exact bytes the configuration hash is taken over.
    ///
    /// Sorted keys, no whitespace, same shape as `rigor::TrialEntry`'s
    /// canonical form and for the same reason: a hash over a struct's
    /// declaration order silently changes when someone reorders fields.
    /// Trailing zeros are stripped first. `Decimal` keeps its scale, so
    /// `0.80` and `0.8` are equal numbers with different string forms, and
    /// without this a coverage requirement typed with a trailing zero would
    /// record as a different trial from the identical one typed without.
    pub fn canonical_json(&self) -> Result<String, EngineError> {
        let normalised = BacktestConfig {
            price_floor: self.price_floor.normalize(),
            min_coverage: self.min_coverage.normalize(),
            cost_per_side: self.cost_per_side.normalize(),
            liquidity_floor_fraction: self
                .liquidity_floor_fraction
                .map(|fraction| fraction.normalize()),
            size_floor_fraction: self
                .size_floor_fraction
                .map(|fraction| fraction.normalize()),
            ..self.clone()
        };
        let value = serde_json::to_value(&normalised).map_err(EngineError::ConfigEncode)?;
        let fields = match value {
            serde_json::Value::Object(fields) => fields,
            // A struct with named fields always serialises to an object. The
            // compiler cannot prove it, so the arm exists and never runs.
            other => unreachable!("a BacktestConfig serialised to {other:?} rather than an object"),
        };
        let sorted: BTreeMap<String, serde_json::Value> = fields.into_iter().collect();
        serde_json::to_string(&sorted).map_err(EngineError::ConfigEncode)
    }

    /// The hash the trial log records for this configuration.
    pub fn config_hash(&self) -> Result<ConfigHash, EngineError> {
        Ok(ConfigHash::of(self.canonical_json()?.as_bytes()))
    }
}
