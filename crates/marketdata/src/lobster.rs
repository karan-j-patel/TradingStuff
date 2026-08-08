//! LOBSTER message and orderbook files, and validation of our reconstruction
//! against theirs.
//!
//! # Why this module exists
//!
//! Every other test in this crate checks the book against our own expectations.
//! That proves self-consistency and nothing else. LOBSTER publishes a message
//! file alongside the book they reconstructed from it, so feeding their messages
//! to our book and diffing against their output is the one check that can catch
//! a mistake we also made in the test.
//!
//! # File formats
//!
//! Both files have one row per event and the same number of rows. Row N of the
//! orderbook file is the state **after** message N.
//!
//! Message file, six columns, no header:
//!
//! ```text
//! 34200.015105074,4,15818974,4,5794000,1
//! |               | |        | |       |
//! |               | |        | |       direction: 1 buy, -1 sell
//! |               | |        | price in units of 1e-4 dollars
//! |               | |        shares
//! |               | order id (0 for hidden executions)
//! |               type: 1 submit, 2 partial cancel, 3 delete,
//! |                     4 visible execution, 5 hidden execution, 7 halt
//! seconds after midnight, decimal
//! ```
//!
//! Orderbook file, `4 * levels` columns:
//! `ask_price_1, ask_size_1, bid_price_1, bid_size_1, ask_price_2, ...`
//!
//! # What the real-data harness does and does not prove
//!
//! It is an **aggregate one-step top-N validation**, and calling it anything
//! broader overstates it.
//!
//! What it establishes: applying one message to a known-correct book produces
//! the aggregate size at every shared price that LOBSTER reports. Measured
//! across all five 2012-06-21 samples, 2,110,855 messages, with no
//! disagreement. It is not vacuously passing either, since inverting the
//! direction column produces 54,443 disagreements.
//!
//! What it does **not** establish is the order identity lifecycle. Because
//! [`Validator::step_isolated`] reseeds from an aggregate snapshot every row,
//! it never holds a real order id long enough to look one up, so no removal it
//! sees is ever resolved by id. A GOOG census makes that exact rather than
//! approximate: 19 partial cancels plus 64,961 deletes plus 7,765 visible
//! executions is 72,745 removals, and the run reports 72,745 taking the unseen
//! path. Not most of them. All of them.
//!
//! So `Add` and `Cancel` and `Execute` keyed by order id are covered by
//! `the_order_identity_lifecycle_survives_a_continuous_stream`, on a synthetic
//! stream where every removal names an order we genuinely added. Real data
//! cannot cover it and should not be described as though it does.
//!
//! 97% to 99% of rows are rows where LOBSTER's own book moved, so the headline
//! is not inflated by messages that trivially change nothing.
//!
//! The rows where it did **not** move are exactly the hidden executions, and
//! exactly is meant literally. GOOG has 147,915 rows, 144,002 of which move the
//! book, and 3,913 hidden executions. AAPL gives 11,332 and 11,332. MSFT gives
//! 3,616 and 3,616. Three files, no residual either way.
//!
//! That is the empirical proof that a hidden execution never touches the
//! visible book, which until now was only an assertion from the documentation.
//!
//! It also bounds what the real data can police. Because those rows are the
//! ones where the visible book is unchanged, wrongly applying hidden executions
//! is invisible to this harness. Hidden orders rest inside the spread, at prices
//! that are not visible levels, so the fallback in [`Reconstructor::remove`]
//! adds and removes the order and the totals match either way. The unit test
//! `a_hidden_execution_leaves_the_book_untouched` is what catches that, and it
//! is the reason that test exists rather than being left to the real data.
//!
//! Queue position is also outside what this can check, because an orderbook row
//! carries no order identities. That is a limit of the reference data.
//!
//! # Four traps, each of which silently corrupts a naive reconstruction
//!
//! **Hidden executions never touch the book.** A type 5 message reports a fill
//! against liquidity that was never displayed, so removing size for it
//! double-counts against liquidity that was displayed. They carry order id 0 and
//! are 5% of GOOG's messages, so the error compounds within seconds.
//!
//! **Unoccupied levels are padded, not omitted.** When the real book is thinner
//! than the requested depth, LOBSTER fills the remaining columns with price
//! 9999999999 on the ask side and -9999999999 on the bid side, size 0. Our book
//! emits only levels that exist, so the padding must be stripped before diffing
//! or every thin moment reads as a bug in correct code.
//!
//! **The window opens mid-session.** Orders resting from before 09:30 are
//! referenced by cancels and executions whose submissions are not in the file.
//! See [`Reconstructor::seeded`] for how that is handled honestly.
//!
//! **Timestamps run finer than the documentation admits.** The readme promises
//! "up to nanoseconds". AMZN row 29332 carries `36754.716797047004`, which is
//! twelve fractional digits. Digits below a nanosecond are dropped rather than
//! rejected, since our clock cannot represent them and the row is otherwise
//! real data.
//!
//! # The file cannot support a full-session replay, and that is not our bug
//!
//! A level-N message file carries events only inside the top-N price range, so
//! an order cancelled while resting outside that range produces a cancellation
//! the file never mentions. A continuous replay therefore accumulates orders
//! that no longer exist, and the accumulation is one sided. In a falling market
//! a stale bid becomes the best bid, where no depth limit reaches it, while
//! stale asks sink out of range and are pruned. Against GOOG that left the ask
//! side agreeing 89% of the time at the inside and the bid side 19%.
//!
//! [`Validator::step_isolated`] is the answer and is the default. Each message
//! is applied to a book seeded from the row before it, so nothing accumulates.
//! [`Validator::step`] keeps the continuous behaviour for anyone who wants to
//! watch the information run out.

use std::collections::{HashMap, HashSet};

use crate::book::{BookError, Granularity, OrderBook};
use crate::event::{
    EventKind, MarketEvent, Nanos, OrderEvent, OrderId, Price, Qty, Side, TradingStatus, Venue,
};

/// LOBSTER quotes in units of 1e-4 dollars and [`Price`] is 1e-9, so every
/// price scales by this. IEX DEEP uses the same 1e-4 scale, which means a
/// mistake in this constant fails loudly in two independent places.
pub const PRICE_SCALE_FROM_LOBSTER: i64 = 100_000;

/// Price LOBSTER writes into an unoccupied ask column.
const ASK_PADDING: i64 = 9_999_999_999;
/// Price LOBSTER writes into an unoccupied bid column.
const BID_PADDING: i64 = -9_999_999_999;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LobsterError {
    #[error("message row has {found} fields, expected 6")]
    MessageFields { found: usize },

    #[error("orderbook row has {found} fields, which is not four per level")]
    SnapshotFields { found: usize },

    #[error("field {field} holds {value:?}, which is not a number")]
    NotANumber { field: &'static str, value: String },

    #[error("field {field} holds {value}, which does not fit the type it represents")]
    OutOfRange { field: &'static str, value: i64 },

    #[error("timestamp {seconds}s overflows when converted to nanoseconds")]
    TimestampOverflow { seconds: i64 },

    #[error("timestamp {value:?} is not seconds-after-midnight with up to nanosecond precision")]
    BadTimestamp { value: String },

    #[error("message type {found} is not one LOBSTER emits")]
    UnknownType { found: i64 },

    #[error("direction {found} is neither 1 (buy) nor -1 (sell)")]
    UnknownDirection { found: i64 },

    #[error("halt phase {found} is not -1 (halt), 0 (quoting) or 1 (trading)")]
    UnknownHaltPhase { found: i64 },
}

/// Which phase of a halt a type 7 message reports.
///
/// LOBSTER encodes this in the price column, which is otherwise meaningless for
/// these rows. Decoding it at parse time keeps that quirk in one place instead
/// of leaking a magic number into every consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltPhase {
    /// Trading stopped. Price column -1.
    Halted,
    /// Quoting resumed but continuous trading has not. Price column 0.
    QuotingResumed,
    /// Continuous trading resumed. Price column 1.
    TradingResumed,
}

/// What a message says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    /// Type 1. A new limit order joined the book.
    Submit,
    /// Type 2. Part of a resting order was withdrawn. `size` is the quantity
    /// removed, not the quantity remaining.
    Cancel,
    /// Type 3. A resting order was withdrawn entirely.
    Delete,
    /// Type 4. A displayed resting order was hit.
    VisibleExecution,
    /// Type 5. Undisplayed liquidity was hit. Order id is 0 and the visible
    /// book does not change.
    HiddenExecution,
    /// Type 7. The size, price and direction columns carry no information
    /// beyond the phase.
    Halt(HaltPhase),
}

