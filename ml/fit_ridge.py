"""Ridge baseline over the exported characteristic panel (milestone 6, round B).

Reads the panel written by the Rust `export` command, rank-transforms each
characteristic cross-sectionally within a month, fits plain L2 ridge on an
expanding window with annual refits, and writes one predicted next-month
return per security per test-month formation.

The protocol below is fixed a priori and is not tuned. Window lengths, the
lambda grid, and the test start are constants, not arguments, so that a rerun
cannot quietly become a different experiment. See
ContinuationDocs/2026-08-14-ridge-spec.md, section "Round B".

Run from the ml directory with `uv run python fit_ridge.py`.
"""

from __future__ import annotations

import hashlib
import json
import sys
import time
from pathlib import Path
from typing import NamedTuple

import numpy as np
import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq
from sklearn.linear_model import Ridge

REPO_ROOT = Path(__file__).resolve().parents[1]
PANEL_PATH = REPO_ROOT / "data" / "curated" / "panel" / "panel.parquet"
OUTPUT_PATH = REPO_ROOT / "data" / "curated" / "predictions" / "predictions.parquet"

CHARACTERISTICS = (
    "momentum_12_1",
    "vol_daily_12m",
    "vol_monthly_36m",
    "log_marketcap",
    "book_to_market",
    "dividend_yield_12m",
    "share_change_24m",
    "median_dollar_volume_12m",
)

# The provenance keys this script knows the panel writer stamps. Every one must
# be present or a prediction cannot be traced back to the data that produced it,
# so a missing key is a hard error rather than a warning. This is a floor and
# not a whitelist: keys the writer adds later are copied through untouched, so a
# new dataset digest upstream needs no edit here.
REQUIRED_PROVENANCE_KEYS = (
    "config_hash",
    "universe_sha256",
    "actions_sha256",
    "delistings_sha256",
    "marketcap_sha256",
    "filings_sha256",
)

FIRST_TRAIN_YEAR = 1999
VALIDATION_YEARS = 3
FIRST_TEST_YEAR = 2012
# Ten log-spaced points, 1e-4 to 1e2, chosen before any result was seen.
ALPHA_GRID = tuple(np.logspace(-4.0, 2.0, 10))

# The target is label_return_1m as shipped, a raw total return rather than an
# excess return. This is a recorded deviation from Gu, Kelly and Xiu (2020).
# A common monthly constant does not move cross-sectional ranks, and no
# risk-free series exists on disk to subtract.
TARGET = "label_return_1m"

# No intercept. With a raw-total-return target an intercept would learn the
# unconditional equity premium, and against the zero-forecast denominator that
# constant alone buys positive R2 that contains no cross-sectional information.
# The measured size of that effect is printed as a labelled diagnostic below.
FIT_INTERCEPT = False


class LeakError(AssertionError):
    """A row from the test year reached fitting or hyperparameter tuning."""


class Panel(NamedTuple):
    """The panel as parallel arrays, sorted by formation month.

    `features` is already rank-transformed and imputed, so it has no missing
    values. `target` keeps NaN where the forward return is unknown.
    """

    ticker: np.ndarray
    permanent_id_kind: np.ndarray
    permanent_id: np.ndarray
    month: np.ndarray  # datetime64[D], the formation month-end
    year: np.ndarray  # int, calendar year of the formation month-end
    features: np.ndarray  # (n, len(CHARACTERISTICS)) float64
    target: np.ndarray  # float64, NaN where the label is null
    eligible: np.ndarray  # bool
    imputed_counts: dict[str, int]  # how many values the median rule filled
    provenance: dict[str, str]


def sha256_of_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def read_provenance(metadata: dict[bytes, bytes] | None) -> dict[str, str]:
    """Take every provenance key the panel carries, and refuse if one is absent.

    Everything except Arrow's own schema block is copied, so a digest added by
    the writer reaches the predictions file with no change here. The keys in
    REQUIRED_PROVENANCE_KEYS are the floor this script can check for itself.
    """
    raw = {
        key.decode(): value.decode()
        for key, value in (metadata or {}).items()
        if not key.decode().startswith("ARROW:")
    }
    missing = [key for key in REQUIRED_PROVENANCE_KEYS if not raw.get(key)]
    if missing:
        raise ValueError(
            f"panel is missing provenance keys {missing}. "
            "A prediction that cannot be traced to its panel is not usable."
        )
    return raw


