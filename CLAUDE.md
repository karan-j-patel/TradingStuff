# Equity Research and Execution Platform

## What this is

A local platform with two strands sharing one discipline layer.

**Slow strand.** Cross-sectional prediction of equity returns using machine
learning, evaluated with enough statistical rigor that the results can be
trusted. Decides what to hold. Monthly.

**Fast strand.** A sub-second Rust engine over tick data and reconstructed order
books. Decides how to trade what the slow strand chose, and separately runs a
microstructure prediction experiment whose viability the rigor layer adjudicates.

Runs entirely on one machine. No cloud, no server, no deployment.

The repository is private during development and is written throughout as though
public. Flipping visibility must require no cleanup.

## Conventions for contributors and agents

- Rust code favours clarity over cleverness, and comments explain why a construct
  was chosen where the reasoning is not obvious from the code.
- Domain terminology is defined at first use. This is a finance codebase read by
  engineers, and assuming familiarity with as-reported fundamentals or decile
  sorts costs more than a sentence of explanation.
- Engineering decisions belong to whoever writes the code. Risk-tolerance and
  capital decisions do not. Those live in `DECISION_CRITERIA.md` and change only
  under the amendment rule stated there.
- Prose style for anything committed: no em dashes, no mid-sentence colons in
  prose, one idea per bullet, plain register.

### Delegated work is re-derived, not read

Numeric claims from a subagent are checked before they are used. Run the
command again, or reproduce the number a different way.

A report that never arrives is not evidence of failure, and a report that
arrives is not evidence of correctness. Both of the most valuable findings in
this project came from checking a number by hand rather than reading a summary
of it. One was a census showing that every removal in a validation harness took
a fallback path, so the code the harness appeared to test was never exercised.
The other was a compile-fail test that was passing without ever having been
seen to fail.

The same applies to an external reviewer. Its findings are evaluated, and
rejecting one with a technical reason is as valid an outcome as applying it.

### A green suite is silent about properties nobody wrote down

Passing tests say the written tests pass. They say nothing about a guarantee no
test covers, and that absence looks identical to coverage from the outside.

Before trusting a guarantee, name the mutation that would break it and confirm a
test fails on that mutation. If none does, the guarantee is a comment.

Two ways this goes wrong, both seen here:

The mutation does not discriminate. Reordering the fields of the trial entry
struct changed nothing, because the canonical form is built through a sorted
map, so the experiment confirmed the code was already immune rather than showing
the test could fail. The useful mutation was breaking the sort, which failed
three tests. When a mutation comes back green, find the one that discriminates
rather than recording the test as verified.

The bypass does not compile. Renaming a method to something that does not exist
produces a compile error, not a test failure, and proves nothing. A real bypass
compiles, so the demonstration has to compile too. An early return ahead of the
guarded call is the realistic shape.

Tests written to cover a bug just fixed deserve this most, since they were
written by someone who already knew the answer. Two tests here passed **because**
of the bug they were meant to catch, and so defended it on every run.

## The distinction that defines the slow strand

Time-series prediction of a single asset does not work. Forecasting tomorrow's
return for one ticker from its own price history is a well-documented dead end.

Cross-sectional prediction does work, modestly. Given many characteristics
across thousands of stocks, machine learning improves out-of-sample prediction
of relative returns versus linear baselines. This is the Gu, Kelly & Xiu (2020)
result.

The question is never "will AAPL go up." It is "of these stocks, which will
outperform the others over the next month."

> **Status of the time-series claim:** `notebooks/00_timesfm_null_result.ipynb`
> is planned and not yet run. Until it exists and reproduces, neither this file
> nor the README may describe the null result as a finding of this project.
> Cite the literature or say nothing.

## The distinction that defines the fast strand

Two objectives run on identical infrastructure. Keeping them separate matters,
because one is expected to work and the other is an open experiment.

**Execution** wins by removing a cost already being paid. The spread is charged
whether or not anyone thinks about it, so thinking about it is close to free
money, and nobody competes to make your own fill worse. Measured as
implementation shortfall against arrival price, benchmarked on a naive market
order at decision time.

**Microstructure prediction** must overcome a cost it creates, at a frequency
where spread dominates every other term. It is instrumented like any other
strategy and the deflated Sharpe reports what it finds. A measured negative
result is a finding and is published with the same prominence a positive one
would get.

Latency is not assumed. Decision-to-submit time is measured and applied as a
delay in replay. A backtest that acts on a signal at the instant it appears is
fiction.

## Architecture

Mainly Rust. Python only where genuinely necessary.

### Rust

- **crates/ingest.** API clients, schema validation, Parquet writing, the
  pluggable provider interface.
