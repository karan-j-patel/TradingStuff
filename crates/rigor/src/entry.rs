//! One record in the trial log, and the canonical form its hash is taken over.
//!
//! # The canonical form, and why it is not cosmetic
//!
//! `trials/README.md` fixes the rule already, so this module implements it
//! rather than inventing it. `entry_hash` is the SHA-256 of the entry
//! serialised as JSON with keys sorted, no whitespace, and `entry_hash` itself
//! excluded. Two writers that agree on the data but disagree on formatting
//! produce different hashes, and the chain then fails to verify for a reason
//! that has nothing to do with tampering.
//!
//! Note what the rule is defined over. It is the *data*, not the bytes on
//! disk. Verification parses a line into a [`TrialEntry`] and re-serialises it
//! canonically, so a file whose keys happen to sit in a different order still
//! verifies. Only a change to a field's value moves the hash.
//!
//! # How `sharpe` is encoded, which the README leaves open
//!
//! As a JSON string, so `1.23` is written `"1.23"`. The README's Python
//! example does not say, and this is the first writer, so the choice is made
//! here.
//!
//! The reason is that `sharpe` is hashed. An f64 has no single reproducible
//! text form, so two runs agreeing on the number could disagree on the hash.
//! [`Decimal`] carries its own scale and its string form round trips exactly,
//! which is the property a hash input needs. The genesis entry's `null` is
//! unaffected, since `None` still serialises to JSON `null`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use jiff::Timestamp;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::TrialError;

/// The `prev_hash` the genesis entry carries, since nothing precedes it.
/// Sixty four zeros, the width of a hex SHA-256.
pub const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A single trial.
///
/// A **trial** is one evaluation of one strategy configuration against
/// historical data. Every point in a hyperparameter grid is its own trial, and
/// so is every hypothesis a language model proposed, because what inflates a
/// best-of Sharpe is the number of things tried rather than the number of
/// things a human remembers trying.
///
/// A **research program** is the named line of enquiry a trial belongs to. It
/// scopes the trial count `N`, which is the statistically correct reading,
/// while the lifetime count over every program is the conservative bound. Rule
/// 2 in `CLAUDE.md` requires both to be reported, always.
///
/// Field order here is the order `CLAUDE.md` states, which is deliberate but
/// carries no weight for hashing. `serde_json` writes struct fields in
/// declaration order rather than sorted order, so the canonical form is built
/// through a [`BTreeMap`] instead. See [`TrialEntry::canonical_json`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialEntry {
    /// When the trial completed. RFC 3339, UTC.
    pub timestamp: Timestamp,
    /// Research program identifier, which scopes `N`.
    pub program: String,
    /// Hash of the strategy configuration that was run.
    pub config_hash: String,
    /// Raw Sharpe the trial produced, and it is a diagnostic rather than
    /// performance. `None` for the genesis entry, which is a root rather than
    /// a result, and `None` for a trial that produced no figure.
    ///
    /// `#[serde(with = ...)]` swaps in rust_decimal's string codec for this one
    /// field. Without it the default codec applies, and the default is not
    /// guaranteed to be the string form the hash depends on.
    #[serde(with = "rust_decimal::serde::str_option")]
    pub sharpe: Option<Decimal>,
    /// The previous entry's `entry_hash`, which is what chains them.
    pub prev_hash: String,
    /// This entry's own hash, excluded from its own hash input.
    pub entry_hash: String,
}

impl TrialEntry {
    /// Build an entry and compute its `entry_hash` from the other five fields.
    ///
    /// There is no way to construct one with a hash that does not match its
    /// contents, short of assembling the struct literally, which is what the
    /// tampering tests do on purpose.
    pub fn new(
        timestamp: Timestamp,
        program: impl Into<String>,
        config_hash: impl Into<String>,
        sharpe: Option<Decimal>,
        prev_hash: impl Into<String>,
    ) -> Result<Self, TrialError> {
        // Built with a placeholder hash, then rebuilt with the real one. The
        // placeholder never reaches the hash input, because canonical_json
        // drops the entry_hash key before serialising.
        let draft = TrialEntry {
            timestamp,
            program: program.into(),
            config_hash: config_hash.into(),
            sharpe,
            prev_hash: prev_hash.into(),
            entry_hash: String::new(),
        };
        let entry_hash = draft.compute_hash()?;
        Ok(TrialEntry {
            entry_hash,
            ..draft
        })
    }

    /// The exact bytes this entry's hash is taken over.
    ///
    /// Public because a hash rule nobody can inspect is a hash rule nobody can
    /// check, and because the genesis test asserts on this string directly.
    pub fn canonical_json(&self) -> Result<String, TrialError> {
        let value =
            serde_json::to_value(self).map_err(|source| TrialError::Canonical { source })?;

        let fields = match value {
            serde_json::Value::Object(fields) => fields,
            // A struct with named fields always serialises to a JSON object.
            // Rust cannot prove that at compile time, so the arm exists and is
            // unreachable in practice.
            other => unreachable!("a TrialEntry serialised to {other:?} rather than an object"),
        };

        // BTreeMap is Rust's ordered map, so iterating it yields keys in sorted
        // order and serde_json writes them in that order. serde_json's own Map
        // happens to be a BTreeMap today, but only while its `preserve_order`
        // feature is off, and depending on a dependency's feature flags for
        // correctness of a hash is exactly the sort of silent breakage this
        // file exists to prevent.
        let sorted: BTreeMap<String, serde_json::Value> = fields
            .into_iter()
            .filter(|(key, _)| key != "entry_hash")
            .collect();

        // `to_string` is serde_json's compact form, which is the "no
        // whitespace" half of the rule.
        serde_json::to_string(&sorted).map_err(|source| TrialError::Canonical { source })
    }