def _decimal_column_to_float(table: pa.Table, name: str) -> np.ndarray:
    """Panel numerics are decimal128(38, 9). Nulls arrive as NaN."""
    column = pc.cast(table.column(name), pa.float64())
    return column.to_numpy(zero_copy_only=False)


def rank_to_unit_interval(values: np.ndarray) -> np.ndarray:
    """Map values to [-1, 1] by cross-sectional rank, averaging ties.

    This is the Gu, Kelly and Xiu (2020) transform. Ranks run from 0 to n-1
    and are rescaled linearly, so the smallest value maps to -1 and the
    largest to +1 regardless of the underlying units. That is why
    log_marketcap can carry a units constant harmlessly.
    """
    n = values.size
    if n == 0:
        return values
    if n == 1:
        return np.zeros(1)
    ordered = np.sort(values)
    unique, first_index, counts = np.unique(ordered, return_index=True, return_counts=True)
    average_rank = first_index + (counts - 1) / 2.0
    ranks = average_rank[np.searchsorted(unique, values)]
    return 2.0 * ranks / (n - 1) - 1.0


def build_features(month: np.ndarray, raw: dict[str, np.ndarray]) -> tuple[np.ndarray, dict[str, int]]:
    """Rank-transform per month, then median-impute what is still missing.

    Order matters and is the Gu, Kelly and Xiu footnote 30 rule. Ranking uses
    only the non-missing values in that month, and the missing ones then take
    the cross-sectional median of the transformed values, which for a
    symmetric rank map sits at or very near zero. The imputation happens here,
    visibly, and never in the exported panel, because filling a hole is a
    modelling choice rather than a fact about the data.

    Ranking spans every panel row in the month, eligible or not. Eligibility
    filtering happens afterwards, so a row's features do not depend on which
    subset is being fitted.
    """
    n = month.size
    features = np.empty((n, len(CHARACTERISTICS)), dtype=np.float64)
    imputed: dict[str, int] = {name: 0 for name in CHARACTERISTICS}
    # Rows arrive sorted by month, so each month is one contiguous block.
    _, starts, counts = np.unique(month, return_index=True, return_counts=True)
    for start, count in zip(starts, counts):
        stop = start + count
        for column, name in enumerate(CHARACTERISTICS):
            block = raw[name][start:stop]
            present = ~np.isnan(block)
            transformed = np.zeros(count, dtype=np.float64)
            if present.any():
                transformed[present] = rank_to_unit_interval(block[present])
                fill = float(np.median(transformed[present]))
            else:
                # Nothing to rank against, so every row takes the same value
                # and the characteristic carries no cross-sectional signal
                # this month.
                fill = 0.0
            transformed[~present] = fill
            imputed[name] += int((~present).sum())
            features[start:stop, column] = transformed
    return features, imputed


def load_panel(path: Path) -> Panel:
    table = pq.read_table(path)
    provenance = read_provenance(pq.ParquetFile(path).metadata.metadata)

    month = table.column("month_end").to_numpy(zero_copy_only=False)
    order = np.argsort(month, kind="stable")
    month = month[order]

    raw = {name: _decimal_column_to_float(table, name)[order] for name in CHARACTERISTICS}
    features, imputed = build_features(month, raw)

    return Panel(
        ticker=np.asarray(table.column("ticker").to_pylist(), dtype=object)[order],
        permanent_id_kind=np.asarray(table.column("permanent_id_kind").to_pylist(), dtype=object)[order],
        permanent_id=np.asarray(table.column("permanent_id").to_pylist(), dtype=object)[order],
        month=month,
        year=month.astype("datetime64[Y]").astype(int) + 1970,
        features=features,
        target=_decimal_column_to_float(table, TARGET)[order],
        eligible=table.column("eligible").to_numpy(zero_copy_only=False).astype(bool)[order],
        imputed_counts=imputed,
        provenance=provenance,
    )


