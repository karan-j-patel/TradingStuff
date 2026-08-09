//! T10 and T11. What the actions writer refuses, and the order it writes in.

use rust_decimal::Decimal;

use super::super::super::*;
use super::super::{day, dec, scratch, sharadar};
use super::{action, dividend};
use crate::actions::{CorporateAction, DividendKind};

// --- T10: writer refusals ---------------------------------------------------

/// A split ratio of zero is refused by the writer. Zero describes nothing, and
/// `price_factor` already declines to produce a factor for it.
#[test]
fn t10_a_zero_split_ratio_is_refused() {
    let directory = scratch("t10-zero");
    let path = directory.join("actions.parquet");

    let broken = action(
        sharadar("ZERO", 1),
        day(2021, 5, 3),
        CorporateAction::Split {
            ratio: Decimal::ZERO,
        },
    );

    let error = write_actions(vec![broken], &path).expect_err("a zero ratio must be refused");
    assert!(
        matches!(error, CurateError::InvalidRecords { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains("ratio"),
        "the error must name the field: {error}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// A negative dividend amount is refused. A negative distribution is not a
/// dividend, it is a sign error somewhere upstream.
#[test]
fn t10_a_negative_dividend_amount_is_refused() {
    let directory = scratch("t10-negative");
    let path = directory.join("actions.parquet");

    let broken = action(
        sharadar("NEG", 2),
        day(2021, 5, 3),
        dividend("-0.47", DividendKind::Cash),
    );

    let error = write_actions(vec![broken], &path).expect_err("a negative amount is refused");
    assert!(
        matches!(error, CurateError::InvalidRecords { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains("amount"),
        "the error must name the field: {error}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// An action with no provenance is refused.
#[test]
fn t10_an_action_with_no_source_is_refused() {
    let directory = scratch("t10-source");
    let path = directory.join("actions.parquet");

    let mut broken = action(
        sharadar("NOSRC", 3),
        day(2021, 5, 3),
        CorporateAction::SpinOff {
            factor: dec("1.032"),
        },
    );
    broken.source = String::new();

    let error = write_actions(vec![broken], &path).expect_err("a sourceless record is refused");
    assert!(
        matches!(error, CurateError::InvalidRecords { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains("source is empty"),
        "the error must carry the reason: {error}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// A refused write leaves an existing good file byte for byte as it was.
#[test]
fn t10_a_refused_write_leaves_the_previous_file_byte_identical() {
    let path = scratch("t10-intact").join("actions.parquet");

    let good = action(
        sharadar("KEEP", 4),
        day(2021, 5, 3),
        CorporateAction::Split { ratio: dec("2") },
    );
    write_actions(vec![good.clone()], &path).expect("the first write succeeds");
    let before = std::fs::read(&path).expect("reading the good file");

    let broken = action(
        sharadar("KEEP", 4),
        day(2021, 6, 1),
        CorporateAction::Split {
            ratio: Decimal::ZERO,
        },
    );
    write_actions(vec![good, broken], &path).expect_err("a zero ratio must be refused");

    let after = std::fs::read(&path).expect("reading the file again");
    assert_eq!(
        before, after,
        "a refused write changed the bytes of the previous file"
    );
}

// --- T11: duplicates --------------------------------------------------------

/// An exact duplicate, identical down to the dividend kind, is refused and the
/// error names the key including the part that made it a duplicate.
#[test]
fn t11_an_exact_duplicate_is_refused_and_named() {
    let directory = scratch("t11-dup");
    let path = directory.join("actions.parquet");

    let once = action(
        sharadar("DIVD", 200),
        day(2021, 5, 3),
        dividend("0.47", DividendKind::Cash),
    );
    let twice = once.clone();

    let error =
        write_actions(vec![once, twice], &path).expect_err("an exact duplicate must be refused");
    let message = error.to_string();

    assert!(
        matches!(error, CurateError::DuplicateRow { .. }),
        "got {error:?}"
    );
    assert!(message.contains("200"), "must name the key: {message}");
    assert!(
        message.contains("2021-05-03"),
        "must name the date: {message}"
    );
    assert!(
        message.contains("dividend") && message.contains("cash"),
        "must name the rest of the key: {message}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// The same asset and date under two different tickers is still one asset, so
/// two identical actions across a rename are a duplicate.
#[test]
fn t11_a_duplicate_across_a_ticker_rename_is_still_a_duplicate() {
    let directory = scratch("t11-rename");
    let path = directory.join("actions.parquet");

    let before = action(
        sharadar("FB", 199_059),
        day(2021, 5, 3),
        CorporateAction::Split { ratio: dec("2") },
    );
    let after = action(
        sharadar("META", 199_059),
        day(2021, 5, 3),
        CorporateAction::Split { ratio: dec("2") },
    );

    let error = write_actions(vec![before, after], &path)
        .expect_err("the same permaticker is the same asset");
    assert!(
        matches!(error, CurateError::DuplicateRow { .. }),
        "got {error:?}"
    );
}

/// Shuffled same-day multi-action rows write in one known order.
///
/// Every row here shares an asset and a date, so the identity and date terms
/// of the sort key are constant and only `action_kind` then `dividend_kind`
/// decide. That is exactly the part of the order the other datasets never
/// exercise.
#[test]
fn t11_same_day_actions_write_in_one_known_order() {
    let path = scratch("t11-order").join("actions.parquet");
    let asset = sharadar("BUSY", 700);
    let date = day(2021, 5, 3);

    let split = action(
        asset.clone(),
        date,
        CorporateAction::Split { ratio: dec("2") },
    );
    let cash = action(asset.clone(), date, dividend("0.47", DividendKind::Cash));
    let special = action(
        asset.clone(),
        date,
        dividend("12.50", DividendKind::Special),
    );
    let stock = action(asset.clone(), date, dividend("0.02", DividendKind::Stock));
    let spin = action(
        asset,
        date,
        CorporateAction::SpinOff {
            factor: dec("1.032"),
        },
    );

    let shuffled = vec![
        spin.clone(),
        stock.clone(),
        split.clone(),
        special.clone(),
        cash.clone(),
    ];
    assert_eq!(write_actions(shuffled, &path).expect("write"), 5);

    // Written out rather than computed, so a change to the order has to be
    // acknowledged here rather than silently agreeing with itself.
    // "dividend" < "spin_off" < "split", then cash < special < stock.
    let expected = vec![cash, special, stock, spin, split];
    assert_eq!(
        read_actions(&path).expect("read"),
        expected,
        "same-day actions are out of order"
    );
}
