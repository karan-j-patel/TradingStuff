//! Reader for IEX DEEP captures, distributed as gzipped pcapng.
//!
//! # Five layers, peeled in order
//!
//! ```text
//! .pcap.gz
//!   1. gzip            streamed, never decompressed to disk
//!   2. pcapng blocks   Enhanced Packet Blocks carry the traffic
//!   3. Ethernet/IP/UDP stripped to reach the multicast payload
//!   4. IEX-TP segment  40-byte header, then N length-prefixed messages
//!   5. DEEP messages   fixed little-endian layouts
//! ```
//!
//! # What DEEP is and is not
//!
//! DEEP is **price-aggregated** depth. It reports the total size resting at
//! each price, not the individual orders composing it, so a book built from it
//! cannot answer queue position. Books built here are therefore
//! [`Granularity::PriceLevel`](crate::book::Granularity::PriceLevel), and the
//! book type enforces that rather than trusting callers to remember.
//!
//! It also covers IEX alone, roughly two to three percent of consolidated
//! volume. Useful for exercising the price-level code path against real data.
//! Not a basis for research conclusions.
//!
//! # Field layouts were verified against a real capture
//!
//! Every offset below was confirmed by decoding a 2026-08-07 capture and
//! checking the results were sane, rather than taken from the specification on
//! faith. A message decoding to a real ticker at a plausible price is strong
//! evidence the offsets are right; a specification misread produces neither.

use std::collections::{HashSet, VecDeque};
use std::io::Read;

use crate::book::Granularity;
use crate::event::{
    EventKind, LevelUpdate, MarketEvent, Nanos, Price, Qty, Side, TradingStatus, Venue,
};

/// pcapng Enhanced Packet Block.
const BLOCK_ENHANCED_PACKET: u32 = 6;
/// Bytes from the start of an EPB to its packet data.
const EPB_HEADER_LEN: usize = 28;
/// Ethernet II header. This capture carries no VLAN tags.
const ETHERNET_HEADER_LEN: usize = 14;
/// UDP header, fixed.
const UDP_HEADER_LEN: usize = 8;
/// IEX-TP segment header.
const IEXTP_HEADER_LEN: usize = 40;
/// Every DEEP message this reader handles is at least this long.
const MIN_MESSAGE_LEN: usize = 18;

/// IEX quotes prices in units of 1e-4 dollars. Ours are 1e-9.
const IEX_PRICE_TO_INTERNAL: i64 = 100_000;

#[derive(Debug, thiserror::Error)]
pub enum IexError {
    #[error("io error reading capture: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a little-endian pcapng file (magic {magic:#010x})")]
    NotPcapNg { magic: u32 },
    #[error("capture ended mid-block")]
    Truncated,
}

/// One decoded DEEP message, with the symbol it applies to.
///
/// The symbol travels alongside rather than inside [`MarketEvent`] because a
/// single capture interleaves every listed security, and the consumer routes to
/// per-symbol books.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolEvent {
    pub symbol: String,
    pub event: MarketEvent,
}

/// Streaming reader over a gzipped pcapng DEEP capture.
///
/// Streams because a day's capture is roughly 13 GB compressed and several
/// times that expanded. Nothing is written to disk and no more than one read
/// buffer is held at a time.
pub struct DeepReader<R: Read> {
    source: R,
    buffer: Vec<u8>,
    cursor: usize,
    /// Symbols to emit. Empty means all, which for a full day is millions of
    /// events and rarely what anyone wants.
    filter: HashSet<String>,
    /// Decoded and awaiting the caller. A queue, not a stack: events
    /// within a segment are already in timestamp order and popping from the
    /// back would reverse them, which the book rejects as out of order.
    pending: VecDeque<SymbolEvent>,
    started: bool,
    finished: bool,
}

impl<R: Read> DeepReader<R> {
    pub fn new(source: R) -> Self {
        Self {
            source,
            buffer: Vec::new(),
            cursor: 0,
            filter: HashSet::new(),
            pending: VecDeque::new(),
            started: false,
            finished: false,
        }
    }