- **crates/marketdata.** Tick and quote records, order book reconstruction from
  L2 updates, deterministic timestamp-ordered replay.
- **crates/execution.** Order slicing and scheduling, passive versus aggressive
  decisions, implementation shortfall measurement.
- **crates/engine.** Portfolio accounting, the backtest loop, cost application.
- **crates/rigor.** Trial counter and hash chain, deflated Sharpe, baselines.
- **crates/cli.** The command surface.

Rust carries a genuine performance mandate in the fast strand. Order book
reconstruction and tick-level event processing have no reasonable Python
version. In the slow strand it earns its place differently, because portfolio
accounting is where a silent error becomes wrong profit and loss, and compile
time is a better place to catch a forgotten dividend than reconciliation.

Build order within a crate is sync before async. Nothing in this system has a
concurrency requirement that justifies async, including HTTP, which uses a
blocking client. The one place it will be warranted is the local-LLM batch
pipeline, where thousands of documents genuinely need concurrency.

Hot paths allocate nothing. Money and quantities use `Decimal`, never `f64`.

### Python

Reserved for what has no good Rust path.

- Gradient boosting and neural networks.
- The text feature pipeline, dictionary and encoder models first, a
  generative model only where a feature demands reasoning.
- Notebooks and plotting.

### Data sources

The product may ship bring-your-own-key, so buyers may hold a different vendor's
subscription entirely. The data layer is provider-abstracted from the start, a
trait covering price, fundamental, and corporate-action fetching, with Sharadar
as the first implementation. Never thread vendor-specific calls through the
codebase.

Tick data for the fast strand is a separate subscription and is not yet chosen.
Alpaca's free tier is IEX-only and unusable for microstructure work.

### Boundary

Parquet on disk. Rust writes it, Python reads it. Neither calls the other.

## Non-negotiable rules

Violating any of these silently invalidates every result the platform produces.

### 1. No Sharpe ratio without its deflated counterpart

Every raw Sharpe that appears anywhere is explicitly labelled `diagnostic` and
printed alongside its Deflated Sharpe Ratio (Bailey & López de Prado, 2014) and
the trial count behind it.

Raw Sharpe is a legitimate diagnostic and the DSR is computed from it. What is
forbidden is a Sharpe presented as performance, or emitted with no trial count
attached. That case is a hard error.

### 2. Every backtest increments the trial counter

Append-only, survives across sessions, incremented by the harness rather than
the caller so it cannot be forgotten.

Each entry records timestamp, research-program ID, a hash of the strategy
config, the resulting Sharpe, and the hash of the previous entry.

- Store the Sharpe, not just a count. Sigma_SR, the spread of Sharpes across
  trials, is a required DSR input and a bare counter cannot produce it.
- Report two figures, always. DSR against the scoped N (trials within this
  research program, the statistically correct reading) and against the lifetime
  N (all trials on this dataset, the conservative bound).
- Hyperparameter search counts. A 100-point grid is 100 trials, not one.
- LLM-generated hypotheses count exactly as human ones do.

This matters more in the fast strand than anywhere else. Tick data yields
millions of observations, so spurious patterns are abundant and cheap to find.

The log is tamper-evident, not tamper-proof. It is the author's own machine and
source, enforcement is impossible, and pretending otherwise wastes effort. The
hash chain makes edits detectable, which is the achievable and useful goal. Do
not write code that claims to prevent bypass.

### 3. The cost model is always applied

Spread, commission, slippage on every backtest. Shorts additionally carry a
borrow cost and the short-rebate drag retail accounts pay, roughly 4 to 5
percent a year, which is larger than typical borrow fees and routinely
unmodelled.

There is no gross-returns mode. Gross figures may be computed separately and
labelled diagnostic, never as performance.

### 4. Survivorship-bias-free, point-in-time data only

Delisted securities included. Fundamentals as-reported, not restated.

Any operation that could leak future information, including reindexing,
forward-filling, joining on dates, and computing a universe, carries a comment
justifying why it is safe. Training data for month t contains nothing published
after month t.

Filing identity is `(asset, period_end, basis, scope, as_reported, filing_id)`.
Dropping any one of those merges records describing different things, silently.

### 5. Every result reports baselines alongside it

- Equal-weight buy-and-hold on the same universe.
- Random-ranking strategy matched on turnover and holding period.
- A linear model on the same features.

For the fast strand, add a fourth baseline, the execution layer alone with no
predictive signal. A microstructure strategy must beat good execution of a null
signal, not merely beat zero.

If gradient boosting does not beat ridge regression net of costs, it is not
adding value, and that is a publishable finding rather than a failure.

