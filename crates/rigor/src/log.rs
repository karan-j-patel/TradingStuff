//! Reading, verifying, and extending the append-only trial log.
//!
//! # What the chain does, stated exactly
//!
//! It is tamper **evident**, not tamper proof. This runs on the author's own
//! machine from source the author controls, so nothing here can stop a
//! determined edit and no code in this file claims otherwise. What it does is
//! make an edit detectable, because changing any field of any entry changes
//! that entry's hash, which orphans every entry after it.
//!
//! # Why the harness appends rather than the caller
//!
//! Rule 2 in `CLAUDE.md` puts the increment in the harness so it cannot be
//! forgotten. [`TrialLog::append`] therefore computes `prev_hash` and
//! `entry_hash` itself and takes neither from the caller. A caller supplies the
//! program, the config hash, and the Sharpe if there is one, and that is all.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rust_decimal::Decimal;

use crate::entry::{TrialEntry, ZERO_HASH};
use crate::error::TrialError;

/// Where the log lives, relative to the repository root.
pub const DEFAULT_PATH: &str = "trials/trials.jsonl";

/// The trial log, loaded into memory.
///
/// Small by construction. A file that grows to a million lines is a file
/// describing a million backtests, which would be a research problem long
/// before it is a memory problem.
#[derive(Debug, Clone)]
pub struct TrialLog {
    path: PathBuf,
    entries: Vec<TrialEntry>,
    /// Whether the file on disk ended with a newline when it was read.
    ///
    /// An append that assumes one and is wrong glues two records onto the same
    /// line, which corrupts the log rather than merely breaking its hashes.
    ends_with_newline: bool,
}

impl TrialLog {
    /// Read the log from disk. Does not verify it. Call [`TrialLog::verify`]
    /// for that, which every command that reads the log should do.
    ///
    /// A blank line is a parse failure rather than something to skip over.
    /// Skipping would make the line numbers in error messages disagree with
    /// what an editor shows, and those line numbers are the whole value of the
    /// error.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, TrialError> {
        let path = path.into();
        let contents = std::fs::read_to_string(&path).map_err(|source| TrialError::Io {
            path: path.clone(),
            source,
        })?;