/// One parsed message row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    /// Nanoseconds since the Unix epoch. LOBSTER stores seconds after local
    /// midnight, so the session's midnight is supplied by the caller rather
    /// than guessed here.
    pub time: Nanos,
    pub message_type: MessageType,
    pub order_id: OrderId,
    pub size: Qty,
    pub price: Price,
    /// Side of the **resting** order, not the aggressor. An execution against a
    /// sell limit order is a buyer-initiated trade, and confusing the two
    /// inverts every order-flow-imbalance feature built on top.
    pub side: Option<Side>,
}

impl Message {
    /// Parse one row.
    ///
    /// `midnight_epoch_nanos` is the Unix nanosecond timestamp of local
    /// midnight on the session date. Passing 0 yields times measured from
    /// midnight, which is enough for ordering but wrong for anything that joins
    /// against another feed.
    pub fn parse(line: &str, midnight_epoch_nanos: i64) -> Result<Self, LobsterError> {
        let mut fields = line.trim().split(',');
        let raw_time = fields
            .next()
            .ok_or(LobsterError::MessageFields { found: 0 })?;
        let raw_type = fields
            .next()
            .ok_or(LobsterError::MessageFields { found: 1 })?;
        let raw_id = fields
            .next()
            .ok_or(LobsterError::MessageFields { found: 2 })?;
        let raw_size = fields
            .next()
            .ok_or(LobsterError::MessageFields { found: 3 })?;
        let raw_price = fields
            .next()
            .ok_or(LobsterError::MessageFields { found: 4 })?;
        let raw_direction = fields
            .next()
            .ok_or(LobsterError::MessageFields { found: 5 })?;
        if fields.next().is_some() {
            return Err(LobsterError::MessageFields {
                found: 7 + fields.count(),
            });
        }

        let after_midnight = parse_after_midnight(raw_time)?;
        let type_code = parse_i64("type", raw_type)?;
        let price_code = parse_i64("price", raw_price)?;
        let direction = parse_i64("direction", raw_direction)?;

        let message_type = match type_code {
            1 => MessageType::Submit,
            2 => MessageType::Cancel,
            3 => MessageType::Delete,
            4 => MessageType::VisibleExecution,
            5 => MessageType::HiddenExecution,
            7 => MessageType::Halt(match price_code {
                -1 => HaltPhase::Halted,
                0 => HaltPhase::QuotingResumed,
                1 => HaltPhase::TradingResumed,
                found => return Err(LobsterError::UnknownHaltPhase { found }),
            }),
            found => return Err(LobsterError::UnknownType { found }),
        };

        // Halt rows set direction to -1 as filler. Reading it as a sell would
        // attribute a side to an event that has none.
        let side = match message_type {
            MessageType::Halt(_) => None,
            _ => Some(match direction {
                1 => Side::Bid,
                -1 => Side::Ask,
                found => return Err(LobsterError::UnknownDirection { found }),
            }),
        };

        let price = match message_type {
            MessageType::Halt(_) => Price(0),
            _ => Price(price_code * PRICE_SCALE_FROM_LOBSTER),
        };

        Ok(Message {
            time: Nanos(midnight_epoch_nanos + after_midnight),
            message_type,
            // Clamping a negative id to zero and truncating an oversized one
            // with `as` would turn a corrupted row into a plausible event.
            // Zero is a real order id in this format, meaning a hidden
            // execution, so a clamped negative would impersonate one. This is a
            // trust boundary and it rejects rather than repairs.
            order_id: parse_field("order id", raw_id, OrderId::try_from)?,
            size: Qty(parse_field("size", raw_size, u32::try_from)?),
            price,
            side,
        })
    }
}

/// Seconds-after-midnight with a decimal fraction, converted to nanoseconds
/// exactly.
///
/// Parsing this as `f64` and multiplying would round. An `f64` holds integers
/// exactly only up to 2^53, and while 34200e9 fits, the intermediate decimal
/// `34200.015105074` does not have an exact binary representation, so the
/// product lands a nanosecond or two off. Splitting the string keeps it exact.
fn parse_after_midnight(field: &str) -> Result<i64, LobsterError> {
    let (whole, fraction) = field.split_once('.').unwrap_or((field, ""));

    let seconds: i64 = whole.parse().map_err(|_| LobsterError::BadTimestamp {
        value: field.to_string(),
    })?;
    if seconds < 0 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(LobsterError::BadTimestamp {
            value: field.to_string(),
        });
    }

    // LOBSTER's readme promises "up to nanoseconds" and the data does not keep
    // that promise. Three of the five 2012-06-21 samples carry twelve
    // fractional digits, at AMZN row 29332, INTC row 178417 and MSFT row 7815.
    //
    // Those digits are not measurements. 36754.716797047004 and
    // 34397.878728270996 end in 004 and 996, which is the signature of a
    // float round trip in LOBSTER's exporter rather than picosecond timing.
    // Discarding them loses nothing that was ever real, and rejecting the row
    // would throw away a genuine event over formatting noise.
    //
    // Truncating can make two adjacent events share a timestamp. That is safe
    // here: the book rejects only events that go strictly backwards, and file
    // order is what actually sequences them.
    let fraction = &fraction[..fraction.len().min(9)];

    // Right-pad the fraction to nanosecond width so "0.5" reads as 500 million
    // nanoseconds rather than 5. A fixed buffer avoids allocating per row, and
    // rows number in the millions.
    let mut nanos_digits = [b'0'; 9];
    nanos_digits[..fraction.len()].copy_from_slice(fraction.as_bytes());
    let nanos: i64 = std::str::from_utf8(&nanos_digits)
        .expect("ASCII digits are valid UTF-8")
        .parse()
        .expect("nine ASCII digits fit in i64");

    // Checked, because a release build wraps silently. A wrapped timestamp
    // would land far in the past and the book's out-of-order guard would then
    // reject every event after it, which reads as a parser bug rather than as
    // the corrupt input it is.
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|scaled| scaled.checked_add(nanos))
        .ok_or(LobsterError::TimestampOverflow { seconds })
}

fn parse_i64(field: &'static str, value: &str) -> Result<i64, LobsterError> {
    value.trim().parse().map_err(|_| LobsterError::NotANumber {
        field,
        value: value.to_string(),
    })
}

/// Parse a field and narrow it to a smaller type, refusing values that do not
/// fit rather than wrapping or clamping them.
///
/// `convert` is the `TryFrom` of the target type. Passing it in keeps one
/// rejection path for every narrowed field instead of a cast per call site,
/// and a cast is exactly what this exists to prevent.
fn parse_field<T, E>(
    field: &'static str,
    value: &str,
    convert: impl Fn(i64) -> Result<T, E>,
) -> Result<T, LobsterError> {
    let parsed = parse_i64(field, value)?;
    convert(parsed).map_err(|_| LobsterError::OutOfRange {
        field,
        value: parsed,
    })
}

/// One orderbook row, with LOBSTER's padding removed.
///
/// Each side is best price first, so `asks[0]` is the lowest ask and `bids[0]`
/// the highest bid, matching [`OrderBook::depth`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub asks: Vec<(Price, u32)>,
    pub bids: Vec<(Price, u32)>,
}

