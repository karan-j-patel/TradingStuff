//! Order book reconstruction from a feed of market events.
//!
//! # What a book is, for a reader new to market microstructure
//!
//! A limit order book is every resting order to buy or sell, organised by
//! price. Buyers rest on the **bid** side, sellers on the **ask** side. The
//! highest bid and the lowest ask are the best prices available, and the gap
//! between them is the **spread**, which is the cost of demanding immediacy.
//!
//! Orders at the same price are filled in arrival order, so **queue position**
//! decides whether a passive order actually trades. Two traders posting the
//! same price get very different outcomes depending on who arrived first. This
//! is the single most important thing an order-level feed tells you and a
//! price-level feed does not.
//!
//! # Granularity is tracked, not assumed
//!
//! A book built from a price-level feed knows the total size at each price but
//! not which orders compose it. Asking it for queue position must fail loudly
//! rather than return a plausible guess, because a strategy that believes it
//! knows its queue position when it does not will systematically overestimate
//! its fill rate.

use std::collections::{BTreeMap, HashMap};

use crate::event::{
    EventKind, MarketEvent, Nanos, OrderEvent, OrderId, Price, Qty, Side, TradingStatus,
};

/// Whether this book was built from order-level or price-level data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// Individual orders are tracked. Queue position is knowable.
    OrderLevel,
    /// Only totals per price are known. Queue position is not recoverable.
    PriceLevel,
}

/// Why applying an event failed.
///
/// These are not warnings. A feed that produces them has been misparsed, has
/// dropped a message, or is being applied out of order, and a book built past
/// one of them is silently wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BookError {
    #[error("order {order_id} already rests in the book")]
    DuplicateOrder { order_id: OrderId },

    #[error("order {order_id} is not in the book")]
    UnknownOrder { order_id: OrderId },

    #[error("tried to remove {requested} from order {order_id} which holds {resting}")]
    OversizedRemoval {
        order_id: OrderId,
        requested: u32,
        resting: u32,
    },

    #[error("price-level feed cannot answer order-level questions")]
    WrongGranularity,

    #[error("event at {event} precedes the book's last update at {last}")]
    OutOfOrder { event: i64, last: i64 },
}

/// One resting order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RestingOrder {
    side: Side,
    price: Price,
    size: Qty,
    /// Arrival order within the book. Monotonic, so comparing two sequences
    /// answers which order is ahead in the queue.
    sequence: u64,
}

/// The state of one price level.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LevelState {
    total: u64,
    /// Order identities in arrival order. Empty for a price-level book.
    ///
    /// A `Vec` with linear removal is adequate because real levels hold a
    /// handful of orders, not thousands. If profiling ever shows this hot,
    /// the replacement is an intrusive linked list keyed from `orders`.
    queue: Vec<OrderId>,
}

/// A reconstructed order book for one instrument at one venue.
#[derive(Debug, Clone)]
pub struct OrderBook {
    granularity: Granularity,
    /// `BTreeMap` because a book is fundamentally sorted by price, and the two
    /// operations that matter (find the best price, walk outward from it) are
    /// exactly what an ordered map is good at. A `HashMap` would make every
    /// best-price query a full scan.
    bids: BTreeMap<Price, LevelState>,
    asks: BTreeMap<Price, LevelState>,
    orders: HashMap<OrderId, RestingOrder>,
    status: TradingStatus,
    last_update: Nanos,
    next_sequence: u64,
}

impl OrderBook {
    pub fn new(granularity: Granularity) -> Self {
        Self {
            granularity,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            orders: HashMap::new(),
            status: TradingStatus::Trading,
            last_update: Nanos(i64::MIN),
            next_sequence: 0,
        }
    }

    pub fn granularity(&self) -> Granularity {
        self.granularity
    }

    pub fn status(&self) -> TradingStatus {
        self.status
    }

    pub fn last_update(&self) -> Nanos {
        self.last_update
    }

    /// Highest resting bid, if any.
    ///
    /// `last_key_value` because `BTreeMap` iterates ascending and the best bid
    /// is the highest price. The best ask is the lowest, hence `first`.
    pub fn best_bid(&self) -> Option<(Price, u64)> {
        self.bids
            .last_key_value()
            .map(|(price, level)| (*price, level.total))
    }

