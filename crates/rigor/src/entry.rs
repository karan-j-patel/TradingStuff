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
//!
//! # Non-ASCII text, and the one place it is restricted
//!
//! The canonical form emits raw UTF-8. It does not escape non-ASCII characters
//! as `\uXXXX`. This is what RFC 8785, the JSON Canonicalization Scheme,
//! specifies, and `trials/README.md` states it explicitly so a second
//! implementation cannot get it wrong by following a default.
//!
//! It is worth stating why that sentence had to be written. Python's
//! `json.dumps` defaults `ensure_ascii` to true, so the obvious Python
//! transcription of the rule escapes non-ASCII and hashes a program named
//! `μstructure-v1` differently from the Rust writer. Neither side is wrong
//! in isolation. The rule simply has to say which it is, and now it does.
//!
//! On top of that, [`validate_program`] restricts the one field that is an
//! identifier rather than free text. See its documentation for why a
//! restriction beats careful handling here.
//!
//! # Writers are constrained, readers are not
//!
//! Both [`validate_program`] and [`ConfigHash`] bind what this code *writes*.
//! Neither is applied when parsing a line off disk, and that asymmetry is
//! deliberate rather than an oversight. A tamper-evident record has to be
//! loadable in order to be reported on, whatever it turns out to hold, so a
//! reader that refused a damaged entry would destroy the evidence it exists to
//! show.

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

/// Longest research program identifier a writer may use.
///
/// Arbitrary but not unbounded. An identifier is meant to be typed at a command
/// line and read in a table, and nothing that gets stored forever should accept
/// unbounded input from a caller.
pub const PROGRAM_MAX_LEN: usize = 64;

/// Check that a research program identifier is one this platform will write.
///
/// Permitted: ASCII letters and digits, plus `-`, `_`, and `.`. Non-empty, and
/// at most [`PROGRAM_MAX_LEN`] characters.
///
/// # Why a restriction rather than careful handling
///
/// `program` is an identifier that feeds a hash contract, and `CLAUDE.md`
/// requires validation at trust boundaries. The canonical form is now pinned to
/// raw UTF-8, so a non-ASCII program name would in fact hash consistently. The
/// restriction is not there to paper over that. It is there because a second
/// implementation in another language is the thing most likely to reintroduce
/// the divergence, by reaching for a default such as Python's
/// `ensure_ascii=True`, and an identifier that is ASCII by construction makes
/// that whole class of divergence unreachable rather than merely handled.
///
/// The restriction applies to what this code *writes*. It is deliberately not
/// applied when parsing a log from disk, because a tamper-evident record has to
/// be readable in order to be reported on, whatever it happens to contain.
pub fn validate_program(program: &str) -> Result<(), TrialError> {
    let reject = |reason: String| {
        Err(TrialError::InvalidProgram {
            program: program.to_string(),
            reason,
        })
    };

    if program.is_empty() {
        return reject(
            "it is empty and a trial must name the line of enquiry it belongs to".into(),
        );
    }

    // `chars().count()` rather than `len()`, which counts bytes. They agree for
    // the ASCII this function permits, but reporting a length in bytes for a
    // string that was rejected for being non-ASCII would confuse the reader.
    let length = program.chars().count();
    if length > PROGRAM_MAX_LEN {
        return reject(format!(
            "it is {length} characters and at most {PROGRAM_MAX_LEN} are allowed"
        ));
    }

    if let Some(bad) = program
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return reject(format!(
            "it contains {bad:?}, and a program identifier may hold only ASCII letters, \
             digits, and the characters - _ ."
        ));
    }

    Ok(())
}

/// Width of a hex SHA-256, which is what a `config_hash` has to be.
pub const CONFIG_HASH_LEN: usize = 64;