impl Snapshot {
    /// Parse one orderbook row. The requested depth is inferred from the column
    /// count rather than passed in, because the file already states it and a
    /// mismatched argument would silently truncate.
    pub fn parse(line: &str) -> Result<Self, LobsterError> {
        let fields: Vec<&str> = line.trim().split(',').collect();
        if fields.is_empty() || !fields.len().is_multiple_of(4) {
            return Err(LobsterError::SnapshotFields {
                found: fields.len(),
            });
        }

        let levels = fields.len() / 4;
        let mut snapshot = Snapshot {
            asks: Vec::with_capacity(levels),
            bids: Vec::with_capacity(levels),
        };

        for level in 0..levels {
            let base = level * 4;
            let ask_price = parse_i64("ask price", fields[base])?;
            let ask_size = parse_i64("ask size", fields[base + 1])?;
            let bid_price = parse_i64("bid price", fields[base + 2])?;
            let bid_size = parse_i64("bid size", fields[base + 3])?;

            // Padding is contiguous: once a side runs out of real levels every
            // deeper column on that side is filler too. Pushing past it would
            // put a sentinel price into the comparison.
            if ask_price != ASK_PADDING && ask_size > 0 && snapshot.asks.len() == level {
                snapshot.asks.push((
                    Price(ask_price * PRICE_SCALE_FROM_LOBSTER),
                    ask_size.max(0) as u32,
                ));
            }
            if bid_price != BID_PADDING && bid_size > 0 && snapshot.bids.len() == level {
                snapshot.bids.push((
                    Price(bid_price * PRICE_SCALE_FROM_LOBSTER),
                    bid_size.max(0) as u32,
                ));
            }
        }

        Ok(snapshot)
    }

    /// Number of levels the file was generated at, real or padded.
    pub fn levels(&self) -> usize {
        self.asks.len().max(self.bids.len())
    }
}

fn at(time: Nanos, kind: EventKind) -> MarketEvent {
    MarketEvent {
        exchange_time: time,
        // LOBSTER is a reconstruction from Nasdaq TotalView-ITCH, not a packet
        // capture, so there is no separate arrival time to report. Equal
        // timestamps make transit zero, which is the honest answer rather than
        // a fabricated latency.
        receive_time: time,
        venue: Venue::Nasdaq,
        kind,
    }
}

/// Drives an [`OrderBook`] from a LOBSTER message stream.
///
/// # Handling orders that predate the file
///
/// The file opens at 09:30 and immediately cancels and executes orders that
/// joined the book before then. There are three ways to respond and only one is
/// honest.
///
/// Ignoring those messages leaves phantom liquidity resting forever. Failing on
/// them abandons the validation entirely. What this does instead is seed the
/// book from LOBSTER's own first snapshot, one synthetic aggregate order per
/// price level, and charge removals of unrecognised orders against that
/// aggregate. The aggregate stands in for "whatever was already here", which is
/// exactly what it is, and it sits at the front of the queue because it did
/// arrive first.
///
/// Below the seeded depth the aggregate does not exist. There a removal is met
/// by adding the order at its reported size and removing it again, which leaves
/// the level unchanged now and, more usefully, brings us into agreement with
/// LOBSTER from that point on: we were short by exactly that order, and after
/// its removal so are they.
///
/// That fallback is deliberately **not** available at a price the book can
/// already see. There, a removal that cannot be satisfied is a real
/// contradiction, since the reference says a size rests at that price and one
/// message just claimed more, so it raises `UnknownOrder` instead. Without that
/// boundary the fallback would absorb genuine reconstruction errors into a
/// net-zero no-op and the harness would report clean books over broken state.
pub struct Reconstructor {
    book: OrderBook,
    /// Synthetic order standing in for pre-existing liquidity at a price.
    ghosts: HashMap<(Side, Price), OrderId>,
    next_ghost: OrderId,
    /// Orders this reconstructor discarded itself, by depth truncation rather
    /// than by any message. A later cancellation naming one of these is not a
    /// lost submission, so it must not be treated as the contradiction that
    /// `remove` otherwise raises. Without this the depth policy would
    /// eventually accuse itself.
    ///
    /// A successful `Add` clears the id, so a venue that reuses identities
    /// cannot have a stale record forgive an unrelated removal later.
    ///
    /// One residual case is not covered and is safe only under the assumption
    /// below. If a venue reuses an id and the reused order's submission is
    /// itself outside a depth-limited feed's published range, no `Add` arrives
    /// to clear the record. LOBSTER identities are session-unique so this
    /// cannot arise here. A feed that reuses identities would need this to hold
    /// `order_id -> (side, price)` and forgive only removals matching the
    /// discarded order.
    truncated: HashSet<OrderId>,
    /// Removals naming an order we never saw submitted. Should be a handful at
    /// the open and then stop. A count that keeps climbing means the parse is
    /// dropping submissions.
    pub unseen_removals: u64,
    /// Hidden executions skipped. Applying these is the classic reconstruction
    /// bug, so the count is surfaced rather than hidden.
    pub hidden_executions: u64,
    pub halts: u64,
    /// Published depth of the feed, if it is depth limited. See
    /// [`OrderBook::truncate`] for why a consumer must not exceed it.
    max_depth: Option<usize>,
    /// Levels dropped for falling outside the published depth. Large is normal
    /// and is a property of the file, not of the reconstruction.
    pub truncations: u64,
}

impl Reconstructor {
    pub fn new() -> Self {
        Self {
            book: OrderBook::new(Granularity::OrderLevel),
            ghosts: HashMap::new(),
            // Ghost identities descend from the top of the id space. Real
            // LOBSTER ids are assigned upward from small numbers, so a
            // collision would need billions of orders in one session.
            next_ghost: OrderId::MAX,
            unseen_removals: 0,
            hidden_executions: 0,
            halts: 0,
            truncated: HashSet::new(),
            max_depth: None,
            truncations: 0,
        }
    }

