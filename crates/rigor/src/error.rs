//! Failures the trial log can report.
//!
//! Every variant that describes a damaged log names the line it found the
//! damage on. That is the whole point of a tamper-evident record. Saying "the
//! chain is broken" and nothing else would leave the reader with a file to
//! bisect by hand.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Everything that can go wrong reading, verifying, or extending the log.
///
/// `thiserror` generates the `Display` and `Error` implementations from the
/// `#[error(...)]` attributes. A Rust newcomer coming from Python can read
/// `#[source]` as "this is the underlying exception", which is what makes
/// `{:#}` in the CLI print the whole chain of causes.
#[derive(Debug, Error)]
pub enum TrialError {
    /// The log could not be read or written at all.
    #[error("could not access the trial log at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The file exists but holds nothing. The genesis entry is mandatory,
    /// because the chain needs a root to hang from.
    #[error("the trial log at {path} is empty, and it must hold at least the genesis entry")]
    Empty { path: PathBuf },

    /// A line was not valid JSON, or was valid JSON of the wrong shape.
    #[error("line {line} of the trial log is not a well formed trial entry")]
    Malformed {
        line: usize,
        #[source]
        source: serde_json::Error,
    },

    /// A line's recorded `entry_hash` disagrees with a hash of its own
    /// contents. Something edited the entry after it was written.
    #[error(
        "line {line} records entry_hash {recorded} but its contents hash to {computed}, so the entry was changed after it was written"
    )]
    HashMismatch {
        line: usize,
        recorded: String,
        computed: String,
    },

    /// A line's `prev_hash` does not name the previous line's `entry_hash`.
    /// Either an entry was removed, or one was inserted, or a `prev_hash` was
    /// rewritten.
    #[error(
        "line {line} names prev_hash {found} but the entry before it hashes to {expected}, so the chain does not link there"
    )]
    BrokenLink {
        line: usize,
        found: String,
        expected: String,
    },

    /// An entry could not be reduced to its canonical JSON form, so no hash of
    /// it could be computed. In practice this cannot happen for the concrete
    /// field types involved, and it is carried rather than unwrapped because
    /// panicking inside a verification routine is worse than reporting.
    #[error("a trial entry could not be reduced to its canonical form for hashing")]
    Canonical {
        #[source]
        source: serde_json::Error,
    },

    /// A research program identifier was rejected at the boundary. See
    /// [`crate::entry::validate_program`] for the rule and for why it is a
    /// restriction rather than an escaping problem to be handled.
    #[error("research program {program:?} is not a usable identifier, {reason}")]
    InvalidProgram { program: String, reason: String },

    /// A `config_hash` was rejected at the boundary. See
    /// [`crate::entry::ConfigHash`] for why a writer may only record a real
    /// hash while a reader may load whatever the file holds.
    #[error("{value:?} is not a usable config_hash, {reason}")]
    InvalidConfigHash { value: String, reason: String },

    /// The path stopped naming the file the append had locked, part way
    /// through the append. See [`crate::log::TrialLog::append`] for the
    /// sequence and `crate::log` for what replaces a file underneath a running
    /// process.
    ///
    /// This is an operator-level event rather than something to retry around.
    /// `CLAUDE.md` makes halting the default response to uncertainty, and after
    /// a replacement nothing in this process knows which file holds what.
    #[error(
        "the trial log at {path} was replaced while a trial was being appended, so the write went to a file that path no longer names and the trial must be treated as unrecorded"
    )]
    Replaced { path: PathBuf },
}

impl TrialError {
    /// The one-based line number this failure was found on, when the failure
    /// is about a particular line.
    ///
    /// Tests assert on this rather than on message text, so that rewording an
    /// error message never silently turns a real check into a passing one.
    pub fn line(&self) -> Option<usize> {
        match self {
            TrialError::Malformed { line, .. }
            | TrialError::HashMismatch { line, .. }
            | TrialError::BrokenLink { line, .. } => Some(*line),
            TrialError::Io { .. }
            | TrialError::Empty { .. }
            | TrialError::Canonical { .. }
            | TrialError::InvalidProgram { .. }
            | TrialError::InvalidConfigHash { .. }
            | TrialError::Replaced { .. } => None,
        }
    }
}
