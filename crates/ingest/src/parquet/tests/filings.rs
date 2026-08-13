//! X-F9 and the filings round trip.
//!
//! X-F8, the life window, lives in the CLI beside the walk that applies it.
//!
//! Every value here is synthetic. The vendor's own rows live only in the
//! gitignored probe output, because the licence covers rows wherever they are
//! written down.

use super::*;
use crate::parquet::{
    FILINGS_UNITS, FILINGS_UNITS_KEY, filings_provenance, read_filings, read_filings_from_bytes,
    write_filings,
};
use crate::provider::{FundamentalRecord, ReportBasis, ReportScope};

fn filing(
    asset: AssetKey,
    as_reported: Date,
    period_end: Date,
    fields: &[(&str, &str)],
) -> FundamentalRecord {
    FundamentalRecord {
        asset,
        as_reported,
        period_end,
        observed_at: as_reported,
        source: "synthetic".into(),
        basis: ReportBasis::AsReported,
        scope: ReportScope::Quarterly,
        filing_id: None,
        fields: fields
            .iter()
            .map(|(name, value)| ((*name).to_string(), dec(value)))
            .collect(),
    }
}

/// Rows survive a round trip, including the distinction between a field the
/// vendor sent as null and one it sent as zero.
///
/// That distinction is the reason the value columns are nullable. For
/// `equityusd` the two states are "this company reported no book value" and
/// "this company's book value is zero", and the value rail excludes the second
/// while the first never reaches it.
#[test]
fn f1_filings_round_trip_and_keep_absent_and_zero_apart() {
    let path = scratch("f1").join("filings.parquet");

    let absent = filing(
        sharadar("NONE", 1),
        day(2024, 3, 15),
        day(2023, 12, 31),
        &[("revenue", "1000")],
    );
    let zero = filing(
        sharadar("ZERO", 2),
        day(2024, 3, 15),
        day(2023, 12, 31),
        &[("equityusd", "0"), ("revenue", "1000")],
    );
    let full = filing(
        sharadar("FULL", 3),
        day(2024, 5, 1),
        day(2024, 3, 31),
        &[
            ("equity", "1234.5"),
            ("equityusd", "5000000000"),
            ("netinc", "10"),
            ("netinccmnusd", "11"),
            ("revenue", "12"),
            ("revenueusd", "13"),
            ("assets", "14"),
            ("sharesbas", "15"),
            ("shareswa", "16"),
        ],
    );

    // Not in the order the writer will choose, so the sort is exercised rather
    // than accidentally satisfied by the input order.
    let input = vec![full.clone(), absent.clone(), zero.clone()];
    assert_eq!(write_filings(input, &path, "synthetic").expect("write"), 3);

    let read = read_filings(&path).expect("read");
    assert_eq!(read.len(), 3);

    let by_ticker = |ticker: &str| {
        read.iter()
            .find(|row| row.asset.ticker == ticker)
            .unwrap_or_else(|| panic!("{ticker} came back"))
    };
    assert_eq!(
        by_ticker("NONE").fields.get("equityusd"),
        None,
        "a field the vendor never sent came back as a value"
    );
    assert_eq!(
        by_ticker("ZERO").fields.get("equityusd"),
        Some(&dec("0")),
        "a reported zero came back as absent, which is the one confusion this \
         schema's nullability exists to prevent"
    );
    assert_eq!(by_ticker("FULL").fields, full.fields);

    // The two dates survive independently. A codec that wrote one column twice
    // would pass every other assertion here.
    assert_eq!(by_ticker("FULL").as_reported, day(2024, 5, 1));
    assert_eq!(by_ticker("FULL").period_end, day(2024, 3, 31));
    assert_ne!(by_ticker("FULL").as_reported, by_ticker("FULL").period_end);

    assert_eq!(by_ticker("FULL").basis, ReportBasis::AsReported);
    assert_eq!(by_ticker("FULL").scope, ReportScope::Quarterly);
    assert_eq!(
        by_ticker("FULL").filing_id,
        None,
        "this vendor ships no accession number and the column must come back null"
    );

    let provenance = filings_provenance(&path).expect("provenance");
    assert_eq!(provenance.units, FILINGS_UNITS);
    assert_eq!(provenance.source, "synthetic");
    assert_eq!(FILINGS_UNITS_KEY, "equity_units");
}

