//! T7. The order rows are written in, which the panel work will depend on.
//!
//! Determinism is exactly the kind of property that silently is not. A round
//! trip cannot notice that rows came back in a different order than last time,
//! so the order is asserted against a written-out expectation rather than
//! against whatever the code happens to produce.

use super::super::*;
use super::{bar, day, dec, scratch, sharadar};
use crate::schema::{AssetKey, PermanentId};

/// Shuffled input writes in one known order, read back in file order.
///
/// Determinism is exactly the kind of property that silently is not. Nothing
/// about a round trip notices that rows came back in a different order than
/// last time, so the order is asserted against a written-out expectation
/// rather than against whatever the code happens to produce.
///
/// The eight rows cover, in the order the sort key considers them: two
/// identity kinds, equal kinds with different ids, equal identities with
/// different dates, equal everything with different tickers, and unidentified
/// keys, which must land after every identified key.
#[test]
fn t7_a_shuffled_batch_writes_in_one_known_order() {
    let path = scratch("t7").join("prices.parquet");

    let cik_low = bar(keyed_cik("CIKA", 100), day(2020, 1, 5), dec("1.00"));
    let cik_high = bar(keyed_cik("CIKB", 900), day(2020, 1, 5), dec("2.00"));
    // Same permanent id under two tickers, which is what a rename looks like.
    let sharadar_early = bar(sharadar("OLD", 500), day(2020, 1, 1), dec("3.00"));
    let sharadar_late = bar(sharadar("OLD", 500), day(2020, 1, 2), dec("4.00"));
    let sharadar_other = bar(sharadar("NEW", 700), day(2020, 1, 1), dec("5.00"));
    let plain_a = bar(AssetKey::ticker_only("AAA"), day(2020, 1, 9), dec("6.00"));
    let plain_z = bar(AssetKey::ticker_only("ZZZ"), day(2020, 1, 1), dec("7.00"));
    let plain_z_later = bar(AssetKey::ticker_only("ZZZ"), day(2020, 1, 2), dec("8.00"));

    let shuffled = vec![
        plain_z_later.clone(),
        sharadar_late.clone(),
        cik_high.clone(),
        plain_a.clone(),
        sharadar_early.clone(),
        plain_z.clone(),
        cik_low.clone(),
        sharadar_other.clone(),
    ];
    assert_eq!(write_prices(shuffled, &path).expect("write"), 8);

    // Written out rather than computed, so a change to the sort has to be
    // acknowledged here rather than silently agreeing with itself.
    let expected = [
        ("CIKA", "sec_cik", "100", day(2020, 1, 5)),
        ("CIKB", "sec_cik", "900", day(2020, 1, 5)),
        // The id outranks the ticker, so 500 precedes 700 even though NEW
        // precedes OLD alphabetically.
        ("OLD", "sharadar", "500", day(2020, 1, 1)),
        ("OLD", "sharadar", "500", day(2020, 1, 2)),
        ("NEW", "sharadar", "700", day(2020, 1, 1)),
        ("AAA", "", "", day(2020, 1, 9)),
        ("ZZZ", "", "", day(2020, 1, 1)),
        ("ZZZ", "", "", day(2020, 1, 2)),
    ];

    let read = read_prices(&path).expect("read");
    let actual: Vec<(String, String, String, jiff::civil::Date)> = read
        .iter()
        .map(|b| {
            let (kind, id) = match &b.asset.permanent {
                None => (String::new(), String::new()),
                Some(PermanentId::Sharadar(n)) => ("sharadar".into(), n.to_string()),
                Some(PermanentId::SecCik(n)) => ("sec_cik".into(), n.to_string()),
                Some(PermanentId::Alpaca(s)) => ("alpaca".into(), s.clone()),
            };
            (b.asset.ticker.clone(), kind, id, b.date)
        })
        .collect();

    for (index, want) in expected.iter().enumerate() {
        let got = &actual[index];
        assert_eq!(
            (got.0.as_str(), got.1.as_str(), got.2.as_str(), got.3),
            *want,
            "row {index} is out of order"
        );
    }

    // Nulls last, stated separately so a failure says which property broke.
    let first_unidentified = actual
        .iter()
        .position(|row| row.1.is_empty())
        .expect("some rows are unidentified");
    assert!(
        actual[first_unidentified..]
            .iter()
            .all(|row| row.1.is_empty()),
        "an identified key sorted after an unidentified one, so nulls-last is broken"
    );
}

fn keyed_cik(ticker: &str, id: u64) -> AssetKey {
    AssetKey {
        ticker: ticker.into(),
        permanent: Some(PermanentId::SecCik(id)),
    }
}
