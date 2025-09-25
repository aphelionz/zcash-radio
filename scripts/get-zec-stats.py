#!/usr/bin/env python3
"""Fetch Zcash stats from CoinMarketCap and persist them for the UI."""

from __future__ import annotations

import json
import math
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

CMC_DETAIL_URL = "https://api.coinmarketcap.com/data-api/v3/cryptocurrency/detail?id=1437"
HEIGHT_FALLBACK_URL = "https://zcash.blockchain.saltlending.com/blocks/tip"
OUTPUT_PATH = Path(__file__).resolve().parent.parent / "public" / "data" / "zec-stats.json"

USER_AGENT = "zcash-radio-scripts/1.0"
TIMEOUT_SECONDS = 10


def fetch_coinmarketcap_payload() -> dict:
    """Return the raw payload from CoinMarketCap or raise on failure."""

    req = urllib.request.Request(
        CMC_DETAIL_URL,
        headers={
            "Accept": "application/json, text/plain, */*",
            "User-Agent": USER_AGENT,
        },
    )

    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_SECONDS) as response:
            if response.status != 200:
                raise RuntimeError(
                    f"unexpected status code {response.status} from CoinMarketCap"
                )
            payload: Any = json.load(response)
    except urllib.error.URLError as exc:  # pragma: no cover - network failure path
        raise RuntimeError(f"failed to reach CoinMarketCap API ({exc})") from exc
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"invalid JSON payload from CoinMarketCap ({exc})") from exc

    if not isinstance(payload, dict):
        raise RuntimeError("CoinMarketCap response is not a JSON object")
    return payload


def extract_rank(payload: dict) -> int | None:
    """Return the best-effort rank from the API response."""

    data = payload.get("data") or {}
    rank = data.get("rank")
    if rank is not None:
        return rank

    statistics = data.get("statistics") or {}
    rank = statistics.get("rank")
    if rank is not None:
        return rank

    market_pairs = statistics.get("marketPairs") or {}
    return market_pairs.get("rank")


def parse_price(statistics: dict) -> tuple[float | None, float | None]:
    """Extract USD and BTC price quotes when available."""

    price_info = statistics.get("price")
    usd_price: float | None = None
    btc_price: float | None = None

    if isinstance(price_info, dict):
        usd_candidate = price_info.get("current") or price_info.get("usd")
        btc_candidate = price_info.get("btc")
        usd_price = _coerce_number(usd_candidate)
        btc_price = _coerce_number(btc_candidate)
    elif isinstance(price_info, (int, float)):
        usd_price = _coerce_number(price_info)

    return usd_price, btc_price


def parse_market_cap(statistics: dict) -> float | None:
    """Return the market cap value when available."""

    market_cap = statistics.get("marketCap")
    if isinstance(market_cap, dict):
        value = (
            market_cap.get("current")
            or market_cap.get("marketCap")
            or market_cap.get("usd")
            or market_cap.get("value")
        )
        return _coerce_number(value)
    if isinstance(market_cap, (int, float)):
        return _coerce_number(market_cap)
    return None


def parse_height(statistics: dict) -> int | None:
    """Extract a best-effort block height from the statistics payload."""

    height = (
        statistics.get("blockHeight")
        or statistics.get("height")
        or statistics.get("blocks")
    )
    if isinstance(height, (int, float)) and not isinstance(height, bool):
        rounded = int(height)
        return max(rounded, 0)
    return None


def fetch_block_height() -> int | None:
    """Fetch a fallback chain height from an alternate API."""

    req = urllib.request.Request(
        HEIGHT_FALLBACK_URL,
        headers={
            "Accept": "application/json",
            "User-Agent": USER_AGENT,
        },
    )

    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_SECONDS) as response:
            if response.status != 200:
                return None
            payload: Any = json.load(response)
    except (urllib.error.URLError, json.JSONDecodeError):  # pragma: no cover - network path
        return None

    height = payload.get("height") if isinstance(payload, dict) else None
    if isinstance(height, (int, float)) and not isinstance(height, bool):
        return max(int(height), 0)
    return None


def build_stats(cmc_payload: dict) -> dict:
    """Assemble the stats object written to disk."""

    data = cmc_payload.get("data") or {}
    symbol = data.get("symbol") or "ZEC"
    name = data.get("name") or "Zcash"
    rank = extract_rank(cmc_payload)
    statistics = data.get("statistics") or {}

    usd_price_cmc, btc_price_cmc = parse_price(statistics)
    market_cap_cmc = parse_market_cap(statistics)
    height = parse_height(statistics)

    height_sources: dict[str, str] = {}
    if height is None:
        height_fallback = fetch_block_height()
        if height_fallback is not None:
            height = height_fallback
            height_sources["height_fallback"] = HEIGHT_FALLBACK_URL

    timestamp = datetime.now(timezone.utc).isoformat()

    milli_btc_usd: float | None = None
    if (
        usd_price_cmc is not None
        and btc_price_cmc is not None
        and btc_price_cmc > 0
    ):
        usd_per_btc = usd_price_cmc / btc_price_cmc
        if math.isfinite(usd_per_btc):
            milli_btc_usd = usd_per_btc * 0.001

    sources: dict[str, str] = {"coinmarketcap": CMC_DETAIL_URL}
    sources.update(height_sources)

    return {
        "name": name,
        "symbol": symbol,
        "rank": rank,
        "usd_zec": usd_price_cmc,
        "btc_zec": btc_price_cmc,
        "mbtc_usd": milli_btc_usd,
        "market_cap_usd": market_cap_cmc,
        "height": height,
        "source": CMC_DETAIL_URL,
        "sources": sources,
        "fetched_at": timestamp,
    }


def _coerce_number(value: object) -> float | None:
    if isinstance(value, str):
        try:
            parsed = float(value)
        except ValueError:
            return None
        if math.isfinite(parsed):
            return parsed
        return None
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        as_float = float(value)
        if math.isfinite(as_float):
            return as_float
    return None


def write_stats(stats: dict) -> None:
    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with OUTPUT_PATH.open("w", encoding="utf-8") as handle:
        json.dump(stats, handle, indent=2, sort_keys=True)
        handle.write("\n")


def main() -> int:
    try:
        payload = fetch_coinmarketcap_payload()
    except RuntimeError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 1

    stats = build_stats(payload)

    try:
        write_stats(stats)
    except OSError as exc:
        print(f"Error: failed to write stats file ({exc})", file=sys.stderr)
        return 1

    try:
        rel_path = OUTPUT_PATH.relative_to(Path.cwd())
    except ValueError:
        rel_path = OUTPUT_PATH

    print(f"Saved Zcash stats to {rel_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