/// X-F9. Two rows sharing `(asset, basis, scope, period_end)` fail the write.
///
/// # Why an error rather than a dedup
///
/// The Phase A probe verified the as-reported dimension carries exactly one row
/// per period on every subject it checked. A duplicate therefore means the fetch
/// or the vendor broke, and silently keeping either row would pick a filing
/// nobody chose. The two rows below carry different book values, so a dedup does
/// not merely tidy the file, it decides a number.
///
/// The two rows differ in `as_reported`, which is what makes this the four-part
/// key rather than the six-part filing identity: an amendment IS a distinct
/// filing, and this dataset holds first filings only.
#[test]
fn x_f9_two_rows_for_one_period_fail_the_write() {
    let path = scratch("xf9").join("filings.parquet");
    let asset = sharadar("DUP", 1);

    let error = write_filings(
        vec![
            filing(
                asset.clone(),
                day(2024, 3, 15),
                day(2023, 12, 31),
                &[("equityusd", "100")],
            ),
            filing(
                asset.clone(),
                day(2024, 4, 20),
                day(2023, 12, 31),
                &[("equityusd", "999")],
            ),
        ],
        &path,
        "synthetic",
    )
    .expect_err("two rows for one period must be refused");

    let message = error.to_string();
    assert!(
        message.contains("DUP") && message.contains("2023-12-31"),
        "the refusal does not name the duplicated filing, got {message}"
    );
    assert!(
        !path.exists(),
        "the write was refused and still left a file behind"
    );

    // The control. The same two rows under different periods are two filings
    // and are written, so the refusal above is about the key and not about the
    // fixture.
    assert_eq!(
        write_filings(
            vec![
                filing(
                    asset.clone(),
                    day(2024, 3, 15),
                    day(2023, 12, 31),
                    &[("equityusd", "100")]
                ),
                filing(
                    asset,
                    day(2024, 4, 20),
                    day(2024, 3, 31),
                    &[("equityusd", "999")]
                ),
            ],
            &path,
            "synthetic",
        )
        .expect("two periods are two filings"),
        2
    );
}

/// The bytes route parses with no file on disk and runs the same provenance
/// check the path route does.
///
/// Present from this dataset's first commit rather than added later: the
/// read-once rule exists so a recorded digest and the parsed records describe
/// the same bytes, and a dataset shipping without it would be the one attachment
/// where that is not true.
#[test]
fn filings_parse_from_bytes_with_the_file_gone() {
    let path = scratch("xf-bytes").join("filings.parquet");
    let rows = vec![filing(
        sharadar("AAAA", 1),
        day(2024, 3, 15),
        day(2023, 12, 31),
        &[("equityusd", "5000000000")],
    )];
    assert_eq!(
        write_filings(rows.clone(), &path, "synthetic").expect("write"),
        1
    );

    let bytes = std::fs::read(&path).expect("reading the fixture bytes");
    std::fs::remove_file(&path).expect("removing the fixture file");
    assert!(!path.exists());

    let read = read_filings_from_bytes(bytes::Bytes::from(bytes))
        .expect("the bytes parse with no file on disk");
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].fields, rows[0].fields);
    assert_eq!(read[0].as_reported, rows[0].as_reported);
}