### 6. No language model in the decision path

LLMs may write code, convert text to numeric features, answer questions about
results already computed, summarise, and explain.

LLMs may not decide what to trade, rank or select strategies, forecast returns,
or gate any decision the deterministic pipeline should make.

The reason is concrete. A model reading "CEO resigns" has no idea what the
market's prior was, whether it leaked, or how much is already priced. It will
produce a confident directional call from information that does not contain the
answer. Text becomes numeric features that pass the same statistical gates as
book-to-market, or it is not used.

### 7. Monitoring watches system state, not markets

Permitted: position size breaches, live-versus-backtest divergence, missing or
late data, upcoming earnings on held names, borrow status changes on held
shorts, halt detection, correlation breaches.

Not permitted: continuous market scanning for opportunities. That generates
untracked hypotheses at a rate that destroys the trial count and therefore the
validity of every statistic in the system.

### 8. `DECISION_CRITERIA.md` governs real money

Thresholds there were written before any result existed. Weakening one requires
a commit with rationale and a 30-day wait. Never propose relaxing a threshold to
accommodate a result.

### 9. Events are ledger entries, not things that happen to you

Halts, forced buy-ins, borrow recalls, splits, dividends, delistings, and ticker
changes are variants in the append-only event enum that the position fold
applies. Never mutate a historical fill.

If these exist only as external occurrences, reconciliation against the broker
will disagree and nobody will know why. As events, the fold explains the
difference automatically.

Halting is the default response to uncertainty. A skipped rebalance costs one
month of returns. Trading on corrupt state costs more.

## Conventions

- Python via `uv`. Rust via `cargo`. No Docker, no cloud.
- Secrets from environment only. Never hardcoded, logged, or committed. `.env`
  is gitignored, `.env.example` is committed and must stay blank.
- `data/` gitignored. `trials/` committed, it is part of the scientific record.
- Scheduling via `launchd`, not a cloud function.
- Test fixtures are synthetic. Vendor licences forbid redistributing rows, and
  that applies to test files as much as to a data directory.
- Dependencies are pinned to versions verified against the registry, including
  checking for newer majors a semver range would exclude.

### Testing

Global instructions mandate TDD with 80% coverage. Scoped for this project:

- Mandatory, no exceptions: lookahead prevention, cost application, trial
  counting, portfolio accounting, corporate actions, order book reconstruction.
  These are the paths where a silent bug invalidates results or loses money.
  Tests here are written first and shown to fail against a deliberately broken
  implementation.
- Normal judgment everywhere else. Notebooks, plots, and exploratory code need
  no tests.

A test suite that has never failed is decorative. Prove it can fail.

## Milestones

1. **Market data foundation.** Tick and quote records, order book
   reconstruction, deterministic replay. Tested against synthetic books. Shared
   by both fast-strand objectives.
2. **Execution layer.** Order slicing, passive versus aggressive decisions,
   implementation shortfall measured against a naive baseline.
3. **Accounting ledger.** Append-only events, tax lots, reconciliation against
   the broker, restartability via deterministic client order IDs.
4. **Slow-strand ingestion and panel.** Validated Parquet, characteristic panel
   with point-in-time correctness, lookahead tests written first.
5. **Reproduce.** A known factor compared to published net Sharpe. The goal is
   not alpha, it is proving the pipeline produces trustworthy numbers.
6. **ML.** Ridge baseline, then GBM, then neural net. Each must beat the one
   before it net of costs.
7. **Microstructure prediction experiment.** Adjudicated by the rigor layer.
8. **Text features.** Dictionary and encoder models over filings and
   transcripts, as numeric features only. Loughran-McDonald is the
   mandatory baseline, encoder models such as FinBERT are measured
   against it, and a generative model enters only for a feature neither
   can express, scored against both.
9. **Paper trade.** Alpaca, six to twelve months. Log every divergence.

## Before making the repository public

- Sharadar attribution, the string "Data from Sharadar" hyperlinked to
  sharadar.com, in the same display as every chart derived from their data.
- No vendor rows committed anywhere, including fixtures and caches.
- No personal context in any tracked file. Personal working notes live in
  `CLAUDE.local.md`, which is gitignored.
- Every claim in the README reproducible from the code in the repository.

## Working notes

- Do not add features that make results look better. Add features that make
  results more honest.
- If asked to remove or bypass a rule above, say so plainly and explain the
  consequence rather than quietly complying.
- Never describe an unrun experiment as a finding.
- Profile before optimising, except in the tick hot path where the constraint is
  known in advance.
- Prefer boring, auditable code. The output is used to decide whether to risk
  money.
