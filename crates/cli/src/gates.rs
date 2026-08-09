//! What `DECISION_CRITERIA.md` requires, and which of it a program can check.
//!
//! # Why this file is mostly a list of things it cannot do
//!
//! A gate item printed as an unchecked box implies a program could tick it if
//! only the work were done. For most of Gate 1 that is false in a way worth
//! being explicit about, because some items need code that does not exist yet
//! and others need elapsed live time or a human reading a discrepancy and
//! deciding it is understood. Printing those identically would let a reader
//! believe the platform is closer to a gate than it is.
//!
//! So each item carries which of the three it is, and `status` prints them in
//! separate groups. Only one item in Gate 1 is checkable today, and it happens
//! to be the one this crate can check.
//!
//! This list mirrors `DECISION_CRITERIA.md` and does not govern anything. That
//! document governs. If the two ever disagree, the document is right and this
//! file is stale.

/// How close a program can get to answering a gate item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkability {
    /// This command checks it, right now, and the result is real.
    Automated,
    /// Code could check it, but the code that would do so is not written. The
    /// string names what is missing.
    BlockedOnCode(&'static str),
    /// No program will ever tick this. It needs elapsed live trading or a
    /// human judgement. The string says which.
    NeedsHuman(&'static str),
}

/// One line of `DECISION_CRITERIA.md`.
#[derive(Debug, Clone, Copy)]
pub struct GateItem {
    pub description: &'static str,
    pub checkability: Checkability,
}

/// Gate 1, before any paper trading.
pub const GATE_ONE: &[GateItem] = &[
    GateItem {
        description: "the lookahead test suite passes, and a deliberately broken pipeline fails it",
        checkability: Checkability::BlockedOnCode(
            "no characteristic panel exists yet, so there is no pipeline to leak through",
        ),
    },
    GateItem {
        description: "a known published factor reproduces within a defensible range of published net figures",
        checkability: Checkability::BlockedOnCode(
            "no backtest engine and no fundamentals panel exist yet",
        ),
    },
    GateItem {
        description: "random noise fed in as the signal produces an unimpressive deflated Sharpe",
        checkability: Checkability::BlockedOnCode("no backtest engine exists yet"),
    },
    GateItem {
        description: "the cost model is applied to every result, and no gross-return path exists",
        checkability: Checkability::BlockedOnCode("no cost model exists yet"),
    },
    GateItem {
        description: "the trial log hash chain verifies",
        checkability: Checkability::Automated,
    },
    GateItem {
        description: "corporate actions are handled and tested, and a delisting mid-hold does not vanish",
        checkability: Checkability::BlockedOnCode(
            "crates/ingest models them, but no position fold exists to apply them to",
        ),
    },
];

/// Gate 2, before one dollar of real money. Every item needs elapsed paper
/// trading or a comparison against a backtest that has not been run, so the
/// gate is summarised as one line rather than itemised into boxes no command
/// can ever tick.
pub const GATE_TWO: GateItem = GateItem {
    description: "all of Gate 1, plus 126 uninterrupted trading days of paper trading and its \
                  live-versus-backtest comparisons",
    checkability: Checkability::NeedsHuman(
        "requires elapsed calendar time and a backtest to compare against",
    ),
};
