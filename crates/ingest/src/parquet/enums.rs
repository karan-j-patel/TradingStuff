//! The closed label sets each enum uses on disk.
//!
//! Every label is lowercase snake_case. The `from` half of each pair errors on
//! anything outside its set rather than falling back to a variant, because a
//! silently substituted default is a wrong value that reads like a real one.
//!
//! These live apart from the rest of the codec because they are pure value
//! mapping. Nothing here touches arrow, a file, or a row.

use super::error::CurateError;
use crate::actions::{Convention, DelistingReason, DividendKind, Listing};
use crate::provider::{ReportBasis, ReportScope};
use crate::schema::{CloseKind, SessionScope};

pub(crate) fn session_str(value: SessionScope) -> &'static str {
    match value {
        SessionScope::RegularHours => "regular_hours",
        SessionScope::IncludingExtended => "including_extended",
    }
}

pub(crate) fn session_from(
    dataset: &'static str,
    value: &str,
) -> Result<SessionScope, CurateError> {
    match value {
        "regular_hours" => Ok(SessionScope::RegularHours),
        "including_extended" => Ok(SessionScope::IncludingExtended),
        other => Err(unknown(dataset, "session", other)),
    }
}

pub(crate) fn close_kind_str(value: CloseKind) -> &'static str {
    match value {
        CloseKind::ClosingAuction => "closing_auction",
        CloseKind::LastTrade => "last_trade",
    }
}

pub(crate) fn close_kind_from(
    dataset: &'static str,
    value: &str,
) -> Result<CloseKind, CurateError> {
    match value {
        "closing_auction" => Ok(CloseKind::ClosingAuction),
        "last_trade" => Ok(CloseKind::LastTrade),
        other => Err(unknown(dataset, "close_kind", other)),
    }
}

pub(crate) fn reason_str(value: DelistingReason) -> &'static str {
    match value {
        DelistingReason::Merger => "merger",
        DelistingReason::Acquired => "acquired",
        DelistingReason::Bankruptcy => "bankruptcy",
        DelistingReason::ExchangeRule => "exchange_rule",
        DelistingReason::Voluntary => "voluntary",
        DelistingReason::Unknown => "unknown",
    }
}

pub(crate) fn reason_from(
    dataset: &'static str,
    value: &str,
) -> Result<DelistingReason, CurateError> {
    match value {
        "merger" => Ok(DelistingReason::Merger),
        "acquired" => Ok(DelistingReason::Acquired),
        "bankruptcy" => Ok(DelistingReason::Bankruptcy),
        "exchange_rule" => Ok(DelistingReason::ExchangeRule),
        "voluntary" => Ok(DelistingReason::Voluntary),
        "unknown" => Ok(DelistingReason::Unknown),
        other => Err(unknown(dataset, "reason", other)),
    }
}

pub(crate) fn listing_str(value: Listing) -> &'static str {
    match value {
        Listing::NyseAmex => "nyse_amex",
        Listing::Nasdaq => "nasdaq",
        Listing::Other => "other",
    }
}

pub(crate) fn listing_from(dataset: &'static str, value: &str) -> Result<Listing, CurateError> {
    match value {
        "nyse_amex" => Ok(Listing::NyseAmex),
        "nasdaq" => Ok(Listing::Nasdaq),
        "other" => Ok(Listing::Other),
        other => Err(unknown(dataset, "listing", other)),
    }
}

pub(crate) fn convention_str(value: Convention) -> &'static str {
    match value {
        Convention::Shumway1997NyseAmex => "shumway_1997_nyse_amex",
        Convention::ShumwayWarther1999Nasdaq => "shumway_warther_1999_nasdaq",
        Convention::CrspMergerUnknown => "crsp_merger_unknown",
    }
}

pub(crate) fn convention_from(
    dataset: &'static str,
    value: &str,
) -> Result<Convention, CurateError> {
    match value {
        "shumway_1997_nyse_amex" => Ok(Convention::Shumway1997NyseAmex),
        "shumway_warther_1999_nasdaq" => Ok(Convention::ShumwayWarther1999Nasdaq),
        "crsp_merger_unknown" => Ok(Convention::CrspMergerUnknown),
        other => Err(unknown(dataset, "imputation_convention", other)),
    }
}

pub(crate) fn dividend_kind_str(value: DividendKind) -> &'static str {
    match value {
        DividendKind::Cash => "cash",
        DividendKind::Stock => "stock",
        DividendKind::Special => "special",
    }
}

pub(crate) fn dividend_kind_from(
    dataset: &'static str,
    value: &str,
) -> Result<DividendKind, CurateError> {
    match value {
        "cash" => Ok(DividendKind::Cash),
        "stock" => Ok(DividendKind::Stock),
        "special" => Ok(DividendKind::Special),
        other => Err(unknown(dataset, "dividend_kind", other)),
    }
}

fn unknown(dataset: &'static str, field: &'static str, value: &str) -> CurateError {
    CurateError::UnknownEnum {
        dataset,
        field,
        value: value.to_owned(),
    }
}

pub(crate) fn basis_str(value: ReportBasis) -> &'static str {
    match value {
        ReportBasis::AsReported => "as_reported",
        ReportBasis::Restated => "restated",
    }
}

/// Both labels are decodable, deliberately.
///
/// A reader that could not name `restated` would report a restated file as
/// corrupt rather than as what it is, and the filings attachment refuses it by
/// name. Refusing at the door with the right word beats failing to parse.
pub(crate) fn basis_from(dataset: &'static str, value: &str) -> Result<ReportBasis, CurateError> {
    match value {
        "as_reported" => Ok(ReportBasis::AsReported),
        "restated" => Ok(ReportBasis::Restated),
        other => Err(unknown(dataset, "basis", other)),
    }
}

pub(crate) fn scope_str(value: ReportScope) -> &'static str {
    match value {
        ReportScope::Quarterly => "quarterly",
        ReportScope::Annual => "annual",
        ReportScope::TrailingTwelveMonths => "trailing_twelve_months",
    }
}

pub(crate) fn scope_from(dataset: &'static str, value: &str) -> Result<ReportScope, CurateError> {
    match value {
        "quarterly" => Ok(ReportScope::Quarterly),
        "annual" => Ok(ReportScope::Annual),
        "trailing_twelve_months" => Ok(ReportScope::TrailingTwelveMonths),
        other => Err(unknown(dataset, "scope", other)),
    }
}
