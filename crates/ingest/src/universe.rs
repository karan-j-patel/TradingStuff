//! The research universe: which securities a backtest is allowed to look at,
//! and how that list was arrived at.
//!
//! # Why a rule and not a list
//!
//! *Survivorship bias* is the error of measuring only the things that lasted
//! and reading the answer as though it described everything. A universe typed
//! out by hand has it built in, because whoever types the tickers already knows
//! which companies are still around, and the strategy then earns the returns of
//! companies selected for having survived.
//!
//! `CLAUDE.md` rule 4 forbids that, so the universe here is a *rule* applied to
//! the vendor's whole security master rather than a list anybody chose. The
//! rule is arithmetic on the vendor's permanent identifier, which is assigned
//! by the vendor for its own reasons and knows nothing about returns.
//!
//! Delisted securities stay in. That is the entire point, and it is why nothing
//! in this module reads `is_delisted` for anything except recording it.
//!
//! # Why a sample and not the whole master
//!
//! Cost. Prices are fetched one security at a time against a host that
//! publishes no rate limits, so the full master is a fetch budget this project
//! does not have yet. A deterministic sample of the master is still
//! survivorship-clean; a smaller hand-picked list would not be. The full-
//! universe run waits on a bigger budget, and this file records exactly which
//! sample was taken so that the two are comparable when it happens.

use std::collections::BTreeMap;

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::schema::{AssetKey, PermanentId};
use crate::sharadar::TickerRow;

/// The divisor in the sample rule.
///
/// Prime, so the residue classes it produces do not line up with any run of
/// consecutive identifiers the vendor may have issued in one batch. 17 keeps
/// roughly one security in seventeen.
pub const SAMPLE_MODULUS: u64 = 17;

/// The residue the sample rule keeps.
///
/// Arbitrary and fixed. Arbitrary because no value of it can be argued for on
/// any grounds that would not also be a way of choosing the answer. Fixed
/// because it goes into the recorded configuration, and a residue that moved
/// between runs would make two runs incomparable while looking identical.
///
/// Changing this is choosing a different universe, so it is a new trial rather
/// than a rerun of the same one.
pub const SAMPLE_RESIDUE: u64 = 3;

/// The last day a security may have started trading and still be eligible.
///
/// # Where the value came from, and why it stayed after its reason expired
///
/// It was the free key's measured window start when this universe was first
/// defined on 2026-08-10. The paid bundle bought later the same day measures
/// 1997-12-31 instead, so the cutoff no longer marks the edge of what the
/// vendor will serve.
///
/// It is kept unchanged anyway, and the reason is reproducibility rather than
/// inertia. This constant decides which securities are in the sample, the
/// sample decides the file, and the file's hash goes into a trial's recorded
/// configuration. Moving it silently redefines the universe between two runs
/// of what looks like the same configuration. Re-deriving it is a decision and
/// a recorded one, not a consequence of changing plan.
///
/// What it still does is useful on its own terms: a security that first traded
/// after this date has no lead-in history before the backtest window, so it
/// could never satisfy the signal's look-back requirement anyway.
pub const COVERAGE_CUTOFF: Date = Date::constant(2021, 8, 10);

/// Why a security master could not be turned into a universe.
///
/// Both variants are refusals rather than recoveries. A row that cannot be
/// sampled is not dropped quietly, because a universe silently missing names is
/// a universe nobody can reproduce.
#[derive(Debug, thiserror::Error)]
pub enum UniverseError {
    #[error(
        "the security {ticker} carries no Sharadar permaticker, so the sample rule \
         has nothing to apply and dropping it silently would change the universe"
    )]
    NoPermanentId { ticker: String },

    #[error(
        "the permaticker {permaticker} appears on more than one row ({first} and {second}), \
         so the security master is not keyed the way this sample assumes"
    )]
    DuplicatePermaticker {
        permaticker: u64,
        first: String,
        second: String,
    },
}

