# Decision Criteria

**Written 2026-08-07, before any backtest has been run.**

That date is the point of this document. Every threshold here was set while no
result existed to defend. Git history is the proof.

Later, there will be a number I like and a reason why this document was too
strict. That reason will feel good. It should be treated as evidence of bias,
not as an argument.

---

## How this document may change

Weakening a threshold requires all three:

1. A commit that changes it, with the reasoning written in the commit message.
2. A **30-day** wait before the new threshold takes effect.
3. The old threshold stays visible in the file, struck through, not deleted.

Tightening a threshold takes effect immediately and needs no wait.

The asymmetry is deliberate. The failure mode this guards against is loosening a
rule in the moment it becomes inconvenient, which is exactly the moment it is
doing its job.

---

## Gate 1. Before any paper trading

All must hold:

- [ ] The lookahead test suite passes, **and** a deliberately-broken version of
      the pipeline fails it. A suite that never fails is decorative.
- [ ] A known published factor (quality + value composite) reproduces within a
      defensible range of published net-of-cost figures, or the discrepancy is
      understood and written down.
- [ ] Random noise fed in as the signal produces an unimpressive deflated
      Sharpe. If noise looks good, the harness is broken.
- [ ] The cost model is applied to every result. No gross-return path exists.
- [ ] The trial log hash chain verifies.
- [ ] Corporate actions (splits, dividends, delistings, ticker changes) are
      handled and tested. A delisting mid-hold must not silently vanish.

## Gate 2. Before one dollar of real money

All of Gate 1, plus all of the following:

| Criterion | Threshold |
|---|---|
| Paper trading duration | ≥ **126 trading days** (~6 months), uninterrupted |
| Deflated Sharpe, scoped | ≥ **0.95** |
| Deflated Sharpe, lifetime | ≥ **0.90** |
| Beats equal-weight buy-and-hold, net of costs | required |
| Beats random ranking at matched turnover | required |
| Beats ridge regression on the same features | required |
| Live vs backtest monthly return correlation | ≥ **0.50** |
| Realized paper Sharpe vs backtest Sharpe | ≥ **50%** of backtest |
| Missed or degraded rebalances during paper period | **0** |

> **Deflated Sharpe** is a probability between 0 and 1. It is the chance the strategy's
> edge is real rather than the luckiest of everything tried. 0.95 means 95%
> confident. It is not a return figure.

---

# Fast strand gates

**Added 2026-08-08, before any fast-strand result existed.**

Gates 1 and 2 above are stated in monthly units and govern the slow strand only.
Nothing in them constrains a sub-second strategy, which inverted the purpose of
this document for as long as the fast strand had no gates of its own. Adding
gates is tightening, so these take effect immediately under the amendment rule.

They are numbered F0 to F2 rather than renumbering the slow-strand gates, which
are referenced elsewhere.

The fast strand has two objectives and they are not judged the same way.
**Execution** removes a cost already being paid and is judged against a naive
baseline, not against a Sharpe ratio. **Microstructure prediction** creates a
cost it must overcome and faces the full rigor treatment. Collapsing the two
would let the easy win launder the hard one.

## Gate F0. Before any expected-value claim is made at all

No number describing what this strand might earn may be stated, written down, or
used in any decision until all of these hold.

- [ ] **Aggregate one-step top-N reconstruction** is validated against the
      LOBSTER orderbook files. Applying one message to a known-correct book
      must reproduce the aggregate size at every price the reference reports,
      top **10 levels** per side, across **all five** sample symbols, with the
      zero-size padding normalized before comparison. Any mismatch is a
      failure, not a tolerance.

      Standard as of 2026-08-08: **2,110,855 messages, zero size mismatches,
      zero crossed books.**

      Word this carefully whenever it is restated. It is not a finding that
      "the reconstructor is correct". It is a finding about aggregate sizes at
      shared prices, one message at a time.

      This is listed first because everything below it is derived from the
      book. A silently wrong reconstructor does not produce a wrong fill rate
      and a right markout, it produces wrong numbers throughout while every
      other box on this list still ticks.