def split_years(test_year: int) -> tuple[tuple[int, ...], tuple[int, ...]]:
    """Training and validation calendar years for one refit.

    Training starts at FIRST_TRAIN_YEAR and expands by one year per refit.
    Validation stays VALIDATION_YEARS long and ends the year before the test
    year. For the first refit that is training 1999 to 2008 and validation
    2009 to 2011 against test year 2012.
    """
    validation_start = test_year - VALIDATION_YEARS
    return (
        tuple(range(FIRST_TRAIN_YEAR, validation_start)),
        tuple(range(validation_start, test_year)),
    )


def assert_no_test_leak(
    test_year: int,
    fit_month: np.ndarray,
    tune_month: np.ndarray,
    test_month: np.ndarray,
) -> None:
    """Refuse to fit if anything from the test year reached fitting or tuning.

    The arrays passed in are read back off the row selections that are about
    to be used, not off the intended year lists, so an indexing mistake fails
    here as loudly as a split mistake does.

    Prior test years reappearing in later training windows is the expanding
    protocol working as designed and is not a leak. What must never happen is
    a formation month at or after the current test year informing the model
    that scores it.
    """
    if test_month.size == 0:
        raise LeakError(f"test year {test_year} selected no rows")
    first_test = test_month.min()
    for role, months in (("training", fit_month), ("validation", tune_month)):
        if months.size == 0:
            raise LeakError(f"{role} window for test year {test_year} selected no rows")
        overlap = np.intersect1d(months, test_month)
        if overlap.size:
            raise LeakError(
                f"{overlap.size} {role} formation months fall inside the test "
                f"window for test year {test_year}, first {overlap.min()}"
            )
        if months.max() >= first_test:
            raise LeakError(
                f"latest {role} formation {months.max()} is not before the first "
                f"test formation {first_test} for test year {test_year}"
            )


def r2_oos(actual: np.ndarray, predicted: np.ndarray) -> float:
    """R2 against a zero forecast, the Gu, Kelly and Xiu (2020) definition.

        R2_oos = 1 - sum((r - rhat)^2) / sum(r^2)

    The denominator is the sum of squared actual returns and is NOT demeaned.
    Subtracting a mean would flatter the number by roughly three percentage
    points and would stop it being comparable in kind to a published figure.
    """
    denominator = float(np.sum(actual**2))
    if denominator == 0.0:
        return float("nan")
    return 1.0 - float(np.sum((actual - predicted) ** 2)) / denominator