/// What happened when prices were fetched for one universe member.
///
/// Recorded rather than inferred. A name with no bars and a name nobody asked
/// for look identical in a prices file, and the difference decides whether the
/// universe was 600 securities or 600 attempts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FetchOutcome {
    /// Bars landed. The dates are what the vendor actually served, which is not
    /// necessarily the window that was asked for.
    Served {
        bars: usize,
        first: Date,
        last: Date,
    },
    /// The vendor declined this security, with its reason.
    Declined { reason: String },
}

/// One security in the universe, with everything the sample was decided on.
///
/// The vendor's own dates are carried rather than looked up again later. They
/// are what the selection was made against, so storing them makes the file
/// self-explaining: a reader can check the rule without refetching the master.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UniverseEntry {
    pub asset: AssetKey,
    pub name: Option<String>,
    pub exchange: Option<String>,
    /// Whether the vendor considers this security delisted.
    ///
    /// Recorded and never filtered on. See the module documentation.
    pub is_delisted: bool,
    pub first_price_date: Date,
    pub last_price_date: Option<Date>,
    /// `None` until prices have been fetched for this name.
    pub outcome: Option<FetchOutcome>,
}

impl UniverseEntry {
    /// The vendor's permanent identifier, which the sample rule is applied to.
    pub fn permaticker(&self) -> Option<u64> {
        match self.asset.permanent {
            Some(PermanentId::Sharadar(id)) => Some(id),
            _ => None,
        }
    }
}

/// Whether one identifier is in the sample.
///
/// Separate from [`select`] so the rule can be stated in one line and tested
/// without a security master around it.
pub fn in_sample(permaticker: u64) -> bool {
    permaticker % SAMPLE_MODULUS == SAMPLE_RESIDUE
}

/// The coverage rule on its own, before the sample rule narrows anything.
///
/// `Ok(None)` means the security is outside the covered window, which is an
/// ordinary outcome. An error means the row cannot be judged at all.
///
/// Shared by [`select`] and [`first_price_year_counts`] deliberately. The two
/// have to agree on what "eligible" means or the distribution would describe a
/// population the universe was not drawn from, which is the exact
/// circumstance under which a skew check reads as reassuring and is not.
fn eligible(row: &TickerRow) -> Result<Option<(u64, Date)>, UniverseError> {
    let Some(PermanentId::Sharadar(permaticker)) = row.asset.permanent else {
        return Err(UniverseError::NoPermanentId {
            ticker: row.asset.ticker.clone(),
        });
    };

    // A security with no first price date has no stated coverage at all, so it
    // cannot satisfy a rule about when coverage began. Refusing it is the same
    // decision as refusing one that started too late.
    Ok(row
        .first_price_date
        .filter(|date| *date <= COVERAGE_CUTOFF)
        .map(|date| (permaticker, date)))
}

/// How the covered securities and the sampled ones spread across listing eras.
///
/// Keyed by the year of `first_price_date`, valued as the eligible count and
/// the sampled count for that year.
///
/// # The question this answers
///
/// The sample rule is arithmetic on the vendor's permanent identifier, and
/// those identifiers appear to be issued in roughly increasing order over time.
/// A modulo over a chronologically assigned identifier is uniform *within* any
/// dense run of identifiers, but nothing guarantees the vendor issued them at a
/// constant rate, and a rule that took one listing era more heavily than
/// another would be sampling by era rather than by cross-section.
///
/// The answer is arithmetic rather than argument, so it is reported rather than
/// reasoned about.
pub fn first_price_year_counts(
    rows: &[TickerRow],
) -> Result<BTreeMap<i16, YearCounts>, UniverseError> {
    let mut counts: BTreeMap<i16, YearCounts> = BTreeMap::new();

    for row in rows {
        let Some((permaticker, first_price_date)) = eligible(row)? else {
            continue;
        };
        let year = counts.entry(first_price_date.year()).or_default();
        year.eligible += 1;
        if in_sample(permaticker) {
            year.sampled += 1;
        }
    }

    Ok(counts)
}

