//! What the writer refuses, and what the reader refuses to believe.
//!
//! These are the tests that would still pass if the codec were replaced by one
//! that silently dropped rows, so each one asserts on the refusal rather than
//! only on the absence of a success.

use std::path::PathBuf;
use std::sync::Arc;

use arrow::array::{ArrayRef, Date32Array, Decimal128Array, StringArray};
use arrow::record_batch::RecordBatch;

use super::super::*;
use super::{bar, day, dec, scratch, sharadar};

// --- T4: empty and duplicate inputs -----------------------------------------

/// An empty batch is refused. Writing a zero-row file over a good one is data
/// loss that reports success.
#[test]
fn t4_an_empty_batch_is_refused_rather_than_written() {
    let directory = scratch("t4-empty");
    let path = directory.join("prices.parquet");

    let error = write_prices(Vec::new(), &path).expect_err("an empty batch must refuse");
    assert!(
        matches!(error, CurateError::EmptyDataset { .. }),
        "got {error:?}"
    );
    assert!(!path.exists(), "nothing may be written for an empty batch");

    let error = write_delistings(Vec::new(), &path).expect_err("an empty batch must refuse");
    assert!(matches!(error, CurateError::EmptyDataset { .. }));
}

/// An existing good file survives a refused write.
///
/// The refusal is only worth anything if it happens before the destination is
/// touched. A writer that truncated first and validated second would pass the
/// test above and still lose the data.
#[test]
fn t4_a_refused_write_leaves_the_previous_file_intact() {
    let path = scratch("t4-intact").join("prices.parquet");
    let good = bar(sharadar("KEEP", 9), day(2020, 1, 2), dec("42.00"));
    write_prices(vec![good.clone()], &path).expect("the first write succeeds");

    write_prices(Vec::new(), &path).expect_err("an empty batch must refuse");

    let read = read_prices(&path).expect("the previous file is still readable");
    assert_eq!(read, vec![good], "a refused write destroyed the good file");
}

