//! End to end tests for the three `curate` arms.

use super::*;
use ingest::parquet::read_prices;
use std::path::PathBuf;

/// All fixture data is synthetic. Vendor licences forbid redistributing
/// rows, and that applies to a test file as much as to a data directory.
const ONE_GOOD_BAR: &str = r#"{"asset":{"ticker":"AAPL","permanent":{"Sharadar":199059}},"date":"2020-01-02","open":"10.25","high":"10.90","low":"10.10","close":"10.75","volume":"1000","close_unadjusted":"41.00","session":"RegularHours","close_kind":"ClosingAuction"}"#;

/// Same shape, no permanent identifier, and a different security.
const ONE_ANONYMOUS_BAR: &str = r#"{"asset":{"ticker":"ZZZZ","permanent":null},"date":"2020-01-02","open":"3.00","high":"3.50","low":"2.90","close":"3.10","volume":"25","close_unadjusted":"12.40","session":"IncludingExtended","close_kind":"LastTrade"}"#;

/// `high` below `low`, which `PriceBar::validate` rejects.
const ONE_INVALID_BAR: &str = r#"{"asset":{"ticker":"BAD","permanent":null},"date":"2020-01-03","open":"10.00","high":"9.00","low":"11.00","close":"10.50","volume":"5","close_unadjusted":"42.00","session":"RegularHours","close_kind":"ClosingAuction"}"#;

/// The same bar twice: numeric fields as bare JSON numbers, then as quoted
/// strings. Every other fixture here quotes its decimals, which left the bare
/// form untested even though it is the form an operator is most likely to
/// write and the one `serde_json`'s `arbitrary_precision` breaks without
/// rust_decimal's `serde-with-arbitrary-precision` to match it.
///
/// `high` carries 18 significant digits, which is past what f64 holds, so this
/// also pins that the value arrives exactly rather than nearly.
const ONE_BARE_NUMBER_BAR: &str = r#"{"asset":{"ticker":"AAPL","permanent":{"Sharadar":199059}},"date":"2020-01-02","open":10.25,"high":123456789.012345678,"low":10.10,"close":10.75,"volume":1000,"close_unadjusted":41.00,"session":"RegularHours","close_kind":"ClosingAuction"}"#;

/// Same values, quoted. A different date only because the writer refuses two
/// rows for one security on one day, which is a guarantee worth not weakening
/// for a test's convenience.
const ONE_QUOTED_NUMBER_BAR: &str = r#"{"asset":{"ticker":"AAPL","permanent":{"Sharadar":199059}},"date":"2020-01-03","open":"10.25","high":"123456789.012345678","low":"10.10","close":"10.75","volume":"1000","close_unadjusted":"41.00","session":"RegularHours","close_kind":"ClosingAuction"}"#;

/// Bare JSON numbers reach `PriceBar` intact, and agree with the quoted form.
///
/// This is the path the connector work found broken: serde_json hands a bare
/// *float* to a deserializer as a magic map rather than as a number, so
/// `Decimal` refuses it unless rust_decimal carries the matching feature. Bare
/// integers were unaffected, which is why `permanent` kept working and nothing
/// in this file noticed.
#[test]
fn a_bare_json_number_curates_identically_to_its_quoted_twin() {
    let dir = scratch("bare-numbers");
    let input = jsonl(&dir, &[ONE_BARE_NUMBER_BAR, ONE_QUOTED_NUMBER_BAR]);

    let dataset = prices_from_file(input, &dir);
    let code = run(&dataset).expect("bare JSON numbers are a valid way to write a bar");
    assert_eq!(code, ExitCode::SUCCESS);

    let bars = read_prices(&prices_path(&dir)).expect("the curated file reads back");
    assert_eq!(bars.len(), 2);
    let prices = |bar: &ingest::AdjustedBar| {
        (
            bar.open,
            bar.high,
            bar.low,
            bar.close,
            bar.volume,
            bar.close_unadjusted,
        )
    };
    assert_eq!(
        prices(&bars[0]),
        prices(&bars[1]),
        "the bare and quoted spellings of one bar produced different numbers"
    );
    assert_eq!(
        bars[0].high,
        rust_decimal::Decimal::from_str_exact("123456789.012345678").unwrap(),
        "18 significant digits did not survive the JSONL path"
    );
}

