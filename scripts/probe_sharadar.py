#!/usr/bin/env python3
"""Probe what a Sharadar API key actually grants access to.

Targets the DIRECT retail channel at api.sharadar.com, which is a separate
credential system from the institutional channel at data.nasdaq.com. A key
issued by one will never authenticate against the other.

The key is read from .env and never printed. Error text is scrubbed before
display, because the key travels in the query string and appears in some
error messages.

Usage:  python3 scripts/probe_sharadar.py
"""

from __future__ import annotations

import json
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

BASE = "https://api.sharadar.com/v1.0/data"

# Documented limit is 2000 requests per 900s, bucketed per key. This probe makes
# well under twenty, so a small courtesy delay is enough.
MIN_INTERVAL = 0.5
TIMEOUT = 30

# Table names on the direct channel. Legacy Nasdaq names (SEP, SF1, ...) are
# aliased and still work, but the new names are canonical.
TABLES = {
    "stocks": "daily open/high/low/close/volume (was SEP)",
    "fundamentals": "income, balance, cashflow as-reported (was SF1)",
    "tickers": "ticker metadata, sectors, delisting dates",
    "daily": "market cap, P/E, EV — derived daily metrics",
    "actions": "splits, dividends, delistings, ticker changes",
    "events": "corporate events calendar",
    "sp500": "S&P 500 constituent history",
    "metrics": "computed financial ratios",
    "insiders": "insider holdings (was SF2)",
    "holdings": "institutional holdings (was SF3)",
    "funds": "ETF prices (was SFP) — expected paid-only",
}

# python.org builds on macOS ship without a usable root certificate store.
try:
    import certifi

    SSL_CONTEXT: ssl.SSLContext | None = ssl.create_default_context(cafile=certifi.where())
except ImportError:
    SSL_CONTEXT = None

_last_request_at = 0.0


class ProbeError(Exception):
    """Raised when a probe cannot complete for a reason worth reporting."""


def load_key(env_path: Path) -> str:
    """Read SHARADAR_API_KEY from a .env file, without a dotenv dependency."""
    if not env_path.exists():
        raise ProbeError(f"No .env at {env_path}. Copy .env.example and fill it in.")

    for raw in env_path.read_text().splitlines():
        line = raw.strip()
        if line.startswith("#") or "=" not in line:
            continue
        name, _, value = line.partition("=")
        if name.strip() == "SHARADAR_API_KEY":
            key = value.strip().strip("'\"")
            if not key:
                raise ProbeError("SHARADAR_API_KEY is present but empty in .env")
            return key

    raise ProbeError("SHARADAR_API_KEY not found in .env")


def key_variants(key: str) -> list[str]:
    """Every textual form the key can take in a response body.

    The key travels in the query string, so urlencode percent-escapes any of
    + / = : characters it contains. An error page or proxy that echoes the
    request URL therefore contains the ENCODED key, which a plain
    `text.replace(key, ...)` would sail straight past — while the script's
    closing banner claims the output is safe to paste.

    Percent-encoding hex is case-insensitive (%2B and %2b are both '+'), so
    both cases are generated. Longest-first ordering matters: redacting a
    shorter overlapping variant first would leave fragments of a longer one.
    """
    if not key:
        return []
    forms = {
        key,
        urllib.parse.quote(key, safe=""),
        urllib.parse.quote_plus(key),
        urllib.parse.quote(key, safe="").lower(),
        urllib.parse.quote(key, safe="").upper(),
    }
    return sorted((f for f in forms if f), key=len, reverse=True)


def scrub(text: str, key: str) -> str:
    """Remove every encoding of the API key from text before displaying it."""
    for form in key_variants(key):
        text = text.replace(form, "<REDACTED>")
    return text


