//! T8 to T11. The corporate actions dataset.
//!
//! All data synthetic. Vendor licences forbid redistributing rows, and that
//! applies to a fixture as much as to a data directory.

use std::path::PathBuf;
use std::sync::Arc;

use arrow::array::{ArrayRef, Date32Array, Decimal128Array, StringArray};
use arrow::record_batch::RecordBatch;
use jiff::civil::Date;

use super::super::*;
use super::{day, dec, scratch, sharadar};
use crate::actions::{ActionRecord, CorporateAction, DividendKind};
use crate::schema::AssetKey;

/// Writer refusals and duplicate handling. Split out for size only; these
/// share the fixtures below.
mod refusals;

pub(super) fn action(asset: AssetKey, effective: Date, action: CorporateAction) -> ActionRecord {
    ActionRecord {
        asset,
        effective,
        action,
        source: "synthetic".into(),
    }
}

pub(super) fn dividend(amount: &str, kind: DividendKind) -> CorporateAction {
    CorporateAction::Dividend {
        amount: dec(amount),
        kind,
    }
}

// --- T8: round trip ---------------------------------------------------------

/// All three kinds survive, including two dividends on one ex-date that differ
/// only by kind.
///
/// The cash and special pair is the case a naive duplicate key destroys. Both
/// are real records, a special dividend is often large enough to dominate a
/// day's return, and silently dropping one would understate that day forever.
#[test]
fn t8_actions_round_trip_across_every_kind() {
    let path = scratch("t8").join("actions.parquet");

    let split = action(
        sharadar("SPLT", 100),
        day(2021, 5, 3),
        CorporateAction::Split { ratio: dec("2") },
    );
    let reverse = action(
        sharadar("SPLT", 100),
        day(2021, 6, 1),
        CorporateAction::Split { ratio: dec("0.1") },
    );
    let cash = action(
        sharadar("DIVD", 200),
        day(2021, 5, 3),
        dividend("0.47", DividendKind::Cash),
    );
    // Same asset, same ex-date, different kind. Not a duplicate.
    let special = action(
        sharadar("DIVD", 200),
        day(2021, 5, 3),
        dividend("12.50", DividendKind::Special),
    );
    let stock = action(
        sharadar("DIVD", 200),
        day(2021, 7, 9),
        dividend("0.02", DividendKind::Stock),
    );
    let spin = action(
        sharadar("SPIN", 300),
        day(2021, 8, 2),
        CorporateAction::SpinOff {
            factor: dec("1.032"),
        },
    );

    let shuffled = vec![
        spin.clone(),
        special.clone(),
        reverse.clone(),
        cash.clone(),
        stock.clone(),
        split.clone(),
    ];
    assert_eq!(write_actions(shuffled, &path).expect("write"), 6);

    let read = read_actions(&path).expect("read");
    assert_eq!(read.len(), 6, "a same-day dividend pair was dropped");

    // The dividend kind is what tells a cash distribution from a special one,
    // and nothing else in the row does.
    let kinds: Vec<DividendKind> = read
        .iter()
        .filter_map(|record| match record.action {
            CorporateAction::Dividend { kind, .. } => Some(kind),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            DividendKind::Cash,
            DividendKind::Special,
            DividendKind::Stock
        ],
        "the dividend kind did not survive the round trip"
    );

    // Sorted by identity id as a string, then effective, then action_kind,
    // then dividend_kind with nulls last.
    let expected = vec![split, reverse, cash, special, stock, spin];
    assert_eq!(read, expected, "rows must round trip exactly and in order");
}

// --- T9: reader strictness --------------------------------------------------

fn write_raw(path: &PathBuf, columns: Vec<ArrayRef>) {
    use ::parquet::arrow::ArrowWriter;

    std::fs::create_dir_all(path.parent().expect("a parent directory")).expect("mkdir");
    let batch = RecordBatch::try_new(super::super::actions::schema(), columns)
        .expect("a well-formed batch");
    let file = std::fs::File::create(path).expect("create");
    let mut writer =
        ArrowWriter::try_new(file, super::super::actions::schema(), None).expect("arrow writer");
    writer.write(&batch).expect("write");
    writer.close().expect("close");
}

fn utf8(values: Vec<Option<&str>>) -> ArrayRef {
    Arc::new(StringArray::from(values))
}

fn decimals(values: Vec<Option<i128>>) -> ArrayRef {
    Arc::new(
        Decimal128Array::from(values)
            .with_precision_and_scale(38, 9)
            .expect("precision and scale accepted"),
    )
}

/// One raw row with every payload column supplied verbatim, so a test can put
/// a combination in them that the writer would never produce.
fn raw_row(
    action_kind: &str,
    ratio: Option<i128>,
    amount: Option<i128>,
    dividend_kind: Option<&str>,
    factor: Option<i128>,
) -> Vec<ArrayRef> {
    vec![
        utf8(vec![Some("GONE")]),
        utf8(vec![None]),
        utf8(vec![None]),
        Arc::new(Date32Array::from(vec![18_000])),
        utf8(vec![Some(action_kind)]),
        decimals(vec![ratio]),
        decimals(vec![amount]),
        utf8(vec![dividend_kind]),
        decimals(vec![factor]),
        utf8(vec![Some("synthetic")]),
    ]
}

/// A split carrying a dividend kind is refused. The two describe different
/// events and a row claiming both has been assembled wrongly.
#[test]
fn t9_a_split_carrying_a_dividend_kind_is_refused() {
    let path = scratch("t9-split-divkind").join("actions.parquet");
    write_raw(
        &path,
        raw_row("split", Some(2_000_000_000), None, Some("cash"), None),
    );

    let error = read_actions(&path).expect_err("a split with a dividend kind must be refused");
    assert!(
        matches!(error, CurateError::ActionInvariant { .. }),
        "got {error:?}"
    );
}

/// A dividend with no kind is refused. Nothing else in the row says whether
/// the distribution was regular, special, or paid in stock.
#[test]
fn t9_a_dividend_without_a_kind_is_refused() {
    let path = scratch("t9-div-nokind").join("actions.parquet");
    write_raw(
        &path,
        raw_row("dividend", None, Some(470_000_000), None, None),
    );

    let error = read_actions(&path).expect_err("a dividend with no kind must be refused");
    assert!(
        matches!(error, CurateError::ActionInvariant { .. }),
        "got {error:?}"
    );
}

/// A split carrying an amount instead of a ratio is refused rather than read
/// as a split with no ratio.
#[test]
fn t9_a_split_carrying_the_wrong_payload_is_refused() {
    let path = scratch("t9-split-amount").join("actions.parquet");
    write_raw(&path, raw_row("split", None, Some(470_000_000), None, None));

    let error = read_actions(&path).expect_err("a split with no ratio must be refused");
    assert!(
        matches!(error, CurateError::ActionInvariant { .. }),
        "got {error:?}"
    );
}

/// An `action_kind` outside the closed set is refused rather than coerced.
#[test]
fn t9_an_unknown_action_kind_is_refused() {
    let path = scratch("t9-unknown").join("actions.parquet");
    write_raw(&path, raw_row("rights_issue", None, None, None, None));

    let error = read_actions(&path).expect_err("an unknown action kind must be refused");
    assert!(
        matches!(error, CurateError::UnknownEnum { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains("rights_issue"),
        "the error must name the value it refused: {error}"
    );
}
