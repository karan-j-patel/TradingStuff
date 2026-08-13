//! Curated Parquet files: the on-disk form of validated records.
//!
//! Rust writes these files and Python reads them. That is the whole boundary
//! between the two languages in this project, and neither side calls the other.
//!
//! # Why not DuckDB
//!
//! DuckDB is SQL-only here by standing rule, and no `RecordBatch` ever crosses
//! between the two stacks. The `duckdb` crate pins `arrow ^58` while this module
//! is built on `arrow 59`, and two arrow majors do not unify, so a dependency on
//! both would not resolve. That is a symptom rather than the reason. The reason
//! is that one Arrow stack with one owner is auditable and two are not.
//!
//! # Why the reader is strict
//!
//! Curated files are written and read by this codebase on both sides. There is
//! no third party to be lenient towards, so a file that violates an invariant
//! means a bug in this crate, and the reader says so loudly.
//!
//! This is deliberately the opposite of the trial log, whose readers are lenient
//! because that file has to stay reportable even when it has been damaged. The
//! difference is what each file is for. A damaged trial log must still testify
//! about the trials it can still read. A damaged curated file has no such duty,
//! and reading it as though it were fine would put wrong numbers into a result.
//!
//! # Identity on disk
//!
//! Three columns, shared by every curated dataset: `ticker`,
//! `permanent_id_kind`, and `permanent_id`. See [`codec::Identity`] for why
//! identity is not flattened into a single string.
//!
//! # Why every dataset has a `read_*_from_bytes` beside its `read_*`
//! <a name="read_from_bytes"></a>
//!
//! A caller that records a file's SHA-256 in a trial and then reads that file's
//! records is making a claim: these numbers came from the bytes under that
//! digest. Hashing one `read` and parsing a second `read` of the same path does
//! not support the claim. Between the two reads the file can be replaced, by a
//! concurrent refetch or by a hand, and the run then produces results from data
//! B under a hash recording data A. Nothing downstream can detect it, because
//! the digest is the only description of the input the trial log keeps.
//!
//! So the bytes are read once and both the digest and the records come from
//! that one buffer. The path-based readers are unchanged in behaviour and
//! delegate to the same body, which is what keeps the metadata and provenance
//! checks identical on both routes rather than merely intended to be.

use std::collections::HashSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use jiff::civil::Date;

mod actions;
mod codec;
mod delistings;
mod enums;
mod error;
mod marketcap;
mod prices;

#[cfg(test)]
mod tests;

pub use actions::{read_actions, read_actions_from_bytes, write_actions};
pub use delistings::{
    DelistingProvenance, delistings_provenance, read_delistings, read_delistings_from_bytes,
    write_delistings,
};
pub use error::CurateError;
pub use marketcap::{
    MarketCapProvenance, UNITS, UNITS_KEY, marketcap_provenance, read_marketcap,
    read_marketcap_from_bytes, write_marketcap,
};
pub use prices::{
    BASIS, BASIS_KEY, Provenance, SOURCE_KEY, prices_provenance, read_prices, write_prices,
};

use crate::schema::AssetKey;
use codec::{Identity, describe, encode_identity};

/// Where curated data lives when nothing says otherwise.
pub const DEFAULT_DATA_ROOT: &str = "data";

/// Resolve the data root: the flag, then `DATA_ROOT`, then the default.
///
/// The flag wins so a test or a one-off run can point somewhere harmless
/// without touching the operator's environment.
pub fn data_root(flag: Option<&str>) -> PathBuf {
    if let Some(path) = flag {
        return PathBuf::from(path);
    }
    match std::env::var_os("DATA_ROOT") {
        // An empty `DATA_ROOT` is a sourced but unfilled `.env.example`, which
        // means unset rather than "write to the current directory".
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(DEFAULT_DATA_ROOT),
    }
}

// One file per dataset.
//
// Known ceiling: partition by year when file size demands it. The reader
// already iterates record batches, so the upgrade is a directory of yearly
// files behind these same two functions rather than a change to any caller.

pub fn prices_path(data_root: &Path) -> PathBuf {
    data_root.join("curated/prices/prices.parquet")
}

pub fn delistings_path(data_root: &Path) -> PathBuf {
    data_root.join("curated/delistings/delistings.parquet")
}

pub fn actions_path(data_root: &Path) -> PathBuf {
    data_root.join("curated/actions/actions.parquet")
}

pub fn marketcap_path(data_root: &Path) -> PathBuf {
    data_root.join("curated/marketcap/marketcap.parquet")
}

/// How many rejected records to name before the message stops being useful.
const REPORTED_REJECTS: usize = 5;

