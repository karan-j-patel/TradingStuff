//! X-W2. Parsing a curated file needs no second read of the path.
//!
//! # The property, and why deleting the file is how it is measured
//!
//! A caller that records a file's SHA-256 in a trial and then reads that file's
//! records is claiming the numbers came from the bytes under that digest.
//! Hashing one `read` and parsing a second `read` of the same path does not
//! support the claim: between the two, the file can be replaced by a concurrent
//! refetch or by a hand, and the run then produces results from data B under a
//! hash recording data A.
//!
//! Deleting the file between the read and the parse is the strongest available
//! statement of the property. A reader that reaches for the path again cannot
//! merely disagree about contents, it cannot succeed at all, so a bytes reader
//! that quietly kept a path around fails here rather than passing by luck.
//!
//! Nothing in the reader is skipped on this route. The from-bytes and
//! path-based entry points delegate to one body, so the metadata and provenance
//! checks that guard the units label run identically. The units refusal is
//! asserted below on the bytes path for exactly that reason.

use super::*;
use crate::actions::{CorporateAction, DividendKind};
use crate::marketcap::MarketCapRecord;
use crate::parquet::{
    read_actions_from_bytes, read_delistings_from_bytes, read_marketcap_from_bytes, write_actions,
    write_delistings, write_marketcap,
};

/// Read a file's bytes and then remove it, so anything reaching for the path
/// afterwards fails.
fn read_then_delete(path: &std::path::Path) -> bytes::Bytes {
    let bytes = std::fs::read(path).expect("reading the fixture bytes");
    std::fs::remove_file(path).expect("removing the fixture file");
    assert!(
        !path.exists(),
        "the fixture file survived removal, so this test cannot discriminate"
    );
    bytes::Bytes::from(bytes)
}

/// X-W2, market cap. The records parse from bytes alone and match what was
/// written.
#[test]
fn x_w2_market_caps_parse_from_bytes_with_the_file_gone() {
    let path = scratch("xw2-marketcap").join("marketcap.parquet");
    let rows = vec![
        MarketCapRecord {
            asset: sharadar("AAAA", 1),
            date: day(2024, 12, 30),
            marketcap: dec("1234567.8"),
            source: "synthetic".into(),
        },
        MarketCapRecord {
            asset: sharadar("BBBB", 2),
            date: day(2024, 12, 31),
            marketcap: dec("0.1"),
            source: "synthetic".into(),
        },
    ];
    assert_eq!(
        write_marketcap(rows.clone(), &path, "synthetic").expect("write"),
        2
    );

    let bytes = read_then_delete(&path);
    let read = read_marketcap_from_bytes(bytes).expect("the bytes parse with no file on disk");
    assert_eq!(
        read, rows,
        "the records parsed from bytes are not the ones written"
    );
}

/// X-W2, actions.
#[test]
fn x_w2_actions_parse_from_bytes_with_the_file_gone() {
    let path = scratch("xw2-actions").join("actions.parquet");
    let rows = vec![crate::actions::ActionRecord {
        asset: sharadar("AAAA", 1),
        effective: day(2024, 3, 15),
        action: CorporateAction::Dividend {
            amount: dec("0.25"),
            kind: DividendKind::Cash,
        },
        source: "synthetic".into(),
    }];
    assert_eq!(
        write_actions(rows.clone(), &path, "synthetic").expect("write"),
        1
    );

    let bytes = read_then_delete(&path);
    let read = read_actions_from_bytes(bytes).expect("the bytes parse with no file on disk");
    assert_eq!(
        read, rows,
        "the records parsed from bytes are not the ones written"
    );
}

/// X-W2, delistings.
#[test]
fn x_w2_delistings_parse_from_bytes_with_the_file_gone() {
    let path = scratch("xw2-delistings").join("delistings.parquet");
    let rows = vec![delisting(
        sharadar("AAAA", 1),
        day(2024, 6, 28),
        TerminalValue::Imputed {
            value: dec("-0.30"),
            convention: Convention::Shumway1997NyseAmex,
        },
    )];
    assert_eq!(
        write_delistings(rows.clone(), &path, "synthetic").expect("write"),
        1
    );

    let bytes = read_then_delete(&path);
    let read = read_delistings_from_bytes(bytes).expect("the bytes parse with no file on disk");
    assert_eq!(
        read, rows,
        "the records parsed from bytes are not the ones written"
    );
}

/// The bytes route runs the same provenance checks the path route does.
///
/// Without this, a from-bytes reader that skipped the units label would satisfy
/// every round trip above and would still accept a file whose market cap column
/// could be out by a factor of a million. The fixture is a file written with no
/// metadata at all, which is what the path reader already refuses.
#[test]
fn the_bytes_route_refuses_a_file_the_path_route_would_refuse() {
    use ::parquet::arrow::ArrowWriter;
    use arrow::array::{ArrayRef, Date32Array, Decimal128Array, StringArray};
    use std::sync::Arc;

    let path = scratch("xw2-unlabelled").join("marketcap.parquet");
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("scratch directory");

    // Written without the units and source keys, which is the one thing wrong
    // with it. The columns are the schema the reader expects, so a refusal here
    // is about the missing label rather than about the shape.
    let schema = crate::parquet::marketcap::schema();
    let batch = arrow::record_batch::RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["AAAA"])) as ArrayRef,
            Arc::new(StringArray::from(vec![Some("sharadar")])),
            Arc::new(StringArray::from(vec![Some("1")])),
            Arc::new(Date32Array::from(vec![20_088])),
            Arc::new(
                Decimal128Array::from(vec![1_000i128])
                    .with_precision_and_scale(
                        crate::parquet::codec::DECIMAL_PRECISION,
                        crate::parquet::codec::DECIMAL_SCALE,
                    )
                    .expect("precision"),
            ),
            Arc::new(StringArray::from(vec!["synthetic"])),
        ],
    )
    .expect("the fixture batch builds");

    let mut file = std::fs::File::create(&path).expect("creating the unlabelled fixture");
    let mut writer = ArrowWriter::try_new(&mut file, schema, None).expect("writer");
    writer.write(&batch).expect("write");
    writer.close().expect("close");
    drop(file);

    // The path route refuses it. Asserted first, so the bytes assertion below
    // is a statement about parity rather than about this particular file.
    assert!(
        crate::parquet::read_marketcap(&path).is_err(),
        "the path reader accepted a file with no units label, so this fixture \
         cannot show the two routes agree"
    );

    let bytes = read_then_delete(&path);
    assert!(
        read_marketcap_from_bytes(bytes).is_err(),
        "the bytes reader accepted a file the path reader refuses, so the two \
         routes do not validate the same things"
    );
}