    /// Bound the book to the depth the file publishes.
    ///
    /// LOBSTER's level-N files carry events only inside the top-N price range.
    /// An order that drifts out of that range and is cancelled there produces a
    /// cancellation the file does not contain, so without this bound the book
    /// accumulates orders that no longer exist. Leaving it unset reproduces
    /// that accumulation, which is occasionally worth doing to see it.
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = Some(depth);
        self
    }

    /// Start from LOBSTER's own first orderbook row, so the top of the book is
    /// correct before the first message we can apply ourselves.
    pub fn seeded(snapshot: &Snapshot, at_time: Nanos) -> Result<Self, BookError> {
        let mut this = Self::new();
        for (side, levels) in [(Side::Ask, &snapshot.asks), (Side::Bid, &snapshot.bids)] {
            for (price, size) in levels {
                let ghost = this.next_ghost;
                this.next_ghost -= 1;
                this.ghosts.insert((side, *price), ghost);
                this.book.apply(&at(
                    at_time,
                    EventKind::Order(OrderEvent::Add {
                        order_id: ghost,
                        side,
                        price: *price,
                        size: Qty(*size),
                    }),
                ))?;
            }
        }
        Ok(this)
    }

    pub fn book(&self) -> &OrderBook {
        &self.book
    }

    /// Apply one message.
    ///
    /// Errors other than an unrecognised order are propagated. A size removal
    /// larger than what rests, for instance, means our reconstruction has
    /// already diverged, and continuing past it produces a book that looks
    /// plausible and is wrong.
    pub fn apply(&mut self, message: &Message) -> Result<(), BookError> {
        let result = self.apply_inner(message);
        // Truncate even when the message failed, so a rejected event cannot
        // leave the book deeper than the feed it came from.
        if let Some(depth) = self.max_depth {
            let dropped = self.book.truncate(depth);
            self.truncations += dropped.len() as u64;
            self.truncated.extend(dropped);
        }
        result
    }

    fn apply_inner(&mut self, message: &Message) -> Result<(), BookError> {
        match message.message_type {
            MessageType::Submit => {
                let side = message.side.expect("submissions carry a direction");
                let applied = self.book.apply(&at(
                    message.time,
                    EventKind::Order(OrderEvent::Add {
                        order_id: message.order_id,
                        side,
                        price: message.price,
                        size: message.size,
                    }),
                ));
                // A venue that reuses order identities within a session would
                // otherwise let a stale truncation record forgive an unrelated
                // removal much later. The id is live again, so forget that we
                // once discarded it.
                if applied.is_ok() {
                    self.truncated.remove(&message.order_id);
                }
                applied
            }

            MessageType::Cancel | MessageType::Delete | MessageType::VisibleExecution => {
                let side = message.side.expect("removals carry a direction");
                self.remove(message, side)
            }

            MessageType::HiddenExecution => {
                // Undisplayed liquidity was never in the visible book, so there
                // is nothing to remove. This is the single most consequential
                // line in the module.
                self.hidden_executions += 1;
                Ok(())
            }

            MessageType::Halt(phase) => {
                self.halts += 1;
                let status = match phase {
                    // Quoting without continuous trading is still not trading.
                    // Auction is the closest thing we model, and calling it
                    // Trading would let a strategy assume fills it cannot get.
                    HaltPhase::Halted => TradingStatus::Halted,
                    HaltPhase::QuotingResumed => TradingStatus::Auction,
                    HaltPhase::TradingResumed => TradingStatus::Trading,
                };
                self.book
                    .apply(&at(message.time, EventKind::Status(status)))
            }
        }
    }

    fn remove(&mut self, message: &Message, side: Side) -> Result<(), BookError> {
        let removal = |order_id: OrderId| match message.message_type {
            MessageType::VisibleExecution => OrderEvent::Execute {
                order_id,
                size: message.size,
            },
            _ => OrderEvent::Cancel {
                order_id,
                size: message.size,
            },
        };

        match self.book.apply(&at(
            message.time,
            EventKind::Order(removal(message.order_id)),
        )) {
            Ok(()) => return Ok(()),
            Err(BookError::UnknownOrder { .. }) => {}
            Err(other) => return Err(other),
        }

        self.unseen_removals += 1;

        // Charge the seeded aggregate at this price, which is the liquidity the
        // order actually belonged to.
        if let Some(ghost) = self.ghosts.get(&(side, message.price)).copied() {
            if self
                .book
                .apply(&at(message.time, EventKind::Order(removal(ghost))))
                .is_ok()
            {
                return Ok(());
            }
            // The aggregate is exhausted or gone. Drop the stale identity so
            // the next removal at this price does not retry it.
            self.ghosts.remove(&(side, message.price));
        }

        // Two conditions gate the fallback, and both must hold.
        //
        // First, the feed must actually be depth limited. The fallback's whole
        // justification is that a depth-limited file omits events outside its
        // published range, so with no declared depth there is no such excuse
        // and an unknown order is simply an unknown order. Inferring the mode
        // from whether a price happens to be visible is not good enough: on a
        // full-depth feed an absent price means we lost the submission, which
        // is exactly the bug this is supposed to surface.
        //
        // Second, the price must be one we cannot see. At a visible price the
        // removal should have been satisfiable and was not, which is a genuine
        // contradiction rather than missing information.
        // An order we truncated away ourselves is missing information just as
        // much as one submitted outside the published range, so it is eligible
        // for the fallback even at a price that has since become visible again
        // through some other order.
        let we_discarded_it = self.truncated.contains(&message.order_id);
        let depth_limited = self.max_depth.is_some();
        if !depth_limited || (!we_discarded_it && self.book.size_at(side, message.price).is_some())
        {
            return Err(BookError::UnknownOrder {
                order_id: message.order_id,
            });
        }

        // Nothing left to charge. Add the order at its reported size and remove
        // it again. The level is unchanged now, and from here on we agree with
        // LOBSTER, who just lost the same shares we never had.
        self.book.apply(&at(
            message.time,
            EventKind::Order(OrderEvent::Add {
                order_id: message.order_id,
                side,
                price: message.price,
                size: message.size,
            }),
        ))?;
        self.book.apply(&at(
            message.time,
            EventKind::Order(removal(message.order_id)),
        ))
    }
}

impl Default for Reconstructor {
    fn default() -> Self {
        Self::new()
    }
}

/// How one row compared, rank for rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowOutcome {
    /// Every level on both sides agrees, in the same order.
    Match,
    /// Carries the shallowest disagreeing rank.
    ///
    /// Rank comparison is the strictest statement available and also the most
    /// misleading one on its own, because it cascades. See [`SideDiff`].
    Divergent { side: Side, level: usize },
}

/// A price where our size and LOBSTER's disagree.
///
/// This is the finding that matters. Everything else the harness reports can be
/// explained by information the file does not contain, but a price both books
/// hold at different sizes can only mean our reconstruction is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeMismatch {
    pub row: u64,
    pub side: Side,
    pub price: Price,
    pub ours: u64,
    pub theirs: u32,
}

/// Price-keyed comparison of one side.
///
/// # Why not compare rank for rank
///
/// LOBSTER's message file only carries events inside the requested price range,
/// so orders resting deeper than the requested depth are invisible to us until
/// the book thins and they surface. When one such level appears, rank
/// comparison shifts every level below it and reports ten failures for one
/// order nobody told us about.
///
/// Keying on price isolates the deficit to the level it belongs to and leaves
/// behind a discriminator worth having:
///
/// - `size_mismatches` can only be our bug. Both books hold the price and
///   disagree on how much rests there.
/// - `missing` is information the file does not contain. LOBSTER reconstructs
///   from the full ITCH stream and we see a filtered slice of it.
/// - `phantom` is liquidity we hold at a price inside LOBSTER's reported window
///   where they hold none. Usually the tail of the same deficit, occasionally a
///   removal we failed to apply, so it is counted rather than ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SideDiff {
    pub matched: usize,
    pub size_mismatches: usize,
    pub missing: usize,
    pub phantom: usize,
}

/// Feeds messages through a [`Reconstructor`] and scores each row against
/// LOBSTER's reference book.
pub struct Validator {
    reconstructor: Reconstructor,
    levels: usize,
    /// Rows compared.
    pub rows: u64,
    /// Rows where every level on both sides agreed, rank for rank.
    pub full_matches: u64,
    /// Rows where our book had the best bid at or above the best ask. Within
    /// one venue this cannot happen, so any count above zero is a bug.
    pub crossed_rows: u64,
    /// Per-level agreement counts, index 0 being the inside of the book.
    pub ask_level_matches: Vec<u64>,
    pub bid_level_matches: Vec<u64>,
    /// Price-keyed totals across every row and both sides. `size_mismatches`
    /// is the one that must be zero for the reconstruction to be trusted.
    pub totals: SideDiff,
    pub first_size_mismatch: Option<SizeMismatch>,
}

impl Validator {
    pub fn new(reconstructor: Reconstructor, levels: usize) -> Self {
        Self {
            reconstructor,
            levels,
            rows: 0,
            full_matches: 0,
            crossed_rows: 0,
            ask_level_matches: vec![0; levels],
            bid_level_matches: vec![0; levels],
            totals: SideDiff::default(),
            first_size_mismatch: None,
        }
    }

    pub fn reconstructor(&self) -> &Reconstructor {
        &self.reconstructor
    }

    pub fn book(&self) -> &OrderBook {
        self.reconstructor.book()
    }

    /// Apply one message and compare against the row that follows it.
    /// Apply one message against a book seeded from the row **before** it, and
    /// compare against the row after it.
    ///
    /// # Why this is the comparison that actually validates anything
    ///
    /// A continuous replay of a level-10 file diverges and stays diverged, and
    /// not because of any error we make. Cancellations that occur while an
    /// order rests outside the published price range are absent from the file,
    /// so the book accumulates orders that no longer exist. Worse, the
    /// accumulation is one-sided. In a falling market a stale bid becomes the
    /// *best* bid, where no depth limit can reach it, while stale asks sink out
    /// of range and are pruned. Against a real GOOG session that left the ask
    /// side agreeing 89% of the time at the inside and the bid side 19%.
    ///
    /// Seeding from the previous row removes the accumulation entirely. Each
    /// call is then an independent assertion about exactly one message applied
    /// to a known-correct book, which is the strongest claim this reference
    /// data can support, repeated once per row.
    ///
    /// What it cannot check is queue position, because a snapshot carries no
    /// order identities. That is a limit of the reference, not of the book.
    pub fn step_isolated(
        &mut self,
        previous: &Snapshot,
        message: &Message,
        reference: &Snapshot,
    ) -> Result<RowOutcome, BookError> {
        let carried = (
            self.reconstructor.unseen_removals,
            self.reconstructor.hidden_executions,
            self.reconstructor.halts,
        );
        // The declared depth is a property of the feed, not of one row, so it
        // has to survive the reseed. Losing it flips `remove` into full-depth
        // strictness, where a removal at a price outside the previous row's
        // published range becomes an error instead of the missing information
        // it actually is.
        let max_depth = self.reconstructor.max_depth;
        self.reconstructor = Reconstructor::seeded(previous, message.time)?;
        self.reconstructor.max_depth = max_depth;
        // Counters describe the session, not the current row, so they survive
        // the reseed.
        self.reconstructor.unseen_removals = carried.0;
        self.reconstructor.hidden_executions = carried.1;
        self.reconstructor.halts = carried.2;
        self.step(message, reference)
    }

