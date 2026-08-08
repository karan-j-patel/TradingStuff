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

The initial-capital figure is the one number here that is not an engineering
judgment. It must not be delegated to Claude.

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

## What I am not claiming

I have never traded. This platform is built to find out whether a modest,
documented effect survives real costs, not because I expect it to make money.
The most likely honest outcome is that machine learning does not beat ridge
regression net of costs, and that outcome gets published in the README with the
same prominence as a positive one would.