    pub fn best_ask(&self) -> Option<(Price, u64)> {
        self.asks
            .first_key_value()
            .map(|(price, level)| (*price, level.total))
    }

    /// Ask minus bid, in fixed-point price units. `None` when either side is
    /// empty, which is a real state at the open and during halts.
    pub fn spread(&self) -> Option<i64> {
        match (self.best_bid(), self.best_ask()) {
            (Some((bid, _)), Some((ask, _))) => Some(ask.0 - bid.0),
            _ => None,
        }
    }

    /// Midpoint of the best bid and ask.
    ///
    /// Integer division truncates by at most one unit of 1e-9, which is far
    /// below any exchange tick and irrelevant at this scale.
    pub fn mid(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some((bid, _)), Some((ask, _))) => Some(Price((bid.0 + ask.0) / 2)),
            _ => None,
        }
    }

    /// True when the best bid is at or above the best ask.
    ///
    /// A genuinely crossed book is possible across venues but not within one,
    /// so within a single venue's feed this means messages were dropped or
    /// applied out of order. Worth checking rather than assuming.
    pub fn is_crossed(&self) -> bool {
        matches!(self.spread(), Some(spread) if spread <= 0)
    }

    /// The top `depth` price levels on one side, best price first.
    pub fn depth(&self, side: Side, depth: usize) -> Vec<(Price, u64)> {
        match side {
            Side::Bid => self
                .bids
                .iter()
                .rev()
                .take(depth)
                .map(|(price, level)| (*price, level.total))
                .collect(),
            Side::Ask => self
                .asks
                .iter()
                .take(depth)
                .map(|(price, level)| (*price, level.total))
                .collect(),
        }
    }

    /// Total size resting ahead of `order_id` at its own price level.
    ///
    /// This is the number that decides whether a passive order fills. Available
    /// only from an order-level feed, and an error rather than a guess
    /// otherwise.
    pub fn queue_ahead(&self, order_id: OrderId) -> Result<u64, BookError> {
        if self.granularity != Granularity::OrderLevel {
            return Err(BookError::WrongGranularity);
        }
        let order = self
            .orders
            .get(&order_id)
            .ok_or(BookError::UnknownOrder { order_id })?;

        let side = self.side_map(order.side);
        let level = match side.get(&order.price) {
            Some(level) => level,
            None => return Ok(0),
        };

        Ok(level
            .queue
            .iter()
            .take_while(|id| **id != order_id)
            .filter_map(|id| self.orders.get(id))
            .map(|resting| u64::from(resting.size.0))
            .sum())
    }

    fn side_map(&self, side: Side) -> &BTreeMap<Price, LevelState> {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    fn side_map_mut(&mut self, side: Side) -> &mut BTreeMap<Price, LevelState> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    /// Apply one event, advancing the book.
    ///
    /// Rejects events that precede the last applied one. Replay must be
    /// timestamp-ordered, and a book fed out of order is wrong in ways that do
    /// not announce themselves.
    pub fn apply(&mut self, event: &MarketEvent) -> Result<(), BookError> {
        if event.exchange_time < self.last_update {
            return Err(BookError::OutOfOrder {
                event: event.exchange_time.0,
                last: self.last_update.0,
            });
        }

        match &event.kind {
            EventKind::Order(order_event) => self.apply_order(*order_event)?,
            EventKind::Level(update) => self.apply_level(update.side, update.price, update.size),
            EventKind::Status(status) => self.status = *status,
            EventKind::Clear => self.clear(),
            // A trade print reports an execution that the order events already
            // describe. Applying it again would double-count, so the book
            // ignores it and leaves trade handling to consumers that want it.
            EventKind::Trade { .. } => {}
        }

        self.last_update = event.exchange_time;
        Ok(())
    }

    fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.orders.clear();
    }

    fn apply_order(&mut self, event: OrderEvent) -> Result<(), BookError> {
        match event {
            OrderEvent::Add {
                order_id,
                side,
                price,
                size,
            } => {
                if self.orders.contains_key(&order_id) {
                    return Err(BookError::DuplicateOrder { order_id });
                }
                let sequence = self.next_sequence;
                self.next_sequence += 1;
                self.orders.insert(
                    order_id,
                    RestingOrder {
                        side,
                        price,
                        size,
                        sequence,
                    },
                );
                let level = self.side_map_mut(side).entry(price).or_default();
                level.total += u64::from(size.0);
                level.queue.push(order_id);
                Ok(())
            }

            OrderEvent::Cancel { order_id, size } | OrderEvent::Execute { order_id, size } => {
                self.remove_size(order_id, size)
            }

            OrderEvent::Modify {
                order_id,
                price,
                size,
            } => {
                // Cancel-and-replace: the order loses queue priority, which is
                // what venues actually do and why this is not a size edit in
                // place. Modelling it as an in-place edit would make a passive
                // strategy look better than it is.
                let existing = *self
                    .orders
                    .get(&order_id)
                    .ok_or(BookError::UnknownOrder { order_id })?;
                self.remove_size(order_id, existing.size)?;
                self.apply_order(OrderEvent::Add {
                    order_id,
                    side: existing.side,
                    price,
                    size,
                })
            }
        }
    }

    fn remove_size(&mut self, order_id: OrderId, size: Qty) -> Result<(), BookError> {
        let order = *self
            .orders
            .get(&order_id)
            .ok_or(BookError::UnknownOrder { order_id })?;

        if size.0 > order.size.0 {
            return Err(BookError::OversizedRemoval {
                order_id,
                requested: size.0,
                resting: order.size.0,
            });
        }

        let remaining = order.size.0 - size.0;
        let side = order.side;
        let price = order.price;

        if let Some(level) = self.side_map_mut(side).get_mut(&price) {
            level.total = level.total.saturating_sub(u64::from(size.0));
            if remaining == 0 {
                level.queue.retain(|id| *id != order_id);
            }
            if level.total == 0 && level.queue.is_empty() {
                self.side_map_mut(side).remove(&price);
            }
        }

        if remaining == 0 {
            self.orders.remove(&order_id);
        } else if let Some(resting) = self.orders.get_mut(&order_id) {
            resting.size = Qty(remaining);
        }

        Ok(())
    }

    fn apply_level(&mut self, side: Side, price: Price, size: Qty) {
        // Price-level feeds report the total after the update, not a delta, so
        // this replaces rather than accumulates. Zero removes the level.
        let map = self.side_map_mut(side);
        if size.0 == 0 {
            map.remove(&price);
        } else {
            map.insert(
                price,
                LevelState {
                    total: u64::from(size.0),
                    queue: Vec::new(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{LevelUpdate, Venue};

    fn px(whole: i64) -> Price {
        Price::from_units(whole, 0)
    }

    fn at(nanos: i64, kind: EventKind) -> MarketEvent {
        MarketEvent {
            exchange_time: Nanos(nanos),
            receive_time: Nanos(nanos),
            venue: Venue::Synthetic,
            kind,
        }
    }

    fn add(nanos: i64, order_id: OrderId, side: Side, price: i64, size: u32) -> MarketEvent {
        at(
            nanos,
            EventKind::Order(OrderEvent::Add {
                order_id,
                side,
                price: px(price),
                size: Qty(size),
            }),
        )
    }

    fn book_with(events: &[MarketEvent]) -> OrderBook {
        let mut book = OrderBook::new(Granularity::OrderLevel);
        for event in events {
            book.apply(event).expect("test events are well formed");
        }
        book
    }

    #[test]
    fn an_empty_book_has_no_prices() {
        let book = OrderBook::new(Granularity::OrderLevel);
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), None);
        assert_eq!(book.spread(), None);
        assert!(!book.is_crossed(), "an empty book is not crossed");
    }

    #[test]
    fn best_prices_are_the_inside_of_the_book() {
        let book = book_with(&[
            add(1, 1, Side::Bid, 99, 100),
            add(2, 2, Side::Bid, 100, 200), // better bid
            add(3, 3, Side::Ask, 102, 150),
            add(4, 4, Side::Ask, 101, 50), // better ask
        ]);
        assert_eq!(book.best_bid(), Some((px(100), 200)));
        assert_eq!(book.best_ask(), Some((px(101), 50)));
        assert_eq!(book.spread(), Some(1_000_000_000)); // one dollar
    }

    #[test]
    fn depth_walks_outward_from_the_inside() {
        let book = book_with(&[
            add(1, 1, Side::Bid, 98, 10),
            add(2, 2, Side::Bid, 100, 20),
            add(3, 3, Side::Bid, 99, 30),
        ]);
        assert_eq!(
            book.depth(Side::Bid, 3),
            vec![(px(100), 20), (px(99), 30), (px(98), 10)],
            "bids descend from the best"
        );
    }

    #[test]
    fn orders_at_one_price_accumulate() {
        let book = book_with(&[
            add(1, 1, Side::Bid, 100, 100),
            add(2, 2, Side::Bid, 100, 50),
        ]);
        assert_eq!(book.best_bid(), Some((px(100), 150)));
    }

    #[test]
    fn a_full_cancel_removes_the_order_and_the_empty_level() {
        let mut book = book_with(&[add(1, 1, Side::Bid, 100, 100)]);
        book.apply(&at(
            2,
            EventKind::Order(OrderEvent::Cancel {
                order_id: 1,
                size: Qty(100),
            }),
        ))
        .expect("cancel applies");
        assert_eq!(book.best_bid(), None, "the level is gone, not zero-sized");
    }

    #[test]
    fn a_partial_cancel_leaves_the_remainder() {
        let mut book = book_with(&[add(1, 1, Side::Bid, 100, 100)]);
        book.apply(&at(
            2,
            EventKind::Order(OrderEvent::Cancel {
                order_id: 1,
                size: Qty(40),
            }),
        ))
        .expect("cancel applies");
        assert_eq!(book.best_bid(), Some((px(100), 60)));
    }

    #[test]
    fn queue_position_reflects_arrival_order() {
        let book = book_with(&[
            add(1, 1, Side::Bid, 100, 100), // first
            add(2, 2, Side::Bid, 100, 50),  // second
            add(3, 3, Side::Bid, 100, 25),  // third
        ]);
        assert_eq!(book.queue_ahead(1).expect("known order"), 0);
        assert_eq!(book.queue_ahead(2).expect("known order"), 100);
        assert_eq!(book.queue_ahead(3).expect("known order"), 150);
    }

    #[test]
    fn a_modify_loses_queue_priority() {
        // The behaviour that makes passive strategies worse than they look.
        // Venues implement modify as cancel-and-replace, so an order that
        // changes size goes to the back of its queue.
        let mut book = book_with(&[
            add(1, 1, Side::Bid, 100, 100),
            add(2, 2, Side::Bid, 100, 50),
        ]);
        assert_eq!(book.queue_ahead(1).expect("known"), 0);

        book.apply(&at(
            3,
            EventKind::Order(OrderEvent::Modify {
                order_id: 1,
                price: px(100),
                size: Qty(80),
            }),
        ))
        .expect("modify applies");

        assert_eq!(
            book.queue_ahead(1).expect("known"),
            50,
            "order 1 now sits behind order 2"
        );
    }

    #[test]
    fn a_price_level_book_refuses_queue_questions() {
        let mut book = OrderBook::new(Granularity::PriceLevel);
        book.apply(&at(
            1,
            EventKind::Level(LevelUpdate {
                side: Side::Bid,
                price: px(100),
                size: Qty(500),
            }),
        ))
        .expect("level applies");

        assert_eq!(book.best_bid(), Some((px(100), 500)));
        assert_eq!(
            book.queue_ahead(1),
            Err(BookError::WrongGranularity),
            "guessing here would overstate fill rates"
        );
    }

    #[test]
    fn level_updates_replace_rather_than_accumulate() {
        let mut book = OrderBook::new(Granularity::PriceLevel);
        for size in [500u32, 300] {
            book.apply(&at(
                1,
                EventKind::Level(LevelUpdate {
                    side: Side::Bid,
                    price: px(100),
                    size: Qty(size),
                }),
            ))
            .expect("level applies");
        }
        assert_eq!(
            book.best_bid(),
            Some((px(100), 300)),
            "MBP reports totals, not deltas"
        );
    }

    #[test]
    fn a_zero_size_level_update_removes_the_level() {
        let mut book = OrderBook::new(Granularity::PriceLevel);
        for size in [500u32, 0] {
            book.apply(&at(
                1,
                EventKind::Level(LevelUpdate {
                    side: Side::Bid,
                    price: px(100),
                    size: Qty(size),
                }),
            ))
            .expect("level applies");
        }
        assert_eq!(book.best_bid(), None);
    }

    #[test]
    fn cancelling_an_unknown_order_is_an_error_not_a_no_op() {
        let mut book = OrderBook::new(Granularity::OrderLevel);
        let result = book.apply(&at(
            1,
            EventKind::Order(OrderEvent::Cancel {
                order_id: 99,
                size: Qty(10),
            }),
        ));
        assert_eq!(result, Err(BookError::UnknownOrder { order_id: 99 }));
    }

    #[test]
    fn adding_a_duplicate_order_id_is_an_error() {
        let mut book = book_with(&[add(1, 1, Side::Bid, 100, 100)]);
        let result = book.apply(&add(2, 1, Side::Bid, 101, 50));
        assert_eq!(result, Err(BookError::DuplicateOrder { order_id: 1 }));
    }

    #[test]
    fn removing_more_than_rests_is_an_error() {
        let mut book = book_with(&[add(1, 1, Side::Bid, 100, 100)]);
        let result = book.apply(&at(
            2,
            EventKind::Order(OrderEvent::Cancel {
                order_id: 1,
                size: Qty(150),
            }),
        ));
        assert_eq!(
            result,
            Err(BookError::OversizedRemoval {
                order_id: 1,
                requested: 150,
                resting: 100
            })
        );
    }

    #[test]
    fn events_out_of_order_are_rejected() {
        let mut book = book_with(&[add(10, 1, Side::Bid, 100, 100)]);
        let result = book.apply(&add(5, 2, Side::Bid, 99, 50));
        assert_eq!(result, Err(BookError::OutOfOrder { event: 5, last: 10 }));
    }

    #[test]
    fn a_clear_discards_the_whole_book() {
        let mut book = book_with(&[
            add(1, 1, Side::Bid, 100, 100),
            add(2, 2, Side::Ask, 101, 100),
        ]);
        book.apply(&at(3, EventKind::Clear)).expect("clear applies");
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), None);
        assert_eq!(
            book.queue_ahead(1),
            Err(BookError::UnknownOrder { order_id: 1 }),
            "orders are discarded too, not just levels"
        );
    }

    #[test]
    fn a_crossed_book_is_detectable() {
        let book = book_with(&[
            add(1, 1, Side::Bid, 101, 100),
            add(2, 2, Side::Ask, 100, 100),
        ]);
        assert!(book.is_crossed(), "bid above ask means dropped messages");
    }

    #[test]
    fn a_halt_is_recorded_without_disturbing_the_book() {
        let mut book = book_with(&[add(1, 1, Side::Bid, 100, 100)]);
        book.apply(&at(2, EventKind::Status(TradingStatus::Halted)))
            .expect("status applies");
        assert_eq!(book.status(), TradingStatus::Halted);
        assert_eq!(
            book.best_bid(),
            Some((px(100), 100)),
            "orders rest through a halt, they just do not trade"
        );
    }

    #[test]
    fn a_trade_print_does_not_double_count_against_order_events() {
        let mut book = book_with(&[add(1, 1, Side::Bid, 100, 100)]);
        book.apply(&at(
            2,
            EventKind::Trade {
                price: px(100),
                size: Qty(40),
                aggressor: Some(Side::Ask),
            },
        ))
        .expect("trade applies");
        assert_eq!(
            book.best_bid(),
            Some((px(100), 100)),
            "the matching Execute event is what removes size"
        );
    }

    #[test]
    fn an_execute_removes_size_the_way_a_cancel_does() {
        let mut book = book_with(&[add(1, 1, Side::Bid, 100, 100)]);
        book.apply(&at(
            2,
            EventKind::Order(OrderEvent::Execute {
                order_id: 1,
                size: Qty(40),
            }),
        ))
        .expect("execute applies");
        assert_eq!(book.best_bid(), Some((px(100), 60)));
    }
}