    pub fn step(
        &mut self,
        message: &Message,
        reference: &Snapshot,
    ) -> Result<RowOutcome, BookError> {
        self.reconstructor.apply(message)?;
        self.rows += 1;
        if self.reconstructor.book().is_crossed() {
            self.crossed_rows += 1;
        }

        // `depth` allocates a small Vec per side per row. This is a validation
        // harness rather than a hot path, and hoisting a reusable buffer into
        // OrderBook's signature to save it would complicate the type every
        // consumer sees.
        //
        // Our depth is taken one level beyond the reference so that liquidity
        // we hold just past LOBSTER's window is visible to the phantom test.
        let ours_ask = self.reconstructor.book().depth(Side::Ask, self.levels + 1);
        let ours_bid = self.reconstructor.book().depth(Side::Bid, self.levels + 1);

        for (side, ours, theirs) in [
            (Side::Ask, &ours_ask, &reference.asks),
            (Side::Bid, &ours_bid, &reference.bids),
        ] {
            let (diff, mismatch) = compare_prices(side, ours, theirs, self.levels);
            self.totals.matched += diff.matched;
            self.totals.size_mismatches += diff.size_mismatches;
            self.totals.missing += diff.missing;
            self.totals.phantom += diff.phantom;

            if self.first_size_mismatch.is_none()
                && let Some((price, ours, theirs)) = mismatch
            {
                self.first_size_mismatch = Some(SizeMismatch {
                    row: self.rows,
                    side,
                    price,
                    ours,
                    theirs,
                });
            }
        }

        let ask_bad = compare_rank(
            &ours_ask,
            &reference.asks,
            self.levels,
            &mut self.ask_level_matches,
        );
        let bid_bad = compare_rank(
            &ours_bid,
            &reference.bids,
            self.levels,
            &mut self.bid_level_matches,
        );

        Ok(match (ask_bad, bid_bad) {
            (None, None) => {
                self.full_matches += 1;
                RowOutcome::Match
            }
            (Some(ask), Some(bid)) if bid < ask => RowOutcome::Divergent {
                side: Side::Bid,
                level: bid,
            },
            (Some(ask), _) => RowOutcome::Divergent {
                side: Side::Ask,
                level: ask,
            },
            (None, Some(bid)) => RowOutcome::Divergent {
                side: Side::Bid,
                level: bid,
            },
        })
    }
}

/// Compare one side by price, which is the comparison that can distinguish our
/// mistakes from information the file does not carry. See [`SideDiff`].
///
/// Returns the totals and the first size disagreement found, if any.
fn compare_prices(
    side: Side,
    ours: &[(Price, u64)],
    theirs: &[(Price, u32)],
    levels: usize,
) -> (SideDiff, Option<(Price, u64, u32)>) {
    let mut diff = SideDiff::default();
    let mut first_mismatch = None;

    for (price, their_size) in theirs {
        match ours.iter().find(|(ours_price, _)| ours_price == price) {
            Some((_, our_size)) if *our_size == u64::from(*their_size) => diff.matched += 1,
            Some((_, our_size)) => {
                diff.size_mismatches += 1;
                first_mismatch.get_or_insert((*price, *our_size, *their_size));
            }
            None => diff.missing += 1,
        }
    }

    // Liquidity of ours that falls inside LOBSTER's reported price window must
    // appear in their row too. Outside the window they simply stopped
    // reporting, so nothing can be concluded and nothing is counted.
    //
    // A row holding fewer than the requested levels is not a window at all. It
    // means their side of the book ended there, so every level of ours is
    // in scope.
    let bounded = theirs.len() >= levels;
    let worst = theirs.last().map(|(price, _)| *price);

    for (price, _) in ours {
        let in_window = match (bounded, worst) {
            (false, _) => true,
            (true, Some(worst)) => match side {
                Side::Ask => *price <= worst,
                Side::Bid => *price >= worst,
            },
            // They reported nothing and the row was full width, which cannot
            // happen, but treating it as out of window keeps this total honest.
            (true, None) => false,
        };
        if in_window && !theirs.iter().any(|(their_price, _)| their_price == price) {
            diff.phantom += 1;
        }
    }

    (diff, first_mismatch)
}

/// Compare one side rank for rank, recording per-level agreement and returning
/// the shallowest disagreement.
fn compare_rank(
    ours: &[(Price, u64)],
    theirs: &[(Price, u32)],
    levels: usize,
    matches: &mut [u64],
) -> Option<usize> {
    let mut first_bad = None;
    // Iterating the counter slice rather than a range keeps the index and the
    // slot it feeds in lockstep, which is what clippy's needless_range_loop
    // guards against.
    for (level, matched) in matches.iter_mut().enumerate().take(levels) {
        let agreed = match (ours.get(level), theirs.get(level)) {
            (Some((price, total)), Some((their_price, their_size))) => {
                price == their_price && *total == u64::from(*their_size)
            }
            // Both sides agree the book is thinner than this. Padding was
            // already stripped, so LOBSTER having no entry here is a statement,
            // not a gap.
            (None, None) => true,
            _ => false,
        };
        if agreed {
            *matched += 1;
        } else if first_bad.is_none() {
            first_bad = Some(level);
        }
    }
    first_bad
}

#[cfg(test)]
mod tests {
    use super::*;

    /// $579.40 in LOBSTER units.
    const GOOG_PRICE: i64 = 5_794_000;

    fn price(lobster_units: i64) -> Price {
        Price(lobster_units * PRICE_SCALE_FROM_LOBSTER)
    }

    #[test]
    fn a_submission_parses_into_a_bid() {
        let message =
            Message::parse("34200.154178213,1,16155653,100,5794000,1", 0).expect("well formed row");
        assert_eq!(message.message_type, MessageType::Submit);
        assert_eq!(message.order_id, 16_155_653);
        assert_eq!(message.size, Qty(100));
        assert_eq!(message.price, price(GOOG_PRICE));
        assert_eq!(message.side, Some(Side::Bid));
    }

    #[test]
    fn direction_names_the_resting_side_not_the_aggressor() {
        // -1 is a sell limit order, so it rests on the ask. An execution
        // against it is a buyer-initiated trade, and reading this column as the
        // aggressor inverts every order-flow feature built on it.
        let message = Message::parse("34200.1,4,123,50,5794000,-1", 0).expect("well formed row");
        assert_eq!(message.side, Some(Side::Ask));
    }

    #[test]
    fn timestamps_keep_every_nanosecond() {
        // Parsing this through f64 and multiplying by 1e9 lands a nanosecond or
        // two off, because the decimal has no exact binary representation.
        let message =
            Message::parse("34200.015105074,1,1,1,5794000,1", 0).expect("well formed row");
        assert_eq!(message.time, Nanos(34_200_015_105_074));
    }

    #[test]
    fn a_short_fraction_pads_rather_than_truncates() {
        // "0.5" is half a second, not five nanoseconds.
        let message = Message::parse("100.5,1,1,1,5794000,1", 0).expect("well formed row");
        assert_eq!(message.time, Nanos(100_500_000_000));
    }

    #[test]
    fn sub_nanosecond_digits_are_dropped_not_rejected() {
        // Taken verbatim from AMZN row 29332 of the 2012-06-21 sample. The
        // readme says nanoseconds, the file says otherwise, and rejecting the
        // row would throw away a real event over digits we cannot store.
        let message =
            Message::parse("36754.716797047004,1,1,1,5794000,1", 0).expect("well formed row");
        assert_eq!(message.time, Nanos(36_754_716_797_047));
    }

    #[test]
    fn a_non_numeric_fraction_is_still_rejected() {
        // Dropping excess precision must not become "accept anything".
        assert!(Message::parse("36754.71x797,1,1,1,5794000,1", 0).is_err());
        assert!(Message::parse("36754.-12,1,1,1,5794000,1", 0).is_err());
    }

