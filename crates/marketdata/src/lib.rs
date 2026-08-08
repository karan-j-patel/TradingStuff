//! Tick-level market data: event types, order book reconstruction, and
//! deterministic replay.
//!
//! Shared by both fast-strand objectives. The execution layer uses the book to
//! decide when and how to place orders the slow strand already chose. The
//! microstructure experiment uses the same book to look for short-horizon
//! predictability. Same machine, different objectives.
//!
//! Development path is deliberately free before it is paid. IEX's DEEP archive
//! provides real price-level data at no cost, and LOBSTER publishes message
//! files alongside their own reconstructed books, which gives this crate a
//! reference output to reproduce exactly rather than only self-consistency.

pub mod book;
pub mod event;
pub mod iex;
pub mod lobster;

pub use book::{BookError, Granularity, OrderBook};
pub use event::{
    Aggressor, EventKind, LevelUpdate, MarketEvent, Nanos, OrderEvent, OrderId, PRICE_SCALE, Price,
    Qty, Side, TradingStatus, Venue,
};
pub use iex::{DeepReader, IexError, SymbolEvent};
pub use lobster::{
    HaltPhase, LobsterError, Message, MessageType, Reconstructor, RowOutcome, Snapshot, Validator,
};
