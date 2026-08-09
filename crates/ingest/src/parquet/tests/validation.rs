//! The writers are the enforcement boundary, not the CLI.
//!
//! Both writers are publicly exported from this crate. The provider fetch
//! pipeline will call them in process without going through `cli curate`, so a
//! guard that lives only in the CLI protects one door of two. These tests call
//! the writers directly, which is exactly what that second caller does.

use super::super::*;
use super::{bar, day, dec, delisting, scratch, sharadar};
use crate::actions::TerminalValue;

/// A bar whose high is below its low is refused by the writer itself.
#[test]
fn m4_write_prices_refuses_an_invalid_bar() {
    let directory = scratch("v-prices");
    let path = directory.join("prices.parquet");

    let mut broken = bar(sharadar("BAD", 1), day(2020, 1, 2), dec("10.00"));
    broken.high = dec("9.00");
    broken.low = dec("11.00");

    let error = write_prices(vec![broken], &path).expect_err("an invalid bar must be refused");
    assert!(
        matches!(error, CurateError::InvalidRecords { .. }),
        "got {error:?}"
    );

    let message = error.to_string();
    assert!(
        message.contains("prices"),
        "must name the dataset: {message}"
    );
    assert!(
        message.contains("BAD"),
        "must name the offending record: {message}"
    );
    assert!(
        message.contains("high 9.00 is below low 11.00"),
        "must carry the reject reason: {message}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// A delisting with no provenance is refused by the writer itself.
#[test]
fn m4_write_delistings_refuses_a_record_with_no_source() {
    let directory = scratch("v-source");
    let path = directory.join("delistings.parquet");

    let mut anonymous = delisting(
        sharadar("GONE", 2),
        day(2021, 3, 1),
        TerminalValue::Observed(dec("-0.5")),
    );
    anonymous.source = String::new();

    let error =
        write_delistings(vec![anonymous], &path).expect_err("a sourceless record must be refused");
    assert!(
        matches!(error, CurateError::InvalidRecords { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains("source is empty"),
        "must carry the reject reason: {error}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// A terminal return below total loss is refused by the writer itself.
#[test]
fn m4_write_delistings_refuses_a_return_below_total_loss() {
    let directory = scratch("v-return");
    let path = directory.join("delistings.parquet");

    let impossible = delisting(
        sharadar("DEEP", 3),
        day(2021, 3, 1),
        TerminalValue::Observed(dec("-2")),
    );

    let error =
        write_delistings(vec![impossible], &path).expect_err("an impossible return is refused");
    assert!(
        matches!(error, CurateError::InvalidRecords { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().contains("DEEP"),
        "must name the offending record: {error}"
    );
    assert!(!path.exists(), "a refused write must leave no file behind");
}

/// A refused write leaves an existing good file byte for byte as it was.
///
/// Checking only that the write returned an error would pass against a writer
/// that truncated the destination and then noticed the problem. Comparing the
/// bytes is what makes the refusal worth having.
#[test]
fn m4_a_refused_write_leaves_the_previous_file_byte_identical() {
    let path = scratch("v-intact").join("prices.parquet");

    let good = bar(sharadar("KEEP", 4), day(2020, 1, 2), dec("42.00"));
    write_prices(vec![good.clone()], &path).expect("the first write succeeds");
    let before = std::fs::read(&path).expect("reading the good file");

    let mut broken = bar(sharadar("KEEP", 4), day(2020, 1, 3), dec("10.00"));
    broken.high = dec("9.00");
    broken.low = dec("11.00");
    write_prices(vec![good, broken], &path).expect_err("an invalid bar must be refused");

    let after = std::fs::read(&path).expect("reading the file again");
    assert_eq!(
        before, after,
        "a refused write changed the bytes of the previous file"
    );
}
