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
//! The defaults are in [`BacktestConfig::momentum_v0`], which is the only
//! configuration this round runs.

use std::collections::BTreeMap;

use rigor::ConfigHash;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::error::EngineError;

/// The research program identifier this configuration belongs to.
pub const PROGRAM: &str = "momentum-v0";

/// One configuration of the momentum backtest.
///
/// `Decimal` fields carry `#[serde(with = "rust_decimal::serde::str")]` for the
/// same reason `rigor::TrialEntry::sharpe` does. The struct feeds a hash, and a
/// number that can be spelled more than one way would let two runs of the same
/// configuration hash differently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BacktestConfig {
    /// Months of price history the signal spans, counting back from the
    /// rebalance month inclusive. Twelve, with one skipped, is the
    /// literature-standard 12-1 momentum.
    pub signal_lookback_months: usize,

    /// Months immediately before the rebalance that the signal ignores. One,
    /// which drops the short-term reversal that contaminates a raw 12-month
    /// return.
    pub signal_skip_months: usize,

    /// The portfolio holds the top `1 / quintile_divisor` of eligible names by
    /// signal. Five, hence "quintile".
    pub quintile_divisor: usize,

    /// Minimum unadjusted close on the rebalance date, in dollars. Sub-$5
    /// stocks carry spreads that a flat cost model does not describe, so they
    /// are excluded rather than modelled badly.
    #[serde(with = "rust_decimal::serde::str")]
    pub price_floor: Decimal,

    /// Fraction of the lead-in window's trading days a name must have a bar on.
    #[serde(with = "rust_decimal::serde::str")]
    pub min_coverage: Decimal,

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
}

impl BacktestConfig {
    /// The one configuration this round runs.
    ///
    /// `Decimal::new(value, scale)` reads as `value * 10^-scale`, so
    /// `Decimal::new(10, 4)` is 0.0010, which is ten basis points.
    pub fn momentum_v0(universe_sha256: impl Into<String>) -> Self {
        Self {
            signal_lookback_months: 12,
            signal_skip_months: 1,
            quintile_divisor: 5,
            price_floor: Decimal::new(5, 0),
            min_coverage: Decimal::new(80, 2),
            cost_per_side: Decimal::new(10, 4),
            random_seed: 20_260_810,
            random_draws: 20,
            sample_modulus: ingest::universe::SAMPLE_MODULUS,
            sample_residue: ingest::universe::SAMPLE_RESIDUE,
            universe_sha256: universe_sha256.into(),
            actions_sha256: None,
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