    #[test]
    fn a_whole_second_needs_no_fraction() {
        let message = Message::parse("34200,1,1,1,5794000,1", 0).expect("well formed row");
        assert_eq!(message.time, Nanos(34_200_000_000_000));
    }

    #[test]
    fn the_session_date_offsets_every_timestamp() {
        const MIDNIGHT: i64 = 1_340_251_200_000_000_000; // 2012-06-21 00:00 UTC
        let message = Message::parse("34200.5,1,1,1,5794000,1", MIDNIGHT).expect("well formed row");
        assert_eq!(message.time, Nanos(MIDNIGHT + 34_200_500_000_000));
    }

    #[test]
    fn a_hidden_execution_has_no_order_id() {
        let message =
            Message::parse("34200.113246707,5,0,1,5795100,1", 0).expect("well formed row");
        assert_eq!(message.message_type, MessageType::HiddenExecution);
        assert_eq!(message.order_id, 0);
    }

    #[test]
    fn halt_rows_carry_a_phase_and_no_side() {
        for (row, phase) in [
            ("36023,7,0,0,-1,-1", HaltPhase::Halted),
            ("36323,7,0,0,0,-1", HaltPhase::QuotingResumed),
            ("36723,7,0,0,1,-1", HaltPhase::TradingResumed),
        ] {
            let message = Message::parse(row, 0).expect("well formed halt");
            assert_eq!(message.message_type, MessageType::Halt(phase));
            assert_eq!(
                message.side, None,
                "the -1 in a halt row is filler, not a sell"
            );
            assert_eq!(message.price, Price(0), "the price column holds the phase");
        }
    }

    #[test]
    fn a_row_with_the_wrong_field_count_is_an_error() {
        assert!(Message::parse("34200.1,1,1,1,5794000", 0).is_err());
        assert!(Message::parse("34200.1,1,1,1,5794000,1,99", 0).is_err());
    }

    #[test]
    fn an_unknown_message_type_is_an_error_not_a_skip() {
        assert_eq!(
            Message::parse("34200.1,6,1,1,5794000,1", 0),
            Err(LobsterError::UnknownType { found: 6 })
        );
    }

    #[test]
    fn padding_is_stripped_from_a_thin_book() {
        // Two real levels, one padded, at the level-3 shape.
        let row = "5794100,100,5794000,200,\
                   5794200,50,5793900,75,\
                   9999999999,0,-9999999999,0";
        let snapshot = Snapshot::parse(row).expect("well formed row");
        assert_eq!(
            snapshot.asks,
            vec![(price(5_794_100), 100), (price(5_794_200), 50)]
        );
        assert_eq!(
            snapshot.bids,
            vec![(price(5_794_000), 200), (price(5_793_900), 75)]
        );
        assert_eq!(snapshot.levels(), 2, "padding is not a level");
    }

    #[test]
    fn one_side_can_be_padded_while_the_other_is_not() {
        let row = "5794100,100,-9999999999,0,\
                   5794200,50,-9999999999,0";
        let snapshot = Snapshot::parse(row).expect("well formed row");
        assert_eq!(snapshot.asks.len(), 2);
        assert!(
            snapshot.bids.is_empty(),
            "an empty bid side is a real state"
        );
    }

    #[test]
    fn a_column_count_that_is_not_four_per_level_is_an_error() {
        assert!(Snapshot::parse("1,2,3").is_err());
    }

    #[test]
    fn a_hidden_execution_leaves_the_book_untouched() {
        let mut reconstructor = Reconstructor::new();
        reconstructor
            .apply(&Message::parse("1,1,10,100,5794000,1", 0).expect("submit"))
            .expect("submit applies");
        reconstructor
            .apply(&Message::parse("2,5,0,40,5794000,1", 0).expect("hidden"))
            .expect("hidden execution applies");

        assert_eq!(
            reconstructor.book().best_bid(),
            Some((price(GOOG_PRICE), 100)),
            "undisplayed liquidity was never in the visible book"
        );
        assert_eq!(reconstructor.hidden_executions, 1);
    }

    #[test]
    fn a_visible_execution_removes_size() {
        let mut reconstructor = Reconstructor::new();
        for row in ["1,1,10,100,5794000,1", "2,4,10,40,5794000,1"] {
            reconstructor
                .apply(&Message::parse(row, 0).expect("row"))
                .expect("applies");
        }
        assert_eq!(
            reconstructor.book().best_bid(),
            Some((price(GOOG_PRICE), 60))
        );
    }

    #[test]
    fn seeding_reproduces_the_reference_row_exactly() {
        let snapshot = Snapshot::parse(
            "5794100,100,5794000,200,\
             5794200,50,5793900,75",
        )
        .expect("row");
        let reconstructor = Reconstructor::seeded(&snapshot, Nanos(0)).expect("seeds");

        assert_eq!(
            reconstructor.book().depth(Side::Ask, 2),
            vec![(price(5_794_100), 100), (price(5_794_200), 50)]
        );
        assert_eq!(
            reconstructor.book().depth(Side::Bid, 2),
            vec![(price(5_794_000), 200), (price(5_793_900), 75)]
        );
    }

    #[test]
    fn a_removal_of_a_pre_existing_order_drains_the_seeded_aggregate() {
        let snapshot = Snapshot::parse("5794100,100,5794000,200").expect("row");
        let mut reconstructor = Reconstructor::seeded(&snapshot, Nanos(0)).expect("seeds");

        // Order 999 rested before the file opened, so no submission exists.
        reconstructor
            .apply(&Message::parse("1,3,999,50,5794000,1", 0).expect("delete"))
            .expect("applies");

        assert_eq!(
            reconstructor.book().best_bid(),
            Some((price(5_794_000), 150)),
            "the aggregate absorbs the removal instead of failing"
        );
        assert_eq!(reconstructor.unseen_removals, 1);
    }

    #[test]
    fn a_removal_below_the_seeded_depth_converges_rather_than_failing() {
        // Nothing seeded at this price. We are short by the order's size before
        // the removal and correct after it, which is the useful half.
        //
        // The declared depth is what makes the fallback legitimate here. On a
        // reconstructor with no declared depth this same message is an error,
        // which `an_unknown_removal_is_an_error_when_the_feed_is_not_depth_limited`
        // pins down.
        let mut reconstructor = Reconstructor::new().with_max_depth(10);
        reconstructor
            .apply(&Message::parse("1,3,999,50,5794000,1", 0).expect("delete"))
            .expect("applies");

        assert_eq!(
            reconstructor.book().best_bid(),
            None,
            "the synthesised order is added and removed, leaving nothing behind"
        );
        assert_eq!(reconstructor.unseen_removals, 1);
    }

    #[test]
    fn the_order_identity_lifecycle_survives_a_continuous_stream() {
        // The real-data harness cannot test this. It reseeds from an aggregate
        // snapshot every row, so it never holds a real order id long enough to
        // look one up, and every removal it sees takes the unknown-order
        // fallback. A GOOG census makes that exact: 72,745 removals, 72,745
        // fallbacks.
        //
        // So the order id path is exercised here instead, on a synthetic stream
        // where every removal names an order we genuinely added. Nothing may
        // take the fallback, and `unseen_removals` staying zero is the
        // assertion that proves it.
        let mut reconstructor = Reconstructor::new();
        let stream = [
            "1,1,100,500,5794000,1",  // bid 579.40 x500
            "2,1,101,300,5794000,1",  // same level, now 800
            "3,1,102,200,5795000,-1", // ask 579.50 x200
            "4,2,100,150,5794000,1",  // partial cancel, level 650
            "5,4,101,300,5794000,1",  // execute all of 101, level 350
            "6,3,100,350,5794000,1",  // delete the remainder of 100
        ];
        for row in stream {
            reconstructor
                .apply(&Message::parse(row, 0).expect("row"))
                .expect("every removal names an order we added");
        }

        assert_eq!(
            reconstructor.unseen_removals, 0,
            "a fallback here means the id lookup silently failed"
        );
        assert_eq!(
            reconstructor.book().best_bid(),
            None,
            "the bid level emptied through real order identities"
        );
        assert_eq!(
            reconstructor.book().best_ask(),
            Some((price(5_795_000), 200)),
            "the untouched ask side is undisturbed"
        );
    }

