//! What `DECISION_CRITERIA.md` requires, and which of it a program can check.
//!
//! # This file does not govern anything
//!
//! `DECISION_CRITERIA.md` governs. This is a reading of it. If the two ever
//! disagree the document is right and this file is stale, which is exactly the
//! failure the test at the bottom exists to make loud.
//!
//! # Why the drift runs one way
//!
//! The amendment rule in that document makes tightening free and weakening
//! expensive. Gates therefore get added and tightened over time, and almost
//! never removed. A registry that is merely stale is not neutral, then. It is
//! stale in the permissive direction, reporting an older and smaller set of
//! requirements than the governing document actually imposes, and a reader of
//! `status` would see a shorter list and conclude the platform is closer to a
//! gate than it is.
//!
//! Before this file carried IDs, `status` knew six of the forty criteria, all
//! of them in Gate 1. That was not drift that accumulated. It was under
//! reporting present from the first commit, and nothing in the build would ever
//! have said so.
//!
//! So every criterion in the document carries a stable ID token like
//! `` `[F1.3]` ``, this table repeats those IDs, and `id_set_matches_document`
//! asserts the two sets are equal. Adding a criterion to the document without
//! adding it here fails the build. So does the reverse.
//!
//! # Why the test matches a token and not markdown
//!
//! The test extracts ID tokens and ignores everything around them. It does not
//! parse headings, tables, or checkboxes. Reformatting the document, rewording
//! a criterion, or moving one between sections is then invisible to the test,
//! which is correct, because none of those changes what is required. Only
//! adding or removing a criterion changes the ID set, and that is the only
//! thing worth failing a build over.
//!
//! # Why most of this file is a list of things it cannot do
//!
//! A gate item printed as an unchecked box implies a program could tick it if
//! only the work were done. For nearly all of these that is false, because some
//! need code that does not exist yet and others need elapsed paper trading or a
//! human reading a discrepancy and deciding it is understood. Printing those
//! identically would let a reader believe a box is merely unticked when it is
//! not yet askable. So each item carries which of the three it is and `status`
//! prints the reason instead of a box.
//!
//! Exactly one item is automated today. Do not add a second by guessing.