    /// Emit only these symbols. Strongly recommended.
    pub fn only(mut self, symbols: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.filter = symbols.into_iter().map(Into::into).collect();
        self
    }

    /// The granularity of books built from this feed.
    ///
    /// Always price-level. Stated as a function so a caller wiring up a book
    /// takes it from the reader rather than choosing, and cannot choose wrong.
    pub fn granularity(&self) -> Granularity {
        Granularity::PriceLevel
    }

    fn refill(&mut self) -> Result<bool, IexError> {
        // Retain the unconsumed tail, since a block can straddle a read.
        if self.cursor > 0 {
            self.buffer.drain(..self.cursor);
            self.cursor = 0;
        }
        let mut chunk = vec![0u8; 4 << 20];
        let read = self.source.read(&mut chunk)?;
        if read == 0 {
            return Ok(false);
        }
        chunk.truncate(read);
        self.buffer.extend_from_slice(&chunk);
        Ok(true)
    }

    /// Decode blocks from the buffer until at least one event is produced or
    /// the buffer is exhausted.
    fn advance(&mut self) -> Result<(), IexError> {
        while self.cursor + 12 <= self.buffer.len() {
            let block_type = read_u32(&self.buffer, self.cursor);
            let block_len = read_u32(&self.buffer, self.cursor + 4) as usize;

            if !self.started {
                // The first block must be a Section Header. Its magic also
                // fixes byte order, and this reader only handles little-endian
                // because that is what IEX publishes.
                if block_type != 0x0A0D_0D0A {
                    return Err(IexError::NotPcapNg { magic: block_type });
                }
                self.started = true;
            }

            if block_len < 12 || self.cursor + block_len > self.buffer.len() {
                return Ok(()); // need more bytes
            }

            if block_type == BLOCK_ENHANCED_PACKET {
                let captured = read_u32(&self.buffer, self.cursor + 20) as usize;
                let start = self.cursor + EPB_HEADER_LEN;
                if start + captured <= self.buffer.len() {
                    let packet = &self.buffer[start..start + captured];
                    decode_packet(packet, &self.filter, &mut self.pending);
                }
            }

            self.cursor += block_len;
            if !self.pending.is_empty() {
                return Ok(());
            }
        }
        Ok(())
    }
}

impl<R: Read> Iterator for DeepReader<R> {
    type Item = Result<SymbolEvent, IexError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.finished {
                return None;
            }
            if let Err(err) = self.advance() {
                self.finished = true;
                return Some(Err(err));
            }
            if self.pending.is_empty() {
                match self.refill() {
                    Ok(true) => continue,
                    Ok(false) => {
                        self.finished = true;
                        // One last pass over whatever remains buffered.
                        if self.advance().is_err() {
                            return None;
                        }
                        if self.pending.is_empty() {
                            return None;
                        }
                    }
                    Err(err) => {
                        self.finished = true;
                        return Some(Err(err));
                    }
                }
            }
        }
    }
}

/// Strip link and transport headers, then decode the IEX-TP segment.
fn decode_packet(packet: &[u8], filter: &HashSet<String>, out: &mut VecDeque<SymbolEvent>) {
    // IPv4's header length is variable, carried in the low nibble of its first
    // byte as a count of 32-bit words. Assuming 20 works for this capture and
    // would silently misalign on any packet carrying options.
    if packet.len() < ETHERNET_HEADER_LEN + 20 {
        return;
    }
    let ip_header_len = ((packet[ETHERNET_HEADER_LEN] & 0x0F) as usize) * 4;
    let payload_start = ETHERNET_HEADER_LEN + ip_header_len + UDP_HEADER_LEN;
    if packet.len() < payload_start + IEXTP_HEADER_LEN {
        return;
    }
    decode_segment(&packet[payload_start..], filter, out);
}

