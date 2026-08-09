//! Deterministic replay of market events, and the latency instrumentation that
//! keeps a backtest honest about when it could have acted.
//!
//! # Why a merge and not a sort
//!
//! Each source is already sorted. A file written by a venue is in the venue's
//! own order, and re-sorting it would be both wasteful and dangerous, because a
//! sort silently repairs corrupt input instead of reporting it. A merge reads
//! every source in stream order and holds only one event per source in memory,
//! so a session of any length costs the same fixed footprint, and an input that
//! goes backwards is caught at the row where it happens rather than being
//! quietly reordered into something plausible.
//!
//! # What determinism means here
//!
//! Same input, same output, on every run and on any machine. That rules out
//! anything that reads a clock, sleeps, or lets threads race, so none of those
//! appear below. It also rules out any tie-break that depends on hash iteration
//! order or on which source happened to be read first.
//!
//! # Latency is measured, never assumed
//!
//! A backtest that acts on a signal at the instant it appears is fiction. Two
//! separate delays sit between an event happening and an order reaching the
//! venue.
//!
//! **Feed transit** is how long the event took to reach this process, already
//! carried on every [`MarketEvent`] as the gap between `exchange_time` and
//! `receive_time`. [`LatencyStats`] reports its distribution.
//!
//! **Decision latency** is how long this process then takes to decide. A replay
//! cannot observe it, so it is configured, and [`DecisionReplay`] adds it to
//! every event's `receive_time`.
//!
//! # Two orderings, and why they cannot be one stream
//!
//! A [`MarketEvent`] carries two timestamps and they do not agree on order.
//! `exchange_time` is when the venue says the event happened. `receive_time` is
//! when this process could first have known about it. Transit is the gap
//! between them, and transit is not a constant. Venues sit at different
//! distances, feeds are handed over at different points, and any one link
//! jitters from packet to packet.
//!
//! Two consumers therefore need two different streams.
//!
//! [`OrderBook::apply`](crate::book::OrderBook::apply) needs venue order. Book
//! state is what the venue held, and rebuilding it in receive order produces a
//! book that never existed. `apply` rejects an event whose `exchange_time` goes
//! backwards, so venue order is not a preference there, it is a precondition.
//! [`Replay`] merges on `exchange_time` and serves that consumer.
//!
//! A strategy needs knowledge order, because it cannot act on what has not
//! reached it. [`DecisionReplay`] merges on `receive_time` plus the configured
//! decision latency, and that sum is the only clock a strategy is given.
//!
//! # The counterexample that forced the split
//!
//! An earlier version of this module merged on `exchange_time` and then
//! annotated `receive_time` by adding a constant, reasoning that a uniform
//! shift cannot reorder anything. The reasoning is correct and the conclusion
//! was still wrong, because a shift only preserves an order that was already
//! there, and these two orders were never the same order.
//!
//! Two events, from two venues.
//!
//! ```text
//! A   exchange_time = 10   receive_time = 1000   happened early, arrived late
//! B   exchange_time = 20   receive_time = 20     happened later, arrived at once
//! ```
//!
//! A merge keyed on `exchange_time` delivers A and then B. Read the receive
//! times off that delivery order and they run 1000 and then 20, which goes
//! backwards. A strategy would make a decision at simulated time 1000 and then
//! be handed information it in fact held at time 20. That is lookahead,
//! arriving through the very API built to prevent it.
//!
//! Adding a constant to both sides leaves 1000 and 20 in the same wrong order,
//! so no latency figure repairs it. The fault is the merge key, not the
//! annotation on top of it.
//!
//! This is why there is no `with_decision_latency` on [`Replay`]. A
//! venue-ordered merge that also stamps a decision clock is precisely the shape
//! that produced the bug, and the cheapest way to stop a future reader
//! rebuilding it is to make the shape inexpressible.
//!
//! # The stamp is a contract the consumer honours
//!
//! A replay stamps *when* a consumer may act. Acting on that stamp is the
//! consumer's job, because only the consumer knows what acting means. The
//! contract is that a decision derived from an observation may not touch the
//! book at any time before that observation's [`Observed::observed_at`].

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::fmt;

use crate::event::{EventKind, MarketEvent, Nanos, Venue};

/// Why a replay stopped.
///
/// There is one variant and that is deliberate. Everything else a replay meets,
/// an empty source, an exhausted source, no sources at all, is ordinary and ends
/// the stream normally. Only corrupt input is an error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    /// A source yielded an event earlier than one it already yielded.
    ///
    /// Sources are required to be sorted. One that is not has been misparsed or
    /// has had rows shuffled, and merging it produces a book that is wrong in a
    /// way no downstream check will notice.
    ///
    /// Sorted by *which* timestamp depends on which merge found the fault, and
    /// the two timestamps in `event` and `previous` are reported in whichever
    /// that is. [`Replay`] requires and reports `exchange_time`.
    /// [`DecisionReplay`] requires and reports `receive_time` shifted by the
    /// decision latency. A file can satisfy one and violate the other, which is
    /// the whole reason there are two merges.
    ///
    /// The field is `source_index` rather than the more natural `source`
    /// because `thiserror` treats a field literally named `source` as the
    /// underlying error in a chain, and demands it implement `std::error::Error`.
    /// A `usize` does not, so the name has to differ.
    #[error("source {source_index} yielded an event at {event} after one at {previous}")]
    SourceWentBackwards {
        source_index: usize,
        event: i64,
        previous: i64,
    },
}

/// One source and the last key it produced.
///
/// The `last` field is what makes the backwards check possible. Checking the
/// merged output instead would not work, because the merge itself guarantees
/// non-decreasing output and would therefore hide the very fault we want.
///
/// Which timestamp `last` holds is decided by the caller of [`Source::pull`],
/// not by this struct. Both merges need the identical bookkeeping over
/// different keys, so the key is a parameter and everything else is shared.
#[derive(Debug)]
struct Source<S> {
    iter: S,
    last: Nanos,
}

impl<S: Iterator<Item = MarketEvent>> Source<S> {
    fn new(iter: S) -> Self {
        Self {
            iter,
            // `i64::MIN` rather than zero, because epoch nanoseconds before
            // 1970 are negative and a zero sentinel would reject them.
            last: Nanos(i64::MIN),
        }
    }

    /// Read one event and key it with `key_of`, or report that this source went
    /// backwards on that key.
    ///
    /// `Ok(None)` means exhausted, which is how a finished source drops out of
    /// a merge without disturbing the others.
    ///
    /// `key_of` is taken as `impl Fn` rather than a boxed closure so the
    /// compiler generates one specialised copy per call site. A reader coming
    /// from Python can treat it as passing a function with no runtime cost for
    /// the indirection.
    fn pull(
        &mut self,
        index: usize,
        key_of: impl Fn(&MarketEvent) -> Nanos,
    ) -> Result<Option<Pending>, ReplayError> {
        // `let ... else` is the idiomatic early return for an `Option`. A
        // Python developer can read it as "unwrap or bail".
        let Some(event) = self.iter.next() else {
            return Ok(None);
        };

        let key = key_of(&event);
        if key < self.last {
            return Err(ReplayError::SourceWentBackwards {
                source_index: index,
                event: key.0,
                previous: self.last.0,
            });
        }
        self.last = key;

        Ok(Some(Pending {
            key,
            source: index,
            event,
        }))
    }
}

