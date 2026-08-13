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
//! The defaults are in [`BacktestConfig::momentum_v0`] and
//! [`BacktestConfig::lowvol_v0`], one per research program.

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

/// The variant of [`LOWVOL_PROGRAM`] that raises the price floor to $10.
pub const VARIANT_PRICE_FLOOR_10: &str = "price-floor-10";

/// The variant of [`LOWVOL_PROGRAM`] that drops the least liquid names.
pub const VARIANT_LIQUIDITY_SCREENED: &str = "liquidity-screened";

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
];

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
/// `Copy` because it is a two-variant tag and threading a borrow of it
/// through the rebalance loop would buy nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Hold the highest quintile by trailing return over the formation window.
    Momentum,
    /// Hold the LOWEST quintile by the sample standard deviation of daily
    /// returns over the formation window.
    LowVolatility,
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

    /// How many month-ends of history a rebalance needs before it can happen.
    ///
    /// The signal reads the close at the end of month `t - lookback` and the
    /// close at the end of month `t - skip`, so the rebalance at month index
    /// `i` needs `i >= lookback`.
    pub fn required_lead_in(&self) -> usize {
        self.signal_lookback_months
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
