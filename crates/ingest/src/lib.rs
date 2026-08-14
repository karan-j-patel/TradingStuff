//! Market data ingestion: fetch, validate, and persist price and fundamental
//! records from pluggable providers.
//!
//! The crate is deliberately provider-agnostic. Sharadar and SEC XBRL are two
//! implementations behind one interface, because the product ships
//! bring-your-own-key and a buyer may hold neither.
//!
//! Prices are stored raw. Adjustment is computed here from explicit corporate
//! action records rather than taken from a vendor, because "adjusted close"
//! names several different numbers and vendors ship whichever they chose.
//!
//! Build order within this crate is sync before async: typed records,
//! validation, and persistence are complete and tested against synthetic
//! fixtures before any HTTP client exists.

pub mod actions;
pub mod adjust;
pub mod adjusted;
pub mod marketcap;
pub mod parquet;
pub mod provider;
pub mod schema;
pub mod sharadar;
pub mod universe;

pub use actions::{
    ActionRecord, ActionReject, Convention, CorporateAction, Delisting, DelistingReason,
    DelistingReject, DividendKind, Listing, TerminalValue, convention_for, flat_convention_for,
    imputed_count, validate_action, validate_delisting,
};
pub use adjust::{
    AdjustedPoint, AdjustedSeries, AdjustmentMode, UnexplainedStep, adjust, unexplained_steps,
};
pub use adjusted::{AdjustedBar, validate_adjusted};
pub use marketcap::{MarketCapRecord, MarketCapReject, validate_marketcap};
pub use parquet::{
    CurateError, DEFAULT_DATA_ROOT, PanelProvenance, PanelRow, PredictionRow,
    PredictionsProvenance, actions_path, data_root, delistings_path, marketcap_path, panel_path,
    predictions_path, prices_path, read_actions, read_delistings, read_marketcap, read_panel,
    read_predictions, read_prices, write_actions, write_delistings, write_marketcap, write_panel,
    write_predictions, write_prices,
};
pub use provider::{
    DateRange, FilingKey, FundamentalRecord, FundamentalSource, PriceSource, ReportBasis,
    ReportScope, Revision, RevisionReport, SourceError, detect_revisions, visible_as_of,
};
pub use schema::{
    AssetKey, CloseKind, PermanentId, PriceBar, Reject, SessionScope, ValidationReport,
    validate_batch,
};
// `universe::select` is deliberately not re-exported flat. A bare `select` at
// the crate root says nothing about what is being selected, and the two callers
// that want it read better spelling out `universe::select`.
pub use universe::{FetchOutcome, UniverseEntry, UniverseError};