    #[test]
    fn a_removal_at_a_price_we_can_see_is_an_error_not_a_papered_over_no_op() {
        // The fallback exists for orders submitted outside the published depth,
        // which we were never told about. A price that is in our own book is a
        // different matter. The reference says size rests there and a single
        // message just claimed more, which is a real inconsistency and must not
        // be absorbed into a net-zero add-and-remove.
        let snapshot = Snapshot::parse("5794100,100,5794000,200").expect("row");
        let mut reconstructor = Reconstructor::seeded(&snapshot, Nanos(0)).expect("seeds");

        let result = reconstructor.apply(
            &Message::parse("1,3,999,500,5794000,1", 0).expect("delete of 500 where 200 rests"),
        );

        assert_eq!(
            result,
            Err(BookError::UnknownOrder { order_id: 999 }),
            "we can see this price, so we are accountable for it"
        );
    }

    #[test]
    fn a_removal_at_a_price_we_cannot_see_still_converges() {
        // The complement of the test above. This price is nowhere in our book
        // AND the feed has declared a published depth, so the order was
        // submitted outside that depth and the fallback is correct rather than
        // a cover-up. Both conditions are required.
        let snapshot = Snapshot::parse("5794100,100,5794000,200").expect("row");
        let mut reconstructor = Reconstructor::seeded(&snapshot, Nanos(0))
            .expect("seeds")
            .with_max_depth(1);

        reconstructor
            .apply(&Message::parse("1,3,999,50,5700000,1", 0).expect("delete far below the book"))
            .expect("an unseen price is eligible for the fallback");

        assert_eq!(reconstructor.unseen_removals, 1);
        assert_eq!(
            reconstructor.book().best_bid(),
            Some((price(5_794_000), 200)),
            "the visible book is untouched"
        );
    }

    #[test]
    fn isolated_stepping_keeps_the_declared_depth_across_every_reseed() {
        // `step_isolated` rebuilds the reconstructor each row, and the declared
        // depth has to survive that. If it does not, `remove` silently flips to
        // full-depth strictness and a removal at a price outside the previous
        // row's published range becomes an error instead of the missing
        // information it is.
        //
        // Two million sample messages did not catch this, because in isolated
        // mode the ghost drain almost always resolves the removal before the
        // strict check is reached. Latent, not absent.
        let seed = Snapshot::parse("5794100,100,5794000,200").expect("row");
        let mut validator = Validator::new(
            Reconstructor::seeded(&seed, Nanos(0))
                .expect("seeds")
                .with_max_depth(10),
            seed.levels(),
        );

        // Delete an order at a price nowhere in the seed, so no ghost exists and
        // the strict check is actually reached.
        let outcome = validator.step_isolated(
            &seed,
            &Message::parse("1,3,999,50,5700000,1", 0).expect("delete far below the book"),
            &seed,
        );

        assert!(
            outcome.is_ok(),
            "the declared depth was dropped on reseed, so a legitimate \
             out-of-range removal was rejected: {outcome:?}"
        );
    }

    #[test]
    fn resubmitting_a_truncated_order_id_clears_its_truncation_record() {
        // If a venue reuses order identities within a session, a stale
        // truncation record would forgive an unrelated removal much later. A
        // successful submission means the id is live again.
        let mut reconstructor = Reconstructor::new().with_max_depth(1);
        for row in [
            "1,1,10,100,5794000,1", // best bid
            "2,1,11,50,5795000,1",  // better bid truncates order 10 away
        ] {
            reconstructor
                .apply(&Message::parse(row, 0).expect("row"))
                .expect("submits apply");
        }
        assert!(reconstructor.truncated.contains(&10), "10 was discarded");

        // The venue hands out id 10 again.
        reconstructor
            .apply(&Message::parse("3,1,10,25,5795000,1", 0).expect("row"))
            .expect("resubmission applies");

        assert!(
            !reconstructor.truncated.contains(&10),
            "a live id must not keep a stale forgiveness record"
        );
    }

    #[test]
    fn cancelling_an_order_our_own_truncation_discarded_is_not_an_error() {
        // The interaction between the depth policy and the strict rule, and the
        // one false positive the strict rule could produce. Order 10 is pushed
        // out by truncation, then the price becomes visible again through a
        // different order. A cancellation for order 10 now arrives at a price
        // the book can see, which without the truncation record would look like
        // a contradiction and hard-error on entirely correct behaviour.
        //
        // It did not fire on any of the 2.1 million sample messages, which is
        // precisely why it is pinned here. A latent false positive that appears
        // months later on other data reads as a reconstruction bug.
        let mut reconstructor = Reconstructor::new().with_max_depth(1);

        for row in [
            "1,1,10,100,5794000,1", // best bid 579.40
            "2,1,11,50,5795000,1",  // better bid 579.50, pushes 579.40 to rank 2
        ] {
            reconstructor
                .apply(&Message::parse(row, 0).expect("row"))
                .expect("submits apply");
        }
        assert!(
            reconstructor.truncated.contains(&10),
            "order 10 was dropped by our own depth policy, not by a message"
        );

        // The old price becomes visible again through a brand new order.
        reconstructor
            .apply(&Message::parse("3,1,12,25,5794000,1", 0).expect("row"))
            .expect("submit applies");

        // Now cancel the order we discarded ourselves.
        reconstructor
            .apply(&Message::parse("4,3,10,100,5794000,1", 0).expect("row"))
            .expect("a cancellation for an order we truncated is missing information, not a lie");
    }

    #[test]
    fn an_unknown_removal_is_an_error_when_the_feed_is_not_depth_limited() {
        // The fallback's entire justification is that a depth-limited file omits
        // events outside its published range. With no declared depth there is no
        // such excuse, and an absent price means we lost the submission, which is
        // precisely the bug the fallback must not absorb.
        //
        // Keying on price visibility alone would let this through, because the
        // price is genuinely not in the book. The mode has to be declared.
        let mut reconstructor = Reconstructor::new();
        let result =
            reconstructor.apply(&Message::parse("1,3,999,50,5794000,1", 0).expect("delete"));

        assert_eq!(
            result,
            Err(BookError::UnknownOrder { order_id: 999 }),
            "a full-depth feed has no missing-information excuse"
        );
        assert_eq!(
            reconstructor.book().best_bid(),
            None,
            "nothing was synthesised into the book on the way out"
        );
    }

    #[test]
    fn an_order_id_that_does_not_fit_is_rejected_not_clamped() {
        // Zero is a real id in this format, meaning a hidden execution, so a
        // negative id clamped to zero would impersonate one.
        assert_eq!(
            Message::parse("1,1,-5,100,5794000,1", 0),
            Err(LobsterError::OutOfRange {
                field: "order id",
                value: -5
            })
        );
    }

    #[test]
    fn a_size_that_does_not_fit_a_u32_is_rejected_not_truncated() {
        // 4294967296 is u32::MAX + 1. Casting would wrap it to a size of zero.
        assert_eq!(
            Message::parse("1,1,10,4294967296,5794000,1", 0),
            Err(LobsterError::OutOfRange {
                field: "size",
                value: 4_294_967_296
            })
        );
    }

    #[test]
    fn a_timestamp_that_overflows_nanoseconds_is_rejected() {
        // A release build would wrap this silently, landing the event far in
        // the past, and the book's out-of-order guard would then reject
        // everything after it.
        // i64::MAX is 9_223_372_036_854_775_807, so 9_223_372_036 seconds still
        // fits once scaled and 9_223_372_037 does not. The boundary is checked
        // in both directions, since an off-by-one here would either reject good
        // rows or let the wrap through.
        assert!(
            Message::parse("9223372036,1,10,100,5794000,1", 0).is_ok(),
            "the largest second that still scales must be accepted"
        );
        assert_eq!(
            Message::parse("9223372037,1,10,100,5794000,1", 0),
            Err(LobsterError::TimestampOverflow {
                seconds: 9_223_372_037
            })
        );
    }