/// Build the error a batch failing domain validation is refused with.
///
/// Shared by both writers so neither can report less than the other. The label
/// closure is the only per-dataset part, since a `PriceBar` and a `Delisting`
/// name themselves differently.
pub(crate) fn invalid_records<T, E: std::fmt::Display>(
    dataset: &'static str,
    total: usize,
    rejected: &[(T, E)],
    label: impl Fn(&T) -> String,
) -> CurateError {
    let mut detail = String::new();
    for (record, reason) in rejected.iter().take(REPORTED_REJECTS) {
        detail.push_str(&format!("\n  {}: {reason}", label(record)));
    }
    if rejected.len() > REPORTED_REJECTS {
        detail.push_str(&format!(
            "\n  and {} more",
            rejected.len() - REPORTED_REJECTS
        ));
    }
    CurateError::InvalidRecords {
        dataset,
        total,
        rejected: rejected.len(),
        detail,
    }
}

/// Reject an empty or duplicated batch, then put it in a deterministic order.
///
/// Shared by both datasets so the three rules land once rather than per caller.
/// `key_of` extracts the identity and date from whatever row type is passed,
/// which is the only thing the two datasets have in common.
///
/// # Lookahead safety
///
/// Sorting reorders whole rows and moves no information across time. Every
/// field travels with the row it belongs to, so no value observed on a later
/// date can reach an earlier one. Rows are never merged, split, or filled.
pub(crate) fn prepare<T>(
    dataset: &'static str,
    rows: Vec<T>,
    key_of: impl Fn(&T) -> RowKey<'_>,
) -> Result<Vec<(Identity, T)>, CurateError> {
    if rows.is_empty() {
        return Err(CurateError::EmptyDataset { dataset });
    }

    // Duplicate detection uses `AssetKey`'s own equality, not the ticker. Two
    // rows carrying the same permanent id under different tickers are the same
    // security, and a check keyed on the ticker string would miss exactly that
    // case, which is the one worth catching.
    let mut seen: HashSet<(AssetKey, Date, Option<&'static str>, Option<&'static str>)> =
        HashSet::with_capacity(rows.len());
    for row in &rows {
        let key = key_of(row);
        if !seen.insert((key.asset.clone(), key.date, key.kind, key.subkind)) {
            return Err(CurateError::DuplicateRow {
                dataset,
                asset: describe(key.asset),
                date: key.date,
                qualifier: key.qualifier(),
            });
        }
    }

    let mut decorated: Vec<(Identity, T)> = rows
        .into_iter()
        .map(|row| {
            // The borrow of `row` ends before the move into the tuple, which is
            // why this needs its own block.
            let identity = encode_identity(key_of(&row).asset);
            (identity, row)
        })
        .collect();

    decorated.sort_by(|a, b| {
        let left = sort_key(&a.0, &key_of(&a.1));
        let right = sort_key(&b.0, &key_of(&b.1));
        left.cmp(&right)
    });

    Ok(decorated)
}

/// What makes a row unique, and what decides where it is written.
///
/// Prices and delistings are keyed on (asset, date) alone. Corporate actions
/// are not: a regular cash dividend and a special dividend on the same
/// ex-date are two real records, so the kind and the dividend kind are part of
/// the key rather than evidence of a duplicate.
pub(crate) struct RowKey<'a> {
    pub asset: &'a AssetKey,
    pub date: Date,
    pub kind: Option<&'static str>,
    pub subkind: Option<&'static str>,
}

impl<'a> RowKey<'a> {
    /// For datasets where (asset, date) is the whole key.
    pub fn simple(asset: &'a AssetKey, date: Date) -> Self {
        Self {
            asset,
            date,
            kind: None,
            subkind: None,
        }
    }

    /// The discriminating part of the key, for an error that has to name it.
    fn qualifier(&self) -> String {
        match (self.kind, self.subkind) {
            (None, _) => String::new(),
            (Some(kind), None) => format!(" ({kind})"),
            (Some(kind), Some(sub)) => format!(" ({kind} {sub})"),
        }
    }
}

/// The total order rows are written in: identity kind with nulls last, then the
/// identifier, the ticker, the date, and finally the per-dataset discriminators.
///
/// The leading `bool` is the nulls-last rule. `false` sorts before `true`, so
/// rows carrying a permanent id come first and unidentified rows land at the
/// end. Within the unidentified group the two identity columns are both `None`
/// and contribute nothing, so ticker and date decide. The trailing terms are
/// `None` for datasets that do not use them, where they likewise contribute
/// nothing.
type SortKey<'a> = (
    bool,
    Option<&'a str>,
    Option<&'a str>,
    &'a str,
    Date,
    Option<&'static str>,
    bool,
    Option<&'static str>,
);

