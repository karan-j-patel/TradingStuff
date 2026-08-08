//! Decode a slice of a real IEX DEEP capture and build a book from it.
//!
//! Usage: cargo run --release --example iex_probe -- <capture.pcap.gz> [SYMBOL...]

use std::collections::HashMap;
use std::fs::File;

use flate2::read::GzDecoder;
use marketdata::book::{Granularity, OrderBook};
use marketdata::event::Side;
use marketdata::iex::DeepReader;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: iex_probe <capture.pcap.gz> [SYMBOL...]");
    let symbols: Vec<String> = args.collect();

    let file = File::open(&path).expect("capture opens");
    let reader = DeepReader::new(GzDecoder::new(file)).only(symbols.clone());

    let mut books: HashMap<String, OrderBook> = HashMap::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut applied = 0usize;
    let mut rejected = 0usize;

    for item in reader.take(400_000) {
        let Ok(symbol_event) = item else { continue };
        *counts.entry(symbol_event.symbol.clone()).or_default() += 1;

        let book = books
            .entry(symbol_event.symbol.clone())
            .or_insert_with(|| OrderBook::new(Granularity::PriceLevel));

        match book.apply(&symbol_event.event) {
            Ok(()) => applied += 1,
            Err(_) => rejected += 1,
        }
    }

    println!("events applied {applied}, rejected {rejected}");
    println!();

    let mut names: Vec<&String> = books.keys().collect();
    names.sort();
    for name in names.into_iter().take(6) {
        let book = &books[name];
        let events = counts.get(name).copied().unwrap_or(0);
        print!("{name:<8} {events:>7} events   ");
        match (book.best_bid(), book.best_ask()) {
            (Some((bid, bid_sz)), Some((ask, ask_sz))) => {
                let spread = book.spread().unwrap_or(0) as f64 / 1e9;
                println!(
                    "bid {} x{bid_sz}  ask {} x{ask_sz}  spread {spread:.4}  crossed {}",
                    bid.to_decimal().round_dp(2),
                    ask.to_decimal().round_dp(2),
                    book.is_crossed()
                );
            }
            _ => println!("one side empty"),
        }
        for (price, size) in book.depth(Side::Bid, 3) {
            println!(
                "           bid depth {} x{size}",
                price.to_decimal().round_dp(2)
            );
        }
    }
}