    #[test]
    fn a_removal_larger_than_what_rests_is_still_an_error() {
        // Convergence handling must not swallow a genuine divergence. This
        // order is known, so an oversized removal means we are already wrong.
        let mut reconstructor = Reconstructor::new();
        reconstructor
            .apply(&Message::parse("1,1,10,100,5794000,1", 0).expect("submit"))
            .expect("applies");
        let result = reconstructor.apply(&Message::parse("2,3,10,150,5794000,1", 0).expect("row"));
        assert_eq!(
            result,
            Err(BookError::OversizedRemoval {
                order_id: 10,
                requested: 150,
                resting: 100
            })
        );
    }

    #[test]
    fn a_halt_is_recorded_without_disturbing_the_book() {
        let mut reconstructor = Reconstructor::new();
        reconstructor
            .apply(&Message::parse("1,1,10,100,5794000,1", 0).expect("submit"))
            .expect("applies");
        reconstructor
            .apply(&Message::parse("2,7,0,0,-1,-1", 0).expect("halt"))
            .expect("applies");

        assert_eq!(reconstructor.book().status(), TradingStatus::Halted);
        assert_eq!(
            reconstructor.book().best_bid(),
            Some((price(GOOG_PRICE), 100)),
            "orders rest through a halt"
        );
    }

    #[test]
    fn a_matching_row_scores_as_a_match() {
        let seed = Snapshot::parse("5794100,100,5794000,200").expect("row");
        let mut validator = Validator::new(
            Reconstructor::seeded(&seed, Nanos(0)).expect("seeds"),
            seed.levels(),
        );

        // A hidden execution leaves the book unchanged, so the reference row
        // repeats the seed.
        let outcome = validator
            .step(
                &Message::parse("1,5,0,10,5794050,1", 0).expect("hidden"),
                &seed,
            )
            .expect("steps");

        assert_eq!(outcome, RowOutcome::Match);
        assert_eq!(validator.full_matches, 1);
        assert_eq!(validator.ask_level_matches, vec![1]);
        assert_eq!(validator.bid_level_matches, vec![1]);
    }

    #[test]
    fn a_level_lobster_can_see_and_we_cannot_is_missing_not_a_mismatch() {
        // The structural limit of the level-10 files. LOBSTER reconstructs from
        // the full ITCH stream, so a level that rested outside the requested
        // price range surfaces in their book with no submission in our message
        // file. Charging that to the reconstruction would be wrong.
        let ours = [(price(5_794_100), 100u64), (price(5_794_300), 50)];
        let theirs = [
            (price(5_794_100), 100u32),
            (price(5_794_200), 10),
            (price(5_794_300), 50),
        ];
        let (diff, mismatch) = compare_prices(Side::Ask, &ours, &theirs, 3);

        assert_eq!(diff.matched, 2);
        assert_eq!(diff.missing, 1, "5794200 was never announced to us");
        assert_eq!(diff.size_mismatches, 0);
        assert_eq!(diff.phantom, 0);
        assert_eq!(mismatch, None);
    }

    #[test]
    fn a_size_disagreement_at_a_shared_price_is_the_finding_that_matters() {
        let ours = [(price(5_794_100), 90u64)];
        let theirs = [(price(5_794_100), 100u32)];
        let (diff, mismatch) = compare_prices(Side::Ask, &ours, &theirs, 1);

        assert_eq!(diff.size_mismatches, 1);
        assert_eq!(diff.matched, 0);
        assert_eq!(
            mismatch,
            Some((price(5_794_100), 90, 100)),
            "both books hold this price, so only our bookkeeping explains the gap"
        );
    }

    #[test]
    fn our_liquidity_past_lobsters_window_is_not_counted_against_us() {
        // Their row is full width, so it stops reporting rather than asserting
        // the book ends. Ours resting deeper is unremarkable.
        let ours = [(price(5_794_100), 100u64), (price(5_794_900), 25)];
        let theirs = [(price(5_794_100), 100u32)];
        let (diff, _) = compare_prices(Side::Ask, &ours, &theirs, 1);

        assert_eq!(diff.phantom, 0, "5794900 is past the reported window");
        assert_eq!(diff.matched, 1);
    }

    #[test]
    fn our_liquidity_inside_lobsters_window_must_appear_in_their_row() {
        // Their row is short of the requested depth, so it does assert the book
        // ends there. Anything of ours is then in scope and unexplained.
        let ours = [(price(5_794_100), 100u64), (price(5_794_900), 25)];
        let theirs = [(price(5_794_100), 100u32)];
        let (diff, _) = compare_prices(Side::Ask, &ours, &theirs, 10);

        assert_eq!(
            diff.phantom, 1,
            "they reported fewer than ten levels, so the book really ended"
        );
    }

    #[test]
    fn the_window_test_respects_which_way_each_side_sorts() {
        // Bids get worse as price falls, so their window runs downward.
        let ours = [(price(5_794_000), 100u64), (price(5_793_000), 25)];
        let theirs = [(price(5_794_000), 100u32)];
        let (diff, _) = compare_prices(Side::Bid, &ours, &theirs, 1);
        assert_eq!(diff.phantom, 0, "5793000 is below their reported window");

        let ours = [(price(5_794_000), 100u64), (price(5_795_000), 25)];
        let (diff, _) = compare_prices(Side::Bid, &ours, &theirs, 1);
        assert_eq!(
            diff.phantom, 1,
            "5795000 is a better bid than any they report"
        );
    }

    #[test]
    fn a_divergence_reports_the_shallowest_bad_level() {
        let seed = Snapshot::parse(
            "5794100,100,5794000,200,\
             5794200,50,5793900,75",
        )
        .expect("row");
        let mut validator = Validator::new(
            Reconstructor::seeded(&seed, Nanos(0)).expect("seeds"),
            seed.levels(),
        );

        // Claim the second bid level shrank when no message says so.
        let wrong = Snapshot::parse(
            "5794100,100,5794000,200,\
             5794200,50,5793900,10",
        )
        .expect("row");
        let outcome = validator
            .step(
                &Message::parse("1,5,0,10,5794050,1", 0).expect("hidden"),
                &wrong,
            )
            .expect("steps");

        assert_eq!(
            outcome,
            RowOutcome::Divergent {
                side: Side::Bid,
                level: 1
            }
        );
        assert_eq!(validator.full_matches, 0);
        assert_eq!(
            validator.bid_level_matches,
            vec![1, 0],
            "level 0 still agreed"
        );
    }

    #[test]
    fn the_validator_walks_a_short_session_to_exact_agreement() {
        // Two levels a side, one submission, one execution, one hidden
        // execution, and one deletion of an order that predates the file.
        let seed = Snapshot::parse(
            "5794100,100,5794000,200,\
             5794200,50,5793900,75",
        )
        .expect("row");
        let mut validator = Validator::new(
            Reconstructor::seeded(&seed, Nanos(0)).expect("seeds"),
            seed.levels(),
        );

        let steps = [
            // Join the inside bid: 200 becomes 300.
            (
                "1,1,10,100,5794000,1",
                "5794100,100,5794000,300,5794200,50,5793900,75",
            ),
            // Hidden execution changes nothing.
            (
                "2,5,0,10,5794050,1",
                "5794100,100,5794000,300,5794200,50,5793900,75",
            ),
            // Hit the order we just saw join: 300 becomes 260.
            (
                "3,4,10,40,5794000,1",
                "5794100,100,5794000,260,5794200,50,5793900,75",
            ),
            // Delete an order that rested before the file opened, drawn from
            // the seeded aggregate: 260 becomes 200.
            (
                "4,3,777,60,5794000,1",
                "5794100,100,5794000,200,5794200,50,5793900,75",
            ),
        ];

        for (message_row, book_row) in steps {
            let message = Message::parse(message_row, 0).expect("message");
            let reference = Snapshot::parse(book_row).expect("book row");
            assert_eq!(
                validator.step(&message, &reference).expect("steps"),
                RowOutcome::Match,
                "row {message_row} diverged"
            );
        }

        assert_eq!(validator.rows, 4);
        assert_eq!(validator.full_matches, 4);
        assert_eq!(validator.crossed_rows, 0);
        assert_eq!(validator.reconstructor().hidden_executions, 1);
        assert_eq!(validator.reconstructor().unseen_removals, 1);
    }
}