def run_refit(panel: Panel, test_year: int) -> dict:
    """Tune lambda on validation, fit on training only, predict the test year.

    Validation tunes but does not train, following the Gu, Kelly and Xiu
    recursive scheme, so the coefficients that score the test year have seen
    training rows only.
    """
    train_years, validation_years = split_years(test_year)

    usable = panel.eligible & ~np.isnan(panel.target)
    fit_rows = np.flatnonzero(usable & np.isin(panel.year, train_years))
    tune_rows = np.flatnonzero(usable & np.isin(panel.year, validation_years))
    # Every panel row in the test year gets a prediction. Eligibility screens
    # are the engine's job, and predicting only part of the cross-section
    # would hand it a hole it cannot distinguish from a missing month.
    test_rows = np.flatnonzero(panel.year == test_year)

    assert_no_test_leak(
        test_year,
        panel.month[fit_rows],
        panel.month[tune_rows],
        panel.month[test_rows],
    )

    x_fit, y_fit = panel.features[fit_rows], panel.target[fit_rows]
    x_tune, y_tune = panel.features[tune_rows], panel.target[tune_rows]

    validation_curve = []
    best = None
    for alpha in ALPHA_GRID:
        model = Ridge(alpha=alpha, fit_intercept=FIT_INTERCEPT, solver="cholesky")
        model.fit(x_fit, y_fit)
        score = r2_oos(y_tune, model.predict(x_tune))
        validation_curve.append((alpha, score))
        if best is None or score > best[1]:
            best = (alpha, score, model)

    alpha, validation_r2, model = best
    scored = test_rows[~np.isnan(panel.target[test_rows])]
    # Two evaluation populations, both reported, because they answer different
    # questions. The eligible subset matches the population the model was
    # trained on and the universe the engine can actually trade. The full set
    # additionally scores the untradable tail, which the model never saw the
    # like of in training and where returns are noisiest.
    scored_eligible = scored[panel.eligible[scored]]

    # Diagnostic only, never the reported figure. Refitting the chosen lambda
    # with an intercept shows how much R2 the unconditional mean return alone
    # would contribute against the zero-forecast denominator.
    with_intercept = Ridge(alpha=alpha, fit_intercept=True, solver="cholesky").fit(x_fit, y_fit)

    return {
        "test_year": test_year,
        "train_years": (train_years[0], train_years[-1]),
        "validation_years": (validation_years[0], validation_years[-1]),
        "n_train": fit_rows.size,
        "n_validation": tune_rows.size,
        "n_test_predicted": test_rows.size,
        "n_test_scored": scored.size,
        "n_test_scored_eligible": scored_eligible.size,
        "alpha": alpha,
        "validation_r2": validation_r2,
        "validation_curve": validation_curve,
        "coefficients": model.coef_.copy(),
        "test_rows": test_rows,
        "predictions": model.predict(panel.features[test_rows]),
        "r2_test": r2_oos(panel.target[scored], model.predict(panel.features[scored])),
        "r2_test_eligible": r2_oos(
            panel.target[scored_eligible], model.predict(panel.features[scored_eligible])
        ),
        "r2_test_with_intercept_diagnostic": r2_oos(
            panel.target[scored], with_intercept.predict(panel.features[scored])
        ),
    }


def write_predictions(panel: Panel, rows: np.ndarray, predictions: np.ndarray, panel_sha256: str) -> None:
    table = pa.table(
        {
            "ticker": pa.array(panel.ticker[rows].tolist(), pa.string()),
            "permanent_id_kind": pa.array(panel.permanent_id_kind[rows].tolist(), pa.string()),
            "permanent_id": pa.array(panel.permanent_id[rows].tolist(), pa.string()),
            "month_end": pa.array(panel.month[rows], pa.date32()),
            "predicted_return_1m": pc.cast(
                pa.array(predictions, pa.float64()), pa.decimal128(38, 9), safe=False
            ),
        }
    )
    fit_spec = {
        "target": TARGET,
        "transform": "cross-sectional rank to [-1,1], median-imputed after transform",
        "first_train_year": FIRST_TRAIN_YEAR,
        "validation_years": VALIDATION_YEARS,
        "first_test_year": FIRST_TEST_YEAR,
        "alpha_grid": [float(a) for a in ALPHA_GRID],
        "fit_intercept": FIT_INTERCEPT,
        "characteristics": list(CHARACTERISTICS),
    }
    added = {
        "panel_sha256": panel_sha256,
        "fit_script": "ml/fit_ridge.py",
        "fit_spec": json.dumps(fit_spec, sort_keys=True),
    }
    # The passthrough is whatever the panel carried, so a key the writer adds
    # later needs no edit here. A name collision would silently replace a
    # provenance value with one of ours, which is the one way this can lie.
    clash = sorted(set(panel.provenance) & set(added))
    if clash:
        raise ValueError(f"panel provenance already uses the keys this fit adds {clash}")
    metadata = {**panel.provenance, **added}
    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    pq.write_table(table.replace_schema_metadata(metadata), OUTPUT_PATH)