/// The JSONL arm of `curate prices`, which is all these tests exercise. The
/// fetch arm has its own test against a fixture transport.
fn prices_from_file(input: String, dir: &Path) -> Dataset {
    Dataset::Prices {
        input: Some(input),
        fetch: false,
        tickers: Vec::new(),
        universe: None,
        from: None,
        to: None,
        data_root: Some(dir.to_string_lossy().into_owned()),
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("curate-cli-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

fn jsonl(dir: &Path, lines: &[&str]) -> String {
    let path = dir.join("input.jsonl");
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("writing the fixture");
    path.to_string_lossy().into_owned()
}

/// T6, the working half. The command produces a file the reader round
/// trips, at the path the data root implies.
#[test]
fn t6_curate_prices_writes_a_file_the_reader_round_trips() {
    let dir = scratch("ok");
    let input = jsonl(&dir, &[ONE_GOOD_BAR, ONE_ANONYMOUS_BAR]);

    let dataset = prices_from_file(input, &dir);
    let code = run(&dataset).expect("curating two valid bars succeeds");
    assert_eq!(code, ExitCode::SUCCESS);

    let path = prices_path(&dir);
    assert!(
        path.exists(),
        "the curated file was not written to {path:?}"
    );

    let bars = read_prices(&path).expect("the curated file reads back");
    assert_eq!(bars.len(), 2);

    // The identified row sorts first, and its identity survived the trip
    // through JSON, validation, Parquet, and back.
    assert_eq!(bars[0].asset.ticker, "AAPL");
    assert!(
        bars[0].asset.is_stable(),
        "the permanent id did not survive the CLI path"
    );
    assert_eq!(
        bars[0].close,
        rust_decimal::Decimal::from_str_exact("10.75").unwrap()
    );
    assert_eq!(bars[1].asset.ticker, "ZZZZ");
    assert!(!bars[1].asset.is_stable());
}

/// T6, the refusing half. One invalid bar fails the command, writes
/// nothing, and names the reject.
///
/// The "writes nothing" half is the part worth testing. A command that
/// wrote the good rows and reported the bad ones would pass a test that
/// only checked the exit code, and would have silently dropped data.
#[test]
fn t6_one_invalid_bar_fails_the_command_and_writes_nothing() {
    let dir = scratch("invalid");
    let input = jsonl(&dir, &[ONE_GOOD_BAR, ONE_INVALID_BAR]);

    let dataset = prices_from_file(input, &dir);
    let error = run(&dataset).expect_err("an invalid bar must fail the command");

    // `{:#}` prints the whole chain of causes, which is what the binary
    // prints, so this asserts on what a person would actually see.
    let message = format!("{error:#}");
    assert!(
        message.contains("BAD"),
        "the error must name the rejected record: {message}"
    );
    assert!(
        message.contains("high 9.00 is below low 11.00"),
        "the error must give the reason: {message}"
    );
    assert!(
        message.contains("1 of 2"),
        "the error must report the counts: {message}"
    );

    assert!(
        !prices_path(&dir).exists(),
        "a refused command wrote a file anyway"
    );
}

/// A malformed line is an error rather than a skipped record.
#[test]
fn t6_a_malformed_line_fails_the_command_by_line_number() {
    let dir = scratch("malformed");
    let input = jsonl(&dir, &[ONE_GOOD_BAR, "{not json"]);

    let dataset = prices_from_file(input, &dir);
    let error = run(&dataset).expect_err("a malformed line must fail the command");
    let message = format!("{error:#}");
    assert!(
        message.contains("line 2"),
        "the error must name the line: {message}"
    );
    assert!(!prices_path(&dir).exists());
}

/// Synthetic. An imputed value that matches its convention's published
/// figure, which is what `Delisting::imputed()` produces.
const ONE_GOOD_DELISTING: &str = r#"{"asset":{"ticker":"GONE","permanent":{"Sharadar":4242}},"date":"2021-03-01","reason":"Bankruptcy","listing":"Nasdaq","terminal":{"Imputed":{"value":"-0.55","convention":"ShumwayWarther1999Nasdaq"}},"source":"synthetic"}"#;

/// Claims the Nasdaq convention while carrying the NYSE/AMEX figure, so
/// the record is incoherent rather than merely unusual.
const ONE_INCOHERENT_DELISTING: &str = r#"{"asset":{"ticker":"WRONG","permanent":null},"date":"2021-03-02","reason":"Bankruptcy","listing":"Nasdaq","terminal":{"Imputed":{"value":"-0.30","convention":"ShumwayWarther1999Nasdaq"}},"source":"synthetic"}"#;

/// A holder cannot lose more than everything.
const ONE_IMPOSSIBLE_DELISTING: &str = r#"{"asset":{"ticker":"DEEP","permanent":null},"date":"2021-03-03","reason":"Bankruptcy","listing":"Nasdaq","terminal":{"Observed":"-1.5"},"source":"synthetic"}"#;

#[test]
fn curate_delistings_writes_a_file_the_reader_round_trips() {
    let dir = scratch("del-ok");
    let input = jsonl(&dir, &[ONE_GOOD_DELISTING]);

    let dataset = Dataset::Delistings {
        input: Some(input),
        fetch: false,
        universe: None,
        from: None,
        to: None,
        data_root: Some(dir.to_string_lossy().into_owned()),
    };
    run(&dataset).expect("curating a valid delisting succeeds");

    let rows = ingest::parquet::read_delistings(&delistings_path(&dir))
        .expect("the curated file reads back");
    assert_eq!(rows.len(), 1);
    assert!(
        !rows[0].terminal.is_observed(),
        "an imputed value came back as observed"
    );
}

/// An imputed value disagreeing with its own convention fails the command
/// and writes nothing.
#[test]
fn a_delisting_disagreeing_with_its_convention_fails_the_command() {
    let dir = scratch("del-incoherent");
    let input = jsonl(&dir, &[ONE_GOOD_DELISTING, ONE_INCOHERENT_DELISTING]);

    let dataset = Dataset::Delistings {
        input: Some(input),
        fetch: false,
        universe: None,
        from: None,
        to: None,
        data_root: Some(dir.to_string_lossy().into_owned()),
    };
    let error = run(&dataset).expect_err("an incoherent delisting must fail the command");
    let message = format!("{error:#}");

    assert!(
        message.contains("WRONG"),
        "the error must name the rejected record: {message}"
    );
    assert!(
        message.contains("1 of 2"),
        "the error must report the counts: {message}"
    );
    assert!(
        !delistings_path(&dir).exists(),
        "a refused command wrote a file anyway"
    );
}

/// A return below total loss fails the command too, so the check is not
/// only about conventions.
#[test]
fn a_delisting_losing_more_than_everything_fails_the_command() {
    let dir = scratch("del-impossible");
    let input = jsonl(&dir, &[ONE_IMPOSSIBLE_DELISTING]);

    let dataset = Dataset::Delistings {
        input: Some(input),
        fetch: false,
        universe: None,
        from: None,
        to: None,
        data_root: Some(dir.to_string_lossy().into_owned()),
    };
    let error = run(&dataset).expect_err("an impossible return must fail the command");
    assert!(format!("{error:#}").contains("DEEP"), "got {error:#}");
    assert!(!delistings_path(&dir).exists());
}

/// Synthetic. A 2-for-1 split and a same-day cash plus special dividend
/// pair, which is the case a naive duplicate key would destroy.
const SPLIT_ACTION: &str = r#"{"asset":{"ticker":"SPLT","permanent":{"Sharadar":100}},"effective":"2021-05-03","action":{"Split":{"ratio":"2"}},"source":"synthetic"}"#;
const CASH_DIVIDEND: &str = r#"{"asset":{"ticker":"DIVD","permanent":{"Sharadar":200}},"effective":"2021-05-03","action":{"Dividend":{"amount":"0.47","kind":"Cash"}},"source":"synthetic"}"#;
const SPECIAL_DIVIDEND: &str = r#"{"asset":{"ticker":"DIVD","permanent":{"Sharadar":200}},"effective":"2021-05-03","action":{"Dividend":{"amount":"12.50","kind":"Special"}},"source":"synthetic"}"#;

/// A zero split ratio, which describes nothing.
const ZERO_RATIO_ACTION: &str = r#"{"asset":{"ticker":"ZERO","permanent":null},"effective":"2021-05-03","action":{"Split":{"ratio":"0"}},"source":"synthetic"}"#;

/// T12, the working half.
#[test]
fn t12_curate_actions_writes_a_file_the_reader_round_trips() {
    let dir = scratch("act-ok");
    let input = jsonl(&dir, &[SPLIT_ACTION, CASH_DIVIDEND, SPECIAL_DIVIDEND]);

    let dataset = Dataset::Actions {
        input: Some(input),
        fetch: false,
        universe: None,
        from: None,
        to: None,
        data_root: Some(dir.to_string_lossy().into_owned()),
    };
    run(&dataset).expect("curating three valid actions succeeds");

    let rows =
        ingest::parquet::read_actions(&actions_path(&dir)).expect("the curated file reads back");
    assert_eq!(
        rows.len(),
        3,
        "the same-day cash and special pair must both survive"
    );

    let kinds: Vec<ingest::DividendKind> = rows
        .iter()
        .filter_map(|r| match r.action {
            ingest::CorporateAction::Dividend { kind, .. } => Some(kind),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![ingest::DividendKind::Cash, ingest::DividendKind::Special],
        "the dividend kind did not survive the CLI path"
    );
}

/// T12, the refusing half. One invalid record fails the command, writes
/// nothing, and names the reject.
#[test]
fn t12_one_invalid_action_fails_the_command_and_writes_nothing() {
    let dir = scratch("act-invalid");
    let input = jsonl(&dir, &[SPLIT_ACTION, ZERO_RATIO_ACTION]);

    let dataset = Dataset::Actions {
        input: Some(input),
        fetch: false,
        universe: None,
        from: None,
        to: None,
        data_root: Some(dir.to_string_lossy().into_owned()),
    };
    let error = run(&dataset).expect_err("a zero ratio must fail the command");
    let message = format!("{error:#}");

    assert!(
        message.contains("ZERO"),
        "the error must name the rejected record: {message}"
    );
    assert!(
        message.contains("ratio"),
        "the error must name the field: {message}"
    );
    assert!(
        message.contains("1 of 2"),
        "the error must report the counts: {message}"
    );
    assert!(
        !actions_path(&dir).exists(),
        "a refused command wrote a file anyway"
    );
}

/// An empty input file is refused rather than written as a zero-row file.
#[test]
fn t6_an_empty_input_file_is_refused() {
    let dir = scratch("empty");
    let path = dir.join("input.jsonl");
    std::fs::write(&path, "").expect("writing an empty fixture");

    let dataset = Dataset::Prices {
        input: Some(path.to_string_lossy().into_owned()),
        fetch: false,
        tickers: Vec::new(),
        universe: None,
        from: None,
        to: None,
        data_root: Some(dir.to_string_lossy().into_owned()),
    };
    let error = run(&dataset).expect_err("an empty file must fail the command");
    assert!(format!("{error:#}").contains("no rows"), "got {error:#}");
    assert!(!prices_path(&dir).exists());
}