- [ ] It is written down that **queue position cannot be validated from LOBSTER
      at all**, and that this is why the fill-rate rule below exists.

      An orderbook row carries no order identities. It reports how much rests
      at each price and says nothing about which orders compose it or in what
      order they arrived. So no amount of reference data of this shape can
      check a queue model, however good the code is.

      That matters more than it sounds. Queue position is the entire economics
      of a passive fill. Whether a resting order ever executes depends on where
      it sits in the queue at its price, and two orders at the same price get
      opposite outcomes based only on who arrived first. A queue simulator
      could therefore be badly wrong while every test in the repository still
      passes, and any backtested fill rate built on it is unvalidated at its
      core.

      **This vindicates the next rule rather than challenging it.** F0 already
      requires the fill rate to be measured in paper trading and already states
      that the captures cannot supply it. This is the evidence for why that
      rule is correct rather than merely cautious. A future session reading
      "measure it in paper trading" as excessive caution, and proposing to
      substitute a backtested estimate, is proposing to substitute a number
      that nothing has ever checked.

- [ ] Passive fill rate is **measured**, not assumed. Reported as fills over
      attempts, with the count, over at least **20 trading days** of paper
      trading.
- [ ] It is written down that this cannot be measured from the IEX or LOBSTER
      captures, because neither contains our orders. A backtest can say where
      our order would have rested. It cannot say whether it would have filled.
- [ ] Decision-to-acknowledgement latency is measured and reported as a
      distribution, median and p95 and p99. A point estimate hides the tail,
      and the tail is when it matters.

- [ ] The **latency horizon** is fixed here, at the measured p99, and written
      down. Gate F1 tests against that recorded number and not against a fresh
      one. A horizon re-derived later would move to wherever the strategy
      needed it to be.

- [ ] **Sigma of daily profit and loss** is measured across the same 20 days
      and written down. The loss caps below use it, and it is fixed at this
      point. It is never re-estimated from live trading, because a strategy
      that starts losing more would otherwise widen its own cap exactly when
      the cap is doing its job.
- [ ] Quoted spread at decision time is recorded per order, so realized capture
      can be compared against it rather than assumed equal to it.
- [ ] Every microstructure hypothesis is in the trial counter, including every
      point of every hyperparameter grid.

## Gate F1. Before the execution layer routes one real order

Execution is judged against the cost of not having it.

| Criterion | Threshold |
|---|---|
| Parent orders measured in paper trading | ≥ **200** |
| Implementation shortfall vs naive market order at decision time | better on the mean **and** on ≥ **60%** of parent orders |
| Markouts at **1s, 10s and 60s** after every passive fill | all three reported, none omitted |
| Mean markout at 60s after a passive fill | must not exceed the spread captured on that fill |
| Realized spread capture, measured against the **NBBO** at decision time | reported per order |
| Latency p99, signal to acknowledgement | at or below the horizon recorded at Gate F0 |
| Fills the reconciliation could not match to an order | **0** |

> Spread capture is measured against the national best bid and offer, never
> against one venue's own book. IEX alone is roughly two to three percent of
> consolidated volume and quotes wider than the NBBO, so measuring against it
> would flatter every result. Our own capture put AAPL at 9 cents on IEX, which
> is not what a routed order pays.

> **Markout** is where the price went after your fill. Capturing two cents of
> spread and then watching the price move three cents against you is a loss
> dressed as a saving. It is the standard measure of adverse selection, which is
> the risk that whoever traded with you knew something.

## Gate F2. Before the prediction strategy trades real money

All of Gate F1, plus all of the following.

| Criterion | Threshold |
|---|---|
| Paper trading duration | ≥ **60 trading days**, uninterrupted |
| Round trips completed in paper | ≥ **2,000** |
| Deflated Sharpe, scoped | ≥ **0.95** |
| Deflated Sharpe, lifetime | ≥ **0.90** |
| Beats the execution layer alone, running a null signal, net of costs | required |
| Beats random entry at matched turnover and holding period | required |
| Beats **logistic regression on order book imbalance**, same features, net of costs | required |
| Realized spread capture vs quoted spread | reported, and the gap explained |

Calendar time alone is a weak gate at this frequency, which is why round trips
are required alongside it. Sixty days of an idle strategy proves nothing.

> Order book imbalance is the ratio of resting size on the bid to resting size
> on the ask. That it predicts short-horizon direction is one of the
> longest-known facts in microstructure. A complex model that merely
> rediscovers it has found nothing, and without this row it would look like a
> result. This is the fast strand's equivalent of the slow strand having to
> beat ridge regression.

## Hard loss caps for the fast strand

