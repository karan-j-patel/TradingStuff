# Trial log

Append-only record of every backtest this platform has run. Committed to git
because it is part of the scientific record, not a build artifact.

`trials.jsonl` is one JSON object per line. The first line is the genesis entry,
which exists so the hash chain has a root and so git can track this directory.

## Why this exists

The Deflated Sharpe Ratio needs two inputs a bare counter cannot supply. It
needs `N`, the number of trials, and it needs `sigma_SR`, the spread of Sharpe
ratios across those trials. So each entry stores the Sharpe it produced, not
just the fact that it happened.

Two figures get reported from this log, always. The scoped `N` counts trials
within one research program and is the statistically correct reading. The
lifetime `N` counts every trial ever run against this dataset and is the
conservative bound.

## Record format

Fields, in the order `CLAUDE.md` states them.

| Field | Meaning |
|---|---|
| `timestamp` | RFC 3339, UTC, when the trial completed |
| `program` | Research program identifier, which scopes `N` |
| `config_hash` | Hash of the strategy configuration that was run |
| `sharpe` | Raw Sharpe the trial produced. Diagnostic, never performance |
| `prev_hash` | Hash of the previous entry, which is what chains them |
| `entry_hash` | This entry's own hash, which the next entry names as its `prev_hash` |

The genesis entry carries `prev_hash` of 64 zeros, since nothing precedes it,
and a null `sharpe`, since it is a root rather than a result.

## Canonical form, which the hash depends on

`entry_hash` is the SHA-256 of the entry serialised as JSON with keys sorted,
no whitespace, and `entry_hash` itself excluded. A writer that formats
differently produces a different hash for identical data and the chain will not
verify, so this rule is not cosmetic.

In Python that serialisation is
`json.dumps(entry, sort_keys=True, separators=(",", ":"))`.

The genesis entry hashes to
`bbdc8721955c4bb3f0ba915f1306070895d598345f3f5308d09cd78093af6db5`. Any
implementation that cannot reproduce that value from the record on line 1 has
the canonical form wrong.

## What the chain does and does not do

It is tamper-evident, not tamper-proof. This runs on the author's own machine
from source they control, so enforcement is impossible and pretending otherwise
would waste effort. Editing an entry breaks every hash after it, which makes the
edit detectable. That is the achievable goal and it is the useful one.

No code should ever claim to prevent bypass.

## Not yet implemented

No Rust code reads or writes this file yet. The harness that increments the
counter is part of the rigor crate and does not exist. When it is written it
must match the format above, and the first thing it should do is verify this
chain from the genesis entry forward.