/// Decode one IEX-TP segment's messages.
fn decode_segment(segment: &[u8], filter: &HashSet<String>, out: &mut VecDeque<SymbolEvent>) {
    let message_count = read_u16(segment, 14) as usize;
    let send_time = read_i64(segment, 32);

    let mut offset = IEXTP_HEADER_LEN;
    for _ in 0..message_count {
        if offset + 2 > segment.len() {
            return;
        }
        let length = read_u16(segment, offset) as usize;
        offset += 2;
        if offset + length > segment.len() {
            return;
        }
        if let Some(event) = decode_message(&segment[offset..offset + length], send_time, filter) {
            out.push_back(event);
        }
        offset += length;
    }
}

/// Decode one DEEP message.
///
/// Returns `None` for message types this reader does not model and for symbols
/// outside the filter.
fn decode_message(message: &[u8], send_time: i64, filter: &HashSet<String>) -> Option<SymbolEvent> {
    if message.len() < MIN_MESSAGE_LEN {
        return None;
    }
    let kind = message[0];
    let exchange_time = read_i64(message, 2);
    let symbol = read_symbol(message, 10);
    if !filter.is_empty() && !filter.contains(&symbol) {
        return None;
    }

    let event_kind = match kind {
        // Price level updates. Buy and sell are separate message types rather
        // than a side field.
        b'8' | b'5' if message.len() >= 30 => {
            let size = read_u32(message, 18);
            let price = read_i64(message, 22);
            EventKind::Level(LevelUpdate {
                side: if kind == b'8' { Side::Bid } else { Side::Ask },
                price: Price(price * IEX_PRICE_TO_INTERNAL),
                // A size of zero removes the level, which the book already
                // handles. It is a normal message, not an anomaly.
                size: Qty(size),
            })
        }

        b'T' if message.len() >= 30 => EventKind::Trade {
            price: Price(read_i64(message, 22) * IEX_PRICE_TO_INTERNAL),
            size: Qty(read_u32(message, 18)),
            // DEEP does not identify the aggressor. Inferring it from the
            // prevailing quote fails during fast markets, which is exactly when
            // it would matter, so it stays unknown.
            aggressor: None,
        },

        // Trading status. 'T' trading, 'H' halted, 'P' paused, 'O' order
        // acceptance period.
        b'H' => EventKind::Status(match message[1] {
            b'H' => TradingStatus::Halted,
            b'P' => TradingStatus::NewsPending,
            b'O' => TradingStatus::Auction,
            _ => TradingStatus::Trading,
        }),

        // Operational halt. 'O' halted, 'N' not halted.
        b'O' => EventKind::Status(if message[1] == b'O' {
            TradingStatus::Halted
        } else {
            TradingStatus::Trading
        }),

        _ => return None,
    };

    Some(SymbolEvent {
        symbol,
        event: MarketEvent {
            exchange_time: Nanos(exchange_time),
            // The segment's send time is the closest thing the capture has to a
            // receive timestamp. Under replay the distinction is cosmetic; it
            // exists so the field is never silently zero.
            receive_time: Nanos(send_time),
            venue: Venue::Iex,
            kind: event_kind,
        },
    })
}

fn read_symbol(bytes: &[u8], offset: usize) -> String {
    String::from_utf8_lossy(&bytes[offset..offset + 8])
        .trim_end()
        .to_string()
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes"))
}