fn sort_key<'a>(identity: &'a Identity, key: &RowKey<'_>) -> SortKey<'a> {
    (
        identity.kind.is_none(),
        identity.kind,
        identity.id.as_deref(),
        &identity.ticker,
        key.date,
        key.kind,
        key.subkind.is_none(),
        key.subkind,
    )
}

/// Distinguishes concurrent temp files within one process. Paired with the pid
/// it makes a name no other writer will pick.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write through a temp file in the destination directory, then rename over the
/// final name.
///
/// A rename within one directory is atomic, so a reader sees either the old
/// file or the new one and never a half-written one. The temp file has to be in
/// the same directory for that to hold, since a rename across filesystems is a
/// copy. The sync happens before the rename so the bytes are durable before the
/// name that publishes them exists.
///
/// A failure removes the temp file and reports the original error, because the
/// cleanup failing is not the thing the caller needs to hear about.
pub(crate) fn write_atomically(
    path: &Path,
    write: impl FnOnce(&mut File) -> Result<(), CurateError>,
) -> Result<(), CurateError> {
    let directory = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory)
        .map_err(|e| CurateError::io(format!("creating {}", directory.display()), e))?;

    let stem = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "curated".to_owned());
    let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = directory.join(format!(".{stem}.{}.{unique}.tmp", std::process::id()));

    let result = (|| {
        let mut file = File::create(&temp)
            .map_err(|e| CurateError::io(format!("creating {}", temp.display()), e))?;
        write(&mut file)?;
        file.sync_all()
            .map_err(|e| CurateError::io(format!("syncing {}", temp.display()), e))
    })();

    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }

    fs::rename(&temp, path).map_err(|e| {
        let _ = fs::remove_file(&temp);
        CurateError::io(
            format!("renaming {} over {}", temp.display(), path.display()),
            e,
        )
    })
}

/// The fixed writer settings every curated file uses.
///
/// Fixed rather than configurable because a knob here changes bytes on disk for
/// no gain a caller could reason about. zstd is the best ratio of the codecs
/// parquet ships by default, and statistics let a reader skip row groups.
pub(crate) fn writer_properties(
    extra: &[(&str, &str)],
) -> ::parquet::file::properties::WriterProperties {
    use ::parquet::basic::{Compression, ZstdLevel};
    use ::parquet::file::metadata::KeyValue;
    use ::parquet::file::properties::{EnabledStatistics, WriterProperties};

    // File-level key-value metadata, so a reader that is not this crate — a
    // DuckDB session, a pandas call — can still see what the numbers are
    // without being told out of band.
    let metadata: Vec<KeyValue> = extra
        .iter()
        .map(|(key, value)| KeyValue::new((*key).to_string(), (*value).to_string()))
        .collect();

    WriterProperties::builder()
        .set_key_value_metadata((!metadata.is_empty()).then_some(metadata))
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .set_statistics_enabled(EnabledStatistics::Chunk)
        .build()
}

/// Open a curated file for reading, naming it if that fails.
pub(crate) fn open(path: &Path) -> Result<File, CurateError> {
    File::open(path).map_err(|e| CurateError::io(format!("opening {}", path.display()), e))
}

/// Read the units and source labels a file declares about itself.
///
/// Shared by the two datasets that store a figure in the vendor's own units,
/// market cap and delistings, so neither can end up checking less than the
/// other. The units contract is pinned by value and the source is only required
/// to be present, for the reason spelled out at
/// [`marketcap::read_marketcap`]: the platform is provider-abstracted, so
/// another vendor's legitimately curated file has to stay readable, while a
/// file that cannot say where its rows came from is not provenance at all.
pub(crate) fn units_and_source(
    dataset: &'static str,
    expected_units: &'static str,
    units_key: &'static str,
    metadata: Option<&Vec<::parquet::file::metadata::KeyValue>>,
) -> Result<(String, String), CurateError> {
    let value = |key: &str| {
        metadata
            .into_iter()
            .flatten()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.value.as_deref())
    };

    let units = match value(units_key) {
        Some(found) if found == expected_units => found.to_owned(),
        Some(other) => {
            return Err(CurateError::UnexpectedMetadata {
                dataset,
                key: units_key,
                expected: expected_units,
                found: other.to_owned(),
            });
        }
        None => {
            return Err(CurateError::MissingMetadata {
                dataset,
                key: units_key,
            });
        }
    };

    let source = value(prices::SOURCE_KEY)
        .ok_or(CurateError::MissingMetadata {
            dataset,
            key: prices::SOURCE_KEY,
        })?
        .to_owned();
    if source.trim().is_empty() {
        return Err(CurateError::UnexpectedMetadata {
            dataset,
            key: prices::SOURCE_KEY,
            expected: "a non-empty source",
            found: source,
        });
    }

    Ok((units, source))
}