/// Two rows for the same security on the same date are an error naming the key,
/// never a silent deduplication.
///
/// The pair here shares a permanent id under two different tickers, which is
/// the same security by `AssetKey`'s own equality. A duplicate check keyed on
/// the ticker string would miss it, and that is exactly the case worth
/// catching, because it is what a ticker rename looks like in a batch.
#[test]
fn t4_a_duplicate_asset_and_date_is_refused_and_named() {
    let directory = scratch("t4-dup");
    let path = directory.join("prices.parquet");

    let before = bar(sharadar("FB", 199_059), day(2020, 1, 2), dec("10.00"));
    let after = bar(sharadar("META", 199_059), day(2020, 1, 2), dec("10.00"));

    let error =
        write_prices(vec![before, after], &path).expect_err("a duplicate key must be refused");
    let message = error.to_string();

    assert!(
        matches!(error, CurateError::DuplicateRow { .. }),
        "got {error:?}"
    );
    assert!(
        message.contains("199059"),
        "the error must name the key: {message}"
    );
    assert!(
        message.contains("2020-01-02"),
        "the error must name the date: {message}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

// --- T5: reader strictness --------------------------------------------------

/// Write a file straight through arrow, bypassing this module's writer.
///
/// The writer builds the identity and terminal columns from an enum, so an
/// invalid combination is unrepresentable on the write side. The only way to
/// test that the reader enforces the invariant is to produce a file the writer
/// would never produce.
fn write_raw(path: &PathBuf, schema: Arc<arrow::datatypes::Schema>, columns: Vec<ArrayRef>) {
    use ::parquet::arrow::ArrowWriter;

    std::fs::create_dir_all(path.parent().expect("a parent directory")).expect("mkdir");
    let batch = RecordBatch::try_new(schema.clone(), columns).expect("a well-formed batch");
    let file = std::fs::File::create(path).expect("create");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("arrow writer");
    writer.write(&batch).expect("write");
    writer.close().expect("close");
}

fn utf8(values: Vec<Option<&str>>) -> ArrayRef {
    Arc::new(StringArray::from(values))
}

fn decimal_column(values: Vec<Option<i128>>) -> ArrayRef {
    Arc::new(
        Decimal128Array::from(values)
            .with_precision_and_scale(38, 9)
            .expect("precision and scale accepted"),
    )
}

/// A terminal state of `observed` with no value is refused rather than read as
/// an unknown or a zero.
#[test]
fn t5_observed_with_a_null_value_is_refused() {
    let path = scratch("t5-observed").join("delistings.parquet");

    write_raw(
        &path,
        super::delistings::schema(),
        vec![
            utf8(vec![Some("GONE")]),
            utf8(vec![None]),
            utf8(vec![None]),
            Arc::new(Date32Array::from(vec![18_000])),
            utf8(vec![Some("bankruptcy")]),
            utf8(vec![Some("nasdaq")]),
            utf8(vec![Some("observed")]),
            decimal_column(vec![None]),
            utf8(vec![None]),
            utf8(vec![Some("synthetic")]),
        ],
    );

    let error = read_delistings(&path).expect_err("observed with no value must be refused");
    assert!(
        matches!(error, CurateError::TerminalInvariant { .. }),
        "got {error:?}"
    );
}

/// An `observed` state carrying an imputation convention is refused. The
/// convention belongs only to an imputed value, and a stray one means the two
/// states have been mixed.
#[test]
fn t5_observed_with_a_convention_is_refused() {
    let path = scratch("t5-observed-conv").join("delistings.parquet");

    write_raw(
        &path,
        super::delistings::schema(),
        vec![
            utf8(vec![Some("GONE")]),
            utf8(vec![None]),
            utf8(vec![None]),
            Arc::new(Date32Array::from(vec![18_000])),
            utf8(vec![Some("bankruptcy")]),
            utf8(vec![Some("nasdaq")]),
            utf8(vec![Some("observed")]),
            decimal_column(vec![Some(-550_000_000)]),
            utf8(vec![Some("shumway_warther_1999_nasdaq")]),
            utf8(vec![Some("synthetic")]),
        ],
    );

    let error = read_delistings(&path).expect_err("observed with a convention must be refused");
    assert!(
        matches!(error, CurateError::TerminalInvariant { .. }),
        "got {error:?}"
    );
}

/// An `imputed` state with no convention is refused, since the convention is
/// the only thing that says what the number assumed.
#[test]
fn t5_imputed_without_a_convention_is_refused() {
    let path = scratch("t5-imputed").join("delistings.parquet");

    write_raw(
        &path,
        super::delistings::schema(),
        vec![
            utf8(vec![Some("GONE")]),
            utf8(vec![None]),
            utf8(vec![None]),
            Arc::new(Date32Array::from(vec![18_000])),
            utf8(vec![Some("bankruptcy")]),
            utf8(vec![Some("nasdaq")]),
            utf8(vec![Some("imputed")]),
            decimal_column(vec![Some(-550_000_000)]),
            utf8(vec![None]),
            utf8(vec![Some("synthetic")]),
        ],
    );

    let error = read_delistings(&path).expect_err("imputed with no convention must be refused");
    assert!(
        matches!(error, CurateError::TerminalInvariant { .. }),
        "got {error:?}"
    );
}

/// Half an identity is refused rather than degraded to a ticker-only key.
///
/// Degrading would produce a key that compares unequal to the identified one it
/// came from, which is a silent split of one company's history.
#[test]
fn t5_a_half_null_identity_is_refused() {
    let path = scratch("t5-half").join("delistings.parquet");

    write_raw(
        &path,
        super::delistings::schema(),
        vec![
            utf8(vec![Some("GONE")]),
            utf8(vec![Some("sharadar")]),
            utf8(vec![None]),
            Arc::new(Date32Array::from(vec![18_000])),
            utf8(vec![Some("bankruptcy")]),
            utf8(vec![Some("nasdaq")]),
            utf8(vec![Some("unknown")]),
            decimal_column(vec![None]),
            utf8(vec![None]),
            utf8(vec![Some("synthetic")]),
        ],
    );

    let error = read_delistings(&path).expect_err("a half-null identity must be refused");
    assert!(
        matches!(error, CurateError::HalfNullIdentity { .. }),
        "got {error:?}"
    );
}

/// The other half of the same invariant: an identifier with no kind is equally
/// corrupt, because nothing says which namespace the number belongs to.
#[test]
fn t5_an_identifier_with_no_kind_is_refused() {
    let path = scratch("t5-nokind").join("prices.parquet");

    write_raw(
        &path,
        super::prices::schema(),
        vec![
            utf8(vec![Some("GONE")]),
            utf8(vec![None]),
            utf8(vec![Some("199059")]),
            Arc::new(Date32Array::from(vec![18_000])),
            decimal_column(vec![Some(10_000_000_000)]),
            decimal_column(vec![Some(10_000_000_000)]),
            decimal_column(vec![Some(10_000_000_000)]),
            decimal_column(vec![Some(10_000_000_000)]),
            decimal_column(vec![Some(0)]),
            utf8(vec![Some("regular_hours")]),
            utf8(vec![Some("closing_auction")]),
        ],
    );

    let error = read_prices(&path).expect_err("an identifier with no kind must be refused");
    assert!(
        matches!(error, CurateError::HalfNullIdentity { .. }),
        "got {error:?}"
    );
}

/// An enum label outside the closed set is refused rather than coerced.
#[test]
fn t5_an_unknown_enum_label_is_refused() {
    let path = scratch("t5-enum").join("prices.parquet");

    write_raw(
        &path,
        super::prices::schema(),
        vec![
            utf8(vec![Some("GONE")]),
            utf8(vec![None]),
            utf8(vec![None]),
            Arc::new(Date32Array::from(vec![18_000])),
            decimal_column(vec![Some(10_000_000_000)]),
            decimal_column(vec![Some(10_000_000_000)]),
            decimal_column(vec![Some(10_000_000_000)]),
            decimal_column(vec![Some(10_000_000_000)]),
            decimal_column(vec![Some(0)]),
            utf8(vec![Some("overnight")]),
            utf8(vec![Some("closing_auction")]),
        ],
    );

    let error = read_prices(&path).expect_err("an unknown session label must be refused");
    assert!(
        matches!(error, CurateError::UnknownEnum { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains("overnight"),
        "the error must name the value it refused: {error}"
    );
}

// --- paths ------------------------------------------------------------------

/// The data root falls back in the documented order. The flag is checked here
/// because it is the only one a test can set without touching the process
/// environment, which the other tests share.
#[test]
fn the_data_root_flag_wins() {
    let root = data_root(Some("/somewhere/else"));
    assert_eq!(
        prices_path(&root),
        PathBuf::from("/somewhere/else/curated/prices/prices.parquet")
    );
    assert_eq!(
        delistings_path(&root),
        PathBuf::from("/somewhere/else/curated/delistings/delistings.parquet")
    );
}

/// A `permanent_id` that is not decimal digits is a typed error naming the
/// value, never a panic.
///
/// The realistic bug here is `.unwrap()` on the parse. That compiles, reads
/// fine, and turns a corrupt file into a crash with no indication of which
/// value caused it.
#[test]
fn t5_a_non_numeric_permanent_id_is_refused_by_name() {
    let path = scratch("t5-nan-id").join("prices.parquet");
    write_raw(
        &path,
        super::super::prices::schema(),
        one_price_row("sharadar", "not-a-number"),
    );

    let error = read_prices(&path).expect_err("a non-numeric permanent id must be refused");
    assert!(
        matches!(error, CurateError::MalformedPermanentId { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains("not-a-number"),
        "the error must name the offending value: {error}"
    );
}

/// A `permanent_id` larger than `u64::MAX` is the same class of failure and
/// must also be a typed error rather than an overflow panic.
#[test]
fn t5_a_permanent_id_beyond_u64_max_is_refused_by_name() {
    let path = scratch("t5-huge-id").join("prices.parquet");
    // u64::MAX is 18446744073709551615, so this is one digit wider.
    let huge = "184467440737095516150";
    write_raw(
        &path,
        super::super::prices::schema(),
        one_price_row("sharadar", huge),
    );

    let error = read_prices(&path).expect_err("an over-u64 permanent id must be refused");
    assert!(
        matches!(error, CurateError::MalformedPermanentId { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains(huge),
        "the error must name the offending value: {error}"
    );
}

/// One well-formed price row with the identity columns supplied verbatim, so a
/// test can put something in them the writer would never produce.
fn one_price_row(kind: &str, id: &str) -> Vec<ArrayRef> {
    vec![
        utf8(vec![Some("GONE")]),
        utf8(vec![Some(kind)]),
        utf8(vec![Some(id)]),
        Arc::new(Date32Array::from(vec![18_000])),
        decimal_column(vec![Some(10_000_000_000)]),
        decimal_column(vec![Some(10_000_000_000)]),
        decimal_column(vec![Some(10_000_000_000)]),
        decimal_column(vec![Some(10_000_000_000)]),
        decimal_column(vec![Some(0)]),
        utf8(vec![Some("regular_hours")]),
        utf8(vec![Some("closing_auction")]),
    ]
}
