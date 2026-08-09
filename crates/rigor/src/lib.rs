//! The discipline layer. Counts trials, chains them so edits are visible, and
//! says how high a Sharpe would have to be before it means anything.
//!
//! # What this crate is for
//!
//! `CLAUDE.md` rule 2 requires that every backtest increments an append-only
//! counter that survives across sessions, and that the counter is incremented
//! by the harness rather than by the caller so it cannot be forgotten. This
//! crate is that harness. It owns the log format and the chain, and the CLI
//! owns none of it.
//!
//! Three ideas, defined here because the rest of the crate assumes them.
//!
//! A **trial** is one evaluation of one strategy configuration against
//! historical data. A hundred point hyperparameter grid is a hundred trials.
//! A hypothesis a language model proposed is a trial exactly as a human one is.
//!
//! A **research program** is the named line of enquiry a trial belongs to. It
//! scopes the trial count `N`. Rule 2 requires reporting two figures always,
//! the scoped `N` which is the statistically correct reading, and the lifetime
//! `N` over every program which is the conservative bound.
//!
//! The **Deflated Sharpe Ratio** is the probability, between zero and one, that
//! a strategy's edge is real rather than the luckiest of everything tried. It
//! needs `N` and it needs `sigma_SR`, the spread of Sharpes across trials,
//! which is why the log stores each trial's Sharpe and not merely a count.
//!
//! # What the chain does not do
//!
//! It is tamper evident, not tamper proof. This runs on the author's own
//! machine from source the author controls. Editing an entry breaks every hash
//! after it, which makes the edit detectable, and detection is the achievable
//! goal. No code here claims to prevent bypass.
//!
//! # Layout
//!
//! - [`entry`] holds one record and the canonical form its hash is taken over.
//! - [`log`] reads, verifies, and extends the file.
//! - [`threshold`] turns a trial count into the Sharpe a result has to clear.

pub mod entry;
pub mod error;
pub mod log;
pub mod threshold;

pub use entry::{ConfigHash, TrialEntry, ZERO_HASH, hash_bytes};
pub use error::TrialError;
pub use log::{DEFAULT_PATH, TrialLog};
pub use threshold::{expected_max_sharpe, sigma_sr, threshold_for};