        let entries = contents
            .lines()
            .enumerate()
            .map(|(index, line)| {
                serde_json::from_str::<TrialEntry>(line).map_err(|source| TrialError::Malformed {
                    line: index + 1,
                    source,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(TrialLog {
            path,
            entries,
            ends_with_newline: contents.is_empty() || contents.ends_with('\n'),
        })
    }

    /// Where this log was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every line, genesis included.
    pub fn entries(&self) -> &[TrialEntry] {
        &self.entries
    }

    /// The genesis entry, which is line 1 by definition of the format.
    pub fn genesis(&self) -> Option<&TrialEntry> {
        self.entries.first()
    }

    /// The actual trials, which is every entry except genesis.
    ///
    /// Genesis is identified by position rather than by its `program` field.
    /// `trials/README.md` defines it as the first line, and matching on the
    /// string "genesis" would silently miscount if a research program were ever
    /// given that name.
    pub fn trials(&self) -> &[TrialEntry] {
        self.entries.get(1..).unwrap_or(&[])
    }

    /// Lifetime `N`, every trial ever run against this dataset. The
    /// conservative bound of the two figures rule 2 requires.
    pub fn lifetime_count(&self) -> usize {
        self.trials().len()
    }

    /// Scoped `N` for one research program. The statistically correct reading
    /// of the two figures rule 2 requires.
    pub fn count_for(&self, program: &str) -> usize {
        self.trials()
            .iter()
            .filter(|entry| entry.program == program)
            .count()
    }

    /// Trial counts per research program, sorted by program name.
    pub fn program_counts(&self) -> BTreeMap<&str, usize> {
        self.trials()
            .iter()
            .fold(BTreeMap::new(), |mut counts, entry| {
                *counts.entry(entry.program.as_str()).or_insert(0) += 1;
                counts
            })
    }

    /// Every Sharpe actually recorded, lifetime. Trials that produced no
    /// figure contribute nothing, which is why this can be shorter than
    /// [`TrialLog::lifetime_count`].
    ///
    /// `sigma_SR`, the spread of these, is a required input to the Deflated
    /// Sharpe Ratio, which is the probability that a strategy's edge is real
    /// rather than the luckiest of everything tried.
    pub fn recorded_sharpes(&self) -> Vec<Decimal> {
        self.trials()
            .iter()
            .filter_map(|entry| entry.sharpe)
            .collect()
    }

    /// Every Sharpe recorded within one research program.
    pub fn recorded_sharpes_for(&self, program: &str) -> Vec<Decimal> {
        self.trials()
            .iter()
            .filter(|entry| entry.program == program)
            .filter_map(|entry| entry.sharpe)
            .collect()
    }

    /// Walk the chain from genesis forward, recomputing every hash and
    /// checking every link.
    ///
    /// Returns the first failure found rather than a list, because after the
    /// first break every later entry is orphaned and reporting all of them
    /// buries the one that matters.
    pub fn verify(&self) -> Result<(), TrialError> {
        if self.entries.is_empty() {
            return Err(TrialError::Empty {
                path: self.path.clone(),
            });
        }

        // What the next entry's prev_hash must equal. Genesis has no
        // predecessor, so it must name the all-zero hash.
        let mut expected_prev = ZERO_HASH.to_string();

        for (index, entry) in self.entries.iter().enumerate() {
            let line = index + 1;

            // Contents before links. An edit to a field breaks that entry's own
            // hash, and reporting the line the edit is on is more useful than
            // reporting the line after it, where the link would also fail.
            let computed = entry.compute_hash()?;
            if computed != entry.entry_hash {
                return Err(TrialError::HashMismatch {
                    line,
                    recorded: entry.entry_hash.clone(),
                    computed,
                });
            }

            if entry.prev_hash != expected_prev {
                return Err(TrialError::BrokenLink {
                    line,
                    found: entry.prev_hash.clone(),
                    expected: expected_prev,
                });
            }

            expected_prev = entry.entry_hash.clone();
        }

        Ok(())
    }

    /// Record one trial and write it to disk.
    ///
    /// Verifies the existing chain first. Appending onto a chain that is
    /// already broken produces a log where the break looks older than it is,
    /// and the point of the record is that its history can be trusted.
    ///
    /// The timestamp is wall-clock time, which is the one place in this crate
    /// where the clock is read. Nothing that has to be reproducible depends on
    /// it, because verification recomputes hashes from the recorded timestamp
    /// rather than from the current one.
    pub fn append(
        &mut self,
        program: &str,
        config_hash: &str,
        sharpe: Option<Decimal>,
    ) -> Result<&TrialEntry, TrialError> {
        self.verify()?;

        let prev_hash = self
            .entries
            .last()
            .map(|entry| entry.entry_hash.clone())
            .unwrap_or_else(|| ZERO_HASH.to_string());

        let entry = TrialEntry::new(Timestamp::now(), program, config_hash, sharpe, prev_hash)?;

        let mut line =
            serde_json::to_string(&entry).map_err(|source| TrialError::Canonical { source })?;
        line.push('\n');
        if !self.ends_with_newline {
            line.insert(0, '\n');
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| TrialError::Io {
                path: self.path.clone(),
                source,
            })?;
        file.write_all(line.as_bytes())
            .map_err(|source| TrialError::Io {
                path: self.path.clone(),
                source,
            })?;

        // In-memory state only changes once the bytes are on disk, so a failed
        // write leaves the log describing what the file actually holds.
        self.entries.push(entry);
        self.ends_with_newline = true;

        Ok(self
            .entries
            .last()
            .expect("an entry was just pushed, so the log is not empty"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::hash_bytes;
    use std::str::FromStr;

    /// The committed log, which is the real acceptance fixture.
    fn repository_log_path() -> PathBuf {
        // CARGO_MANIFEST_DIR is crates/rigor, so the repository root is two up.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(DEFAULT_PATH)
    }

    /// Write lines to a scratch file and load them.
    ///
    /// Uses a per-test filename under the target directory rather than a temp
    /// crate, because one dependency avoided is one dependency that cannot
    /// break the build.
    fn log_from_lines(name: &str, lines: &[String]) -> TrialLog {
        let path = std::env::temp_dir().join(format!("rigor-test-{name}.jsonl"));
        let body = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        std::fs::write(&path, body).expect("scratch log writes");
        TrialLog::load(path).expect("scratch log loads")
    }

    fn genesis() -> TrialEntry {
        TrialEntry::new(
            Timestamp::from_str("2026-08-08T00:00:00Z").expect("valid timestamp"),
            "genesis",
            ZERO_HASH,
            None,
            ZERO_HASH,
        )
        .expect("genesis builds")
    }

    fn trial(program: &str, seed: &str, sharpe: Option<&str>, prev: &str) -> TrialEntry {
        TrialEntry::new(
            Timestamp::from_str("2026-08-08T01:00:00Z").expect("valid timestamp"),
            program,
            hash_bytes(seed.as_bytes()),
            sharpe.map(|value| Decimal::from_str(value).expect("valid decimal")),
            prev,
        )
        .expect("trial builds")
    }

    fn line_of(entry: &TrialEntry) -> String {
        serde_json::to_string(entry).expect("entry serialises")
    }

    #[test]
    fn the_committed_log_loads_and_verifies() {
        let log = TrialLog::load(repository_log_path()).expect("committed log loads");

        log.verify().expect("committed log verifies");
        assert_eq!(
            log.genesis().expect("genesis exists").entry_hash,
            "bbdc8721955c4bb3f0ba915f1306070895d598345f3f5308d09cd78093af6db5"
        );
    }

    #[test]
    fn a_tampered_entry_is_reported_on_its_own_line() {
        let first = genesis();
        let second = trial("momentum-v1", "cfg-a", Some("0.8"), &first.entry_hash);
        let third = trial("momentum-v1", "cfg-b", Some("1.1"), &second.entry_hash);

        // Rewrite the Sharpe on line 2 and leave its recorded entry_hash alone,
        // which is what an edit in a text editor looks like.
        let tampered = TrialEntry {
            sharpe: Some(Decimal::from_str("9.9").expect("valid decimal")),
            ..second.clone()
        };

        let log = log_from_lines(
            "tampered",
            &[line_of(&first), line_of(&tampered), line_of(&third)],
        );

        let error = log.verify().expect_err("tampering must be detected");
        assert_eq!(error.line(), Some(2));
        assert!(
            matches!(error, TrialError::HashMismatch { .. }),
            "expected a hash mismatch, got {error:?}"
        );
    }

    #[test]
    fn a_broken_link_is_reported_even_when_every_entry_hashes_correctly() {
        let first = genesis();
        let second = trial("momentum-v1", "cfg-a", Some("0.8"), &first.entry_hash);
        // Line 3 is internally consistent, since its entry_hash was computed
        // over this prev_hash, but the prev_hash names genesis rather than line
        // 2. That is what removing a line looks like after a careful edit.
        let third = trial("momentum-v1", "cfg-b", Some("1.1"), ZERO_HASH);

        let log = log_from_lines(
            "broken-link",
            &[line_of(&first), line_of(&second), line_of(&third)],
        );

        let error = log.verify().expect_err("a broken link must be detected");
        assert_eq!(error.line(), Some(3));
        assert!(
            matches!(error, TrialError::BrokenLink { .. }),
            "expected a broken link, got {error:?}"
        );
    }

    #[test]
    fn a_removed_entry_is_detected() {
        let first = genesis();
        let second = trial("momentum-v1", "cfg-a", Some("0.8"), &first.entry_hash);
        let third = trial("momentum-v1", "cfg-b", Some("1.1"), &second.entry_hash);

        // Drop line 2. Line 3 still hashes correctly but now points at a hash
        // that no preceding line carries.
        let log = log_from_lines("removed", &[line_of(&first), line_of(&third)]);

        let error = log.verify().expect_err("a removal must be detected");
        assert_eq!(error.line(), Some(2));
        assert!(matches!(error, TrialError::BrokenLink { .. }));
    }

    #[test]
    fn an_empty_log_is_rejected_because_genesis_is_mandatory() {
        let path = std::env::temp_dir().join("rigor-test-empty.jsonl");
        std::fs::write(&path, "").expect("empty log writes");

        let log = TrialLog::load(&path).expect("empty log loads");

        assert!(matches!(
            log.verify().expect_err("an empty log must be rejected"),
            TrialError::Empty { .. }
        ));
    }

    #[test]
    fn a_malformed_line_names_its_line_number() {
        let path = std::env::temp_dir().join("rigor-test-malformed.jsonl");
        std::fs::write(&path, format!("{}\nnot json at all\n", line_of(&genesis())))
            .expect("malformed log writes");

        let error = TrialLog::load(&path).expect_err("malformed JSON must be rejected");

        assert_eq!(error.line(), Some(2));
        assert!(matches!(error, TrialError::Malformed { .. }));
    }

    #[test]
    fn appending_produces_a_chain_that_verifies() {
        let path = std::env::temp_dir().join("rigor-test-append.jsonl");
        std::fs::write(&path, format!("{}\n", line_of(&genesis()))).expect("seed log writes");

        let mut log = TrialLog::load(&path).expect("seed log loads");
        log.append("momentum-v1", &hash_bytes(b"cfg-a"), None)
            .expect("first append succeeds");
        log.append(
            "momentum-v1",
            &hash_bytes(b"cfg-b"),
            Some(Decimal::from_str("1.25").expect("valid decimal")),
        )
        .expect("second append succeeds");

        log.verify().expect("the in-memory chain verifies");

        // And it verifies again after a round trip through the file, which is
        // what proves the written bytes and the in-memory entries agree.
        let reloaded = TrialLog::load(&path).expect("appended log reloads");
        reloaded.verify().expect("the written chain verifies");
        assert_eq!(reloaded.lifetime_count(), 2);
        assert_eq!(
            reloaded.trials()[1].sharpe,
            Some(Decimal::from_str("1.25").expect("valid decimal"))
        );
    }

    #[test]
    fn appending_to_a_file_with_no_trailing_newline_does_not_glue_two_records_together() {
        let path = std::env::temp_dir().join("rigor-test-no-newline.jsonl");
        // Deliberately no trailing newline.
        std::fs::write(&path, line_of(&genesis())).expect("seed log writes");

        let mut log = TrialLog::load(&path).expect("seed log loads");
        log.append("momentum-v1", &hash_bytes(b"cfg-a"), None)
            .expect("append succeeds");

        let reloaded = TrialLog::load(&path).expect("appended log reloads");
        reloaded.verify().expect("the written chain verifies");
        assert_eq!(reloaded.entries().len(), 2);
    }

    #[test]
    fn appending_onto_a_broken_chain_is_refused() {
        let first = genesis();
        let second = trial("momentum-v1", "cfg-a", Some("0.8"), &first.entry_hash);
        let tampered = TrialEntry {
            program: "momentum-v2".to_string(),
            ..second
        };
        let path = std::env::temp_dir().join("rigor-test-refuse.jsonl");
        std::fs::write(
            &path,
            format!("{}\n{}\n", line_of(&first), line_of(&tampered)),
        )
        .expect("broken log writes");

        let mut log = TrialLog::load(&path).expect("broken log loads");

        assert!(matches!(
            log.append("momentum-v1", "cfg-b", None)
                .expect_err("appending onto a broken chain must be refused"),
            TrialError::HashMismatch { line: 2, .. }
        ));
        // And nothing was written.
        assert_eq!(
            TrialLog::load(&path).expect("reloads").entries().len(),
            2,
            "a refused append still wrote a line"
        );
    }

    #[test]
    fn counts_are_reported_both_scoped_and_lifetime() {
        let first = genesis();
        let second = trial("momentum-v1", "cfg-a", Some("0.8"), &first.entry_hash);
        let third = trial("momentum-v1", "cfg-b", Some("1.1"), &second.entry_hash);
        let fourth = trial("value-v1", "cfg-c", None, &third.entry_hash);

        let log = log_from_lines(
            "counts",
            &[
                line_of(&first),
                line_of(&second),
                line_of(&third),
                line_of(&fourth),
            ],
        );
        log.verify().expect("chain verifies");

        // Genesis is not a trial, so four lines are three trials.
        assert_eq!(log.lifetime_count(), 3);
        assert_eq!(log.count_for("momentum-v1"), 2);
        assert_eq!(log.count_for("value-v1"), 1);
        assert_eq!(log.count_for("never-run"), 0);

        let counts = log.program_counts();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts.get("momentum-v1"), Some(&2));
        assert_eq!(counts.get("value-v1"), Some(&1));

        // A trial with no Sharpe is counted as a trial but contributes no
        // Sharpe, which is why these two numbers differ.
        assert_eq!(log.recorded_sharpes().len(), 2);
        assert_eq!(log.recorded_sharpes_for("momentum-v1").len(), 2);
        assert_eq!(log.recorded_sharpes_for("value-v1").len(), 0);
    }
}