These are enforced in code, not by a human watching a screen. A cap that
depends on someone noticing is not a cap. The check runs in the order path and
refuses to submit once tripped, and a strategy that cannot reach the check must
not be allowed to send orders at all.

They halt automatically. They are not advisory and there is no discretion.

| Trigger | Action |
|---|---|
| Daily loss exceeding the **daily cap** defined below | halt the strand for the remainder of the day |
| Loss of **3x the daily cap** over any rolling 5 trading days | halt the strand pending written review |
| Fast-strand allocation | ≤ **20%** of committed capital until Gate F2 has held for 60 days |
| Notional per position | ≤ **10%** of fast-strand allocation |

**The daily cap** is the larger of **1% of fast-strand allocation** and **0.5
sigma of daily profit and loss** as measured at Gate F0 and fixed there.

A flat 1% does not survive contact with the arithmetic. At the initial cap the
strand holds $10,000, so 1% is $100, and a strategy doing hundreds of round
trips a day has daily variance well above that. It would halt on noise most
days, and a cap that trips constantly is a cap that gets argued away. The
sigma term makes the number derivable from what the strategy actually does
while keeping the 1% as a floor.

The sigma is fixed at Gate F0 and never re-estimated from live trading. A cap
that tracks live volatility widens exactly when the strategy is deteriorating,
which is when it is most needed.

### The size the allocation actually permits

At $50,000 committed and a 20% cap, the fast strand holds **$10,000**, so a
position is at most **$1,000**. That is 2 shares of a $500 stock and about 17
of a $60 one.

This is written down because the struck arithmetic in the handoff assumed 800
shares, which at any liquid price is several times the entire allocation. One
of those two numbers had to be fiction and it was the 800.

**The strategy design fits inside this constraint, not the reverse.** If a
sub-second strategy needs size that this allocation cannot fund, that is a
finding to confront at design time and it belongs in the record. It is not a
reason to raise the allocation, and raising it would be a weakening subject to
the 30-day rule.

> The caps are tighter than the slow strand's drawdown halt because the failure
> mode is different. A monthly strategy loses slowly enough to notice. A
> sub-second strategy with a sign error loses for as long as it is switched on.

**The bold numbers in this section are risk-tolerance decisions and are
provisional until ratified.** The structure and the units are engineering. How
much money to risk is not.

### Amendments to this section

Everything here was added or tightened on 2026-08-08, before any fast-strand
result existed. Tightening takes effect immediately, so none of it waited.

- **2026-08-08.** Section created. Gates F0 to F2 and the loss caps.
- **2026-08-08.** LOBSTER book validation added to F0, listed first. Every
  other measurement in F0 is derived from the reconstructed book, so a wrong
  book yields wrong numbers while every other box still ticks.
- **2026-08-08.** That checkbox reworded to say what was actually validated,
  aggregate one-step top-N reconstruction, after an adversarial review found
  the claim was being stated as "the reconstructor is correct". Standard
  recorded as 2,110,855 messages with zero mismatches.
- **2026-08-08.** Added the queue-position finding to F0. It cannot be
  validated from LOBSTER at all, since an orderbook row carries no order
  identities, and it is the entire economics of a passive fill. Recorded as
  the justification for the paper-trading fill-rate rule so that rule is not
  later mistaken for excessive caution.
- **2026-08-08.** Daily loss cap changed from a flat 1% to the larger of 1%
  and 0.5 sigma. A $100 cap on a $10,000 allocation would have halted on noise
  most days, and a cap that trips constantly is one that gets weakened.
- **2026-08-08.** Notional per position added at 10% of allocation, and the
  size the allocation actually permits written out, after the struck 800-share
  arithmetic was found to exceed the whole allocation several times over.
- **2026-08-08.** Logistic regression on order book imbalance added to F2, so
  the fast strand has the simple-model baseline the slow strand already had in
  ridge regression.
- **2026-08-08.** Latency horizon pinned to the p99 recorded at F0 rather than
  left as "the horizon the scheduler assumes", which was defined nowhere.

---

## Capital

### The breadth constraint that comes first

A cross-sectional strategy's edge is statistical. It requires enough positions
for idiosyncratic noise to average out, and below that floor it is not a smaller
version of the tested strategy. It is a different, noisier one that the backtest
does not describe.

