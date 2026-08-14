"""Pins for the ridge fit that must keep failing if someone breaks them.

Run with `uv run python test_leak_pin.py` from the ml directory. Plain asserts,
no framework, so there is nothing to install and pytest can still collect it.

The one that matters is the leak pin. It is not enough for the guard to work
when called directly, because a guard nobody calls is a comment, so the second
test drives a whole refit through the real code path with a deliberately
broken split and requires the failure.
"""

from __future__ import annotations

import numpy as np
from sklearn.linear_model import Ridge

import fit_ridge
from fit_ridge import (
    FIRST_TRAIN_YEAR,
    VALIDATION_YEARS,
    LeakError,
    Panel,
    assert_no_test_leak,
    r2_oos,
    rank_to_unit_interval,
    run_refit,
    split_years,
)

FIRST_SYNTHETIC_YEAR = FIRST_TRAIN_YEAR
LAST_SYNTHETIC_YEAR = 2012
NAMES_PER_MONTH = 40


def synthetic_panel() -> Panel:
    """A panel shaped like the real one and large enough to fit, nothing more.

    The features are already rank-transformed in the real pipeline, so the
    synthetic ones are drawn straight from [-1, 1] and the target carries a
    little signal plus noise. No vendor rows, per the fixture rule.
    """
    rng = np.random.default_rng(20260814)
    starts = np.arange(
        f"{FIRST_SYNTHETIC_YEAR}-01", f"{LAST_SYNTHETIC_YEAR + 1}-01", dtype="datetime64[M]"
    )
    month_ends = (starts + np.timedelta64(1, "M")).astype("datetime64[D]") - np.timedelta64(1, "D")
    month = np.repeat(month_ends, NAMES_PER_MONTH)
    n = month.size
    features = rng.uniform(-1.0, 1.0, size=(n, len(fit_ridge.CHARACTERISTICS)))
    target = features @ np.full(len(fit_ridge.CHARACTERISTICS), 0.01) + rng.normal(0.0, 0.1, n)
    identity = np.array([f"SYN{i % NAMES_PER_MONTH:03d}" for i in range(n)], dtype=object)
    return Panel(
        ticker=identity,
        permanent_id_kind=np.full(n, "synthetic", dtype=object),
        permanent_id=identity,
        month=month,
        year=month.astype("datetime64[Y]").astype(int) + 1970,
        features=features,
        target=target,
        eligible=np.ones(n, dtype=bool),
        imputed_counts={name: 0 for name in fit_ridge.CHARACTERISTICS},
        provenance={key: "0" * 64 for key in fit_ridge.REQUIRED_PROVENANCE_KEYS},
    )


def leaked_split_years(test_year: int) -> tuple[tuple[int, ...], tuple[int, ...]]:
    """The realistic mistake, an off-by-one that runs validation through the
    test year instead of stopping the year before it."""
    validation_start = test_year - VALIDATION_YEARS + 1
    return (
        tuple(range(FIRST_TRAIN_YEAR, validation_start)),
        tuple(range(validation_start, test_year + 1)),
    )


def test_guard_rejects_a_test_year_inside_validation() -> None:
    test_month = np.array(["2012-01-31", "2012-02-29"], dtype="datetime64[D]")
    clean = np.array(["2011-11-30", "2011-12-31"], dtype="datetime64[D]")
    assert_no_test_leak(2012, clean, clean, test_month)  # the honest split passes

    try:
        assert_no_test_leak(2012, clean, np.concatenate([clean, test_month[:1]]), test_month)
    except LeakError as error:
        assert "validation" in str(error)
    else:
        raise AssertionError("guard accepted a validation window holding a test month")


def test_refit_refuses_a_leaked_split() -> None:
    """The guard has to be wired into the path that fits, not merely exist."""
    panel = synthetic_panel()
    honest = run_refit(panel, LAST_SYNTHETIC_YEAR)
    assert honest["n_train"] > 0 and honest["n_validation"] > 0

    original = fit_ridge.split_years
    fit_ridge.split_years = leaked_split_years
    try:
        run_refit(panel, LAST_SYNTHETIC_YEAR)
    except LeakError:
        pass
    else:
        raise AssertionError("a refit tuned on the test year and nothing stopped it")
    finally:
        fit_ridge.split_years = original


def test_provenance_passes_unknown_keys_through_and_still_demands_the_known_ones() -> None:
    """A digest added by the panel writer must reach the predictions file
    without an edit here, and a missing known key must still be fatal."""
    full = {key.encode(): b"0" * 64 for key in fit_ridge.REQUIRED_PROVENANCE_KEYS}
    carried = fit_ridge.read_provenance(
        {**full, b"prices_sha256": b"beef" * 16, b"ARROW:schema": b"ignored"}
    )
    assert carried["prices_sha256"] == "beef" * 16
    assert "ARROW:schema" not in carried
    assert set(fit_ridge.REQUIRED_PROVENANCE_KEYS) <= set(carried)

    without_config_hash = {k: v for k, v in full.items() if k != b"config_hash"}
    try:
        fit_ridge.read_provenance(without_config_hash)
    except ValueError as error:
        assert "config_hash" in str(error)
    else:
        raise AssertionError("a panel with no config_hash was accepted")


def test_r2_uses_the_zero_forecast_denominator() -> None:
    """A demeaned denominator would score the mean forecast at zero here."""
    actual = np.array([0.01, 0.02, 0.03])
    predicted = np.full(3, 0.02)
    assert abs(r2_oos(actual, predicted) - (1.0 - 2.0 / 14.0)) < 1e-12


def test_rank_transform_spans_the_interval_and_averages_ties() -> None:
    assert np.allclose(rank_to_unit_interval(np.array([5.0, 1.0, 3.0])), [1.0, -1.0, 0.0])
    assert np.allclose(rank_to_unit_interval(np.array([1.0, 1.0, 3.0])), [-0.5, -0.5, 1.0])


def test_sklearn_alpha_is_the_unscaled_ridge_penalty() -> None:
    """Pins what a grid point means. sklearn adds alpha to X'X directly rather
    than scaling it by the sample size, so a lambda of 1e2 against 30,000 rows
    is a light penalty and the grid has to be read that way."""
    rng = np.random.default_rng(7)
    x = rng.normal(size=(200, 5))
    y = rng.normal(size=200)
    alpha = 3.7
    fitted = Ridge(alpha=alpha, fit_intercept=False, solver="cholesky").fit(x, y).coef_
    closed_form = np.linalg.solve(x.T @ x + alpha * np.eye(5), x.T @ y)
    assert np.allclose(fitted, closed_form, atol=1e-10)


def test_split_years_matches_the_spec() -> None:
    assert split_years(2012) == (tuple(range(1999, 2009)), (2009, 2010, 2011))
    assert split_years(2013) == (tuple(range(1999, 2010)), (2010, 2011, 2012))


if __name__ == "__main__":
    for name, case in sorted(globals().items()):
        if name.startswith("test_") and callable(case):
            case()
            print(f"ok  {name}")