    /// SHA-256 of [`TrialEntry::canonical_json`], lowercase hex.
    pub fn compute_hash(&self) -> Result<String, TrialError> {
        Ok(hash_bytes(self.canonical_json()?.as_bytes()))
    }

    /// Whether the recorded `entry_hash` matches a fresh hash of the contents.
    pub fn hash_matches(&self) -> Result<bool, TrialError> {
        Ok(self.compute_hash()? == self.entry_hash)
    }
}

/// SHA-256 of arbitrary bytes, lowercase hex.
///
/// Exposed because callers need it for `config_hash` too, and a second hex
/// encoder somewhere else in the tree is how two encoders end up disagreeing.
pub fn hash_bytes(data: &[u8]) -> String {
    // sha2 0.11 note for anyone reading this after using 0.10. The digest is a
    // `hybrid_array::Array` rather than a `GenericArray`, and it carries no
    // `LowerHex` implementation, so the familiar `format!("{:x}", digest)` does
    // not compile. Encoding by hand is four lines and avoids a `hex` dependency
    // for four lines of work.
    let digest = Sha256::digest(data);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        // Writing into a String is infallible, so the Result is discarded
        // rather than propagated up through an API that has no other way to
        // fail.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// Line 1 of `trials/trials.jsonl`, inline so the test does not depend on
    /// reading a file. The log-level tests read the real file.
    const GENESIS_LINE: &str = r#"{"config_hash":"0000000000000000000000000000000000000000000000000000000000000000","entry_hash":"bbdc8721955c4bb3f0ba915f1306070895d598345f3f5308d09cd78093af6db5","prev_hash":"0000000000000000000000000000000000000000000000000000000000000000","program":"genesis","sharpe":null,"timestamp":"2026-08-08T00:00:00Z"}"#;

    /// The acceptance test for the whole canonical form. If this fails, the
    /// canonical form is wrong. The expected value is not negotiable, it is
    /// published in `trials/README.md`.
    #[test]
    fn the_genesis_entry_reproduces_its_published_hash() {
        let genesis: TrialEntry = serde_json::from_str(GENESIS_LINE).expect("genesis parses");

        assert_eq!(
            genesis.compute_hash().expect("genesis hashes"),
            "bbdc8721955c4bb3f0ba915f1306070895d598345f3f5308d09cd78093af6db5"
        );
    }

    #[test]
    fn the_canonical_form_sorts_keys_drops_entry_hash_and_uses_no_whitespace() {
        let genesis: TrialEntry = serde_json::from_str(GENESIS_LINE).expect("genesis parses");

        assert_eq!(
            genesis.canonical_json().expect("genesis serialises"),
            r#"{"config_hash":"0000000000000000000000000000000000000000000000000000000000000000","prev_hash":"0000000000000000000000000000000000000000000000000000000000000000","program":"genesis","sharpe":null,"timestamp":"2026-08-08T00:00:00Z"}"#
        );
    }

    #[test]
    fn the_recorded_entry_hash_never_enters_its_own_hash_input() {
        let genesis: TrialEntry = serde_json::from_str(GENESIS_LINE).expect("genesis parses");
        let vandalised = TrialEntry {
            entry_hash: "not a hash at all".to_string(),
            ..genesis.clone()
        };

        assert_eq!(
            genesis.canonical_json().expect("hashes"),
            vandalised.canonical_json().expect("hashes"),
        );
    }

    #[test]
    fn a_sharpe_is_written_as_a_string_and_round_trips_exactly() {
        let entry = TrialEntry::new(
            Timestamp::from_str("2026-08-08T12:00:00Z").expect("valid timestamp"),
            "momentum-v1",
            "abc123",
            // A scale that f64 cannot hold exactly, which is the point.
            Some(Decimal::from_str("1.234567890123456789").expect("valid decimal")),
            ZERO_HASH,
        )
        .expect("entry builds");

        let line = serde_json::to_string(&entry).expect("entry serialises");
        assert!(line.contains(r#""sharpe":"1.234567890123456789""#));

        let parsed: TrialEntry = serde_json::from_str(&line).expect("entry parses");
        assert_eq!(parsed, entry);
        assert!(parsed.hash_matches().expect("hashes"));
    }

    #[test]
    fn changing_any_field_changes_the_hash() {
        let base = TrialEntry::new(
            Timestamp::from_str("2026-08-08T12:00:00Z").expect("valid timestamp"),
            "momentum-v1",
            "abc123",
            Some(Decimal::from_str("0.5").expect("valid decimal")),
            ZERO_HASH,
        )
        .expect("entry builds");

        let variants = [
            TrialEntry {
                program: "momentum-v2".to_string(),
                ..base.clone()
            },
            TrialEntry {
                config_hash: "abc124".to_string(),
                ..base.clone()
            },
            TrialEntry {
                sharpe: Some(Decimal::from_str("0.50").expect("valid decimal")),
                ..base.clone()
            },
            TrialEntry {
                sharpe: None,
                ..base.clone()
            },
            TrialEntry {
                timestamp: Timestamp::from_str("2026-08-08T12:00:01Z").expect("valid timestamp"),
                ..base.clone()
            },
        ];

        for variant in variants {
            assert_ne!(
                variant.compute_hash().expect("hashes"),
                base.compute_hash().expect("hashes"),
                "a changed field left the hash alone, so tampering would be invisible"
            );
        }
    }

    #[test]
    fn hash_bytes_matches_a_known_sha256() {
        // The SHA-256 of the empty input, which every implementation agrees on.
        assert_eq!(
            hash_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