/// How close a program can get to answering a gate item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkability {
    /// This command checks it, right now, and the result is real.
    Automated,
    /// Code could check it, but the code that would do so is not written. The
    /// string names what is missing.
    BlockedOnCode(&'static str),
    /// No program will ever tick this on its own. It needs elapsed paper
    /// trading or a human judgement. The string says which.
    NeedsHuman(&'static str),
}

/// One criterion of `DECISION_CRITERIA.md`.
///
/// `&'static str` throughout rather than `String`, so the whole registry is
/// baked into the binary with no allocation and no startup cost. A newcomer to
/// Rust meets this pattern constantly. `&'static str` is a borrowed string that
/// lives for the entire program, which is what a string literal in the source
/// always is.
#[derive(Debug, Clone, Copy)]
pub struct GateItem {
    /// The token as it appears in the document, without the brackets or the
    /// backticks. `G1.5` here matches `` `[G1.5]` `` there.
    pub id: &'static str,
    /// A short restatement. The document holds the authoritative wording and
    /// this is deliberately not a copy of it, because a copy would rot silently
    /// while an ID mismatch cannot.
    pub summary: &'static str,
    pub checkability: Checkability,
}

/// A gate, which is a named group of criteria that must all hold together.
#[derive(Debug, Clone, Copy)]
pub struct Gate {
    pub heading: &'static str,
    pub items: &'static [GateItem],
}

/// Every criterion in `DECISION_CRITERIA.md`, in document order.
pub const GATES: &[Gate] = &[
    Gate {
        heading: "Gate 1, before any paper trading",
        items: GATE_ONE,
    },
    Gate {
        heading: "Gate 2, before one dollar of real money",
        items: GATE_TWO,
    },
    Gate {
        heading: "Gate F0, before any expected-value claim about the fast strand",
        items: GATE_F0,
    },
    Gate {
        heading: "Gate F1, before the execution layer routes one real order",
        items: GATE_F1,
    },
    Gate {
        heading: "Gate F2, before the prediction strategy trades real money",
        items: GATE_F2,
    },
];

/// Reasons repeated across several items, named once so a reword cannot make
/// two items disagree about the same fact.
const NO_ENGINE: &str = "no backtest engine and no baselines exist yet";
const NO_DSR: &str =
    "rigor computes sigma_SR and a selection-bias threshold, but no deflated Sharpe yet";
const NO_ORDER_PATH: &str = "no order path exists, so nothing records or reconciles a fill";
const NEEDS_PAPER: &str = "requires paper trading, which has not started";

const GATE_ONE: &[GateItem] = &[
    GateItem {
        id: "G1.1",
        summary: "the lookahead test suite passes, and a deliberately broken pipeline fails it",
        checkability: Checkability::BlockedOnCode(
            "no characteristic panel exists yet, so there is no pipeline to leak through",
        ),
    },
    GateItem {
        id: "G1.2",
        summary: "a known published factor reproduces within a defensible range of published net figures",
        checkability: Checkability::BlockedOnCode(
            "no backtest engine and no fundamentals panel exist yet",
        ),
    },
    GateItem {
        id: "G1.3",
        summary: "random noise fed in as the signal produces an unimpressive deflated Sharpe",
        checkability: Checkability::BlockedOnCode(NO_DSR),
    },
    GateItem {
        id: "G1.4",
        summary: "the cost model is applied to every result, and no gross-return path exists",
        checkability: Checkability::BlockedOnCode("no cost model exists yet"),
    },
    GateItem {
        id: "G1.5",
        summary: "the trial log hash chain verifies",
        checkability: Checkability::Automated,
    },
    GateItem {
        id: "G1.6",
        summary: "corporate actions are handled and tested, and a delisting mid-hold does not vanish",
        checkability: Checkability::BlockedOnCode(
            "crates/ingest models them, but no position fold exists to apply them to",
        ),
    },
];

const GATE_TWO: &[GateItem] = &[
    GateItem {
        id: "G2.1",
        summary: "paper trading runs at least 126 trading days, uninterrupted",
        checkability: Checkability::NeedsHuman(
            "requires elapsed calendar time, which no program can shorten",
        ),
    },
    GateItem {
        id: "G2.2",
        summary: "deflated Sharpe scoped to the research program, at least 0.95",
        checkability: Checkability::BlockedOnCode(NO_DSR),
    },
    GateItem {
        id: "G2.3",
        summary: "deflated Sharpe against the lifetime trial count, at least 0.90",
        checkability: Checkability::BlockedOnCode(NO_DSR),
    },
    GateItem {
        id: "G2.4",
        summary: "beats equal-weight buy-and-hold on the same universe, net of costs",
        checkability: Checkability::BlockedOnCode(NO_ENGINE),
    },
    GateItem {
        id: "G2.5",
        summary: "beats random ranking at matched turnover",
        checkability: Checkability::BlockedOnCode(NO_ENGINE),
    },
    GateItem {
        id: "G2.6",
        summary: "beats ridge regression on the same features",
        checkability: Checkability::BlockedOnCode(NO_ENGINE),
    },
    GateItem {
        id: "G2.7",
        summary: "live versus backtest monthly return correlation, at least 0.50",
        checkability: Checkability::NeedsHuman(
            "requires a paper trading record and a backtest to correlate it against",
        ),
    },
    GateItem {
        id: "G2.8",
        summary: "realized paper Sharpe reaches at least 50 percent of the backtest Sharpe",
        checkability: Checkability::NeedsHuman(
            "requires a paper trading record and a backtest to compare it against",
        ),
    },
    GateItem {
        id: "G2.9",
        summary: "zero missed or degraded rebalances during the paper period",
        checkability: Checkability::NeedsHuman(NEEDS_PAPER),
    },
];

const GATE_F0: &[GateItem] = &[
    GateItem {
        id: "F0.1",
        summary: "aggregate one-step top-10 book reconstruction validated against LOBSTER, all five symbols",
        checkability: Checkability::BlockedOnCode(
            "crates/marketdata/examples/lobster_validate.rs runs it, but the sample files are \
             licensed and not in the repository, so status cannot rerun it here",
        ),
    },
    GateItem {
        id: "F0.2",
        summary: "it is written down that queue position cannot be validated from LOBSTER at all",
        checkability: Checkability::NeedsHuman(
            "a claim about what reference data cannot show, which only a reader can confirm is written down",
        ),
    },
    GateItem {
        id: "F0.3",
        summary: "passive fill rate measured as fills over attempts, over at least 20 trading days",
        checkability: Checkability::NeedsHuman(
            "requires paper trading, because no capture contains our own orders",
        ),
    },
    GateItem {
        id: "F0.4",
        summary: "it is written down that fill rate cannot be measured from the IEX or LOBSTER captures",
        checkability: Checkability::NeedsHuman(
            "a claim about what the captures cannot show, which only a reader can confirm is written down",
        ),
    },
    GateItem {
        id: "F0.5",
        summary: "decision-to-acknowledgement latency measured as a distribution, median and p95 and p99",
        checkability: Checkability::NeedsHuman(
            "requires an order path sending real orders and receiving real acknowledgements",
        ),
    },
    GateItem {
        id: "F0.6",
        summary: "the latency horizon fixed at the measured p99 and written down",
        checkability: Checkability::NeedsHuman(
            "depends on the measurement above, and is then a decision recorded by a person",
        ),
    },
    GateItem {
        id: "F0.7",
        summary: "sigma of daily profit and loss measured across the same 20 days and fixed there",
        checkability: Checkability::NeedsHuman(NEEDS_PAPER),
    },
    GateItem {
        id: "F0.8",
        summary: "quoted spread at decision time recorded per order",
        checkability: Checkability::BlockedOnCode(NO_ORDER_PATH),
    },
    GateItem {
        id: "F0.9",
        summary: "no strategy-facing API accepts MarketEvent, and every decision path takes Observed",
        checkability: Checkability::BlockedOnCode(
            "crates/execution does not exist yet, so there is no strategy-facing API to check",
        ),
    },
    GateItem {
        id: "F0.10",
        summary: "every microstructure hypothesis is in the trial counter, every grid point included",
        checkability: Checkability::NeedsHuman(
            "the counter records what it is given, and whether anything was withheld is not visible from inside it",
        ),
    },
];

const GATE_F1: &[GateItem] = &[
    GateItem {
        id: "F1.1",
        summary: "at least 200 parent orders measured in paper trading",
        checkability: Checkability::NeedsHuman(NEEDS_PAPER),
    },
    GateItem {
        id: "F1.2",
        summary: "implementation shortfall beats a naive market order on the mean and on at least 60 percent of parents",
        checkability: Checkability::NeedsHuman(NEEDS_PAPER),
    },
    GateItem {
        id: "F1.3",
        summary: "markouts at 1s, 10s and 60s reported after every passive fill, none omitted",
        checkability: Checkability::NeedsHuman(NEEDS_PAPER),
    },
    GateItem {
        id: "F1.4",
        summary: "mean markout at 60s does not exceed the spread captured on that fill",
        checkability: Checkability::NeedsHuman(NEEDS_PAPER),
    },
    GateItem {
        id: "F1.5",
        summary: "realized spread capture reported per order against the NBBO at decision time",
        checkability: Checkability::NeedsHuman(
            "requires paper trading and a consolidated NBBO feed, which the IEX capture is not",
        ),
    },
    GateItem {
        id: "F1.6",
        summary: "latency p99 at or below the horizon recorded at Gate F0",
        checkability: Checkability::NeedsHuman(
            "compares one measurement against another, and neither has been taken",
        ),
    },
    GateItem {
        id: "F1.7",
        summary: "zero fills the reconciliation could not match to an order",
        checkability: Checkability::BlockedOnCode(NO_ORDER_PATH),
    },
];

const GATE_F2: &[GateItem] = &[
    GateItem {
        id: "F2.1",
        summary: "paper trading runs at least 60 trading days, uninterrupted",
        checkability: Checkability::NeedsHuman(
            "requires elapsed calendar time, which no program can shorten",
        ),
    },
    GateItem {
        id: "F2.2",
        summary: "at least 2,000 round trips completed in paper",
        checkability: Checkability::NeedsHuman(NEEDS_PAPER),
    },
    GateItem {
        id: "F2.3",
        summary: "deflated Sharpe scoped to the research program, at least 0.95",
        checkability: Checkability::BlockedOnCode(NO_DSR),
    },
    GateItem {
        id: "F2.4",
        summary: "deflated Sharpe against the lifetime trial count, at least 0.90",
        checkability: Checkability::BlockedOnCode(NO_DSR),
    },
    GateItem {
        id: "F2.5",
        summary: "beats the execution layer alone running a null signal, net of costs",
        checkability: Checkability::BlockedOnCode(
            "no execution layer exists, so the null-signal baseline cannot be run",
        ),
    },
    GateItem {
        id: "F2.6",
        summary: "beats random entry at matched turnover and holding period",
        checkability: Checkability::BlockedOnCode(NO_ENGINE),
    },
    GateItem {
        id: "F2.7",
        summary: "beats logistic regression on order book imbalance, same features, net of costs",
        checkability: Checkability::BlockedOnCode(NO_ENGINE),
    },
    GateItem {
        id: "F2.8",
        summary: "realized spread capture versus quoted spread reported, and the gap explained",
        checkability: Checkability::NeedsHuman(
            "the gap is explained in prose written by a person, not computed",
        ),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The governing document, found relative to this crate rather than to the
    /// working directory. `CARGO_MANIFEST_DIR` is set by cargo at compile time
    /// and points at `crates/cli`, so the test passes wherever it is run from.
    /// It is read at runtime rather than with `include_str!` so that a falsified
    /// document is seen immediately without a rebuild.
    const DOC_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../DECISION_CRITERIA.md");

    /// Is this the inside of an ID token, `G1` `.` `5` shaped?
    ///
    /// Deliberately strict and deliberately not tied to the five gates that
    /// exist today, so a Gate F3 added later is picked up without editing the
    /// matcher, while ordinary bracketed prose is not.
    fn is_gate_id(token: &str) -> bool {
        let Some((gate, number)) = token.split_once('.') else {
            return false;
        };
        let gate_ok = gate.starts_with(|c: char| c.is_ascii_uppercase())
            && gate
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        let number_ok = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
        gate_ok && number_ok
    }

    /// Every ID token in the document, in the order they appear.
    ///
    /// A `Vec` rather than a set, because duplicates are themselves a drift
    /// symptom worth failing on. Two criteria tagged `G2.4` would collapse into
    /// one entry in a set and the count would still look right.
    ///
    /// The lifetime here is worth reading. The returned `&str` slices borrow
    /// from `doc`, so the compiler will not let the document string be dropped
    /// while the result is still alive. No copying happens.
    fn document_ids(doc: &str) -> Vec<&str> {
        doc.match_indices("`[")
            .filter_map(|(start, _)| {
                // `match_indices` yields byte offsets. Slicing at them is safe
                // because the pattern is ASCII, so the offset is always on a
                // character boundary even though the document holds characters
                // like the multi-byte >= sign.
                let rest = &doc[start + 2..];
                let end = rest.find("]`")?;
                let token = &rest[..end];
                is_gate_id(token).then_some(token)
            })
            .collect()
    }

    fn registry_ids() -> Vec<&'static str> {
        GATES
            .iter()
            .flat_map(|gate| gate.items.iter().map(|item| item.id))
            .collect()
    }

    #[test]
    fn ids_are_unique_in_both_places() {
        let doc = std::fs::read_to_string(DOC_PATH).expect("reading DECISION_CRITERIA.md");

        for (label, ids) in [
            ("DECISION_CRITERIA.md", document_ids(&doc)),
            ("the gate registry", registry_ids()),
        ] {
            let mut seen = BTreeSet::new();
            let duplicates: BTreeSet<_> = ids.iter().filter(|id| !seen.insert(**id)).collect();
            assert!(
                duplicates.is_empty(),
                "duplicate gate IDs in {label}: {duplicates:?}\n\
                 Two criteria share an ID, so one of them is invisible to the drift check."
            );
        }
    }

    #[test]
    fn id_set_matches_document() {
        let doc = std::fs::read_to_string(DOC_PATH).expect("reading DECISION_CRITERIA.md");

        let in_document: BTreeSet<&str> = document_ids(&doc).into_iter().collect();
        let in_registry: BTreeSet<&str> = registry_ids().into_iter().collect();

        // Both directions get their own message, because they mean opposite
        // things and the fix is different for each.
        let missing_from_registry: Vec<_> = in_document.difference(&in_registry).copied().collect();
        assert!(
            missing_from_registry.is_empty(),
            "gate criteria in DECISION_CRITERIA.md that crates/cli does not know about: {missing_from_registry:?}\n\
             A gate was added or tightened and `status` would silently under-report it, showing a \
             shorter and more permissive list than the document requires. Add each ID to the \
             registry in this file with an honest checkability, and do not mark anything Automated \
             without writing the check."
        );

        let missing_from_document: Vec<_> = in_registry.difference(&in_document).copied().collect();
        assert!(
            missing_from_document.is_empty(),
            "gate criteria `status` reports that no longer exist in DECISION_CRITERIA.md: {missing_from_document:?}\n\
             Either the criterion was removed, which under the amendment rule needs a commit and a \
             reason, or an ID token was lost in an edit. Check the document first. Deleting the \
             registry entry to make this pass is only correct if the criterion really is gone."
        );

        // Belt and braces. The set comparison above cannot see a count change
        // that adds and removes in the same edit, but this can.
        assert_eq!(
            in_document.len(),
            in_registry.len(),
            "gate criteria counts disagree between the document and the registry"
        );
    }

    /// The matcher is the whole test. If it silently matched nothing, both
    /// directions above would compare two empty sets and pass forever.
    #[test]
    fn matcher_accepts_ids_and_rejects_ordinary_brackets() {
        assert!(is_gate_id("G1.1"));
        assert!(is_gate_id("F0.10"));
        assert!(!is_gate_id("G1"), "no dot");
        assert!(!is_gate_id("g1.1"), "lowercase gate");
        assert!(!is_gate_id("G1.x"), "non-numeric index");
        assert!(!is_gate_id("G1."), "empty index");
        assert!(!is_gate_id("Bailey & Lopez de Prado, 2014"), "a citation");

        // Markdown checkboxes have no backtick, so they must not be picked up.
        assert!(document_ids("- [ ] a criterion with no token").is_empty());
        assert_eq!(document_ids("- [ ] `[G1.1]` tagged"), vec!["G1.1"]);
    }
}
