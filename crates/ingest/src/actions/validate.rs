//! Validation for corporate actions and delistings.
//!
//! Split out of `actions.rs` for file size alone. Every name here is
//! re-exported from the parent, so `ingest::validate_delisting` and
//! `crate::actions::DelistingReject` resolve exactly as they did before.
//!
//! Every check is an arithmetic or type-level fact rather than a risk
//! threshold. A return below -1 is impossible, not conservative, and a split
//! ratio of zero describes nothing. Nothing here belongs in
//! `DECISION_CRITERIA.md`.

use rust_decimal::Decimal;

use super::{ActionRecord, Convention, CorporateAction, Delisting, TerminalValue};

/// Why a delisting record was rejected.
///
/// These are type-level and arithmetic facts rather than risk thresholds. A
/// return below -1 is not a conservative choice, it is impossible, and an
/// imputed value that disagrees with its own convention is incoherent rather
/// than aggressive. Nothing here belongs in `DECISION_CRITERIA.md`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DelistingReject {
    #[error("ticker is empty")]
    EmptyTicker,

    /// Vendors disagree about corporate actions more than about prices, so a
    /// record with no provenance cannot be reconciled against anything later.
    #[error("source is empty, and a delisting with no provenance cannot be reconciled")]
    EmptySource,

    #[error("terminal return {value} is below -1, and a holder cannot lose more than everything")]
    ReturnBelowTotalLoss { value: Decimal },

    /// A convention is a fixed published number. A record claiming to use one
    /// while carrying a different value is describing two different things at
    /// once, and downstream sensitivity analysis would vary the wrong one.
    #[error(
        "terminal value {value} is imputed from {convention:?}, whose published figure is {published}"
    )]
    ImputedValueDisagreesWithConvention {
        value: Decimal,
        convention: Convention,
        published: Decimal,
    },
}

/// Check one delisting against every rule, returning the first violation.
pub fn validate_delisting(delisting: &Delisting) -> Result<(), DelistingReject> {
    if delisting.asset.ticker.trim().is_empty() {
        return Err(DelistingReject::EmptyTicker);
    }
    if delisting.source.trim().is_empty() {
        return Err(DelistingReject::EmptySource);
    }

    // Applies to observed and imputed alike. The bound is inclusive because
    // losing exactly everything is the ordinary outcome of a bankruptcy.
    if let Some(value) = delisting.terminal.value()
        && value < Decimal::NEGATIVE_ONE
    {
        return Err(DelistingReject::ReturnBelowTotalLoss { value });
    }

    // Only imputed values are checked against a convention. An observed value
    // is a measurement, and holding it to a published average would reject
    // real data for disagreeing with an assumption.
    if let TerminalValue::Imputed { value, convention } = delisting.terminal {
        let published = convention.value();
        if value != published {
            return Err(DelistingReject::ImputedValueDisagreesWithConvention {
                value,
                convention,
                published,
            });
        }
    }

    Ok(())
}

/// Why a corporate action record was rejected.
///
/// Every payload here is a multiplier or a per-share amount, and none of them
/// mean anything at zero or below. `ActionRecord::price_factor` already
/// returns `None` for these, so such a record could never adjust a price. The
/// point of refusing at the writer is to turn silent dead weight into a loud
/// vendor-data error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionReject {
    #[error("ticker is empty")]
    EmptyTicker,

    #[error("source is empty, and a corporate action with no provenance cannot be reconciled")]
    EmptySource,

    #[error("{field} must be strictly positive, got {value}")]
    NonPositive { field: &'static str, value: Decimal },
}