/// The head event of one source, waiting in the heap.
///
/// Ordering uses only `(key, source)` and never looks at the event, which is
/// why this type implements the comparison traits by hand rather than deriving
/// them. Deriving would drag `MarketEvent` into the comparison, and `EventKind`
/// is not `Ord` in the first place.
#[derive(Debug)]
struct Pending {
    /// The ordering key, whose meaning belongs to the merge that built it.
    /// `exchange_time` under [`Replay`], `receive_time` plus decision latency
    /// under [`DecisionReplay`].
    key: Nanos,
    source: usize,
    event: MarketEvent,
}

impl Pending {
    /// The full ordering key. `(key, source_index)`.
    fn ord_key(&self) -> (Nanos, usize) {
        (self.key, self.source)
    }
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.ord_key() == other.ord_key()
    }
}

impl Eq for Pending {}

impl Ord for Pending {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ord_key().cmp(&other.ord_key())
    }
}

impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A k-way merge over sorted event sources, emitting in non-decreasing
/// `exchange_time` order.
///
/// This is the reconstruction path and its consumer is
/// [`OrderBook::apply`](crate::book::OrderBook::apply), which requires venue
/// order. It is deliberately unable to stamp a decision clock. Anything that
/// decides takes an [`Observed`] from a [`DecisionReplay`] instead, for the
/// reason set out in the module docs.
///
/// # Tie-breaking
///
/// Runs of events sharing one nanosecond are normal rather than exceptional, so
/// the rule has to be stated rather than left to whatever the heap does.
///
/// **Ties break by source index ascending, and within a single source by the
/// order that source yielded them.**
///
/// Source index is the position in the list the caller passed to [`Replay::new`],
/// so the caller controls it and it does not vary between runs. Within-source
/// order is the source's own order, which the merge never reorders.
///
/// The consequence worth being explicit about is that this drains rather than
/// interleaves. Given three events at one nanosecond from source 0 and two from
/// source 1, all three of source 0's come out first. Across venues at an
/// identical nanosecond there is no true order to recover, so any rule is a
/// convention and this is the one that falls out of the ordering key. Within a
/// venue there *is* a true order, and that is the part the merge preserves.
///
/// A [`BinaryHeap`] is not a stable sort in general, and the usual fix is to
/// carry a monotonic sequence number in the key. That is unnecessary here for a
/// structural reason worth spelling out. The heap holds **at most one event per
/// source**, because a source is only read again after its previous event has
/// been popped. Source index is therefore unique across every entry in the heap,
/// which makes `(exchange_time, source_index)` a total order with no ties left
/// to break, and a sequence number would only restate what the index already
/// says. `sources_are_polled_one_event_at_a_time` pins that invariant.
///
/// # Type parameter
///
/// `S` is one concrete source type, so every source must be the same type. Real
/// merges are usually heterogeneous, a LOBSTER reader alongside an IEX reader,
/// and the way to do that is `Replay<Box<dyn Iterator<Item = MarketEvent>>>`,
/// since a boxed iterator is itself an iterator. Keeping the generic form means
/// the common single-type case pays no dynamic dispatch.
#[derive(Debug)]
pub struct Replay<S> {
    sources: Vec<Source<S>>,
    heap: BinaryHeap<Reverse<Pending>>,
}

impl<S: Iterator<Item = MarketEvent>> Replay<S> {
    /// Build a replay over `sources`, each of which must already be sorted by
    /// `exchange_time`.
    ///
    /// This reads the first event from each source immediately, which is what
    /// primes the heap. An empty source simply contributes nothing, and an empty
    /// list of sources produces a replay that ends at once.
    pub fn new(sources: impl IntoIterator<Item = S>) -> Self {
        let sources: Vec<Source<S>> = sources.into_iter().map(Source::new).collect();

        let mut replay = Self {
            heap: BinaryHeap::with_capacity(sources.len()),
            sources,
        };

        for index in 0..replay.sources.len() {
            // The first pull from a source cannot fail, because `last` starts at
            // the minimum representable timestamp and nothing precedes it. That
            // is why `new` can prime the heap without returning a `Result`.
            let primed = replay.pull(index);
            debug_assert!(primed.is_ok(), "the first pull from a source cannot fail");
        }

        replay
    }

    /// Read one event from source `index`, keyed on venue time, into the heap.
    ///
    /// This push never allocates in steady state. The heap was built with
    /// capacity for every source and holds at most one event per source, so its
    /// length never exceeds the capacity reserved once in `new`.
    fn pull(&mut self, index: usize) -> Result<(), ReplayError> {
        let pulled = self.sources[index].pull(index, |event| event.exchange_time)?;
        // The borrow of `self.sources` ends with the line above, which is what
        // lets `self.heap` be touched here. Rust ends a borrow at its last use
        // rather than at the end of the block, so no explicit scope is needed.
        if let Some(pending) = pulled {
            self.heap.push(Reverse(pending));
        }
        Ok(())
    }
}

impl<S: Iterator<Item = MarketEvent>> Iterator for Replay<S> {
    /// Yielding a `Result` rather than panicking or skipping means a corrupt
    /// source is a decision the caller makes, not one this module makes for it.
    type Item = Result<MarketEvent, ReplayError>;

    fn next(&mut self) -> Option<Self::Item> {
        // `?` on an `Option` returns `None` from the function, which is exactly
        // what an empty heap means. Either every source is exhausted, or a
        // fault emptied the heap on purpose.
        let Reverse(pending) = self.heap.pop()?;

        if let Err(error) = self.pull(pending.source) {
            // Halting is the default response to uncertainty, and that includes
            // the event just popped. It is individually well ordered, but it
            // came out of a stream now known to be corrupt, so emitting it
            // first would hand the caller one more row of a run that cannot be
            // trusted. `pending` is dropped here rather than returned.
            //
            // Clearing the heap is what makes every call after the error return
            // `None`. It also removes the need for a separate "have we failed"
            // flag, since an empty heap already says so.
            self.heap.clear();
            return Some(Err(error));
        }

        Some(Ok(pending.event))
    }
}