Measured against this account: shorts cannot be fractional, so each needs roughly
$600–1,000 of whole shares, and only **36.8%** of US equities are shortable at
all (measured 2026-08-07, and it moves daily).

```
under $10k    long-only. A long/short book cannot be staffed here.
$25–30k       ~30 shorts. This is a quintile sort, not a decile. Say so.
$50k          floor for a credible long/short book
$100k         true decile of a ~1,000-name universe
```

**If the number below lands under $50k, run long-only.** Do not shrink the
long/short book to fit. That silently invalidates the backtest it is based on.

### Maximum initial capital

**$50,000.** Set 2026-08-07, before any backtest existed.

This is the floor for a long/short book, not a comfortable level. What it implies,
so that later surprise is not mistaken for bad luck:

| | |
|---|---|
| Shortable positions supportable | ~50 (whole shares only, ~$600–1,000 each) |
| Therefore universe should be sized to | **~500 names**, so a decile is ~50 |
| Running a 3,000-name universe at this size | a decile would be 300 names, unstaffable. Do not. |
| Halt threshold at a 20% drawdown | $40,000 |
| Halt threshold at the 25% cap | $37,500 |

The universe row is the operative one. At $50k the decile sort has to run over a
narrower universe for the breadth to exist. That is a different study from the
one Gu, Kelly & Xiu ran, and results must be reported as such rather than
compared to their figures as though the universes matched.

Two further notes, recorded now while they cost nothing to accept:

- If liquid net worth is such that $50,000 exceeds 5% of it, this figure is too
  high and should be reduced. In that case see the breadth constraint above
  and go long-only rather than shrinking the long/short book.
- $50,000 must be an amount that can go to zero without consequence. Not
  "unlikely to". The threshold is whether the loss changes anything that matters.

### Maximum drawdown before halting

```
halt at  1.5 × worst backtested drawdown,  floored at 15%,  capped at 25%
```

Relative rather than flat, deliberately. A threshold tighter than the backtest's
own worst period halts on normal variance and teaches nothing. Looser than 25%
is not halting, it is hoping, and discipline usually breaks before that anyway.

The 1.5× multiplier exists because live drawdowns routinely exceed backtested
ones. If the backtest's worst was 12%, halt at 18%. If it was 20%, the cap binds
and the strategy itself deserves a second look.

### Maximum total capital ever committed

```
3 × initial, unlocked in stages:

  initial       on passing Gate 2
  2 × initial   after 6 months live, tracking backtest within tolerance
  3 × initial   after 12 months
```

Increases are gated on **elapsed live performance**, never on returns. A strategy
that made money for three months has told you almost nothing, and scaling on a
good quarter is how a small loss becomes a large one.

The initial-capital figure is the one number in this document that is not an
engineering judgment. It must not be delegated to a tool, an adviser, or a
model, and it must be set before Gate 2 rather than at it.

## Position and exposure limits

Enforced in code, checked every rebalance, breach halts trading:

| Limit | Value |
|---|---|
| Maximum single position | 5% of portfolio |
| Maximum sector exposure | 25% |
| Gross exposure | ≤ 150% |
| Net exposure | ≤ ±30% |
| Minimum positions held | 30 |

The minimum-position floor matters: a cross-sectional strategy's edge is
statistical. Concentrated into a handful of names it is no longer the strategy
that was tested.

## Abort conditions. Halt immediately, no discretion

Trading stops on any of these. Restart requires diagnosis and a written note.

- Any lookahead test failing
- Trial log hash chain failing to verify
- Drawdown exceeding the limit above
- Three consecutive months of live-vs-backtest divergence beyond tolerance
- Any position limit breach
- Stale, missing, or late data at rebalance time
- Any unhandled corporate action
- Any reconciliation mismatch between expected and broker-reported positions

Halting is the default on uncertainty. A skipped rebalance costs one month of
returns. Trading on corrupt state costs more.

## Review

- Monthly during paper trading: divergence report, written.
- Before any threshold change: re-read this document top to bottom.
- The trial count is reported alongside every result, always.

---

## What this document does not claim

This platform is built to find out whether a modest, documented effect survives
real costs, not on the expectation that it will make money. The most likely
honest outcome is that machine learning does not beat ridge regression net of
costs, and that outcome gets published with the same prominence a positive one
would get.

The thresholds above are set accordingly. They assume the strategy does not
work until evidence says otherwise, rather than assuming it works and looking
for a reason to stop.
