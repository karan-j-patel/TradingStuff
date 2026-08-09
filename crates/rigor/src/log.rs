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
//!
//! # Why [`TrialLog::append`] takes a file lock, with the numbers
//!
//! This is not ceremony and it is not defensive habit. It is here because the
//! version without it was measured losing trials.
//!
//! Twenty concurrent `backtest` invocations against a one-line log, which
//! should leave twenty-one lines, produced instead, on four consecutive runs,
//! logs of 6, 8, 14, and 17 lines. Every one of those runs reported the chain
//! BROKEN, and the lifetime `N` came back as 5, 7, 13, and 16 against a true
//! count of 20. An earlier run of the same script produced 4 lines and a
//! lifetime `N` of 3.
//!
//! An undercount is the worst shape this failure could take. It does not raise
//! an error at the point of use, and `N` is a denominator of sorts in the
//! Deflated Sharpe. A missing trial makes every result in the system look
//! **better** than it is, quietly, which is precisely what rule 2 exists to
//! prevent.
//!
//! Two things were wrong and only one of them was the missing lock.
//!
//! 1. Nothing serialised the writers, so two processes could interleave.
//! 2. `prev_hash` was read from a snapshot loaded before any critical section,
//!    so two writers who had each loaded the same file would compute the same
//!    `prev_hash` and write sibling entries. Wrapping a lock around that logic
//!    unchanged would still produce siblings, only more reliably.
//!
//! The load-bearing part of the fix is therefore that the log is re-read from
//! disk **inside** the lock, and `prev_hash` comes from that re-read head. See
//! [`TrialLog::append`] for the sequence.
//!
//! # Two readers, and why there have to be two
//!
//! [`TrialLog::load_shared_locked`] is what a command should call. It holds a
//! shared lock while it reads, so it never observes a file part way through
//! somebody's append.
//!
//! [`TrialLog::load`] takes no lock and stays that way out of necessity rather
//! than taste. `append` reads the log from inside its own exclusive critical
//! section. On Unix a lock is held per open file description rather than per
//! process, so a shared lock taken there on a second descriptor would queue
//! behind the exclusive lock the same thread already holds, and the writer
//! would deadlock with itself. That nested read therefore calls `load`, and so
//! does any test reading a file nobody is writing.
//!
//! Do not read more into the shared lock than it does. It is documented
//! honestly at [`TrialLog::load_shared_locked`], and it does not prevent a lost
//! trial.
//!
//! # Why `append` compares file identity
//!
//! The lock is taken on a file descriptor and the rest of the work names a
//! path, and those are two different things that a rename can separate. See
//! [`same_file`] for what that costs and how it is detected.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rust_decimal::Decimal;

use crate::entry::{ConfigHash, TrialEntry, ZERO_HASH};
use crate::error::TrialError;

/// Where the log lives, relative to the repository root.
pub const DEFAULT_PATH: &str = "trials/trials.jsonl";

