# Cross-Sectional Equity Return Prediction — Research Platform

## What this is

A local research platform that predicts the **cross-section** of equity returns
using machine learning, and evaluates those predictions with enough statistical
discipline that the results can be trusted.

Runs entirely on one machine. No cloud, no server, no deployment.

This is a **public repository**. Code, documentation, and commit history are all
part of the deliverable.

## Conventions for contributors and agents

- Rust code favours clarity over cleverness, and comments explain *why* a
  construct was chosen where the reasoning is not obvious from the code.
- Domain terminology is defined at first use. This is a finance codebase read by
  engineers; assuming familiarity with as-reported fundamentals or decile sorts
  costs more than a sentence of explanation.
- Engineering decisions belong to whoever is writing the code. Risk-tolerance and
  capital decisions do not — those live in `DECISION_CRITERIA.md` and change only
  under the amendment rule stated there.

## The distinction that defines this project

**Time-series prediction of a single asset does not work.** Forecasting
tomorrow's return for one ticker from its own price history is a well-documented
dead end.

**Cross-sectional prediction does work, modestly.** Given many characteristics
across thousands of stocks, machine learning improves out-of-sample prediction
of *relative* returns versus linear baselines. This is the Gu, Kelly & Xiu
(2020) result, and it is what this platform is built to exploit.

The question is never "will AAPL go up." It is "of these 3,000 stocks, which
will outperform the others over the next month."

> **Status of the time-series claim:** `notebooks/00_timesfm_null_result.ipynb`
> is **planned and not yet run**. Until it exists and reproduces, neither this
> file nor the README may describe the null result as a finding of this project.
> Cite the literature or say nothing.

## Architecture

**Mainly Rust. Python only where genuinely necessary.**

Rust has no performance mandate here — a monthly rebalance over 20 years is 240
iterations. Rust is used because the author is learning it, and because the
money-critical accounting path benefits from compile-time guarantees. Both are
legitimate reasons; neither is a speed claim. Do not justify Rust on speed.

### Rust

- **Ingestion** — API clients, schema validation, Parquet writing.
- **Data access** — DuckDB queries, panel construction.
- **Portfolio accounting** — positions, lots, cost basis, fills, corporate
  actions. This is where a silent error becomes wrong P&L.
- **Backtest engine** — the replay loop, cost application.
- **Rigor layer** — trial counter and hash chain, deflated Sharpe, baselines.
- **CLI** — the command surface.

Build order within Rust is **sync before async**. Typed records, validation,
Parquet writing, and tests come first, against local files. HTTP clients, rate
limiting, and retry come second. Async is the hardest part of the language and
must not be someone's first exposure to it.

### Python

Reserved for what has no good Rust path:

- Gradient boosting and neural networks (LightGBM, PyTorch).
- The local LLM text feature pipeline (Ollama).
- Notebooks and plotting.

### Data sources

The product may be sold with **bring-your-own-key**, so buyers may hold a
different vendor's subscription entirely. The data layer is therefore
provider-abstracted from the start: a trait covering price, fundamental, and
corporate-action fetching, with Sharadar (via `data.nasdaq.com`) as the first
implementation. Never thread vendor-specific calls through the codebase.

### Boundary

Parquet on disk. Rust writes it, Python reads it. Neither calls the other.

## Non-negotiable rules

Violating any of these silently invalidates every result the platform produces.

### 1. No Sharpe ratio without its deflated counterpart

Every raw Sharpe that appears anywhere is explicitly labelled `diagnostic` and
printed alongside its Deflated Sharpe Ratio (Bailey & López de Prado, 2014) and
the trial count behind it.

Raw Sharpe is a legitimate diagnostic — the DSR is computed *from* it. What is
forbidden is a Sharpe presented as performance, or emitted with no trial count
attached. That case is a hard error.

This mirrors the treatment of gross returns in rule 3.

### 2. Every backtest increments the trial counter

Append-only, survives across sessions, incremented by the harness rather than
the caller so it cannot be forgotten.

Each entry records: timestamp, research-program ID, a hash of the strategy
config, the resulting Sharpe, and the hash of the previous entry.

- **Store the Sharpe, not just a count.** `σ_SR`, the spread of Sharpes across
  trials, is a required DSR input. A bare counter cannot produce it.