/// E1 and E2. Scope is part of the duplicate key AND is written per row.
///
/// # Why one test kills two slips
///
/// No fixture anywhere wrote an annual row, so two independent mutations lived
/// undetected. Dropping `scope` from the duplicate key makes a quarterly and an
/// annual sharing a period end collide, and the write is refused for a
/// duplicate that is not one. Hardcoding the scope label writes both rows as
/// quarterly, and the annual comes back mislabelled. A single fixture holding
/// both scopes at one period end fails against either.
///
/// The pair is real rather than contrived: a company's Q4 and its full year both
/// end on 31 December, so an annual and a quarterly record genuinely share an
/// asset, a period end and a basis. Without scope they are indistinguishable and
/// revenue figures differing by a factor of four look like a vendor rewriting
/// history. That is why `FilingKey` carries it.
#[test]
fn e1_e2_a_quarterly_and_an_annual_sharing_a_period_end_are_two_filings() {
    let path = scratch("e1e2").join("filings.parquet");
    let asset = sharadar("YEAREND", 1);
    let period = day(2024, 12, 31);

    let quarterly = filing(
        asset.clone(),
        day(2025, 2, 14),
        period,
        &[("revenue", "100")],
    );
    let annual = FundamentalRecord {
        scope: ReportScope::Annual,
        ..filing(
            asset.clone(),
            day(2025, 3, 1),
            period,
            &[("revenue", "400")],
        )
    };

    // Written, not refused. A duplicate key missing its scope term rejects this
    // pair as one filing recorded twice.
    assert_eq!(
        write_filings(vec![annual, quarterly], &path, "synthetic")
            .expect("a quarterly and an annual sharing a period end are two filings"),
        2
    );

    let read = read_filings(&path).expect("read");
    assert_eq!(read.len(), 2);

    let scope_of = |scope: ReportScope| {
        read.iter()
            .find(|row| row.scope == scope)
            .unwrap_or_else(|| panic!("no row came back with scope {scope:?}"))
    };
    // Each row keeps its own scope. A hardcoded label writes both as quarterly
    // and this lookup finds no annual at all.
    assert_eq!(
        scope_of(ReportScope::Annual).fields.get("revenue"),
        Some(&dec("400")),
        "the annual row did not come back as annual, so the scope label is not \
         written per row"
    );
    assert_eq!(
        scope_of(ReportScope::Quarterly).fields.get("revenue"),
        Some(&dec("100"))
    );
    assert_eq!(scope_of(ReportScope::Annual).period_end, period);
    assert_eq!(scope_of(ReportScope::Quarterly).period_end, period);
}

/// E3. The units label is pinned to its literal, not to the constant.
///
/// The round-trip test asserts `provenance.units == FILINGS_UNITS`, which is
/// true of any value the constant takes and so says nothing about what is
/// written on disk. The label is the only thing standing between a reader and a
/// figure a million times wrong, and a reader outside this crate compares it
/// against a literal it holds itself.
#[test]
fn e3_the_units_label_is_the_exact_string_readers_outside_this_crate_expect() {
    let path = scratch("e3").join("filings.parquet");
    write_filings(
        vec![filing(
            sharadar("AAAA", 1),
            day(2024, 3, 15),
            day(2023, 12, 31),
            &[("equityusd", "1")],
        )],
        &path,
        "synthetic",
    )
    .expect("write");

    let provenance = filings_provenance(&path).expect("provenance");
    assert_eq!(
        provenance.units, "usd_as_shipped",
        "the units label on disk is not the string this dataset promises, so a \
         reader checking it against its own literal would refuse a valid file \
         or accept an invalid one"
    );
    assert_eq!(FILINGS_UNITS, "usd_as_shipped");
    assert_eq!(FILINGS_UNITS_KEY, "equity_units");
}

/// E4. The join key is declared NOT NULL on disk.
///
/// `as_reported` is the column every research join uses and the one the
/// visibility rule reads. Declaring it nullable would let a file exist whose
/// filing dates are absent, and the reader would fail on a row rather than the
/// writer failing on the file. The two dates are pinned together because a
/// schema that relaxed one would relax the other for the same reason.
#[test]
fn e4_the_two_dates_are_not_nullable_on_disk() {
    let schema = crate::parquet::filings::schema();

    for column in ["as_reported", "period_end", "ticker", "basis", "scope"] {
        let field = schema
            .field_with_name(column)
            .unwrap_or_else(|_| panic!("the schema has no {column} column"));
        assert!(
            !field.is_nullable(),
            "{column} is declared nullable, so a file with it absent is a valid \
             file and the failure moves from the writer to whoever reads a row"
        );
    }

    // The value columns are the opposite case and are asserted here so the two
    // rules cannot drift into each other: the vendor expresses "no figure" as a
    // null on a row it still ships.
    for column in ["equityusd", "revenue", "filing_id"] {
        assert!(
            schema
                .field_with_name(column)
                .unwrap_or_else(|_| panic!("the schema has no {column} column"))
                .is_nullable(),
            "{column} is not nullable, so a vendor null has nowhere to go"
        );
    }
}
