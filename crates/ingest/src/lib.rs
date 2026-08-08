//! Market data ingestion: fetch, validate, and persist price and fundamental
//! records from pluggable providers.
//!
//! The crate is deliberately provider-agnostic. Sharadar and SEC XBRL are two
//! implementations behind one interface, because the product ships
//! bring-your-own-key and a buyer may hold neither.
//!
//! Build order within this crate is sync before async: typed records,
//! validation, and persistence are complete and tested against synthetic
//! fixtures before any HTTP client exists.

pub mod provider;
pub mod schema;

pub use provider::{
    DateRange, FilingKey, FundamentalRecord, FundamentalSource, PriceSource, ReportBasis,
    ReportScope, Revision, RevisionReport, SourceError, detect_revisions, visible_as_of,
};
pub use schema::{
    AdjustmentJump, AssetKey, PermanentId, PriceBar, Reject, ValidationReport,
    find_adjustment_jumps, validate_batch,
};