- **Report two figures, always.** DSR against the scoped N (trials within this
  research program — the statistically correct reading) and against the lifetime
  N (all trials on this dataset — the conservative bound).
- **Hyperparameter search counts.** A 100-point grid is 100 trials, not one.
- **LLM- or agent-generated hypotheses count exactly as human ones do.**

The log is **tamper-evident, not tamper-proof.** It is the author's own machine
and source; enforcement is impossible and pretending otherwise wastes effort.
The hash chain makes edits detectable, which is the achievable and useful goal.
Do not write code that claims to prevent bypass.

### 3. The cost model is always applied

Spread, commission, slippage on every backtest. Shorts additionally carry a
borrow cost, defaulting to a conservative non-zero value.

There is no gross-returns mode. Gross figures may be computed separately and
labelled diagnostic — never as performance.

### 4. Survivorship-bias-free, point-in-time data only

Delisted securities included. Fundamentals as-reported, not restated.

Any operation that could leak future information — reindexing, forward-filling,
joining on dates, computing a universe — carries a comment justifying why it is
safe. Training data for month *t* contains nothing published after month *t*.

### 5. Every result reports baselines alongside it

- Equal-weight buy-and-hold on the same universe
- Random-ranking strategy matched on turnover and holding period
- A linear model (OLS or ridge) on the same features

That third one is the honest test of whether ML is earning its complexity. If
gradient boosting does not beat ridge regression net of costs, it is not adding
value, and that is a publishable finding rather than a failure.

### 6. No language model in the decision path

LLMs may: write code, convert text to numeric features, answer questions about
results already computed, summarise, explain.

LLMs may not: decide what to trade, rank or select strategies, forecast returns,
or gate any decision the deterministic pipeline should make.

### 7. Monitoring watches system state, not markets

Permitted: position size breaches, live-vs-backtest divergence, missing or late
data, upcoming earnings on held names, correlation breaches.

Not permitted: continuous market scanning for opportunities. That generates
untracked hypotheses at a rate that destroys the trial count and therefore the
validity of every statistic in the system.

### 8. `DECISION_CRITERIA.md` governs real money

Thresholds there were written before any result existed. Weakening one requires
a commit with rationale and a 30-day wait. Never propose relaxing a threshold to
accommodate a result.

## Conventions

- Python via `uv`. Rust via `cargo`. No Docker, no cloud.
- Secrets from environment only. Never hardcoded, logged, or committed.
  `.env` is gitignored; `.env.example` is committed and must stay blank.
- `data/` gitignored. `trials/` committed — it is part of the scientific record.
- Scheduling via `launchd`, not a cloud function.

### Testing

Global instructions mandate TDD with 80% coverage. Scoped for this project:

- **Mandatory, no exceptions:** lookahead prevention, cost application, trial
  counting, portfolio accounting, corporate actions. These are the paths where
  a silent bug invalidates results or loses money. Tests here must be written
  first and must be shown to fail against a deliberately broken implementation.
- **Normal judgment:** everything else. Notebooks, plots, and exploratory code
  need no tests.

A test suite that has never failed is decorative. Prove it can fail.

## Milestones

1. **Ingestion.** Validated Parquet from a small ticker set. Prove the pipeline
   before paying for a full bundle.
2. **Null result.** Run the time-series experiment. Earn the README claim, and
   smoke-test the data path end to end.
3. **Panel.** Characteristic panel with point-in-time correctness. Lookahead
   tests written here, first.
4. **Reproduce.** Quality-plus-value composite versus published net Sharpe. The
   goal is not alpha — it is proving the pipeline produces trustworthy numbers.
   A mismatch means a bug, and finding it is the point.
5. **ML.** Ridge baseline first, then GBM, then neural net. Each must beat the
   one before it net of costs, or it does not ship.
6. **Text features.** Local LLM over transcripts. Does the panel improve?
7. **Paper trade.** Alpaca, six to twelve months. Log every divergence.

## Working notes

- Do not add features that make results look better. Add features that make
  results more honest.
- If asked to remove or bypass a rule above, say so plainly and explain the
  consequence rather than quietly complying.
- Never describe an unrun experiment as a finding. Never write a README claim
  the repository cannot reproduce.
- Profile before optimising. This workload is small; slowness almost always
  means a CSV read, a row-wise loop, or a missing cache.
- Prefer boring, auditable code. The output is used to decide whether to risk
  money, and the source is public.