/// Whether an open file and a path still name the same file on disk.
///
/// # The bug this exists for
///
/// A *TOCTOU*, time of check to time of use, is a defect where something is
/// checked and then acted on, and the thing changes in between, so the check
/// described a state that no longer exists by the time it is relied upon.
///
/// [`TrialLog::append`] has that shape built into it. It locks a file
/// *descriptor*, which is an open handle to one particular file, and then does
/// the rest of its work through a *path*, which is only a name in a directory.
/// A name can be pointed somewhere else while a handle stays pointed at what it
/// opened. `git checkout`, restoring a backup, and any editor that writes a
/// temporary file and renames it into place all do exactly that.
///
/// The consequence here is specific and it is the worst shape a trial-log
/// failure can take. The append writes through the handle, so the bytes land in
/// the file the name *used* to refer to. Every later reader opens the name and
/// gets the other file, which does not hold that entry. The trial is gone and
/// the command that wrote it printed success.
///
/// # What `dev` and `ino` are
///
/// Unix identifies a file by a pair of numbers. `ino` is the inode number, and
/// an inode is the actual file object that a directory entry points at, holding
/// the contents and the metadata. `dev` identifies the filesystem that inode
/// lives on, and it is needed because inode numbers are only unique within one
/// filesystem. The pair together is unique across the machine.
///
/// A rename moves the *name* and never touches the inode, so comparing the pair
/// reported by the open handle against the pair the name currently resolves to
/// answers precisely the question that matters. Are these still the same file.
///
/// `File::metadata` stats the descriptor, `std::fs::metadata` stats the path,
/// and the difference between those two calls is the entire check.
///
/// # Behaviour off Unix
///
/// Windows exposes no `dev`/`ino` pair, so this compiles to an unconditional
/// `true` there and a replacement goes undetected. Stating that plainly beats a
/// portable-looking check that quietly does nothing. Windows does refuse to
/// rename over a file that is open by default, which narrows the gap without
/// closing it. The platform this project runs on is Unix.
///
/// A path that cannot be stat'ed at all, because the log was deleted rather
/// than replaced, surfaces as [`TrialError::Io`] naming the path. That is a
/// different message for the same class of event and it is equally loud, which
/// is all this needs to be.
#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> Result<bool, TrialError> {
    // `MetadataExt` is the Unix-only extension trait carrying dev() and ino().
    // Imported as `_` because only its methods are wanted, never its name.
    use std::os::unix::fs::MetadataExt as _;

    let io_error = |source| TrialError::Io {
        path: path.to_path_buf(),
        source,
    };

    let locked = file.metadata().map_err(io_error)?;
    let named = std::fs::metadata(path).map_err(io_error)?;

    Ok(locked.dev() == named.dev() && locked.ino() == named.ino())
}