def fetch(table: str, key: str, params: dict[str, str] | None = None) -> tuple[dict, dict]:
    """GET one page from a table. Returns (parsed body, response headers)."""
    global _last_request_at

    query = urllib.parse.urlencode({**(params or {}), "format": "json", "api_key": key})
    url = f"{BASE}/{table}?{query}"

    elapsed = time.monotonic() - _last_request_at
    if elapsed < MIN_INTERVAL:
        time.sleep(MIN_INTERVAL - elapsed)
    _last_request_at = time.monotonic()

    try:
        with urllib.request.urlopen(url, timeout=TIMEOUT, context=SSL_CONTEXT) as response:
            headers = dict(response.headers)
            return json.loads(response.read().decode()), headers
    except urllib.error.HTTPError as exc:
        body = exc.read().decode(errors="replace")[:300]
        raise ProbeError(f"HTTP {exc.code}: {scrub(body, key)}") from None
    except urllib.error.URLError as exc:
        reason = scrub(str(exc.reason), key)
        if "CERTIFICATE_VERIFY_FAILED" in reason:
            raise ProbeError(
                "TLS certificates missing. Run:\n"
                "         python3 -m pip install --upgrade certifi"
            ) from None
        raise ProbeError(f"network error: {reason}") from None
    except TimeoutError:
        # Unentitled tables sometimes hang rather than returning 403 promptly.
        # TimeoutError is not a URLError subclass, so it needs its own arm.
        raise ProbeError(f"timed out after {TIMEOUT}s") from None
    except json.JSONDecodeError:
        raise ProbeError("response was not valid JSON") from None
    except OSError as exc:
        raise ProbeError(f"connection failed: {scrub(str(exc), key)}") from None


def check_key_is_real(key: str) -> tuple[bool, str]:
    """Verify the key actually authenticates.

    This matters more than it looks. Free-tier row queries do not validate the
    key at all — a bogus key, or none, returns the same Dow-30 data with HTTP
    200. Only the bulk endpoint checks credentials, so it is the only honest
    test of whether a key is real.
    """
    try:
        fetch("stocks", key, {"bulk": "true"})
    except ProbeError as exc:
        message = str(exc)
        if "Unauthorized" in message or "valid API key" in message:
            return False, "rejected by the bulk endpoint — key is not valid"
        if "403" in message or "tier" in message.lower():
            return True, "authenticated; bulk download not included on this tier"
        return False, message[:150]
    return True, "authenticated, bulk download permitted"


def rows_of(payload: dict) -> list[dict]:
    data = payload.get("data", [])
    return data if isinstance(data, list) else []


def probe_table(table: str, key: str) -> tuple[bool, str]:
    """Check whether one table is readable on this key."""
    try:
        payload, _ = fetch(table, key, {"limit": "1"})
    except ProbeError as exc:
        detail = str(exc)
        if "Exceeds free tier" in detail:
            return False, "free tier does not cover this table"
        return False, detail.split("\n")[0][:110]

    rows = rows_of(payload)
    if not rows:
        return True, "entitled, no rows returned"
    return True, f"{len(rows[0])} columns"


# The tickers table spans every Sharadar dataset at once — equities, ETFs, and
# 13F institutional filers — so it must be filtered by `table` or it returns
# investor codes like "A16ZCA" alongside real symbols.
DOW_SAMPLE = ["AAPL", "MSFT", "JPM", "KO", "CAT"]
NON_DOW_SAMPLE = ["TSLA", "META", "GOOGL", "NFLX", "F"]


def probe_metadata_universe(key: str) -> int:
    """Count equities listed in the metadata table.

    Metadata visibility is not entitlement: the free tier exposes the full
    ticker catalogue while restricting which symbols actually return prices.
    """
    payload, _ = fetch("tickers", key, {"table": "SEP", "fields": "ticker", "limit": "10000"})
    return len({row.get("ticker") for row in rows_of(payload) if row.get("ticker")})


def has_price_data(ticker: str, key: str) -> tuple[bool, str]:
    """Test whether prices are actually retrievable for one symbol.

    This is the only honest entitlement test. Metadata for a symbol proves
    nothing about whether its price history is included in the tier.
    """
    try:
        payload, _ = fetch("stocks", key, {"ticker": ticker, "fields": "date,close", "limit": "1"})
    except ProbeError as exc:
        detail = str(exc)
        if "Exceeds free tier" in detail or "403" in detail:
            return False, "outside tier"
        return False, detail[:60]
    return (True, "ok") if rows_of(payload) else (False, "no rows")