fn read_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("8 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a price level update exactly as the wire format specifies.
    fn price_level(kind: u8, symbol: &str, size: u32, price_e4: i64, time: i64) -> Vec<u8> {
        let mut msg = vec![kind, 0x01];
        msg.extend_from_slice(&time.to_le_bytes());
        let mut sym = format!("{symbol:<8}").into_bytes();
        sym.truncate(8);
        msg.extend_from_slice(&sym);
        msg.extend_from_slice(&size.to_le_bytes());
        msg.extend_from_slice(&price_e4.to_le_bytes());
        msg
    }

    #[test]
    fn a_buy_level_update_decodes_to_a_bid() {
        // $81.87, the price a real SHY message carried in the verified capture.
        let msg = price_level(b'8', "SHY", 100, 818_700, 1_700_000_000_000_000_000);
        let decoded = decode_message(&msg, 0, &HashSet::new()).expect("decodes");

        assert_eq!(decoded.symbol, "SHY");
        match decoded.event.kind {
            EventKind::Level(update) => {
                assert_eq!(update.side, Side::Bid);
                assert_eq!(update.size, Qty(100));
                assert_eq!(update.price.to_decimal().to_string(), "81.870000000");
            }
            other => panic!("expected a level update, got {other:?}"),
        }
    }

    #[test]
    fn a_sell_level_update_decodes_to_an_ask() {
        let msg = price_level(b'5', "TLTW", 500, 213_500, 1);
        let decoded = decode_message(&msg, 0, &HashSet::new()).expect("decodes");
        match decoded.event.kind {
            EventKind::Level(update) => assert_eq!(update.side, Side::Ask),
            other => panic!("expected a level update, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_size_update_is_a_normal_message_not_an_error() {
        // Observed in the real capture. It removes the level, which the book
        // handles; dropping it here would leave stale depth resting forever.
        let msg = price_level(b'8', "PWR", 0, 6_741_800, 1);
        let decoded = decode_message(&msg, 0, &HashSet::new()).expect("decodes");
        match decoded.event.kind {
            EventKind::Level(update) => assert_eq!(update.size, Qty(0)),
            other => panic!("expected a level update, got {other:?}"),
        }
    }

    #[test]
    fn the_price_scale_matches_lobster() {
        // Both feeds quote in 1e-4 dollars, so one conversion constant serves
        // both and a bug in it fails loudly in two places rather than hiding.
        let msg = price_level(b'8', "PWR", 2, 6_741_800, 1);
        let decoded = decode_message(&msg, 0, &HashSet::new()).expect("decodes");
        match decoded.event.kind {
            EventKind::Level(update) => {
                assert_eq!(update.price.to_decimal().to_string(), "674.180000000");
            }
            other => panic!("expected a level update, got {other:?}"),
        }
    }

    #[test]
    fn the_symbol_filter_excludes_everything_else() {
        let filter: HashSet<String> = ["AAPL".to_string()].into_iter().collect();
        let wanted = price_level(b'8', "AAPL", 100, 1_000_000, 1);
        let unwanted = price_level(b'8', "MSFT", 100, 1_000_000, 1);

        assert!(decode_message(&wanted, 0, &filter).is_some());
        assert!(decode_message(&unwanted, 0, &filter).is_none());
    }

    #[test]
    fn a_halt_decodes_to_a_status_event() {
        let mut msg = vec![b'H', b'H'];
        msg.extend_from_slice(&1i64.to_le_bytes());
        msg.extend_from_slice(b"AAPL    ");
        let decoded = decode_message(&msg, 0, &HashSet::new()).expect("decodes");
        assert_eq!(decoded.event.kind, EventKind::Status(TradingStatus::Halted));
    }

    #[test]
    fn an_unmodelled_message_type_is_skipped_rather_than_misread() {
        // 'I' is auction information and was the fourth most common type in the
        // capture. Decoding it with the price-level layout would produce
        // plausible nonsense, which is worse than ignoring it.
        let mut msg = vec![b'I', 0x01];
        msg.extend_from_slice(&1i64.to_le_bytes());
        msg.extend_from_slice(b"AAPL    ");
        msg.extend_from_slice(&[0u8; 20]);
        assert!(decode_message(&msg, 0, &HashSet::new()).is_none());
    }

    #[test]
    fn a_truncated_message_is_skipped_rather_than_panicking() {
        assert!(decode_message(&[b'8', 0x01, 0x00], 0, &HashSet::new()).is_none());
    }

    #[test]
    fn trailing_symbol_padding_is_trimmed() {
        let msg = price_level(b'8', "F", 100, 100_000, 1);
        let decoded = decode_message(&msg, 0, &HashSet::new()).expect("decodes");
        assert_eq!(decoded.symbol, "F", "not 'F       '");
    }

    #[test]
    fn a_deep_reader_reports_price_level_granularity() {
        // Books built from DEEP cannot answer queue position, and the reader
        // states that rather than leaving it to the caller to remember.
        let reader = DeepReader::new(std::io::empty());
        assert_eq!(reader.granularity(), Granularity::PriceLevel);
    }
}
