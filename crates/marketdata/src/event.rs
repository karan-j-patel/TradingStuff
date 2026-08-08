//! Market events at order-level and price-level granularity.
//!
//! # Two granularities, deliberately not merged
//!
//! Feeds come in two shapes and this crate ingests both.
//!
//! **Order-level (MBO, "market by order")** reports individual orders joining,
//! leaving, and executing. Every order has an identity, so queue position is
//! knowable. Databento's Nasdaq ITCH feed is this shape.
//!
//! **Price-level (MBP, "market by price")** reports the total size resting at
//! each price, with no order identity. IEX's free DEEP archive is this shape.
//!
//! Collapsing them into one representation would throw away the thing that
//! makes MBO worth paying for. Queue position determines whether a passive
//! order actually fills, and no amount of price-level data recovers it.
//!
//! # Why prices are integers here
//!
//! The rest of the platform uses `Decimal` for money, and that is right for
//! accounting. It is wrong for a book that processes millions of updates, where
//! every comparison and every insertion touches a price.
//!
//! Prices here are fixed-point integers in units of 1e-9 of the quote currency.
//! Integer comparison is a single instruction, the representation is exact, and
//! ordering is the natural integer ordering, which matters because a book is
//! fundamentally a sorted structure. Conversion to `Decimal` happens once at the
//! reporting boundary rather than millions of times inside the hot path.

use std::fmt;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Fixed-point price in units of 1e-9 of the quote currency.
///
/// A `i64` covers roughly ±9.2 billion units at this scale, which is ±$9.2
/// billion per share. Signed rather than unsigned because arithmetic on price
/// differences is routine and an underflow on subtraction would be silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Price(pub i64);

/// Number of fixed-point units in one whole currency unit.
pub const PRICE_SCALE: i64 = 1_000_000_000;

impl Price {
    /// Build from whole units and nanounits, avoiding float entirely.
    pub const fn from_units(whole: i64, nanos: i64) -> Self {
        Price(whole * PRICE_SCALE + nanos)
    }

    /// Convert for display or accounting. Deliberately not `From`, so that
    /// crossing out of the hot path is visible at the call site.
    pub fn to_decimal(self) -> Decimal {
        Decimal::new(self.0, 9)
    }

    /// Convert from an accounting price. Truncates below 1e-9, which is far
    /// finer than any exchange quotes.
    pub fn from_decimal(value: Decimal) -> Option<Self> {
        let scaled = value * Decimal::from(PRICE_SCALE);
        scaled.trunc().try_into().ok().map(Price)
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_decimal())
    }
}

/// Share quantity. Integer because US equities trade in whole shares on an
/// exchange book. Fractional shares exist only as a broker-level construct and
/// never appear in a venue feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Qty(pub u32);

/// Nanoseconds since the Unix epoch, UTC.
///
/// Exchange feeds timestamp to the nanosecond and the ordering of events within
/// a microsecond is meaningful, so a coarser type would silently merge events
/// that a book must apply in sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Nanos(pub i64);

impl Nanos {
    pub const fn saturating_sub(self, other: Nanos) -> i64 {
        self.0.saturating_sub(other.0)
    }
}

/// Which side of the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Bid,
    Ask,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }
}

/// Venue-assigned order identity. Unique only within a venue and a session.
pub type OrderId = u64;

/// An order-level operation, from an MBO feed.
///
/// `Cancel` carries the size removed rather than the size remaining, because
/// partial cancels are common and a feed reporting "remaining" would require
/// the book to already know the original to compute the delta. Reporting the
/// delta directly means a lost message is detectable rather than silently
/// producing a wrong resting size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderEvent {
    Add {
        order_id: OrderId,
        side: Side,
        price: Price,
        size: Qty,
    },
    /// Size removed from a resting order. Equal to the full resting size for a
    /// complete cancellation.
    Cancel { order_id: OrderId, size: Qty },
    /// Price or size change. Most venues implement this as cancel-and-replace
    /// and the order loses queue priority, which is why it is a distinct
    /// variant rather than a `Cancel` followed by an `Add`.
    Modify {
        order_id: OrderId,
        price: Price,
        size: Qty,
    },
    /// A resting order was hit. `size` is the executed quantity, which may be
    /// less than the resting size.
    Execute { order_id: OrderId, size: Qty },
}

impl OrderEvent {
    pub fn order_id(&self) -> OrderId {
        match self {
            OrderEvent::Add { order_id, .. }
            | OrderEvent::Cancel { order_id, .. }
            | OrderEvent::Modify { order_id, .. }
            | OrderEvent::Execute { order_id, .. } => *order_id,
        }
    }
}

/// A price-level update, from an MBP feed such as IEX DEEP.
///
/// `size` is the **total** resting quantity at that price after the update, not
/// a delta. A size of zero removes the level. This is the convention IEX DEEP
/// uses and it is the reason MBP feeds cannot be converted to MBO: the identity
/// of which orders make up that total is simply not transmitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelUpdate {
    pub side: Side,
    pub price: Price,
    pub size: Qty,
}

/// Why a security is not trading normally.
///
/// Halts are frequent enough at sub-second resolution to be a first-class
/// concern rather than an edge case. A resting order during a halt does not
/// fill, and on resume the price commonly gaps through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradingStatus {
    Trading,
    /// Limit up / limit down volatility pause. Typically five minutes.
    Halted,
    /// Halted pending a news release.
    NewsPending,
    /// Auction period, opening or closing. Quotes exist but continuous trading
    /// does not.
    Auction,
}

