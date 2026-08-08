# Cross-Sectional Equity Return Prediction

A local research platform for predicting the cross-section of equity returns,
built so its results can be trusted rather than merely produced.

Runs entirely on one machine. No cloud, no server, no deployment.

> **Status: early.** The ingestion layer's schema, validation, and provider
> interfaces exist and are tested. Nothing downstream is built yet and no
> backtest has been run. This README describes what the code does today, not
> what it is planned to do.

## The question it asks

Not "will this stock go up." Forecasting one asset from its own price history is
a well-documented dead end.

Instead, of these thousands of stocks, which will outperform the others over the
next month. Given many characteristics across a wide universe, machine learning
improves out-of-sample prediction of relative returns versus linear baselines.
That is the [Gu, Kelly & Xiu (2020)](https://doi.org/10.1093/rfs/hhaa009)
result, and it is what this platform is built to test.

The published effect is modest. The platform assumes most of what it measures
will turn out not to work, and is designed to make that outcome visible rather
than easy to explain away.

## Why the results should be believable

Most of the engineering here defends against fooling yourself rather than
against slow code.

- Every reported Sharpe ratio carries its deflated counterpart and the trial
  count behind it. Try enough strategies and the best one looks excellent by
  luck alone. The [Deflated Sharpe Ratio](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=2460551)
  discounts for exactly that.
- The trial log is append-only and hash-chained, so the multiple-testing
  correction cannot quietly drift. A 100-point hyperparameter sweep counts as
  100 trials rather than one.
- Point-in-time correctness is enforced in three layers. Filing dates rather
  than fiscal periods, as-reported rather than restated figures, and periodic
  re-fetching that diffs against stored snapshots so a vendor silently revising
  history becomes a visible event.
- Costs are always applied. Spread, commission, slippage, borrow, and the
  short-rebate drag retail accounts actually pay. There is no gross-returns
  mode.
- Three baselines appear beside every result. Buy-and-hold, random ranking at
  matched turnover, and ridge regression on the same features. If gradient
  boosting does not beat ridge net of costs, that is the finding.

Thresholds for risking real money were written in
[DECISION_CRITERIA.md](DECISION_CRITERIA.md) before any result existed.
Weakening one requires a commit with rationale plus a 30-day wait. The git
history is the proof they were not fitted to a number worth liking.

## Architecture

```
crates/ingest        Rust    typed records, validation, provider interfaces
  schema.rs                  price bars, asset identity, adjustment continuity
  provider.rs                pluggable sources, point-in-time contracts

scripts/             Python  operational tooling
research/            Python  models, panel, statistics          (not yet built)
```

Rust owns ingestion, portfolio accounting, the backtest engine, and the
statistics layer. Not for speed. A monthly rebalance over twenty years is 240
iterations and Python would be instant. The reason is that the accounting path
is where a silent error becomes wrong profit and loss, and a type system that
turns a forgotten dividend or a mis-keyed ticker into a compile error is worth
more there than convenience.

Python covers what has no good Rust path. Gradient boosting, neural networks,
and the local-LLM text feature pipeline.

The two sides communicate through Parquet on disk. Neither calls the other.

Data sources sit behind traits rather than being wired to one vendor, so a
subscription to a different provider is a new implementation rather than a
rewrite.

## Running the tests

```bash
cargo test --package ingest      # 30 tests, no network required
```

Every fixture is synthetic. Market data licences forbid redistributing vendor
rows, and that applies to test files as much as to a data directory.

## Licence

MIT. See [LICENSE](LICENSE).
