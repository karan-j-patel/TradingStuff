//! Everything that can go wrong reading or writing a curated file.
//!
//! Each variant carries enough to investigate without re-running the command:
//! which dataset, which field, which row, and the offending value. A curated
//! file is written and read by this codebase on both sides, so a violation here
//! means a bug rather than a bad upstream row, and the message is aimed at
//! whoever has to find it.

use jiff::civil::Date;

/// A failure while curating, in either direction.
///
/// `#[from]` on the two foreign variants generates `From` impls, which is what
/// lets `?` convert an arrow or parquet error into this type with no explicit
/// mapping at the call site.
#[derive(Debug, thiserror::Error)]
pub enum CurateError {
    /// Writing zero rows over a good file is data loss wearing a success
    /// message, so an empty input is refused rather than honoured.
    #[error("{dataset} has no rows, and writing a zero-row file would destroy the previous one")]
    EmptyDataset { dataset: &'static str },

    #[error(
        "{dataset} holds two rows for {asset} on {date}{qualifier}, which is never deduplicated silently"
    )]
    DuplicateRow {
        dataset: &'static str,
        asset: String,
        date: Date,
        /// Names the rest of the key for datasets where (asset, date) is not
        /// all of it. Empty for prices and delistings.
        qualifier: String,
    },

    /// Every Decimal column is stored at scale 9. A value needing more is a
    /// write error rather than a rounded value, because rounding money is how a
    /// reconciliation break gets created and then not noticed.
    #[error(
        "{dataset}.{field} value {value} is not exactly representable at scale 9, and nothing here rounds"
    )]
    InexactDecimal {
        dataset: &'static str,
        field: &'static str,
        value: String,
    },

    #[error("{dataset}.{field} value {value} does not fit a decimal(38, 9)")]
    DecimalOutOfRange {
        dataset: &'static str,
        field: &'static str,
        value: String,
    },

    /// Enums are a closed set on disk. An unrecognised label is refused rather
    /// than coerced to a default, because a coerced default is a wrong value
    /// that reads as a real one.
    #[error("{dataset}.{field} holds unknown value {value:?}, which is not in the closed set")]
    UnknownEnum {
        dataset: &'static str,
        field: &'static str,
        value: String,
    },

    #[error("{dataset}.{field} is null at row {row}, where the schema requires a value")]
    NullInRequiredColumn {
        dataset: &'static str,
        field: &'static str,
        row: usize,
    },

    /// The identity columns must be all-null or all-present together. Half of a
    /// permanent identifier is not a weaker identifier, it is a corrupt one.
    #[error(
        "{dataset} row {row} has permanent_id_kind {kind:?} and permanent_id {id:?}, which must be both null or both set"
    )]
    HalfNullIdentity {
        dataset: &'static str,
        row: usize,
        kind: Option<String>,
        id: Option<String>,
    },

    /// A `TerminalValue` state that disagrees with the columns carrying it. An
    /// imputed value that reads back as observed turns a convention into a
    /// fact, which is the whole reason these are three columns and not one.
    #[error("{dataset} row {row} is {kind}, but {detail}")]
    TerminalInvariant {
        dataset: &'static str,
        row: usize,
        kind: String,
        detail: String,
    },

    /// Records that fail the crate's own domain validation never reach a file.
    ///
    /// The writers are publicly exported, so this check cannot live in the CLI.
    /// A caller reaching `write_prices` directly is the case that matters, and
    /// it is the one the CLI guard would have missed entirely.
    #[error(
        "{dataset}: {rejected} of {total} records failed validation, so nothing was written.{detail}"
    )]
    InvalidRecords {
        dataset: &'static str,
        total: usize,
        rejected: usize,
        detail: String,
    },

    /// A corporate action row whose payload columns disagree with its kind.
    ///
    /// Each kind owns exactly one payload, so a split carrying a dividend kind
    /// or a dividend with no kind means the row was assembled from two
    /// different events. Reading it as either one would invent a fact.
    #[error("{dataset} row {row} is a {kind}, but {detail}")]
    ActionInvariant {
        dataset: &'static str,
        row: usize,
        kind: String,
        detail: String,
    },

    #[error("{dataset} has no column named {field}")]
    MissingColumn {
        dataset: &'static str,
        field: &'static str,
    },

    #[error("{dataset}.{field} is {found}, expected {expected}")]
    ColumnType {
        dataset: &'static str,
        field: &'static str,
        expected: String,
        found: String,
    },

    #[error("{dataset}.{field} holds {value:?}, which is not a valid identifier for kind {kind}")]
    MalformedPermanentId {
        dataset: &'static str,
        field: &'static str,
        kind: String,
        value: String,
    },

    #[error("{context}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Arrow(#[from] arrow::error::ArrowError),

    /// The leading `::` names the external crate rather than the module this
    /// file lives in, which is also called `parquet`. Without it the reader has
    /// to work out which one is meant, and in some positions so does the
    /// compiler.
    #[error(transparent)]
    Parquet(#[from] ::parquet::errors::ParquetError),
}

impl CurateError {
    /// Attach a human description to an IO failure.
    ///
    /// `std::io::Error` says "No such file or directory" without saying which
    /// file, which is useless in a log a week later.
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        CurateError::Io {
            context: context.into(),
            source,
        }
    }
}