/// A k-way merge over sorted event sources, emitting in non-decreasing
/// **decision time** order, which is `receive_time` plus the configured
/// decision latency.
///
/// This is the strategy path. It is the only way to obtain an [`Observed`], and
/// the only guarantee worth stating is this one.
///
/// **The sequence of [`Observed::observed_at`] it yields never decreases.**
///
/// # Why the key is not `exchange_time`
///
/// Because knowing something and it having happened are different events at
/// different times, and merging on the second and reporting the first produces
/// lookahead. The module docs carry the worked counterexample.
///
/// # Sorted means sorted by receive time here
///
/// A k-way merge only emits in key order when every source is already in key
/// order, so each source must be non-decreasing in `receive_time`. That is a
/// different requirement from the one [`Replay`] makes, and a file can satisfy
/// one and violate the other. A source that breaks it stops the replay with
/// [`ReplayError::SourceWentBackwards`], which is what turns the guarantee
/// above into something enforced rather than something hoped for.
///
/// # Tie-breaking
///
/// The same documented rule [`Replay`] uses. **Ties break by source index
/// ascending, and within a single source by the order that source yielded
/// them.** Source index is the position in the list passed to
/// [`DecisionReplay::new`], so the caller controls it and it cannot vary
/// between runs. The heap holds at most one event per source, so
/// `(decision_time, source_index)` has no ties left to break.
///
/// # Why this is not an `Iterator`
///
/// [`Observed`] borrows the event it views, so `next_observation` returns a
/// value that borrows the replay. Rust's `Iterator` cannot express that, since
/// `Item` is one fixed type that cannot mention the lifetime of the `&mut self`
/// each call receives. The usual name for the missing feature is a lending
/// iterator.
///
/// The restriction is worth more than the convenience it costs. One
/// observation is alive at a time, so an `Observed` cannot be collected into a
/// `Vec` and compared against one from further down the stream, which is
/// another route to acting on information out of order.
#[derive(Debug)]
pub struct DecisionReplay<S> {
    sources: Vec<Source<S>>,
    heap: BinaryHeap<Reverse<Pending>>,
    decision_latency: i64,
    /// The event currently on loan to the caller as an [`Observed`]. Held here
    /// because an `Observed` borrows rather than copies, so the event has to
    /// outlive the call that produced it.
    current: Option<MarketEvent>,
}

impl<S: Iterator<Item = MarketEvent>> DecisionReplay<S> {
    /// Build a decision replay over `sources`, each of which must already be
    /// sorted by `receive_time`, delaying every observation by
    /// `decision_latency` nanoseconds.
    ///
    /// The latency is a constructor argument rather than a builder method
    /// because the merge key depends on it. Setting it after the heap was
    /// primed would leave already-keyed events in the heap under the old key,
    /// and a zero default would quietly model a machine that decides
    /// instantly. Pass `0` to say so on purpose.
    ///
    /// Taking `u64` rather than `i64` refuses a negative latency at the type
    /// level. A negative decision latency is acting before the event arrives,
    /// which is the exact fiction this type exists to prevent.
    pub fn new(sources: impl IntoIterator<Item = S>, decision_latency: u64) -> Self {
        let sources: Vec<Source<S>> = sources.into_iter().map(Source::new).collect();

        let mut replay = Self {
            heap: BinaryHeap::with_capacity(sources.len()),
            sources,
            // Saturate rather than panic or wrap. A caller passing a latency
            // beyond 292 years has a bug, and the saturated value makes every
            // event unactionable, which is the loudest harmless failure
            // available.
            decision_latency: i64::try_from(decision_latency).unwrap_or(i64::MAX),
            current: None,
        };

        for index in 0..replay.sources.len() {
            let primed = replay.pull(index);
            debug_assert!(primed.is_ok(), "the first pull from a source cannot fail");
        }

        replay
    }

    /// Read one event from source `index`, keyed on decision time, into the
    /// heap.
    fn pull(&mut self, index: usize) -> Result<(), ReplayError> {
        let latency = self.decision_latency;
        // Saturating rather than checked, and it is worth being precise about
        // what that costs. `saturating_add` is non-decreasing, so it cannot
        // turn an ordered pair into a reversed one and the merge stays correct.
        // The one thing it can do is collapse two distinct receive times onto
        // `i64::MAX`, which turns a genuine backwards step into a tie and hides
        // it. Reaching that needs a latency of roughly 238 years, at which
        // point every event is unactionable anyway.
        let pulled = self.sources[index].pull(index, |event| {
            Nanos(event.receive_time.0.saturating_add(latency))
        })?;
        if let Some(pending) = pulled {
            self.heap.push(Reverse(pending));
        }
        Ok(())
    }

    /// The next observation, or `None` when the stream has ended.
    ///
    /// Named rather than being `Iterator::next` because this type cannot be an
    /// `Iterator`, for the reason given on the type. The name also warns a
    /// reader not to expect `map` and `collect` to be available.
    ///
    /// The returned [`Observed`] borrows this replay, so it must be dropped
    /// before the next call. That is the compiler enforcing one decision at a
    /// time rather than a style preference.
    pub fn next_observation(&mut self) -> Option<Result<Observed<'_>, ReplayError>> {
        let Reverse(pending) = self.heap.pop()?;

        if let Err(error) = self.pull(pending.source) {
            // Fail stop before emit, exactly as in `Replay`. See the comment
            // there for why the already-popped event goes in the bin.
            self.heap.clear();
            return Some(Err(error));
        }

        // `MarketEvent` is `Copy`, so this is a fresh value rather than a
        // mutation of anything the source still holds. Building a new event
        // instead of editing one in place is the project's default.
        //
        // The same saturating add the merge key used, so `observed_at` and the
        // key that ordered this event are the same number by construction.
        // That equality is what makes the non-decreasing guarantee follow from
        // the merge rather than needing its own argument.
        let mut event = pending.event;
        event.receive_time = Nanos(event.receive_time.0.saturating_add(self.decision_latency));

        // `Option::insert` stores the event and hands back a reference to it in
        // one step, which is what lets the borrow in the return value point at
        // something this struct owns.
        Some(Ok(Observed::of(self.current.insert(event))))
    }
}

