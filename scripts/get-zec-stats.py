#!/usr/bin/env python3
"""Fetch Zcash stats from CoinMarketCap and Blockchair and persist them."""

from __future__ import annotations

import json
import math
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

CMC_DETAIL_URL = "https://api.coinmarketcap.com/data-api/v3/cryptocurrency/detail?id=1437"
BLOCKCHAIR_URL = "https://api.blockchair.com/zcash/stats"
HEIGHT_FALLBACK_URL = "https://zcash.blockchain.saltlending.com/blocks/tip"
OUTPUT_PATH = (
    Path(__file__).resolve().parent.parent / "public" / "data" / "zec-stats.json"
)


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
    req = urllib.request.Request(
        HEIGHT_FALLBACK_URL,
        headers={
            "Accept": "application/json",
            "User-Agent": "zcash-radio-scripts/1.0",
        },
    )

    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            if response.status != 200:
                return None
            payload = json.load(response)
    except (urllib.error.URLError, json.JSONDecodeError):
        return None

    height = payload.get("height") if isinstance(payload, dict) else None
    if isinstance(height, (int, float)) and not isinstance(height, bool):
        return max(int(height), 0)
    return None


def fetch_blockchair_stats() -> dict | None:
    req = urllib.request.Request(
        BLOCKCHAIR_URL,
        headers={
            "Accept": "application/json",
            "User-Agent": "zcash-radio-scripts/1.0",
        },
    )

    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            if response.status != 200:
                return None
            payload = json.load(response)
    except (urllib.error.URLError, json.JSONDecodeError):
        return None

    data = payload.get("data") if isinstance(payload, dict) else None
    if not isinstance(data, dict):
        return None

    height = data.get("best_block_height") or data.get("blocks")
    height_value = None
    if isinstance(height, (int, float)) and not isinstance(height, bool):
        height_value = max(int(height), 0)

    usd_price = _coerce_number(data.get("market_price_usd"))
    btc_price = _coerce_number(data.get("market_price_btc"))
    market_cap = data.get("market_cap")
    if market_cap is None:
        market_cap = data.get("market_cap_usd")
    market_cap_value = _coerce_number(market_cap)

    return {
        "height": height_value,
        "usd_zec": usd_price,
        "btc_zec": btc_price,
        "market_cap_usd": market_cap_value,
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


def _first_not_none(*values):
    for value in values:
        if value is not None:
            return value
    return None


def build_stats(cmc_payload: dict, blockchair_stats: dict | None) -> dict:
    data = cmc_payload.get("data") or {}
    symbol = data.get("symbol") or "ZEC"
    name = data.get("name") or "Zcash"
    rank = extract_rank(cmc_payload)
    statistics = data.get("statistics") or {}

    usd_price_cmc, btc_price_cmc = parse_price(statistics)
    market_cap_cmc = parse_market_cap(statistics)
    height_cmc = parse_height(statistics)

    timestamp = datetime.now(timezone.utc).isoformat()

    usd_price = _first_not_none(
        blockchair_stats.get("usd_zec") if blockchair_stats else None,
        usd_price_cmc,
    )
    btc_price = _first_not_none(
        blockchair_stats.get("btc_zec") if blockchair_stats else None,
        btc_price_cmc,
    )
    market_cap_value = _first_not_none(
        blockchair_stats.get("market_cap_usd") if blockchair_stats else None,
        market_cap_cmc,
    )
    height = _first_not_none(
        blockchair_stats.get("height") if blockchair_stats else None,
        height_cmc,
    )
    if height is None:
        height = fetch_block_height()

    milli_btc_usd: float | None = None
    if (
        usd_price is not None
        and btc_price is not None
        and btc_price > 0
    ):
        usd_per_btc = usd_price / btc_price
        if math.isfinite(usd_per_btc):
            milli_btc_usd = usd_per_btc * 0.001

    return {
        "name": name,
        "symbol": symbol,
        "rank": rank,
        "usd_zec": usd_price,
        "btc_zec": btc_price,
        "mbtc_usd": milli_btc_usd,
        "market_cap_usd": market_cap_value,
        "height": height,
        "source": CMC_DETAIL_URL,
        "sources": {
            "coinmarketcap": CMC_DETAIL_URL,
            "blockchair": BLOCKCHAIR_URL,
        },
        "fetched_at": timestamp,
    }


def main() -> int:
    req = urllib.request.Request(
        CMC_DETAIL_URL,
        headers={
            "Accept": "application/json, text/plain, */*",
            "User-Agent": "zcash-radio-scripts/1.0",
        },
    )

    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            if response.status != 200:
                print(
                    f"Error: unexpected status code {response.status} from CoinMarketCap",
                    file=sys.stderr,
                )
                return 1
            payload = json.load(response)
    except urllib.error.URLError as exc:
        print(f"Error: failed to reach CoinMarketCap API ({exc})", file=sys.stderr)
        return 1
    except json.JSONDecodeError as exc:
        print(f"Error: invalid JSON response ({exc})", file=sys.stderr)
        return 1

    blockchair_stats = fetch_blockchair_stats()
    if blockchair_stats is None:
        print("Warning: Blockchair stats unavailable; falling back to CoinMarketCap-only data", file=sys.stderr)

    stats = build_stats(payload, blockchair_stats)

    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    try:
        with OUTPUT_PATH.open("w", encoding="utf-8") as handle:
            json.dump(stats, handle, indent=2, sort_keys=True)
            handle.write("\n")
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