def probe_history(ticker: str, key: str) -> str:
    """Report the depth of available price history for one ticker."""
    try:
        payload, _ = fetch(
            "stocks", key,
            {"ticker": ticker, "fields": "date,close", "limit": "10000", "sort": "date.asc"},
        )
    except ProbeError as exc:
        return f"unavailable ({exc})"

    rows = rows_of(payload)
    if not rows:
        return "no rows returned"
    dates = sorted(row["date"] for row in rows if row.get("date"))
    note = "  (hit the 10000-row page cap)" if len(rows) >= 10000 else ""
    return f"{len(rows):,} rows, {dates[0]} to {dates[-1]}{note}"


def probe_point_in_time(ticker: str, key: str) -> str:
    """Check whether fundamentals carry as-reported filing dates.

    Column naming differs from the Nasdaq channel: what was `datekey` there is
    simply `date` here — the day the filing became public. `calendardate` is the
    fiscal period it describes, and `reportperiod` the exact period end.

    The distinction is the whole point-in-time guarantee. Apple's Q3 2026 ended
    2026-06-27 but was not filed until 2026-07-31; joining research data on
    calendardate would use those figures five weeks before anyone could have
    known them, silently inventing alpha that no one could have traded.
    """
    try:
        payload, _ = fetch(
            "fundamentals", key,
            {"ticker": ticker, "fields": "ticker,date,calendardate,reportperiod,dimension",
             "limit": "10000", "sort": "date.asc"},
        )
    except ProbeError as exc:
        return f"unavailable ({exc})"

    rows = rows_of(payload)
    if not rows:
        return "entitled but returned no rows"
    if "date" not in rows[0]:
        return f"{len(rows):,} filings, but NO filing-date column — not point-in-time"

    filed = sorted(row["date"] for row in rows if row.get("date"))
    dimensions = sorted({row.get("dimension") for row in rows if row.get("dimension")})
    lag = ""
    if rows[-1].get("date") and rows[-1].get("reportperiod"):
        lag = f", latest filing lag {rows[-1]['reportperiod']} -> {rows[-1]['date']}"
    return (
        f"{len(rows):,} filings, {filed[0]} to {filed[-1]}{lag}\n"
        f"         dimensions available: {', '.join(dimensions) or 'none reported'}"
    )


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    try:
        key = load_key(root / ".env")
    except ProbeError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    print("Sharadar entitlement probe — direct channel (api.sharadar.com)")
    print("=" * 66)

    print("\nKEY VALIDITY")
    valid, detail = check_key_is_real(key)
    print(f"  {'valid' if valid else 'INVALID'} — {detail}")
    print("  (free-tier row queries accept any key, so this bulk check is the")
    print("   only meaningful test of whether the key is real)")

    print("\nTABLE ACCESS")
    for table, description in TABLES.items():
        ok, detail = probe_table(table, key)
        print(f"  [{'yes' if ok else ' no'}] {table:<13} {description}")
        if not ok:
            print(f"         -> {detail}")

    print("\nMETADATA CATALOGUE")
    try:
        count = probe_metadata_universe(key)
        capped = " (page cap — the real catalogue is larger)" if count >= 10000 else ""
        print(f"  {count:,} equities listed in metadata{capped}")
        print("  Metadata visibility is NOT entitlement — see below.")
    except ProbeError as exc:
        print(f"  could not enumerate: {exc}")

    print("\nPRICE ENTITLEMENT (the test that matters)")
    entitled = []
    for ticker in DOW_SAMPLE:
        ok, detail = has_price_data(ticker, key)
        print(f"  Dow      {ticker:<6} {'yes' if ok else 'no '}  {detail}")
        if ok:
            entitled.append(ticker)
    for ticker in NON_DOW_SAMPLE:
        ok, detail = has_price_data(ticker, key)
        print(f"  non-Dow  {ticker:<6} {'yes' if ok else 'no '}  {detail}")
        if ok:
            entitled.append(ticker)

    sample = entitled[0] if entitled else "AAPL"

    print("\nPRICE HISTORY DEPTH")
    for ticker in entitled[:3] or ["AAPL"]:
        print(f"  {ticker:<6} {probe_history(ticker, key)}")

    print("\nPOINT-IN-TIME FUNDAMENTALS")
    print(f"  {sample:<6} {probe_point_in_time(sample, key)}")

    print("\n" + "=" * 66)
    print("Key was never printed. Safe to paste this output.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