/// The only view of an event a decision-maker is permitted.
///
/// # The bug this type prevents
///
/// A [`MarketEvent`] carries two timestamps. `exchange_time` is when the venue
/// says something happened, and `receive_time` is when this process could
/// actually have known about it. A strategy that reads `exchange_time` and
/// treats it as the current moment is acting on information it did not have
/// yet, and nothing about that mistake looks wrong from the outside. It
/// compiles, it runs, no test fails, and it produces backtest numbers that are
/// plausible and fictional.
///
/// `Observed` removes the mistake rather than warning about it. There is no
/// accessor for `exchange_time` and there is not going to be one, so lookahead
/// stops being something a reviewer has to notice and becomes something the
/// compiler rejects.
///
/// ```compile_fail
/// use marketdata::replay::Observed;
///
/// fn decide(event: Observed<'_>) {
///     // No such method. That absence is the entire purpose of the type.
///     let _now = event.exchange_time();
/// }
/// ```
///
/// The wrapped event is a private field for the same reason, so reaching past
/// the accessors does not compile either.
///
/// ```compile_fail
/// use marketdata::replay::Observed;
///
/// fn decide(event: Observed<'_>) {
///     let _now = event.event.exchange_time;
/// }
/// ```
///
/// # There is one way to get one
///
/// [`DecisionReplay::next_observation`] and nothing else. The constructor is
/// private, so a loose [`MarketEvent`] cannot be wrapped at a call site.
///
/// ```compile_fail
/// use marketdata::replay::Observed;
/// use marketdata::{EventKind, MarketEvent, Nanos, TradingStatus, Venue};
///
/// let event = MarketEvent {
///     exchange_time: Nanos(1),
///     receive_time: Nanos(2),
///     venue: Venue::Synthetic,
///     kind: EventKind::Status(TradingStatus::Trading),
/// };
/// // Private. Wrapping one event says nothing about the order of the rest.
/// let _observed = Observed::of(&event);
/// ```
///
/// That restriction is what upgrades the type from a convention to a
/// guarantee. Hiding `exchange_time` on a single event stops one reader
/// misreading one timestamp. It does nothing about a *sequence* of events
/// handed over in the wrong order, where every individual `observed_at` is
/// truthful and the run as a whole still contains lookahead. Only the merge
/// that produced the sequence can rule that out, so only that merge is allowed
/// to mint the type.
///
/// # The split is by consumer, not by stream
///
/// [`Replay`] goes on yielding whole [`MarketEvent`]s and that is deliberate.
/// [`OrderBook::apply`](crate::book::OrderBook::apply) legitimately needs
/// `exchange_time`, because book state is venue truth and rebuilding it in
/// receive order would produce a book the venue never had. So reconstruction
/// takes the whole event from a [`Replay`], and anything that decides takes an
/// `Observed` from a [`DecisionReplay`].
///
/// # Reading the lifetime
///
/// `Observed<'a>` borrows the event instead of copying it, even though
/// [`MarketEvent`] is `Copy` and copying it costs almost nothing. The borrow
/// buys something the copy would not. The `'a` parameter ties the view to the
/// event the caller is handling right now, so an `Observed` cannot outlive that
/// event, cannot be stashed in a collection that survives the loop, and cannot
/// be held back to be compared against an observation from further along the
/// stream. A reader new to Rust can take `'a` as a compiler-checked note saying
/// this value is on loan for the length of one decision.
///
/// # The rule this type only half enforces, and what the other half is
///
/// This makes venue time unreadable for anyone holding an `Observed`. It cannot
/// make anyone hold one. `Replay` still yields a raw [`MarketEvent`] carrying
/// both timestamps, because reconstruction genuinely needs venue ordering and
/// [`crate::book::OrderBook`] rejects events whose exchange time goes backwards.
///
/// So the barrier is opt in, and an opt in barrier protects only the person who
/// already knows about the hazard. The other half is a convention that future
/// code has to keep:
///
/// **Every strategy-facing signature takes `Observed`, never `MarketEvent`.**
///
/// That is the whole rule. A decision function accepting a `MarketEvent`
/// compiles, runs, produces plausible numbers, and is lookahead, which is the
/// failure this type exists to prevent and the one it cannot prevent by itself.
/// The execution layer is the first place this will be tested.
///
/// # Known ceiling
///
/// Decision latency is one constant applied to every event, so this models a
/// machine whose think time never varies. Real decision latency is
/// heavy-tailed, and the tail is the part that costs money, which is exactly
/// the point [`LatencySummary`] makes about refusing to report a mean.
///
/// Making it vary is a change to the key `DecisionReplay` merges on and
/// nothing else, because the merge is already keyed on the number a strategy
/// acts at rather than on venue time plus an annotation. The reordering that a
/// per-event latency causes is therefore handled by the merge that is already
/// there. It is not built because nothing in this system has measured a
/// decision latency distribution yet, and a model fitted to a distribution
/// nobody has measured is scaffolding rather than instrumentation.
pub struct Observed<'a> {
    event: &'a MarketEvent,
}

impl<'a> Observed<'a> {
    /// Wrap an event as the view a decision-maker is allowed to hold.
    ///
    /// Private, and that is the load-bearing part of this type. A public
    /// constructor would let any caller wrap any [`MarketEvent`] in any order,
    /// which reduces `Observed` to a naming convention. Only
    /// [`DecisionReplay::next_observation`] calls this, because only it knows
    /// the sequence is in decision order.
    fn of(event: &'a MarketEvent) -> Self {
        Self { event }
    }

    /// The only clock a strategy has.
    ///
    /// Already carries the decision latency the [`DecisionReplay`] was built
    /// with. A decision derived from this observation may act at or after this
    /// instant and at no time before it.
    ///
    /// Successive calls across one replay never go backwards. That is the
    /// guarantee stated on [`DecisionReplay`].
    pub fn observed_at(&self) -> Nanos {
        self.event.receive_time
    }

    pub fn kind(&self) -> &EventKind {
        &self.event.kind
    }

    pub fn venue(&self) -> Venue {
        self.event.venue
    }
}

/// Written out rather than derived, because a derived `Debug` would print the
/// wrapped [`MarketEvent`] whole, `exchange_time` included. A type whose claim
/// is that venue time is unreachable should not hand it back through a
/// formatter.
impl fmt::Debug for Observed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Observed")
            .field("observed_at", &self.observed_at())
            .field("venue", &self.venue())
            .field("kind", self.kind())
            .finish()
    }
}

/// A recorder for latency samples, in nanoseconds.
///
/// Kept separate from [`Replay`] rather than folded into it, for two reasons.
/// The merge then allocates nothing per event, since collecting samples is the
/// only part that grows. And a caller who wants latency of something other than
/// feed transit, decision time measured live for instance, can feed the same
/// recorder without the replay needing to know.
#[derive(Debug, Clone, Default)]
pub struct LatencyStats {
    samples: Vec<i64>,
}

impl LatencyStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-allocate for `capacity` samples. Worth doing for a full session,
    /// where the sample count is known from the row count in advance.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
        }
    }

    pub fn record(&mut self, nanos: i64) {
        self.samples.push(nanos);
    }

    /// Record this event's feed transit, the gap between `exchange_time` and
    /// `receive_time`.
    ///
    /// Events from a [`Replay`] measure transit alone, because `Replay` passes
    /// `receive_time` through untouched. A [`DecisionReplay`] adds its decision
    /// latency to `receive_time`, so anything sampled downstream of one would
    /// read as transit plus that latency. It hands out [`Observed`] rather than
    /// [`MarketEvent`], so this method cannot be pointed at one by accident.
    pub fn record_event(&mut self, event: &MarketEvent) {
        self.record(event.transit_nanos());
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The `p`th percentile by the nearest-rank method, or `None` when nothing
    /// has been recorded. `p` above 100 is clamped to 100.
    ///
    /// Nearest rank means the result is always a value that actually occurred.
    /// The interpolating alternative would invent a latency nobody measured,
    /// and a made-up tail number is worse than a slightly coarse real one.
    ///
    /// Takes `&mut self` because it sorts in place. Repeated calls are cheap,
    /// since sorting an already-sorted slice is close to linear.
    pub fn percentile(&mut self, p: u32) -> Option<i64> {
        if self.samples.is_empty() {
            return None;
        }
        self.samples.sort_unstable();

        let p = u64::from(p.min(100));
        let n = self.samples.len() as u64;
        // Nearest rank is ceil(p * n / 100), 1-indexed, floored at 1 so that
        // p = 0 returns the minimum rather than falling off the front.
        let rank = p.saturating_mul(n).div_ceil(100).max(1);
        let index = (rank - 1) as usize;
        self.samples.get(index).copied()
    }

    /// The distribution, or `None` when nothing has been recorded.
    pub fn summary(&mut self) -> Option<LatencySummary> {
        Some(LatencySummary {
            count: self.len(),
            min: self.percentile(0)?,
            p50: self.percentile(50)?,
            p95: self.percentile(95)?,
            p99: self.percentile(99)?,
            max: self.percentile(100)?,
        })
    }
}