/// A `config_hash` a writer is allowed to record.
///
/// # Why this is a type and not a `String`
///
/// `config_hash` is the only field that says *which* configuration was tried,
/// so it is the only field that distinguishes a genuinely new trial from a
/// repeat of one already counted. A plain string parameter accepts
/// `"lookback=12"`. Such an entry hashes, chains, and verifies exactly as a
/// real one does, and the count stays correct, so nothing fails. What is lost
/// is the record's ability to prove what was run, silently and permanently,
/// because the log is append-only and the configuration is not stored anywhere
/// else.
///
/// A newtype, which is Rust's term for a struct wrapping exactly one value in
/// order to give it a distinct type, turns that from something a reviewer has
/// to notice into something the compiler refuses. The inner `String` is
/// private, so the only ways to obtain one are [`ConfigHash::of`], which
/// hashes, and [`ConfigHash::parse`], which validates.
///
/// # The asymmetry with reading
///
/// [`TrialEntry::config_hash`] stays a plain `String` and deserialisation
/// checks nothing. See the module documentation for why that is deliberate.
///
/// # Examples
///
/// The supported way to record a configuration.
///
/// ```
/// use jiff::Timestamp;
/// use rigor::{ConfigHash, TrialEntry, ZERO_HASH};
///
/// let config = ConfigHash::of(b"lookback=12");
/// let entry = TrialEntry::new(Timestamp::now(), "momentum-v1", &config, None, ZERO_HASH)
///     .expect("entry builds");
/// assert_eq!(entry.config_hash, config.as_str());
/// ```
///
/// Handing the configuration itself to the field named for its hash does not
/// compile. Before this type existed it did, and the log accepted it.
///
/// ```compile_fail
/// use jiff::Timestamp;
/// use rigor::{TrialEntry, ZERO_HASH};
///
/// let entry = TrialEntry::new(
///     Timestamp::now(),
///     "momentum-v1",
///     "lookback=12",
///     None,
///     ZERO_HASH,
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigHash(String);

impl ConfigHash {
    /// Hash a strategy configuration. This is the normal way to make one.
    pub fn of(config: &[u8]) -> Self {
        ConfigHash(hash_bytes(config))
    }

    /// Accept a hash that already exists, for instance one read back out of a
    /// log or produced by another tool, after checking that it is one.
    ///
    /// Lowercase only. [`hash_bytes`] emits lowercase and the canonical form is
    /// defined over the exact text, so the same configuration spelled in upper
    /// case would produce a different `entry_hash` while describing the same
    /// thing. Accepting both spellings would make that divergence reachable.
    pub fn parse(value: &str) -> Result<Self, TrialError> {
        let reject = |reason: String| {
            Err(TrialError::InvalidConfigHash {
                value: value.to_string(),
                reason,
            })
        };

        // Bytes rather than characters, because a hex digest is ASCII by
        // definition and a multi-byte character is one of the things being
        // rejected here rather than something to measure politely.
        if value.len() != CONFIG_HASH_LEN {
            return reject(format!(
                "it is {} bytes and a hex SHA-256 is exactly {CONFIG_HASH_LEN}",
                value.len()
            ));
        }

        if let Some(bad) = value
            .chars()
            .find(|c| !c.is_ascii_digit() && !matches!(c, 'a'..='f'))
        {
            return reject(format!(
                "it contains {bad:?}, and a hex SHA-256 holds only the digits 0-9 and the \
                 lowercase letters a-f"
            ));
        }

        Ok(ConfigHash(value.to_string()))
    }