/// Eligible and sampled counts for one listing year.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct YearCounts {
    /// Securities whose coverage began in this year and reaches the window.
    pub eligible: usize,
    /// How many of those the sample rule kept.
    pub sampled: usize,
}

/// Turn a security master into a universe.
///
/// Three filters, in this order and no others.
///
/// 1. The security must have started trading on or before [`COVERAGE_CUTOFF`],
///    so that a lead-in window exists inside the served history.
/// 2. The permanent identifier must satisfy [`in_sample`].
/// 3. Nothing else. In particular neither `is_delisted` nor `last_price_date`
///    is consulted, because filtering on either is survivorship bias.
///
/// The result is ordered by permanent identifier, which makes the file this
/// produces byte-identical across two runs over the same master. The hash of
/// that file is what a trial records, so an unstable order would make an
/// otherwise identical configuration look like a different one.
pub fn select(rows: &[TickerRow]) -> Result<Vec<UniverseEntry>, UniverseError> {
    let mut selected: Vec<UniverseEntry> = Vec::new();

    for row in rows {
        let Some((permaticker, first_price_date)) = eligible(row)? else {
            continue;
        };
        if !in_sample(permaticker) {
            continue;
        }

        if let Some(existing) = selected
            .iter()
            .find(|entry| entry.permaticker() == Some(permaticker))
        {
            return Err(UniverseError::DuplicatePermaticker {
                permaticker,
                first: existing.asset.ticker.clone(),
                second: row.asset.ticker.clone(),
            });
        }

        selected.push(UniverseEntry {
            asset: row.asset.clone(),
            name: row.name.clone(),
            exchange: row.exchange.clone(),
            is_delisted: row.is_delisted,
            first_price_date,
            last_price_date: row.last_price_date,
            outcome: None,
        });
    }

    // Sorted on the identifier the sample was taken on, so the file is a
    // function of the master rather than of the order the pages arrived in.
    selected.sort_by_key(|entry| entry.permaticker());
    Ok(selected)
}

/// Serialise a universe as JSONL, one entry per line, trailing newline.
///
/// JSONL rather than Parquet because this file is read by people as often as by
/// code, and because it is an input to a curated dataset rather than one of
/// them.
pub fn to_jsonl(entries: &[UniverseEntry]) -> Result<String, serde_json::Error> {
    let mut text = String::new();
    for entry in entries {
        text.push_str(&serde_json::to_string(entry)?);
        text.push('\n');
    }
    Ok(text)
}

/// Parse a universe file, naming the line that failed.
///
/// A blank or malformed line is an error rather than a skipped record, for the
/// same reason the curated readers are strict: a universe that is quietly one
/// name short still produces a number, and nobody finds out.
pub fn from_jsonl(text: &str) -> Result<Vec<UniverseEntry>, UniverseParseError> {
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|source| UniverseParseError {
                line: index + 1,
                source,
            })
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
#[error("the universe file could not be parsed at line {line}: {source}")]
pub struct UniverseParseError {
    pub line: usize,
    #[source]
    pub source: serde_json::Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(permaticker: u64, ticker: &str, delisted: bool, first_price: Option<Date>) -> TickerRow {
        TickerRow {
            asset: AssetKey {
                ticker: ticker.to_string(),
                permanent: Some(PermanentId::Sharadar(permaticker)),
            },
            name: Some("Fabricated Holdings Inc".to_string()),
            exchange: Some("NASDAQ".to_string()),
            category: Some("Domestic Common Stock".to_string()),
            is_delisted: delisted,
            first_price_date: first_price,
            last_price_date: Some(Date::constant(2026, 8, 7)),
        }
    }

    /// Well inside the window, so a test about one filter is not accidentally
    /// also a test about the other.
    fn early() -> Option<Date> {
        Some(Date::constant(2015, 1, 2))
    }