/// A latency distribution, in nanoseconds.
///
/// There is no mean here and there will not be one. Latency is heavy-tailed,
/// the tail is what breaks a strategy, and a mean is exactly the statistic that
/// hides it. A run whose median is 40 microseconds and whose p99 is 9
/// milliseconds is a run where one trade in a hundred happens somewhere else
/// entirely, and its mean would read as comfortable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencySummary {
    pub count: usize,
    pub min: i64,
    pub p50: i64,
    pub p95: i64,
    pub p99: i64,
    pub max: i64,
}

impl fmt::Display for LatencySummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "n={} min={}ns p50={}ns p95={}ns p99={}ns max={}ns",
            self.count, self.min, self.p50, self.p95, self.p99, self.max
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::{Granularity, OrderBook};
    use crate::event::{EventKind, LevelUpdate, Price, Qty, Side, TradingStatus, Venue};

    /// A trade print stamped `exchange` at the venue and `receive` on arrival.
    /// The kind is irrelevant to ordering, so the price doubles as a label
    /// letting a test say which event it got.
    fn split_event(exchange: i64, receive: i64, label: i64) -> MarketEvent {
        MarketEvent {
            exchange_time: Nanos(exchange),
            receive_time: Nanos(receive),
            venue: Venue::Synthetic,
            kind: EventKind::Trade {
                price: Price(label),
                size: Qty(1),
                aggressor: None,
            },
        }
    }

    /// A trade print that reached this process the instant it happened, which
    /// is the shape most ordering tests want because it makes the two orders
    /// agree and isolates whatever else the test is checking.
    fn event_at(nanos: i64, label: i64) -> MarketEvent {
        split_event(nanos, nanos, label)
    }

    /// The label a test attached with `event_at`.
    fn label_of_kind(kind: &EventKind) -> i64 {
        match kind {
            EventKind::Trade { price, .. } => price.0,
            _ => panic!("not a labelled trade"),
        }
    }

    fn label_of(event: &MarketEvent) -> i64 {
        label_of_kind(&event.kind)
    }

    fn source(events: &[(i64, i64)]) -> std::vec::IntoIter<MarketEvent> {
        events
            .iter()
            .map(|&(nanos, label)| event_at(nanos, label))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// A source of `(exchange_time, receive_time, label)` triples, for the
    /// tests where the two clocks deliberately disagree.
    fn split_source(events: &[(i64, i64, i64)]) -> std::vec::IntoIter<MarketEvent> {
        events
            .iter()
            .map(|&(exchange, receive, label)| split_event(exchange, receive, label))
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn collect(replay: Replay<std::vec::IntoIter<MarketEvent>>) -> Vec<MarketEvent> {
        replay
            .map(|result| result.expect("replay failed"))
            .collect()
    }

    /// Drain a replay keeping the outcomes, for tests that expect a fault.
    fn collect_results(
        replay: Replay<std::vec::IntoIter<MarketEvent>>,
    ) -> Vec<Result<MarketEvent, ReplayError>> {
        replay.collect()
    }

    /// Drain a decision replay into `(observed_at, label)` pairs.
    ///
    /// Written as a loop rather than an iterator chain because
    /// `DecisionReplay` is not an `Iterator`, and the loop is what a caller has
    /// to write too. Each `Observed` is read and dropped inside one iteration,
    /// which is the borrow the type enforces.
    fn observe(replay: &mut DecisionReplay<std::vec::IntoIter<MarketEvent>>) -> Vec<(i64, i64)> {
        let mut seen = Vec::new();
        while let Some(outcome) = replay.next_observation() {
            let observation = outcome.expect("decision replay failed");
            seen.push((
                observation.observed_at().0,
                label_of_kind(observation.kind()),
            ));
        }
        seen
    }

    #[test]
    fn events_from_several_sources_come_out_in_timestamp_order() {
        let replay = Replay::new([
            source(&[(10, 1), (40, 4), (70, 7)]),
            source(&[(20, 2), (50, 5)]),
            source(&[(30, 3), (60, 6), (80, 8)]),
        ]);

        let times: Vec<i64> = collect(replay)
            .iter()
            .map(|event| event.exchange_time.0)
            .collect();
        assert_eq!(times, vec![10, 20, 30, 40, 50, 60, 70, 80]);
    }

    #[test]
    fn interleaved_runs_stay_in_order_when_one_source_dominates() {
        // A realistic shape. One venue is far busier than the other, so the
        // merge spends long stretches drawing from a single source.
        let replay = Replay::new([
            source(&[(1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 0)]),
            source(&[(4, 9)]),
        ]);

        let times: Vec<i64> = collect(replay)
            .iter()
            .map(|event| event.exchange_time.0)
            .collect();
        assert_eq!(times, vec![1, 2, 3, 4, 4, 5, 6]);
    }

    #[test]
    fn ties_break_by_source_index_then_by_within_source_order() {
        // Every event shares one nanosecond, which is what a LOBSTER run of
        // identical stamps looks like. Labels encode source and position, so
        // the expected sequence is fully determined by the documented rule.
        //
        // Note this drains rather than interleaving. Source 0 gives up both of
        // its events before source 1 is read at all, because source index is
        // the whole tie-break once timestamps are equal.
        let replay = Replay::new([
            source(&[(100, 11), (100, 12)]),
            source(&[(100, 21), (100, 22)]),
            source(&[(100, 31)]),
        ]);

        let labels: Vec<i64> = collect(replay).iter().map(label_of).collect();
        assert_eq!(labels, vec![11, 12, 21, 22, 31]);
    }

    #[test]
    fn sources_are_polled_one_event_at_a_time() {
        // The invariant that makes source index a sufficient tie-break. If the
        // heap ever held two events from one source, the ordering key would no
        // longer be unique and ties would fall back to heap internals.
        let mut replay = Replay::new([source(&[(1, 1), (2, 2), (3, 3)]), source(&[(1, 4)])]);
        assert_eq!(replay.heap.len(), 2, "one event per source, not more");

        replay.next();
        assert_eq!(replay.heap.len(), 2, "popping one refills exactly one");
    }

    #[test]
    fn the_same_input_replays_identically_twice() {
        let build = || {
            Replay::new([
                source(&[(5, 1), (5, 2), (9, 3)]),
                source(&[(5, 4), (7, 5)]),
                source(&[(5, 6)]),
            ])
        };

        let first: Vec<i64> = collect(build()).iter().map(label_of).collect();
        let second: Vec<i64> = collect(build()).iter().map(label_of).collect();
        assert_eq!(first, second);
        // Both t=5 events from source 0, then source 1's, then source 2's,
        // then t=7 and t=9 in time order.
        assert_eq!(first, vec![1, 2, 4, 6, 5, 3]);
    }

    #[test]
    fn an_empty_source_contributes_nothing() {
        let replay = Replay::new([source(&[]), source(&[(1, 1), (2, 2)]), source(&[])]);

        let labels: Vec<i64> = collect(replay).iter().map(label_of).collect();
        assert_eq!(labels, vec![1, 2]);
    }

    #[test]
    fn an_exhausted_source_does_not_stall_the_others() {
        // Source 0 runs dry early. The merge must keep draining source 1 rather
        // than stopping at the first source that ends.
        let replay = Replay::new([source(&[(1, 1)]), source(&[(2, 2), (3, 3), (4, 4)])]);

        let labels: Vec<i64> = collect(replay).iter().map(label_of).collect();
        assert_eq!(labels, vec![1, 2, 3, 4]);
    }

    #[test]
    fn no_sources_at_all_yields_nothing() {
        let replay: Replay<std::vec::IntoIter<MarketEvent>> = Replay::new([]);
        assert_eq!(collect(replay).len(), 0);
    }

    #[test]
    fn a_source_that_goes_backwards_is_surfaced_as_an_error() {
        let mut replay = Replay::new([source(&[(10, 1), (5, 2), (20, 3)])]);

        // `new` primed the heap with the event at 10, so the first call pops
        // it and then reads the event at 5, which is where the fault is found.
        // The error comes out at once and the event at 10 does not.
        assert_eq!(
            replay.next().unwrap().unwrap_err(),
            ReplayError::SourceWentBackwards {
                source_index: 0,
                event: 5,
                previous: 10,
            }
        );
    }

    #[test]
    fn a_backwards_source_emits_nothing_at_all() {
        // Fail stop before emit. The event at 10 was individually well
        // ordered and is still discarded, because it belongs to a stream now
        // known to be corrupt, and CLAUDE.md is explicit that halting is the
        // default response to uncertainty.
        //
        // This also covers the failure mode the check exists to prevent. A
        // sort would have produced 5, 10, 20 and looked perfectly healthy.
        let replay = Replay::new([source(&[(10, 1), (5, 2), (20, 3)])]);
        let outcomes: Vec<_> = replay.collect();

        assert!(outcomes.iter().any(|outcome| outcome.is_err()));
        let emitted: Vec<i64> = outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().ok())
            .map(|event| event.exchange_time.0)
            .collect();
        assert!(emitted.is_empty(), "no event may precede the error");
    }

    #[test]
    fn a_decision_replay_with_a_backwards_source_emits_nothing_either() {
        // The same fail-stop rule on the decision path. Receive times go
        // backwards within one source, which is the ordering `DecisionReplay`
        // requires, so the merge cannot honour its guarantee and stops.
        let mut replay = DecisionReplay::new(
            [split_source(&[(10, 100, 1), (20, 50, 2), (30, 300, 3)])],
            0,
        );

        assert_eq!(
            replay
                .next_observation()
                .unwrap()
                .expect_err("a backwards receive_time must be an error"),
            ReplayError::SourceWentBackwards {
                source_index: 0,
                event: 50,
                previous: 100,
            }
        );
        assert!(replay.next_observation().is_none());
    }

    #[test]
    fn a_receive_ordered_source_may_be_venue_disordered_and_the_reverse() {
        // The two merges make different demands, and this pins that they are
        // genuinely different rather than one implying the other.
        //
        // Venue times ascend, receive times do not.
        let events = [(10, 100, 1), (20, 50, 2)];
        assert!(
            collect_results(Replay::new([split_source(&events)]))
                .iter()
                .all(|outcome| outcome.is_ok())
        );
        let mut decision = DecisionReplay::new([split_source(&events)], 0);
        assert!(decision.next_observation().unwrap().is_err());

        // Receive times ascend, venue times do not.
        let events = [(20, 50, 1), (10, 100, 2)];
        assert!(
            collect_results(Replay::new([split_source(&events)]))
                .iter()
                .any(|outcome| outcome.is_err())
        );
        let mut decision = DecisionReplay::new([split_source(&events)], 0);
        assert_eq!(observe(&mut decision), vec![(50, 1), (100, 2)]);
    }

    #[test]
    fn a_failed_replay_stops_rather_than_continuing() {
        // Halting is the default response to uncertainty. Once a source is
        // known corrupt, further events from any source are not trustworthy,
        // including the two source 1 still has queued.
        let mut replay = Replay::new([source(&[(10, 1), (5, 2)]), source(&[(11, 3), (12, 4)])]);

        assert!(replay.next().unwrap().is_err());
        assert!(replay.next().is_none());
        assert!(replay.next().is_none());
    }

    #[test]
    fn a_source_repeating_a_timestamp_is_fine() {
        // Equal is not backwards. Venues stamp bursts identically all the time
        // and rejecting that would reject most real files.
        let replay = Replay::new([source(&[(10, 1), (10, 2), (10, 3)])]);
        let labels: Vec<i64> = collect(replay).iter().map(label_of).collect();
        assert_eq!(labels, vec![1, 2, 3]);
    }

    #[test]
    fn negative_timestamps_are_not_treated_as_backwards() {
        // Epoch nanoseconds before 1970 are negative. A zero sentinel for the
        // per-source watermark would have rejected the first of these.
        let replay = Replay::new([source(&[(-100, 1), (-50, 2), (0, 3)])]);
        let labels: Vec<i64> = collect(replay).iter().map(label_of).collect();
        assert_eq!(labels, vec![1, 2, 3]);
    }

    #[test]
    fn a_replay_leaves_receive_time_alone() {
        // `Replay` is the reconstruction path and stamps no decision clock, so
        // both timestamps pass through exactly as the source wrote them.
        let replay = Replay::new([split_source(&[(1_000, 1_800, 1)])]);
        let events = collect(replay);
        assert_eq!(events[0].exchange_time, Nanos(1_000));
        assert_eq!(events[0].receive_time, Nanos(1_800));
        assert_eq!(events[0].transit_nanos(), 800);
    }

    #[test]
    fn decision_latency_shifts_observations_by_exactly_that_amount() {
        const FIFTY_MICROS: u64 = 50_000;

        let events = [(1_000, 1_200, 1), (2_000, 2_900, 2)];
        let mut without = DecisionReplay::new([split_source(&events)], 0);
        let mut with = DecisionReplay::new([split_source(&events)], FIFTY_MICROS);

        assert_eq!(observe(&mut without), vec![(1_200, 1), (2_900, 2)]);
        assert_eq!(
            observe(&mut with),
            vec![(1_200 + 50_000, 1), (2_900 + 50_000, 2)],
            "every observation moves by the latency and by nothing else"
        );
    }

    #[test]
    fn decision_latency_stacks_on_transit_a_source_already_carried() {
        // A live capture arrives with real transit on it. The decision latency
        // adds to that rather than replacing it, so the observation lands 10
        // microseconds after the venue stamped the event.
        let mut replay = DecisionReplay::new([split_source(&[(1_000, 1_000 + 8_000, 1)])], 2_000);
        assert_eq!(observe(&mut replay), vec![(1_000 + 10_000, 1)]);
    }

    #[test]
    fn decision_latency_does_not_reorder_a_decision_merge() {
        // A uniform shift cannot reorder a stream that was already keyed on the
        // thing being shifted. This was also true of the old venue-keyed merge,
        // and was exactly the true-but-irrelevant fact that hid the bug, since
        // the shift was applied to a key the merge had not used.
        let ordered = |latency: u64| -> Vec<i64> {
            let mut replay = DecisionReplay::new(
                [
                    split_source(&[(10, 10, 1), (30, 30, 3)]),
                    split_source(&[(20, 20, 2), (40, 40, 4)]),
                ],
                latency,
            );
            observe(&mut replay)
                .iter()
                .map(|&(_, label)| label)
                .collect()
        };
        assert_eq!(ordered(0), ordered(1_000_000));
        assert_eq!(ordered(0), vec![1, 2, 3, 4]);
    }

    #[test]
    fn an_absurd_latency_saturates_rather_than_overflowing() {
        let mut replay = DecisionReplay::new([split_source(&[(1, i64::MAX - 1, 1)])], u64::MAX);
        assert_eq!(observe(&mut replay), vec![(i64::MAX, 1)]);
    }

    #[test]
    fn merged_events_apply_to_a_book_without_going_out_of_order() {
        // The point of the ordering guarantee. `OrderBook::apply` rejects an
        // event earlier than the last it saw, so a merge that got ordering
        // wrong would surface here as a `BookError::OutOfOrder`.
        let level = |nanos: i64, price: i64, size: u32| MarketEvent {
            exchange_time: Nanos(nanos),
            receive_time: Nanos(nanos),
            venue: Venue::Synthetic,
            kind: EventKind::Level(LevelUpdate {
                side: Side::Bid,
                price: Price(price),
                size: Qty(size),
            }),
        };

        let replay = Replay::new([
            vec![level(30, 100, 10), level(10, 101, 20)].into_iter(),
            vec![level(20, 102, 30)].into_iter(),
        ]);

        // Source 0 is deliberately backwards, so this run must fail at the
        // replay rather than reaching the book with a plausible-looking order.
        let outcomes: Vec<_> = replay.collect();
        assert!(outcomes.iter().any(|outcome| outcome.is_err()));

        // The same events, correctly sorted within each source, feed the book
        // cleanly.
        let replay = Replay::new([
            vec![level(10, 101, 20), level(30, 100, 10)].into_iter(),
            vec![level(20, 102, 30)].into_iter(),
        ]);
        let mut book = OrderBook::new(Granularity::PriceLevel);
        for outcome in replay {
            let event = outcome.expect("replay failed");
            book.apply(&event).expect("book rejected an ordered event");
        }
    }

    /// A consumer that can reach nothing but what it observed.
    ///
    /// A named type rather than a closure in the test body, because the thing
    /// under test is what a decision-maker can reach, and a closure over the
    /// enclosing scope could reach the events themselves and prove nothing.
    #[derive(Default)]
    struct Decider {
        observations: Vec<Nanos>,
    }

    impl Decider {
        fn observe(&mut self, observation: Observed<'_>) {
            // The consumer's clock is the last thing it observed. An
            // observation arriving before that clock would be handing a
            // decision information out of order, which is the failure this
            // whole barrier exists to make impossible.
            if let Some(&clock) = self.observations.last() {
                assert!(
                    observation.observed_at() >= clock,
                    "observed {:?} while the clock read {:?}",
                    observation.observed_at(),
                    clock
                );
            }
            self.observations.push(observation.observed_at());
        }
    }

    /// Feed a decision replay to a `Decider`, which asserts per observation
    /// that its clock never runs backwards.
    fn drive(replay: &mut DecisionReplay<std::vec::IntoIter<MarketEvent>>, decider: &mut Decider) {
        while let Some(outcome) = replay.next_observation() {
            decider.observe(outcome.expect("decision replay failed"));
        }
    }

    #[test]
    fn an_event_that_happened_early_and_arrived_late_cannot_reach_a_decision_first() {
        // The counterexample from the module docs, and the regression this
        // whole split exists for. Against a venue-keyed merge with a latency
        // annotation, A is delivered first and the decider's clock reads 1000
        // before it is handed B at 20.
        let mut replay = DecisionReplay::new(
            [
                split_source(&[(10, 1_000, 1)]), // happened early, arrived late
                split_source(&[(20, 20, 2)]),    // happened later, arrived at once
            ],
            0,
        );

        let mut decider = Decider::default();
        drive(&mut replay, &mut decider);

        // Knowledge order, not venue order. B is observable first because it
        // is the one that had actually arrived.
        assert_eq!(decider.observations, vec![Nanos(20), Nanos(1_000)]);
        // `is_sorted` is the standard library check for non-decreasing and
        // needs no comparator on an `Ord` type.
        assert!(decider.observations.is_sorted());
    }

    #[test]
    fn a_decision_cannot_see_information_it_has_not_received_yet() {
        // Transit varies per event and per source, which is the realistic case
        // and the one where venue order and receive order disagree repeatedly.
        const LATENCY: u64 = 40_000;

        let mut replay = DecisionReplay::new(
            [
                split_source(&[(10, 900, 1), (40, 950, 4), (70, 1_400, 7)]),
                split_source(&[(20, 25, 2), (50, 1_100, 5), (60, 1_200, 6)]),
            ],
            LATENCY,
        );

        let mut decider = Decider::default();
        drive(&mut replay, &mut decider);

        let shifted = |receive: i64| Nanos(receive + i64::try_from(LATENCY).unwrap());
        assert_eq!(
            decider.observations,
            vec![
                shifted(25),
                shifted(900),
                shifted(950),
                shifted(1_100),
                shifted(1_200),
                shifted(1_400),
            ]
        );
        assert!(decider.observations.is_sorted());

        // The per-observation assertion inside `observe` only means anything if
        // it actually ran, once for every event the replay held.
        assert_eq!(decider.observations.len(), 6);
    }

    #[test]
    fn an_observation_carries_the_rest_of_the_event_untouched() {
        let event = MarketEvent {
            exchange_time: Nanos(1_000),
            receive_time: Nanos(1_900),
            venue: Venue::Nasdaq,
            kind: EventKind::Status(TradingStatus::Halted),
        };

        let mut replay = DecisionReplay::new([vec![event].into_iter()], 0);
        let observed = replay
            .next_observation()
            .expect("one event")
            .expect("decision replay failed");

        assert_eq!(observed.observed_at(), Nanos(1_900));
        assert_eq!(observed.venue(), Venue::Nasdaq);
        assert!(matches!(
            observed.kind(),
            EventKind::Status(TradingStatus::Halted)
        ));
        // The formatter is hand-written so that venue time cannot leak through
        // it. Pinning that here means a later `#[derive(Debug)]` would fail.
        assert!(!format!("{observed:?}").contains("1000"));
    }

    #[test]
    fn decision_ties_break_by_source_index_ascending() {
        // The documented rule, on the decision key this time. Every event
        // shares one receive time, so source index is the whole tie-break and
        // source 0 drains before source 1 is read at all.
        let mut replay = DecisionReplay::new(
            [
                split_source(&[(5, 100, 11), (6, 100, 12)]),
                split_source(&[(1, 100, 21), (2, 100, 22)]),
                split_source(&[(9, 100, 31)]),
            ],
            0,
        );

        let labels: Vec<i64> = observe(&mut replay)
            .iter()
            .map(|&(_, label)| label)
            .collect();
        assert_eq!(labels, vec![11, 12, 21, 22, 31]);
    }

    #[test]
    fn the_same_input_replays_identically_through_a_decision_replay() {
        let build = || {
            DecisionReplay::new(
                [
                    split_source(&[(50, 5, 1), (10, 5, 2), (1, 9, 3)]),
                    split_source(&[(90, 5, 4), (2, 7, 5)]),
                    split_source(&[(70, 5, 6)]),
                ],
                7_500,
            )
        };

        let first = observe(&mut build());
        let second = observe(&mut build());
        assert_eq!(first, second);
        // Both receive-time-5 events from source 0, then source 1's, then
        // source 2's, then 7 and 9 in order, each shifted by the latency.
        assert_eq!(
            first,
            vec![
                (7_505, 1),
                (7_505, 2),
                (7_505, 4),
                (7_505, 6),
                (7_507, 5),
                (7_509, 3),
            ]
        );
    }

    #[test]
    fn percentiles_match_a_hand_checked_small_array() {
        // Five samples. Nearest rank is ceil(p * 5 / 100), 1-indexed.
        //   p0  -> rank 0, floored to 1 -> 10
        //   p50 -> ceil(250/100) = 3    -> 30
        //   p95 -> ceil(475/100) = 5    -> 50
        //   p99 -> ceil(495/100) = 5    -> 50
        let mut stats = LatencyStats::new();
        for sample in [30, 10, 50, 20, 40] {
            stats.record(sample);
        }

        assert_eq!(stats.percentile(0), Some(10));
        assert_eq!(stats.percentile(50), Some(30));
        assert_eq!(stats.percentile(95), Some(50));
        assert_eq!(stats.percentile(99), Some(50));
        assert_eq!(stats.percentile(100), Some(50));
    }

    #[test]
    fn percentiles_match_a_hand_checked_hundred_sample_array() {
        // One hundred samples, values 1 to 100 shuffled by construction. Rank
        // equals p exactly, so the p-th percentile is the value p.
        let mut stats = LatencyStats::with_capacity(100);
        for sample in (1..=100).rev() {
            stats.record(sample);
        }

        assert_eq!(stats.percentile(50), Some(50));
        assert_eq!(stats.percentile(95), Some(95));
        assert_eq!(stats.percentile(99), Some(99));
        assert_eq!(stats.percentile(100), Some(100));
    }

    #[test]
    fn a_percentile_above_one_hundred_clamps() {
        let mut stats = LatencyStats::new();
        stats.record(7);
        assert_eq!(stats.percentile(4_000), Some(7));
    }

    #[test]
    fn percentiles_are_stable_across_repeated_calls() {
        // `percentile` sorts in place, so a second call must not see different
        // data from the first.
        let mut stats = LatencyStats::new();
        for sample in [9, 3, 7, 1, 5] {
            stats.record(sample);
        }
        assert_eq!(stats.percentile(50), stats.percentile(50));
        assert_eq!(stats.len(), 5, "sorting must not drop samples");
    }

    #[test]
    fn an_empty_recorder_has_no_distribution() {
        let mut stats = LatencyStats::new();
        assert!(stats.is_empty());
        assert_eq!(stats.percentile(50), None);
        assert!(stats.summary().is_none());
    }

    #[test]
    fn a_summary_reports_the_whole_distribution() {
        let mut stats = LatencyStats::new();
        for sample in 1..=100 {
            stats.record(sample);
        }

        let summary = stats.summary().unwrap();
        assert_eq!(
            summary,
            LatencySummary {
                count: 100,
                min: 1,
                p50: 50,
                p95: 95,
                p99: 99,
                max: 100,
            }
        );
        assert_eq!(
            summary.to_string(),
            "n=100 min=1ns p50=50ns p95=95ns p99=99ns max=100ns"
        );
    }

    #[test]
    fn a_summary_shows_the_tail_a_mean_would_hide() {
        // Ninety-nine samples at 40 microseconds and one at 9 milliseconds. The
        // mean is about 130 microseconds, which reads as fine. The p99 is what
        // says one decision in a hundred lands somewhere else entirely.
        let mut stats = LatencyStats::with_capacity(100);
        for _ in 0..99 {
            stats.record(40_000);
        }
        stats.record(9_000_000);

        let summary = stats.summary().unwrap();
        assert_eq!(summary.p50, 40_000);
        assert_eq!(summary.p95, 40_000);
        assert_eq!(summary.p99, 40_000);
        assert_eq!(summary.max, 9_000_000);
    }

    #[test]
    fn recording_events_measures_their_transit() {
        let mut stats = LatencyStats::new();
        for transit in [1_000, 3_000, 2_000] {
            stats.record_event(&MarketEvent {
                exchange_time: Nanos(500),
                receive_time: Nanos(500 + transit),
                venue: Venue::Iex,
                kind: EventKind::Status(TradingStatus::Trading),
            });
        }

        assert_eq!(stats.len(), 3);
        assert_eq!(stats.percentile(0), Some(1_000));
        assert_eq!(stats.percentile(100), Some(3_000));
    }

    #[test]
    fn a_replay_run_can_be_instrumented_end_to_end() {
        // What a caller actually writes. Drain the replay, record transit as
        // events pass, report the distribution at the end.
        //
        // Instrumentation reads from `Replay` rather than `DecisionReplay`,
        // because transit is a property of the feed and mixing a configured
        // decision latency into the sample would report a number that is part
        // measurement and part assumption.
        let replay = Replay::new([
            split_source(&[(1, 1_001, 1), (3, 3_003, 3)]),
            split_source(&[(2, 2_002, 2), (4, 4_004, 4)]),
        ]);

        let mut stats = LatencyStats::with_capacity(4);
        let mut labels = Vec::new();
        for outcome in replay {
            let event = outcome.expect("replay failed");
            stats.record_event(&event);
            labels.push(label_of(&event));
        }

        assert_eq!(labels, vec![1, 2, 3, 4]);
        let summary = stats.summary().unwrap();
        assert_eq!(summary.count, 4);
        assert_eq!(summary.min, 1_000);
        assert_eq!(summary.max, 4_000);
    }
}