    /// The hex digest, for writing into a record or printing.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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
    ///
    /// The `program` check lives here rather than in the log's append, because
    /// this constructor is the only route by which a program name reaches a
    /// hash. Validating in the caller would leave the constructor itself as an
    /// unchecked way in.
    ///
    /// `config_hash` needs no check here at all, and that is the point of
    /// [`ConfigHash`]. The value cannot be anything but a hash by the time it
    /// arrives, so there is no runtime branch left to forget.
    ///
    /// `prev_hash` is deliberately still a string. It is not supplied by a
    /// caller in any real path, because [`crate::log::TrialLog::append`]
    /// computes it from the head of the chain it just re-read, so typing it
    /// would constrain nobody.
    pub fn new(
        timestamp: Timestamp,
        program: impl Into<String>,
        config_hash: &ConfigHash,
        sharpe: Option<Decimal>,
        prev_hash: impl Into<String>,
    ) -> Result<Self, TrialError> {
        let program = program.into();
        validate_program(&program)?;

        // Built with a placeholder hash, then rebuilt with the real one. The
        // placeholder never reaches the hash input, because canonical_json
        // drops the entry_hash key before serialising.
        let draft = TrialEntry {
            timestamp,
            program,
            config_hash: config_hash.as_str().to_string(),
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
            &ConfigHash::of(b"abc123"),
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
            &ConfigHash::of(b"abc123"),
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

    /// The cross-language acceptance test for the canonical form.
    ///
    /// The genesis entry is pure ASCII, so it cannot tell the two candidate
    /// canonical forms apart. This entry can. It carries a quote, a backslash,
    /// a tab, a newline, a C0 control character, and two non-ASCII characters,
    /// which between them exercise every escaping decision the two writers
    /// could disagree on.
    ///
    /// The expected hash was produced by Python, not by this code, which is the
    /// only way the test proves agreement rather than self-consistency.
    ///
    /// ```text
    /// entry = {
    ///     "timestamp": "2026-08-08T12:34:56Z",
    ///     "program": "momentum-v1.2_ok",
    ///     "config_hash": 'quote " backslash \\ tab \t newline \n unit-sep \x01 mu μ check ✓',
    ///     "sharpe": "-1.234567890123456789",
    ///     "prev_hash": "0" * 64,
    /// }
    /// raw = json.dumps(entry, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    /// hashlib.sha256(raw.encode("utf-8")).hexdigest()
    /// # 3131620b14833621647859398dfe0b908eca266be66a554a19eacf9031b6e9f1
    /// ```
    ///
    /// Dropping `ensure_ascii=False` from that snippet, which is the default
    /// and was what `trials/README.md` used to say, yields
    /// `2aedd161a7c117d4cf62825de42a20a6770f836db88cc5bb7beea8fd544c2119`
    /// instead. That difference is the whole reason this test exists.
    ///
    /// The entry is assembled as a struct literal rather than through
    /// text like this in `config_hash` at all. That restriction binds writers
    /// and the canonical form does not. Verification has to reproduce the hash
    /// of whatever a file holds, including records written before the
    /// restriction existed or by another implementation, so the escaping rule
    /// still has to be pinned over text no writer here would produce.
    #[test]
    fn the_canonical_form_agrees_with_python_on_escaping_and_non_ascii() {
        let draft = TrialEntry {
            timestamp: Timestamp::from_str("2026-08-08T12:34:56Z").expect("valid timestamp"),
            program: "momentum-v1.2_ok".to_string(),
            config_hash:
                "quote \" backslash \\ tab \t newline \n unit-sep \u{1} mu \u{3bc} check \u{2713}"
                    .to_string(),
            sharpe: Some(Decimal::from_str("-1.234567890123456789").expect("valid decimal")),
            prev_hash: ZERO_HASH.to_string(),
            entry_hash: String::new(),
        };
        let entry = TrialEntry {
            entry_hash: draft.compute_hash().expect("entry hashes"),
            ..draft
        };

        // The canonical string is asserted as well as the hash. When this test
        // fails, seeing which bytes moved is the difference between a minute
        // and an afternoon.
        assert_eq!(
            entry.canonical_json().expect("entry serialises"),
            "{\"config_hash\":\"quote \\\" backslash \\\\ tab \\t newline \\n unit-sep \\u0001 \
             mu \u{3bc} check \u{2713}\",\
             \"prev_hash\":\"0000000000000000000000000000000000000000000000000000000000000000\",\
             \"program\":\"momentum-v1.2_ok\",\
             \"sharpe\":\"-1.234567890123456789\",\
             \"timestamp\":\"2026-08-08T12:34:56Z\"}"
        );
        assert_eq!(
            entry.entry_hash, "3131620b14833621647859398dfe0b908eca266be66a554a19eacf9031b6e9f1",
            "the Rust canonical form no longer agrees with the documented Python one"
        );
    }

    #[test]
    fn a_non_ascii_program_is_refused_at_the_boundary() {
        let error = TrialEntry::new(
            Timestamp::from_str("2026-08-08T12:00:00Z").expect("valid timestamp"),
            "\u{3bc}structure-v1",
            &ConfigHash::of(b"abc123"),
            None,
            ZERO_HASH,
        )
        .expect_err("a non-ASCII program must be refused");

        match error {
            TrialError::InvalidProgram { program, .. } => {
                assert_eq!(
                    program, "\u{3bc}structure-v1",
                    "the error must name the value it rejected"
                );
            }
            other => panic!("expected InvalidProgram, got {other:?}"),
        }
    }

    #[test]
    fn program_identifiers_are_accepted_or_refused_by_the_documented_rule() {
        for good in [
            "genesis",
            "momentum-v1",
            "value_v2",
            "micro.structure.3",
            "A1",
            &"x".repeat(PROGRAM_MAX_LEN),
        ] {
            assert!(
                validate_program(good).is_ok(),
                "{good:?} should be a usable program identifier"
            );
        }

        for bad in [
            "",                               // nothing to scope N by
            "has space",                      // not in the charset
            "slash/v1",                       // not in the charset
            "quote\"v1",                      // would exercise JSON escaping
            "\u{3bc}structure-v1",            // non-ASCII, the divergence class
            &"x".repeat(PROGRAM_MAX_LEN + 1), // one over the cap
        ] {
            assert!(
                matches!(
                    validate_program(bad),
                    Err(TrialError::InvalidProgram { .. })
                ),
                "{bad:?} should have been refused"
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