    fn tickers(entries: &[UniverseEntry]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.asset.ticker.as_str())
            .collect()
    }

    /// Hand-checked against the rule, which is `permaticker % 17 == 3`.
    ///
    /// 3 % 17 = 3, kept. 17 % 17 = 0, dropped. 20 % 17 = 3, kept.
    /// 21 % 17 = 4, dropped. 37 % 17 = 3, kept. 100 % 17 = 15, dropped.
    #[test]
    fn u1_the_sample_keeps_exactly_the_residue_class() {
        let master = [
            row(3, "AAA", false, early()),
            row(17, "BBB", false, early()),
            row(20, "CCC", false, early()),
            row(21, "DDD", false, early()),
            row(37, "EEE", false, early()),
            row(100, "FFF", false, early()),
        ];

        let universe = select(&master).expect("selection succeeds");

        assert_eq!(tickers(&universe), ["AAA", "CCC", "EEE"]);
    }

    #[test]
    fn u2_the_coverage_cutoff_is_inclusive_on_its_own_day() {
        // All three are in the residue class, so the only thing separating them
        // is the date. One day either side of the cutoff and the cutoff itself.
        let master = [
            row(3, "ON", false, Some(COVERAGE_CUTOFF)),
            row(20, "AFTER", false, Some(Date::constant(2021, 8, 11))),
            row(37, "BEFORE", false, Some(Date::constant(2021, 8, 9))),
        ];

        let universe = select(&master).expect("selection succeeds");

        assert_eq!(
            tickers(&universe),
            ["ON", "BEFORE"],
            "the cutoff must include its own day and exclude the day after"
        );
    }

    /// Rule 4. This is the test the whole module exists for.
    #[test]
    fn u3_a_delisted_security_stays_in_the_universe() {
        let master = [
            row(3, "ALIVE", false, early()),
            row(20, "GONE", true, early()),
        ];

        let universe = select(&master).expect("selection succeeds");

        assert_eq!(
            tickers(&universe),
            ["ALIVE", "GONE"],
            "a delisted security was filtered out, which is survivorship bias"
        );
        assert!(universe[1].is_delisted, "the status must still be recorded");
    }

    #[test]
    fn u3_a_security_still_trading_is_not_filtered_either() {
        // The guard above must not be satisfiable by keeping everything that is
        // delisted and nothing else.
        let master = [row(3, "ALIVE", false, early())];
        assert_eq!(tickers(&select(&master).expect("selects")), ["ALIVE"]);
    }

    #[test]
    fn u4_a_security_with_no_first_price_date_is_excluded() {
        let master = [
            row(3, "NODATE", false, None),
            row(20, "DATED", false, early()),
        ];

        let universe = select(&master).expect("selection succeeds");

        assert_eq!(
            tickers(&universe),
            ["DATED"],
            "a security with no stated coverage cannot satisfy a coverage rule"
        );
    }

    #[test]
    fn u5_a_row_without_a_permaticker_is_an_error_rather_than_a_skip() {
        // The sample rule is arithmetic on the permanent identifier. A row
        // without one cannot be sampled, and skipping it would remove a name
        // from the universe with nothing recording that it happened.
        let mut orphan = row(3, "ORPHAN", false, early());
        orphan.asset.permanent = None;

        let error = select(&[orphan]).expect_err("a row with no permaticker must be refused");

        assert!(matches!(error, UniverseError::NoPermanentId { .. }));
        assert!(
            error.to_string().contains("ORPHAN"),
            "the error must name the security it refused: {error}"
        );
    }

    #[test]
    fn u6_a_repeated_permaticker_is_an_error() {
        // Two rows sharing a permanent identifier means the master is not keyed
        // as this sample assumes, and one of the two would silently win.
        let master = [
            row(3, "FIRST", false, early()),
            row(3, "SECOND", false, early()),
        ];

        let error = select(&master).expect_err("a repeated permaticker must be refused");

        assert!(matches!(
            error,
            UniverseError::DuplicatePermaticker { permaticker: 3, .. }
        ));
    }

    #[test]
    fn u7_a_universe_round_trips_through_jsonl() {
        let master = [
            row(3, "AAA", false, early()),
            row(20, "GONE", true, early()),
        ];
        let mut universe = select(&master).expect("selection succeeds");
        universe[0].outcome = Some(FetchOutcome::Served {
            bars: 1258,
            first: Date::constant(2021, 8, 10),
            last: Date::constant(2026, 8, 7),
        });
        universe[1].outcome = Some(FetchOutcome::Declined {
            reason: "the vendor served no rows".to_string(),
        });

        let text = to_jsonl(&universe).expect("serialises");
        let parsed = from_jsonl(&text).expect("parses");

        assert_eq!(parsed, universe);
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn u7_a_malformed_line_names_its_line_number() {
        let error = from_jsonl("{\"not\":\"an entry\"}\n").expect_err("must be refused");
        assert_eq!(error.line, 1);
    }

    /// The distribution has to describe the population the universe was drawn
    /// from. If the two ever disagree about what "eligible" means, a skew check
    /// reads as reassuring while describing a different set of securities.
    #[test]
    fn u9_the_year_distribution_counts_the_same_population_select_draws_from() {
        let master = [
            // Two eras, and a name excluded by the cutoff in each, so a
            // distribution built on the wrong predicate shows up as a count.
            row(3, "OLD-IN", false, Some(Date::constant(1999, 3, 1))),
            row(4, "OLD-OUT", false, Some(Date::constant(1999, 6, 1))),
            row(20, "NEW-IN", false, Some(Date::constant(2015, 2, 2))),
            row(21, "NEW-OUT", false, Some(Date::constant(2015, 4, 4))),
            row(37, "TOO-LATE", false, Some(Date::constant(2021, 8, 11))),
            row(54, "NO-DATE", false, None),
        ];

        let counts = first_price_year_counts(&master).expect("counts");
        let universe = select(&master).expect("selects");

        assert_eq!(
            counts[&1999],
            YearCounts {
                eligible: 2,
                sampled: 1
            }
        );
        assert_eq!(
            counts[&2015],
            YearCounts {
                eligible: 2,
                sampled: 1
            }
        );
        assert!(
            !counts.contains_key(&2021),
            "a security past the cutoff must not appear in the distribution at all"
        );

        let sampled: usize = counts.values().map(|year| year.sampled).sum();
        assert_eq!(
            sampled,
            universe.len(),
            "the distribution's sampled total disagrees with the universe it is \
             supposed to describe"
        );
    }

    #[test]
    fn u9_a_row_without_a_permaticker_is_refused_by_the_distribution_too() {
        // Same refusal as `select`, because the two share one predicate. A
        // distribution that silently skipped what `select` refuses would put a
        // different denominator behind the skew figure.
        let mut orphan = row(3, "ORPHAN", false, early());
        orphan.asset.permanent = None;

        assert!(matches!(
            first_price_year_counts(&[orphan]).expect_err("must be refused"),
            UniverseError::NoPermanentId { .. }
        ));
    }

    /// The file's hash goes into a trial's recorded configuration, so the file
    /// has to be a function of the master and not of the order it arrived in.
    #[test]
    fn u8_selection_is_ordered_by_permaticker_whatever_order_the_pages_arrived_in() {
        let forwards = [
            row(3, "AAA", false, early()),
            row(20, "BBB", false, early()),
            row(37, "CCC", false, early()),
        ];
        let backwards = [
            row(37, "CCC", false, early()),
            row(3, "AAA", false, early()),
            row(20, "BBB", false, early()),
        ];

        let one = select(&forwards).expect("selects");
        let other = select(&backwards).expect("selects");

        assert_eq!(tickers(&one), ["AAA", "BBB", "CCC"]);
        assert_eq!(
            to_jsonl(&one).expect("serialises"),
            to_jsonl(&other).expect("serialises"),
            "two orderings of the same master produced different files, so the recorded \
             config hash would depend on page arrival order"
        );
    }
}
