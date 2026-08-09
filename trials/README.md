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
| `program` | Research program identifier, which scopes `N`. ASCII only, see below |
| `config_hash` | Hash of the strategy configuration that was run |
| `sharpe` | Raw Sharpe the trial produced. Diagnostic, never performance |
| `prev_hash` | Hash of the previous entry, which is what chains them |
| `entry_hash` | This entry's own hash, which the next entry names as its `prev_hash` |

The genesis entry carries `prev_hash` of 64 zeros, since nothing precedes it,
and a null `sharpe`, since it is a root rather than a result.

`sharpe` is written as a JSON string, so `1.23` appears as `"1.23"`. A float has
no single reproducible text form and this field is hashed.

## Canonical form, which the hash depends on

`entry_hash` is the SHA-256 of the entry serialised as JSON with keys sorted,
no whitespace, `entry_hash` itself excluded, and the text encoded as raw UTF-8.
A writer that formats differently produces a different hash for identical data
and the chain will not verify, so this rule is not cosmetic.

Raw UTF-8 means non-ASCII characters are emitted as themselves, not as `\uXXXX`
escape sequences. This is what RFC 8785, the JSON Canonicalization Scheme,
specifies. It has to be stated because the obvious Python transcription gets it
wrong by default. `json.dumps` defaults `ensure_ascii` to true, so the version
of this document that omitted the flag described a different hash from the one
the code computes, for any entry holding a non-ASCII character.

In Python the serialisation is

```python
json.dumps(entry, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
```

Since the genesis record is pure ASCII, both forms hash it identically and the
published value below is unaffected by this correction.

The genesis entry hashes to
`bbdc8721955c4bb3f0ba915f1306070895d598345f3f5308d09cd78093af6db5`. Any
implementation that cannot reproduce that value from the record on line 1 has
the canonical form wrong. Genesis alone cannot catch an `ensure_ascii` mistake,
so `crates/rigor/src/entry.rs` also carries a cross-language test over an entry
that holds quotes, backslashes, control characters, and non-ASCII text.

## What `program` may contain

ASCII letters and digits plus `-`, `_`, and `.`, non-empty, at most 64
characters. A writer rejects anything else rather than escaping it.

The canonical form above already handles non-ASCII correctly, so this is not a
patch over a bug. It is there because the next implementation of this format
will be in another language, and an identifier that is ASCII by construction
makes the `ensure_ascii` class of divergence unreachable rather than merely
handled. The restriction binds writers only. A reader must be able to load and
report on whatever the file actually holds.

## What the chain does and does not do

It is tamper-evident, not tamper-proof. This runs on the author's own machine
from source they control, so enforcement is impossible and pretending otherwise
would waste effort. Editing an entry breaks every hash after it, which makes the
edit detectable. That is the achievable goal and it is the useful one.

No code should ever claim to prevent bypass.

## Appending concurrently

A writer takes an exclusive lock on this file, then re-reads it from disk while
holding that lock, and only then computes `prev_hash` from the head it just
read. Any implementation that reads the head before locking will write entries
that name the same predecessor, and the log will hold siblings rather than a
chain.

The re-read is the part that matters. It was measured: twenty concurrent
appends without it produced logs of 6, 8, 14, and 17 lines against an expected
21, every one of them with a broken chain and an undercounted `N`. An undercount
raises no error and makes every result look better than it is, which is the
exact failure the trial counter exists to prevent.