def main() -> int:
    started = time.perf_counter()
    panel_sha256 = sha256_of_file(PANEL_PATH)
    panel = load_panel(PANEL_PATH)
    loaded = time.perf_counter()

    print(f"panel {PANEL_PATH}")
    print(f"  rows {panel.month.size}, months {np.unique(panel.month).size}, "
          f"{panel.month.min()} to {panel.month.max()}")
    print(f"  sha256 {panel_sha256}")
    for key in sorted(panel.provenance):
        print(f"  {key} {panel.provenance[key]}")
    trainable = panel.eligible & ~np.isnan(panel.target)
    print(f"  eligible rows {int(panel.eligible.sum())}, "
          f"eligible with a label {int(trainable.sum())} (the training population)")
    print("  median-imputed after rank transform, whole panel:")
    for name, count in panel.imputed_counts.items():
        print(f"    {name:26s} {count:6d}  {count / panel.month.size:.4%}")

    last_year = int(panel.year.max())
    results = [run_refit(panel, year) for year in range(FIRST_TEST_YEAR, last_year + 1)]

    print("\nrefits")
    print(f"  {'test':>4} {'train':>11} {'valid':>9} {'n_train':>8} {'n_valid':>8} "
          f"{'n_pred':>7} {'n_score':>7} {'n_elig':>7} {'lambda':>10} {'val R2':>9} "
          f"{'test R2':>9} {'test R2 el':>10}")
    for r in results:
        print(f"  {r['test_year']:>4} {r['train_years'][0]}-{r['train_years'][1]:>4} "
              f"{r['validation_years'][0]}-{r['validation_years'][1]} "
              f"{r['n_train']:>8} {r['n_validation']:>8} {r['n_test_predicted']:>7} "
              f"{r['n_test_scored']:>7} {r['n_test_scored_eligible']:>7} "
              f"{r['alpha']:>10.4g} {r['validation_r2']:>9.4%} "
              f"{r['r2_test']:>9.4%} {r['r2_test_eligible']:>10.4%}")

    rows = np.concatenate([r["test_rows"] for r in results])
    predictions = np.concatenate([r["predictions"] for r in results])
    scored = ~np.isnan(panel.target[rows])
    scored_eligible = scored & panel.eligible[rows]
    overall = r2_oos(panel.target[rows][scored], predictions[scored])
    overall_eligible = r2_oos(panel.target[rows][scored_eligible], predictions[scored_eligible])
    print(f"\nR2_oos overall, zero-forecast denominator")
    print(f"  every labelled test row, {int(scored.sum())} rows          {overall:>9.4%}")
    print(f"  eligible labelled test rows, {int(scored_eligible.sum())} rows  "
          f"{overall_eligible:>9.4%}   (the training population and the tradable universe)")
    print("diagnostic only, same lambda refitted with an intercept, per test year:")
    print("  " + "  ".join(f"{r['test_year']}:{r['r2_test_with_intercept_diagnostic']:.3%}"
                           for r in results))

    print("\nprediction coverage per test month, predictions over panel rows in the month")
    for r in results:
        months, counts = np.unique(panel.month[r["test_rows"]], return_counts=True)
        panel_counts = np.array([int((panel.month == m).sum()) for m in months])
        eligible_counts = np.array([int(panel.eligible[panel.month == m].sum()) for m in months])
        print(f"  {r['test_year']}  months {months.size}  "
              f"predicted {int(counts.sum())}  panel rows {int(panel_counts.sum())}  "
              f"coverage {counts.sum() / panel_counts.sum():.4%}  "
              f"of which eligible {int(eligible_counts.sum())}")
        for month, count, panel_count, eligible_count in zip(months, counts, panel_counts, eligible_counts):
            print(f"    {month}  predicted {count:5d}  panel {panel_count:5d}  eligible {eligible_count:5d}")

    print("\nvalidation curve of the first refit, lambda then validation R2")
    for alpha, score in results[0]["validation_curve"]:
        print(f"  {alpha:>10.4g} {score:>10.5%}")

    print("\ncoefficients of the final refit")
    for name, coefficient in zip(CHARACTERISTICS, results[-1]["coefficients"]):
        print(f"  {name:26s} {coefficient:+.6f}")

    write_predictions(panel, rows, predictions, panel_sha256)
    finished = time.perf_counter()
    print(f"\nwrote {OUTPUT_PATH}, {rows.size} rows")
    print(f"wall time {finished - started:.1f}s, of which panel load {loaded - started:.1f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