/// Check one corporate action against every rule, returning the first
/// violation.
pub fn validate_action(record: &ActionRecord) -> Result<(), ActionReject> {
    if record.asset.ticker.trim().is_empty() {
        return Err(ActionReject::EmptyTicker);
    }
    if record.source.trim().is_empty() {
        return Err(ActionReject::EmptySource);
    }

    // Each kind carries one multiplier or per-share amount, and none of them
    // mean anything at zero or below. The field name travels with the error so
    // a vendor-data problem points at a column rather than at a record.
    let (field, value) = match &record.action {
        CorporateAction::Split { ratio } => ("ratio", *ratio),
        CorporateAction::Dividend { amount, .. } => ("amount", *amount),
        CorporateAction::SpinOff { factor } => ("factor", *factor),
    };
    if value <= Decimal::ZERO {
        return Err(ActionReject::NonPositive { field, value });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{DelistingReason, Listing};
    use crate::schema::AssetKey;
    use jiff::civil::Date;
    use rust_decimal::prelude::FromPrimitive;

    fn dec(v: f64) -> Decimal {
        Decimal::from_f64(v).expect("test literal is representable")
    }

    fn date(y: i16, m: i8, d: i8) -> Date {
        Date::new(y, m, d).expect("valid test date")
    }

    // --- validation -----------------------------------------------------

    fn valid() -> Delisting {
        Delisting {
            asset: AssetKey::ticker_only("GONE"),
            date: date(2026, 6, 1),
            reason: DelistingReason::Bankruptcy,
            listing: Listing::Nasdaq,
            terminal: TerminalValue::Observed(dec(-0.92)),
            source: "synthetic".into(),
        }
    }

    #[test]
    fn a_well_formed_delisting_is_accepted() {
        assert_eq!(validate_delisting(&valid()), Ok(()));
    }

    #[test]
    fn an_empty_ticker_is_rejected() {
        let mut d = valid();
        d.asset = AssetKey::ticker_only("   ");
        assert_eq!(validate_delisting(&d), Err(DelistingReject::EmptyTicker));
    }

    #[test]
    fn an_empty_source_is_rejected() {
        let mut d = valid();
        d.source = "  ".into();
        assert_eq!(validate_delisting(&d), Err(DelistingReject::EmptySource));
    }

    #[test]
    fn a_return_below_total_loss_is_rejected() {
        let mut d = valid();
        d.terminal = TerminalValue::Observed(dec(-1.5));
        assert!(matches!(
            validate_delisting(&d),
            Err(DelistingReject::ReturnBelowTotalLoss { .. })
        ));
    }

    /// Losing exactly everything is the common case for a bankruptcy, so the
    /// boundary is inclusive. An exclusive check here would reject most of the
    /// records this validation exists to protect.
    #[test]
    fn a_return_of_exactly_total_loss_is_accepted() {
        let mut d = valid();
        d.terminal = TerminalValue::Observed(dec(-1.0));
        assert_eq!(validate_delisting(&d), Ok(()));
    }

    #[test]
    fn an_unknown_terminal_value_has_no_return_to_check() {
        let mut d = valid();
        d.terminal = TerminalValue::Unknown;
        assert_eq!(validate_delisting(&d), Ok(()));
    }

    #[test]
    fn an_imputed_value_disagreeing_with_its_convention_is_rejected() {
        let mut d = valid();
        d.terminal = TerminalValue::Imputed {
            // The Nasdaq convention is -0.55, so -0.30 here is incoherent.
            value: dec(-0.30),
            convention: Convention::ShumwayWarther1999Nasdaq,
        };
        assert!(matches!(
            validate_delisting(&d),
            Err(DelistingReject::ImputedValueDisagreesWithConvention { .. })
        ));
    }

    /// Whatever `imputed()` produces must pass, or the two halves of this crate
    /// disagree about what a convention is.
    #[test]
    fn every_convention_agrees_with_what_imputed_produces() {
        for reason in [
            DelistingReason::Merger,
            DelistingReason::Acquired,
            DelistingReason::Bankruptcy,
            DelistingReason::ExchangeRule,
            DelistingReason::Voluntary,
            DelistingReason::Unknown,
        ] {
            for listing in [Listing::NyseAmex, Listing::Nasdaq, Listing::Other] {
                let mut d = valid();
                d.reason = reason;
                d.listing = listing;
                d.terminal = TerminalValue::Unknown;
                let d = d.imputed();
                assert_eq!(
                    validate_delisting(&d),
                    Ok(()),
                    "imputed() produced a record its own validator rejects, {reason:?} {listing:?}"
                );
            }
        }
    }

    /// The convention check applies only to imputed values. An observed -0.92
    /// on a Nasdaq bankruptcy is a measurement, and comparing it against the
    /// -0.55 convention would reject real data.
    #[test]
    fn an_observed_value_is_never_checked_against_a_convention() {
        let mut d = valid();
        d.terminal = TerminalValue::Observed(dec(-0.92));
        assert_eq!(validate_delisting(&d), Ok(()));
    }
}