/// Always true off Unix, where there is no `dev`/`ino` pair to compare. See the
/// Unix version for what is given up.
#[cfg(not(unix))]
fn same_file(_file: &File, _path: &Path) -> Result<bool, TrialError> {
    Ok(true)
}

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

        Self::from_contents(path, &contents)
    }

    /// Read the log while holding a shared lock on it. This is what a command
    /// reading the log should call.
    ///
    /// A *shared* lock, `flock(LOCK_SH)` on Unix, may be held by any number of
    /// readers at once and excludes only the exclusive lock a writer takes. So
    /// readers never queue behind one another, and no reader observes the file
    /// part way through an append.
    ///
    /// # What this buys, without inflating it
    ///
    /// Modest, and worth stating exactly rather than implying the lock prevents
    /// a lost trial, because it does not. The window it closes is the one
    /// between an appending writer's `write_all` and its `sync_data`. In that
    /// window the bytes are visible to any reader but are not yet durable, so a
    /// reader can print a chain and an `N` that a machine crash in the next
    /// millisecond would take back. Losing anything therefore needs a crash
    /// inside a window measured in microseconds.
    ///
    /// It is a few lines and it removes a way for a printed number to be wrong,
    /// so it is worth taking. That is the whole of the claim.
    ///
    /// [`TrialLog::load`] remains for the nested read inside `append`, which
    /// cannot take this lock without deadlocking against itself, and for tests
    /// reading a file nobody is writing. See the module documentation.
    pub fn load_shared_locked(path: impl Into<PathBuf>) -> Result<Self, TrialError> {
        let path = path.into();
        let io_error = |source| TrialError::Io {
            path: path.clone(),
            source,
        };

        let mut file = OpenOptions::new()
            .read(true)
            .open(&path)
            .map_err(io_error)?;
        file.lock_shared().map_err(io_error)?;

        // Read through the locked descriptor rather than by re-opening the
        // path, for the same reason `append` compares file identity. Opening
        // the name again would read whatever the name points at now, which is
        // not necessarily what was locked.
        let mut contents = String::new();
        file.read_to_string(&mut contents).map_err(io_error)?;

        // The lock is released by dropping `file`, which happens here at the
        // end of the function whichever way it exits.
        Self::from_contents(path, &contents)
    }

    /// Parse a whole log out of the text of the file. Shared by both readers so
    /// that neither can drift into parsing the format differently.
    fn from_contents(path: PathBuf, contents: &str) -> Result<Self, TrialError> {
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

    /// Record one trial and write it to disk, safely against other writers.
    ///
    /// # The critical section
    ///
    /// A *critical section* is a stretch of work that must not interleave with
    /// the same work running elsewhere. Here it has to cover reading the head of
    /// the chain and writing the entry that names it, because those two steps
    /// are only correct as a pair. Anything that can read the head between them
    /// will chain onto an entry that is about to stop being last.
    ///
    /// The section is bounded by an exclusive lock on the log file, taken with
    /// [`std::fs::File::lock`], which is `flock` on Unix and `LockFileEx` on
    /// Windows. It blocks rather than failing, which is what a research tool
    /// wants. A backtest that waits a millisecond for another backtest is fine.
    /// A backtest that refuses to record itself is not.
    ///
    /// Inside it, in order:
    ///
    /// 1. Check that the path still names the file that was locked. See
    ///    [`same_file`]. A lock on a descriptor says nothing about what the
    ///    name points at, and every step below this one uses the name.
    /// 2. Re-read the log from disk. This is the step whose absence caused the
    ///    original defect. The snapshot in `self` may be arbitrarily old.
    /// 3. Verify the re-read chain, so a log that is already broken is not
    ///    extended into a longer broken one.
    /// 4. Take `prev_hash` from the re-read head, never from `self`.
    /// 5. Write the line, then `sync_data` it, still holding the lock.
    /// 6. Check file identity again, because a replacement can also land after
    ///    step 1 and this is what decides whether the bytes just synced are
    ///    reachable by name at all.
    ///
    /// A failed identity check is [`TrialError::Replaced`] and is not retried.
    /// After a replacement nothing in this process knows which file holds what,
    /// and `CLAUDE.md` makes halting the default response to uncertainty. A
    /// skipped trial costs a rerun. A trial reported as recorded that is not
    /// costs the trial count, which costs every statistic downstream of it.
    ///
    /// After the section, `self` is refreshed from what was actually committed,
    /// which includes any entries other writers added while this one waited.
    ///
    /// # How the lock is released
    ///
    /// By dropping the [`std::fs::File`]. Rust runs destructors on every exit
    /// path from a scope, which covers the early return that `?` compiles to
    /// and covers unwinding from a panic, so there is no path out of this
    /// function that leaves the lock held. That is also why there is no guard
    /// type here. `File`'s own `Drop` closes the descriptor, and closing the
    /// descriptor releases the lock, so a guard would add a name and nothing
    /// else. If the process is killed outright the kernel closes it anyway.
    ///
    /// # Why `sync_data` and not `sync_all`
    ///
    /// `sync_data` is `fdatasync`, which flushes the file contents plus the
    /// metadata required to retrieve them, and the file length is part of that
    /// second category. An append is exactly the case `fdatasync` was specified
    /// for. `sync_all` would additionally flush metadata this record does not
    /// depend on, such as the modification time, and pay for it. The trial log
    /// is the scientific record, so losing a committed entry to a crash is not
    /// acceptable and some sync has to happen. The cheaper sufficient one is
    /// the right one.
    ///
    /// The timestamp is wall-clock time, which is the one place in this crate
    /// where the clock is read. Nothing that has to be reproducible depends on
    /// it, because verification recomputes hashes from the recorded timestamp
    /// rather than from the current one.
    pub fn append(
        &mut self,
        program: &str,
        config_hash: &ConfigHash,
        sharpe: Option<Decimal>,
    ) -> Result<&TrialEntry, TrialError> {
        // Cloned up front so the closure below borrows a local rather than
        // `self`, which is borrowed mutably for the rest of the function.
        let path = self.path.clone();
        let io_error = |source| TrialError::Io {
            path: path.clone(),
            source,
        };

        // `read(true)` is not for our benefit, since the reload below opens its
        // own handle. Windows refuses to lock a file opened append-only, so the
        // read flag is what makes this lock portable. `append(true)` keeps the
        // O_APPEND guarantee that each write goes to the current end of file
        // rather than to a remembered offset.
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&self.path)
            .map_err(io_error)?;

        // ---- critical section begins -------------------------------------
        file.lock().map_err(io_error)?;

        // The lock is on the descriptor. Everything below it reaches the log
        // through the path instead, so the two have to still be the same file.
        // See `same_file` for what separates them and why the answer is to
        // refuse rather than to recover.
        if !same_file(&file, &self.path)? {
            // Cloned because `io_error` above borrows `path` and is used again
            // further down. Moving it here would end that borrow early and the
            // compiler refuses, which is the borrow checker doing its job.
            return Err(TrialError::Replaced { path: path.clone() });
        }

        // Re-read inside the lock. Whatever `self` held is now irrelevant, and
        // treating it as relevant is what lost seventeen of twenty trials.
        let mut committed = TrialLog::load(&self.path)?;
        committed.verify()?;

        let prev_hash = committed
            .entries
            .last()
            .map(|entry| entry.entry_hash.clone())
            .unwrap_or_else(|| ZERO_HASH.to_string());

        let entry = TrialEntry::new(Timestamp::now(), program, config_hash, sharpe, prev_hash)?;

        let mut line =
            serde_json::to_string(&entry).map_err(|source| TrialError::Canonical { source })?;
        line.push('\n');
        if !committed.ends_with_newline {
            line.insert(0, '\n');
        }

        file.write_all(line.as_bytes()).map_err(io_error)?;
        file.sync_data().map_err(io_error)?;

        // Checked a second time, because the replacement can just as easily
        // land after the first check as before it. This one decides whether the
        // bytes that were just made durable are reachable by name at all. If
        // they are not, `self` is left describing what it described before, so
        // nothing in this process claims the trial was recorded.
        if !same_file(&file, &self.path)? {
            return Err(TrialError::Replaced { path: path.clone() });
        }

        drop(file);
        // ---- critical section ends ---------------------------------------

        // In-memory state only changes once the bytes are on disk, so a failed
        // write leaves the log describing what the file actually holds. It is
        // replaced rather than pushed to, because the reload may have picked up
        // entries another writer committed while this one waited on the lock.
        committed.entries.push(entry);
        committed.ends_with_newline = true;
        self.entries = committed.entries;
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
    use std::str::FromStr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

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
            &ConfigHash::parse(ZERO_HASH).expect("the genesis config_hash is 64 hex zeros"),
            None,
            ZERO_HASH,
        )
        .expect("genesis builds")
    }

    fn trial(program: &str, seed: &str, sharpe: Option<&str>, prev: &str) -> TrialEntry {
        TrialEntry::new(
            Timestamp::from_str("2026-08-08T01:00:00Z").expect("valid timestamp"),
            program,
            &ConfigHash::of(seed.as_bytes()),
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
        log.append("momentum-v1", &ConfigHash::of(b"cfg-a"), None)
            .expect("first append succeeds");
        log.append(
            "momentum-v1",
            &ConfigHash::of(b"cfg-b"),
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
        log.append("momentum-v1", &ConfigHash::of(b"cfg-a"), None)
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
            log.append("momentum-v1", &ConfigHash::of(b"cfg-b"), None)
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

    /// The regression test for the lost-trial defect, and the reason
    /// [`TrialLog::append`] locks.
    ///
    /// It fails in two distinct ways against the broken implementation, and
    /// both were confirmed by reverting the fix.
    ///
    /// - Remove the lock entirely and writers interleave.
    /// - Keep the lock but move the reload back outside it, which is the
    ///   tempting half fix, and every writer still computes `prev_hash` from
    ///   the same stale snapshot. The file then holds sibling entries, so it
    ///   has the right number of lines and does not verify.
    ///
    /// The barrier is what makes the second case reliable rather than a matter
    /// of scheduling luck.
    #[test]
    fn concurrent_appends_all_land_and_leave_a_verifying_chain() {
        const WRITERS: usize = 16;

        let path = std::env::temp_dir().join(format!(
            "rigor-test-concurrent-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(&path, format!("{}\n", line_of(&genesis()))).expect("seed log writes");

        // A Barrier releases every waiting thread only once all of them have
        // arrived. It is here to force the worst case rather than hope for it.
        // Every writer loads the log before waiting, so every writer reaches
        // the append holding an identical and already stale snapshot. A lock
        // on its own does not save that. Only re-reading inside the lock does.
        let barrier = std::sync::Barrier::new(WRITERS);

        // Borrowed rather than moved, so each closure can capture the shared
        // reference by copy while still taking `index` by value.
        let path = &path;
        let barrier = &barrier;

        // `thread::scope` is what lets these threads borrow locals at all. The
        // scope cannot return until every thread inside it has joined, which is
        // the proof the compiler needs that the borrows outlive the threads.
        std::thread::scope(|scope| {
            for index in 0..WRITERS {
                scope.spawn(move || {
                    let mut log = TrialLog::load(path).expect("writer loads the log");
                    barrier.wait();
                    log.append(
                        "race",
                        &ConfigHash::of(format!("cfg-{index}").as_bytes()),
                        None,
                    )
                    .expect("every concurrent append must succeed");
                });
            }
        });

        let reloaded = TrialLog::load(path).expect("the log reloads");

        reloaded
            .verify()
            .expect("concurrent appends left a chain that does not verify");
        assert_eq!(
            reloaded.entries().len(),
            WRITERS + 1,
            "expected genesis plus one line per writer"
        );
        assert_eq!(
            reloaded.lifetime_count(),
            WRITERS,
            "trials were lost, which is the failure that makes every result look better than it is"
        );
        assert_eq!(reloaded.count_for("race"), WRITERS, "scoped count is wrong");

        // Every writer used a distinct config, so a log that dropped or
        // duplicated one shows up here even if the counts happened to line up.
        let mut configs: Vec<&str> = reloaded
            .trials()
            .iter()
            .map(|entry| entry.config_hash.as_str())
            .collect();
        configs.sort_unstable();
        configs.dedup();
        assert_eq!(
            configs.len(),
            WRITERS,
            "two writers recorded the same config"
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

    /// A log swapped out from under a running append must fail loudly rather
    /// than report success onto a file nobody will ever read again.
    ///
    /// # How the race is arranged, and what could not be arranged
    ///
    /// The appender has to open the file *before* the path is redirected. If it
    /// opens after, it opens the replacement and there is nothing to detect.
    /// The only lever this test has over that ordering is the lock. It holds an
    /// exclusive lock on the original file, so the appender opens that file and
    /// then blocks, and the rename lands while it is blocked.
    ///
    /// What cannot be arranged is proof that the appender reached its `open`
    /// before the rename, because nothing observable happens between a thread
    /// setting a flag and the syscall it makes next. So the attempt is retried.
    /// Losing the race makes the append plainly succeed, which is detected and
    /// retried rather than reported as a failure.
    ///
    /// It still falsifies what it is here for. With both identity checks
    /// removed every attempt succeeds, so the test fails. With either one
    /// removed the other catches it, because the path stays redirected for the
    /// whole of the append.
    #[test]
    fn an_append_onto_a_replaced_log_refuses_rather_than_reporting_success() {
        const ATTEMPTS: usize = 16;

        let mut outcome = None;

        for attempt in 0..ATTEMPTS {
            let dir = std::env::temp_dir().join(format!(
                "rigor-test-replaced-{}-{attempt}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch directory");

            let path = dir.join("trials.jsonl");
            let seed = format!("{}\n", line_of(&genesis()));
            std::fs::write(&path, &seed).expect("seed log writes");

            // The appender opens this same file and then blocks on this lock.
            let holder = OpenOptions::new()
                .read(true)
                .append(true)
                .open(&path)
                .expect("the holder opens the log");
            holder.lock().expect("the holder takes the exclusive lock");

            // Byte for byte identical to the file it replaces, so that nothing
            // except the identity of the file distinguishes the two. An append
            // that landed on the wrong one would leave a log that looks
            // entirely reasonable, which is what makes this worth detecting.
            let replacement = dir.join("restored.jsonl");
            std::fs::write(&replacement, &seed).expect("the replacement writes");

            let started = AtomicBool::new(false);
            let path_ref = &path;
            let started_ref = &started;

            let result = std::thread::scope(|scope| {
                let appender = scope.spawn(move || {
                    let mut log = TrialLog::load(path_ref).expect("the appender loads the log");
                    started_ref.store(true, Ordering::SeqCst);
                    // The error is returned rather than unwrapped, because a
                    // lost race is an ordinary outcome here and not a bug.
                    log.append("race", &ConfigHash::of(b"cfg"), None)
                        .map(|_| ())
                        .err()
                });

                while !started.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                // Longer than the appender's next syscall by several orders of
                // magnitude. Not a guarantee, which is what the retry is for.
                std::thread::sleep(Duration::from_millis(50));

                std::fs::rename(&replacement, path_ref).expect("the log is replaced");
                drop(holder);

                appender.join().expect("the appender thread does not panic")
            });

            if let Some(error) = result {
                outcome = Some(error);
                break;
            }
        }

        let error = outcome.expect(
            "no attempt detected the replacement, so an append can land on an orphaned file and \
             still report the trial as recorded",
        );
        assert!(
            matches!(error, TrialError::Replaced { .. }),
            "the replacement must be named as such rather than surfacing as something else, \
             got {error:?}"
        );
    }

    /// The shared lock on the reader is real rather than decorative.
    ///
    /// A writer takes the exclusive lock, flips a flag just before releasing
    /// it, and the reader starts only once the lock is confirmed held. A reader
    /// that returns while that flag is still false did not wait, so it took no
    /// lock. `flock` genuinely blocks, so the passing direction here is not a
    /// matter of timing luck.
    ///
    /// Note what this does not test. It does not test that a reader never sees
    /// a half written line, because the window between an appender's
    /// `write_all` and its `sync_data` is microseconds wide and cannot be
    /// opened up from outside. See [`TrialLog::load_shared_locked`], which is
    /// deliberately modest about what the lock is worth.
    #[test]
    fn a_shared_locked_read_waits_for_an_exclusive_writer() {
        let path =
            std::env::temp_dir().join(format!("rigor-test-shared-{}.jsonl", std::process::id()));
        std::fs::write(&path, format!("{}\n", line_of(&genesis()))).expect("seed log writes");

        let locked = AtomicBool::new(false);
        let released = AtomicBool::new(false);
        let path = &path;
        let locked = &locked;
        let released = &released;

        std::thread::scope(|scope| {
            scope.spawn(move || {
                let file = OpenOptions::new()
                    .read(true)
                    .append(true)
                    .open(path)
                    .expect("the writer opens the log");
                file.lock().expect("the writer takes the exclusive lock");
                locked.store(true, Ordering::SeqCst);

                // Nothing is written. What is under test is the waiting.
                std::thread::sleep(Duration::from_millis(200));

                // Stored before the unlock and never after, so a reader that
                // genuinely waited cannot observe it as false.
                released.store(true, Ordering::SeqCst);
                drop(file);
            });

            while !locked.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }

            let log = TrialLog::load_shared_locked(path).expect("the shared read succeeds");

            assert!(
                released.load(Ordering::SeqCst),
                "the reader returned while a writer still held the exclusive lock, so \
                 load_shared_locked is not taking a lock at all"
            );
            log.verify()
                .expect("the log read under the shared lock verifies");
        });
    }
}