/// Which side initiated a trade, where the feed reports it.
///
/// `None` is common and honest. Many feeds do not identify the aggressor, and
/// inferring it from the prevailing quote is an approximation that fails during
/// fast markets, which is precisely when it matters.
pub type Aggressor = Option<Side>;

/// Anything a feed can tell us, tagged with when and where.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    Order(OrderEvent),
    Level(LevelUpdate),
    Trade {
        price: Price,
        size: Qty,
        aggressor: Aggressor,
    },
    Status(TradingStatus),
    /// A full book replacement. Feeds emit these on recovery and at session
    /// start, and applying one must discard prior state rather than merge.
    Clear,
}

/// One event on one instrument at one venue.
///
/// `Copy` deliberately. The whole struct is small integers and fits in a few
/// registers, so passing it by value through the replay loop costs nothing and
/// avoids the lifetime plumbing a borrowed form would need.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MarketEvent {
    /// Exchange timestamp, when the venue says it happened.
    pub exchange_time: Nanos,
    /// When this process received it. Equal to `exchange_time` in replay, and
    /// meaningfully later in live operation. Keeping both is what makes latency
    /// measurable rather than assumed.
    pub receive_time: Nanos,
    pub venue: Venue,
    pub kind: EventKind,
}

impl MarketEvent {
    /// Feed latency for this event, in nanoseconds.
    ///
    /// Zero under replay by construction. In live operation this is the number
    /// that decides whether a strategy is implementable, so it is measured
    /// rather than estimated.
    pub fn transit_nanos(&self) -> i64 {
        self.receive_time.saturating_sub(self.exchange_time)
    }
}

/// Where an event came from.
///
/// Tagged because a consolidated view merges venues, and a book built from one
/// venue is not the national book. IEX alone is roughly two to three percent of
/// consolidated volume, so a strategy validated against it has been validated
/// against a small and unrepresentative slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Venue {
    Iex,
    Nasdaq,
    Nyse,
    Arca,
    Bats,
    Edgx,
    /// Synthetic events from tests and generated fixtures. Present in the enum
    /// so a fixture can never be mistaken for real venue data downstream.
    Synthetic,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_round_trip_through_decimal() {
        let price = Price::from_units(412, 470_000_000); // $412.47
        assert_eq!(price.to_decimal().to_string(), "412.470000000");
        assert_eq!(Price::from_decimal(price.to_decimal()), Some(price));
    }

    #[test]
    fn prices_order_as_integers() {
        let mut levels = [
            Price::from_units(412, 500_000_000),
            Price::from_units(412, 400_000_000),
            Price::from_units(413, 0),
        ];
        levels.sort();
        assert_eq!(levels[0], Price::from_units(412, 400_000_000));
        assert_eq!(levels[2], Price::from_units(413, 0));
    }

    #[test]
    fn a_sub_penny_price_survives_exactly() {
        // Sub-penny quoting exists in dark venues and price improvement, and a
        // float representation would not hold this exactly.
        let price = Price::from_units(10, 1); // $10.000000001
        assert_eq!(price.0, 10_000_000_001);
        assert_ne!(price, Price::from_units(10, 0));
    }

    #[test]
    fn sides_invert() {
        assert_eq!(Side::Bid.opposite(), Side::Ask);
        assert_eq!(Side::Ask.opposite(), Side::Bid);
    }

    #[test]
    fn every_order_event_exposes_its_id() {
        let events = [
            OrderEvent::Add {
                order_id: 7,
                side: Side::Bid,
                price: Price::from_units(100, 0),
                size: Qty(50),
            },
            OrderEvent::Cancel {
                order_id: 7,
                size: Qty(50),
            },
            OrderEvent::Modify {
                order_id: 7,
                price: Price::from_units(101, 0),
                size: Qty(25),
            },
            OrderEvent::Execute {
                order_id: 7,
                size: Qty(25),
            },
        ];
        for event in events {
            assert_eq!(event.order_id(), 7);
        }
    }

    #[test]
    fn replayed_events_report_zero_transit() {
        let event = MarketEvent {
            exchange_time: Nanos(1_000_000_000),
            receive_time: Nanos(1_000_000_000),
            venue: Venue::Synthetic,
            kind: EventKind::Status(TradingStatus::Trading),
        };
        assert_eq!(event.transit_nanos(), 0);
    }

    #[test]
    fn live_events_report_real_transit() {
        // 85 milliseconds, which is a realistic retail round trip and roughly
        // 85,000 times a colocated firm's. Written out in nanoseconds because
        // the unit is easy to get wrong by three orders of magnitude.
        const EIGHTY_FIVE_MS: i64 = 85 * 1_000_000;

        let event = MarketEvent {
            exchange_time: Nanos(1_000_000_000),
            receive_time: Nanos(1_000_000_000 + EIGHTY_FIVE_MS),
            venue: Venue::Nasdaq,
            kind: EventKind::Status(TradingStatus::Trading),
        };
        assert_eq!(event.transit_nanos(), EIGHTY_FIVE_MS);
    }

    #[test]
    fn a_market_event_stays_small_enough_to_pass_by_value() {
        // Copy semantics are only free if the struct actually is small. If this
        // grows, the replay loop should switch to passing by reference.
        assert!(
            size_of::<MarketEvent>() <= 64,
            "MarketEvent grew to {} bytes",
            size_of::<MarketEvent>()
        );
    }
}
