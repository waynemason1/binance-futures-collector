"""
Capture Desk: lean backend for the Binance futures collector dashboard.

Reads the collector's raw output directly (a stats.json pulse + the CSV tree).
No database, no trading logic, just what the collector is actually doing.

Run:  DATA_DIR=../../data uvicorn server:app --host 127.0.0.1 --port 8000

The server binds loopback by default and trusts its LAN: anyone who can reach
the port can read data and request config changes. Expose it beyond localhost
(--host 0.0.0.0) only on a network where that is acceptable, or put it behind
a reverse proxy that authenticates.
"""
from __future__ import annotations

import csv
import io
import json
import os
import re
import shutil
import tempfile
import time
import zipfile
from collections import deque
from collections.abc import Iterator
from datetime import datetime, timedelta, timezone
from functools import lru_cache
from pathlib import Path

import tomlkit
from fastapi import Depends, FastAPI, HTTPException, Query, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, StreamingResponse
from pydantic import BaseModel
from starlette.background import BackgroundTask

# ---------------------------------------------------------------------------
# Where the collector writes. Defaults to the collector's ./data next to this
# repo; override with DATA_DIR.
# ---------------------------------------------------------------------------
DATA_DIR = Path(os.environ.get("DATA_DIR", Path(__file__).resolve().parents[2] / "data")).resolve()
FUTURES = DATA_DIR / "futures"
STATS_FILE = DATA_DIR / "stats" / "stats.json"

# The data streams the collector captures, in display order, with transport and
# a short human label. `key` matches the on-disk datatype directory name.
STREAMS = [
    {"key": "orderbooks",          "label": "Order Book",     "transport": "ws"},
    {"key": "trades",              "label": "Trades",         "transport": "ws"},
    {"key": "klines",              "label": "Klines 1m",      "transport": "ws"},
    {"key": "liquidations",        "label": "Liquidations",   "transport": "ws"},
    {"key": "funding_rates",       "label": "Funding",        "transport": "rest"},
    {"key": "open_interest",       "label": "Open Interest",  "transport": "rest"},
    {"key": "long_short_ratio",    "label": "Long / Short",   "transport": "rest"},
    {"key": "taker_buy_sell_ratio","label": "Taker Ratio",    "transport": "rest"},
    {"key": "mark_price_klines",   "label": "Mark Price",     "transport": "rest"},
    {"key": "index_price_klines",  "label": "Index Price",    "transport": "rest"},
    {"key": "premium_index_klines","label": "Premium Index",  "transport": "rest"},
]
STREAM_KEYS = [s["key"] for s in STREAMS]

# Single source of truth for the version: the collector's Cargo.toml. Hardcoding
# it here too would let the two drift, and a version you cannot trust is worse than
# no version at all.
def _project_version() -> str:
    cargo = Path(__file__).resolve().parents[2] / "Cargo.toml"
    try:
        for line in cargo.read_text().splitlines():
            if line.startswith("version"):
                return line.split('"')[1]
    except (OSError, IndexError):
        pass
    return "unknown"


VERSION = _project_version()

app = FastAPI(title="Capture Desk", version=VERSION)

# Same-origin by default: the UI reaches the API through Vite's /api proxy, so
# no CORS headers are needed at all. Hosting the UI on a different origin is
# opt-in via DASHBOARD_CORS_ORIGINS (comma-separated), never a wildcard.
_cors_origins = [o.strip() for o in os.environ.get("DASHBOARD_CORS_ORIGINS", "").split(",") if o.strip()]
if _cors_origins:
    app.add_middleware(
        CORSMiddleware, allow_origins=_cors_origins, allow_methods=["*"], allow_headers=["*"],
    )


def _require_custom_header(request: Request) -> None:
    """CSRF guard for state-changing endpoints. A browser will not attach a
    custom header cross-site without a CORS preflight, and preflight fails
    unless the origin was explicitly allowed above, so requiring one blocks
    drive-by form/fetch POSTs from malicious pages. The dashboard UI and any
    scripted caller just send `X-Capture-Desk: 1`."""
    if request.headers.get("x-capture-desk") != "1":
        raise HTTPException(403, "missing X-Capture-Desk header (CSRF guard)")


# Request inputs that become path components. Symbols and dates come straight
# from the URL, so validate their shape and confine every assembled path to the
# futures root before it touches the filesystem (defense in depth).
_FUTURES_ROOT = FUTURES.resolve()
_SYMBOL_RE = re.compile(r"^[A-Za-z0-9]{1,24}$")
_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def _check_symbol(symbol: str) -> str:
    """Reject anything that isn't a plain ticker (blocks `..`, `/`, etc.)."""
    if not _SYMBOL_RE.match(symbol):
        raise HTTPException(400, "invalid symbol")
    return symbol


def _check_date(date: str | None) -> str | None:
    """Reject anything that isn't a YYYY-MM-DD day directory name."""
    if date is not None and not _DATE_RE.match(date):
        raise HTTPException(400, "invalid date (expected YYYY-MM-DD)")
    return date


def _dir(*parts: str) -> Path:
    p = FUTURES.joinpath(*parts)
    # Catch-all: even if a caller forgot to validate, never resolve outside root.
    if parts and _FUTURES_ROOT not in p.resolve().parents and p.resolve() != _FUTURES_ROOT:
        raise HTTPException(400, "invalid path")
    return p


def _symbol_dirs(datatype: str) -> list[Path]:
    d = _dir(datatype)
    if not d.is_dir():
        return []
    return [p for p in d.iterdir() if p.is_dir()]


def _latest_mtime(path: Path) -> float:
    """Newest file mtime under `path` (0 if none)."""
    newest = 0.0
    for root, _dirs, files in os.walk(path):
        for f in files:
            if f.endswith(".csv"):
                m = os.path.getmtime(os.path.join(root, f))
                if m > newest:
                    newest = m
    return newest


def _parse_iso(s: str) -> datetime:
    """fromisoformat with two portability shims for Python < 3.11: a trailing
    'Z' and more than 6 fractional-second digits (the collector's RFC-3339
    timestamps carry nanoseconds) both choke older parsers."""
    s = re.sub(r"\.(\d{6})\d+", r".\1", str(s).replace("Z", "+00:00"))
    return datetime.fromisoformat(s)


@app.get("/api/health")
def health():
    """The live pulse: whatever the collector last wrote to stats.json."""
    if not STATS_FILE.exists():
        return {"live": False, "reason": "no stats.json yet", "data_dir": DATA_DIR.name,
                "version": VERSION}
    try:
        # The collector rewrites stats.json every 5s; a read that lands mid-write
        # can see truncated JSON, so treat that as "not live yet", never a 500.
        stats = json.loads(STATS_FILE.read_text())
    except (ValueError, OSError):
        return {"live": False, "reason": "stats.json unreadable", "data_dir": DATA_DIR.name,
                "version": VERSION}
    try:
        ts = _parse_iso(stats["timestamp"]).timestamp()
    except Exception:
        ts = os.path.getmtime(STATS_FILE)
    age = max(0.0, time.time() - ts)
    stats["age_seconds"] = round(age, 1)
    stats["live"] = age < 15  # stats.json is rewritten every 5s
    stats["data_dir"] = DATA_DIR.name
    stats["version"] = VERSION
    return stats


# How long since a symbol last wrote before we treat it as no-longer-receiving,
# sized to each stream's *full refresh cycle* so a healthy stream reads 100%.
# Constant WS streams go stale fast; the /futures/data REST metrics (long/short,
# taker) are rate-limited and take ~20 min to cycle all symbols, so their window
# must exceed that or a mid-cycle snapshot looks incomplete. Liquidations are
# event-driven (a symbol only writes when it liquidates), so it is inherently
# partial. That is expected, not a fault.
_LIVE_WINDOW = {
    "orderbooks": 90, "trades": 150, "klines": 150, "liquidations": 3600,
    "funding_rates": 1200, "open_interest": 1200,
    "long_short_ratio": 1500, "taker_buy_sell_ratio": 1500,
    "mark_price_klines": 300, "index_price_klines": 300, "premium_index_klines": 300,
}
_streams_cache: dict = {"t": 0.0, "data": None}
_STREAMS_TTL = 8.0


def _symbol_latest_mtime(sym_dir: Path) -> float:
    # Newest CSV mtime in the symbol's most recent day partition.
    days = [d for d in sym_dir.iterdir() if d.is_dir()]
    if not days:
        return 0.0
    latest = max(days, key=lambda d: d.name)
    newest = 0.0
    for f in latest.glob("*.csv"):
        m = f.stat().st_mtime
        if m > newest:
            newest = m
    return newest


def _stream_freshness(datatype: str, now: float, window: float) -> tuple[set, set, float]:
    """(all symbols, symbols written-to within the window, newest mtime)."""
    base = _dir(datatype)
    if not base.is_dir():
        return set(), set(), 0.0
    seen: set = set()
    fresh: set = set()
    newest = 0.0
    for sym_dir in base.iterdir():
        if not sym_dir.is_dir():
            continue
        seen.add(sym_dir.name)
        m = _symbol_latest_mtime(sym_dir)
        if m > newest:
            newest = m
        if m and (now - m) < window:
            fresh.add(sym_dir.name)
    return seen, fresh, newest


# Event-driven WS lanes: rows only arrive when the market produces the event
# (a trade print, a liquidation), so a connected symbol can sit outside the
# freshness window just because nobody traded it. The order book is the
# connection signal (every live symbol streams depth continuously), so for
# these lanes a non-fresh symbol whose book IS fresh is "quiet" (connected,
# market silent), and only book-dead-too counts as "stale".
_EVENT_DRIVEN_WS = ("trades", "klines", "liquidations")


@app.get("/api/streams")
def streams():
    """Per-stream status. `live_symbols` is how many symbols are still receiving
    (within the stream's cadence window); `symbols` is everything ever captured.
    Event-driven WS lanes also split the remainder into `quiet_symbols` vs
    `stale_symbols` (see _EVENT_DRIVEN_WS)."""
    now = time.time()
    if _streams_cache["data"] and now - _streams_cache["t"] < _STREAMS_TTL:
        return _streams_cache["data"]
    _, book_fresh, _ = _stream_freshness("orderbooks", now, _LIVE_WINDOW["orderbooks"])
    out = []
    for s in STREAMS:
        win = _LIVE_WINDOW.get(s["key"], 300)
        seen, fresh, newest = _stream_freshness(s["key"], now, win)
        row = {
            **s,
            "symbols": len(seen),
            "live_symbols": len(fresh),
            "last_written": round(now - newest, 1) if newest else None,
            # Use the same cadence-aware window as live_symbols, not a flat 120s, so
            # slow REST streams (funding/OI cycle ~20 min) don't blink idle mid-cycle.
            "active": bool(newest and (now - newest) < win),
        }
        if s["key"] in _EVENT_DRIVEN_WS:
            behind = seen - fresh
            quiet = behind & book_fresh
            row["quiet_symbols"] = len(quiet)
            row["stale_symbols"] = len(behind) - len(quiet)
        out.append(row)
    data = {"streams": out}
    _streams_cache.update(t=now, data=data)
    return data


@app.get("/api/coverage")
def coverage():
    """What's on disk: totals, per-stream breakdown, gaps."""
    all_symbols: set[str] = set()
    per_stream = []
    total_files = 0
    for s in STREAMS:
        syms = _symbol_dirs(s["key"])
        files = sum(1 for _r, _d, fs in os.walk(_dir(s["key"])) for f in fs if f.endswith(".csv"))
        total_files += files
        for p in syms:
            all_symbols.add(p.name)
        per_stream.append({"key": s["key"], "label": s["label"],
                           "transport": s["transport"], "symbols": len(syms), "files": files})

    gaps_dir = FUTURES / "gaps"
    gaps = 0
    if gaps_dir.is_dir():
        gaps = sum(1 for _r, _d, fs in os.walk(gaps_dir) for _f in fs)

    return {
        "symbols_total": len(all_symbols),
        "files_total": total_files,
        "gaps": gaps,
        "streams": per_stream,
        "data_dir": DATA_DIR.name,
    }


# A symbol quiet longer than this is delisted/dead, not "recently gone quiet",
# excluded so a freshly-stalled live symbol isn't crowded out of the attention list.
_STALE_CEILING = 6 * 3600


def _stalest(datatype: str, now: float, window: float, limit: int) -> list[dict]:
    """Symbols whose latest write is older than the cadence window but not so old
    they're clearly delisted: live symbols that recently went quiet."""
    base = _dir(datatype)
    if not base.is_dir():
        return []
    stale = []
    for sym_dir in base.iterdir():
        if not sym_dir.is_dir():
            continue
        m = _symbol_latest_mtime(sym_dir)
        age = (now - m) if m else None
        if age is not None and window <= age < _STALE_CEILING:
            stale.append({"symbol": sym_dir.name.upper(), "age": round(age)})
    stale.sort(key=lambda x: -x["age"])
    return stale[:limit]


_health_cache: dict = {"t": 0.0, "data": None}


@app.get("/api/health/coverage")
def health_coverage():
    """What's captured over time + which symbols have gone quiet. Live per-stream
    receiving counts are the Console's job; this view is about on-disk coverage and
    symbols that need attention, so it doesn't duplicate the Console pulse."""
    now = time.time()
    if _health_cache["data"] and now - _health_cache["t"] < 8.0:
        return _health_cache["data"]
    per = [{**s, "symbols": len(_symbol_dirs(s["key"]))} for s in STREAMS]
    # Order-book freshness is the canonical "is this symbol alive" signal: every live
    # symbol streams constant depth diffs, so a stale book means a stalled symbol.
    attention = _stalest("orderbooks", now, _LIVE_WINDOW["orderbooks"], 12)
    # Days present per datatype. Collection is universe-wide, so a canonical always-on
    # symbol's date dirs represent the stream's coverage (cheap: one iterdir per stream).
    coverage = {}
    for s in STREAMS:
        base = _dir(s["key"])
        ref = base / "btcusdt"
        if not ref.is_dir() and base.is_dir():
            subs = [d for d in base.iterdir() if d.is_dir()]
            ref = subs[0] if subs else None
        coverage[s["key"]] = (
            sorted((d.name for d in ref.iterdir() if d.is_dir()), reverse=True)[:14]
            if ref and ref.is_dir() else []
        )
    data = {"streams": per, "attention": attention, "coverage": coverage}
    _health_cache.update(t=now, data=data)
    return data


@app.get("/api/gaps/{symbol}")
def gaps_for(symbol: str):
    """Recorded resync/gap events for a symbol, from the collector's gap registry.
    Empty is the healthy case; populated when the collector detects a stream break."""
    _check_symbol(symbol)
    path = DATA_DIR / "gaps" / f"{symbol.lower()}_gaps.json"
    events: list = []
    if path.is_file():
        try:
            parsed = json.loads(path.read_text())
            if isinstance(parsed, list):
                events = parsed
        except (ValueError, OSError):
            pass
    return {"symbol": symbol.upper(), "events": events[-200:]}


@app.get("/api/symbols")
def symbols():
    """Every symbol with captured klines, live ones first.

    `active` means the collector wrote a kline for it in the last 2 minutes,
    i.e. it's streaming right now. Directories left behind by earlier runs are
    still listed (so on-disk history stays browsable) but sorted last, so the
    picker leads with what is actually live.
    """
    now = time.time()
    have: dict[str, list[str]] = {}
    for s in STREAMS:
        for p in _symbol_dirs(s["key"]):
            have.setdefault(p.name, []).append(s["key"])

    out = []
    for sym, keys in have.items():
        kdir = _dir("klines", sym)
        newest = _latest_mtime(kdir) if kdir.is_dir() else 0.0
        out.append({
            "symbol": sym.upper(),
            "id": sym,
            "streams": sorted(keys),
            "active": bool(newest and (now - newest) < 120),
            "last_kline": round(now - newest, 1) if newest else None,
        })
    out.sort(key=lambda r: (not r["active"], r["id"]))
    return {"symbols": out}


def _latest_csv(datatype: str, symbol: str, day: str | None = None) -> Path | None:
    base = _dir(datatype, symbol)
    if not base.is_dir():
        return None
    scope = base / day if day else base
    if not scope.is_dir():
        return None
    csvs = sorted(scope.rglob("*.csv"))
    return csvs[-1] if csvs else None


def _tail_rows(path: Path, n: int = 200) -> list[dict]:
    """Last `n` data rows without reading the whole file: walk 64 KiB blocks
    backwards from EOF until the tail holds `n` complete lines, then parse just
    those with the header row. Day files reach hundreds of MB (btcusdt trades)
    and this runs on every poll; materializing the full file as dicts was the
    dashboard's biggest memory draw."""
    with path.open("rb") as f:
        header = f.readline()
        data_start = f.tell()
        f.seek(0, os.SEEK_END)
        pos = f.tell()
        if pos <= data_start:
            return []
        buf = b""
        while pos > data_start and buf.count(b"\n") <= n:
            step = min(65536, pos - data_start)
            pos -= step
            f.seek(pos)
            buf = f.read(step) + buf
        if pos > data_start:
            # Landed mid-line: drop the partial first line.
            buf = buf[buf.find(b"\n") + 1:]
    if not buf.endswith(b"\n"):
        # The collector may be mid-append; never parse a torn last row.
        buf = buf[:buf.rfind(b"\n") + 1]
    text = header.decode("utf-8", "replace") + buf.decode("utf-8", "replace")
    return list(csv.DictReader(io.StringIO(text)))[-n:]


def _candles(symbol: str, n: int = 180) -> list[dict]:
    """Aggregate raw kline update-rows into one OHLC candle per minute (last wins)."""
    p = _latest_csv("klines", symbol)
    if not p:
        return []
    by_min: dict[str, dict] = {}
    with p.open() as f:
        for row in csv.DictReader(f):
            try:
                int(row["timestamp_ms"])  # skip a corrupt row so the sort can't 500
            except (ValueError, KeyError, TypeError):
                continue
            by_min[row["timestamp_ms"]] = row
    rows = sorted(by_min.values(), key=lambda r: int(r["timestamp_ms"]))[-n:]
    out = []
    for r in rows:
        try:
            out.append({
                "time": int(r["timestamp_ms"]) // 1000,
                "open": float(r["open"]), "high": float(r["high"]),
                "low": float(r["low"]), "close": float(r["close"]),
                "volume": float(r.get("volume", 0) or 0),
            })
        except (ValueError, KeyError):
            continue
    return out


def _recent_trades(symbol: str, n: int = 60) -> list[dict]:
    p = _latest_csv("trades", symbol)
    if not p:
        return []
    out = []
    for r in _tail_rows(p, n):
        try:
            out.append({
                "t": int(r["timestamp_ms"]),
                "price": float(r["price"]),
                "qty": float(r["quantity"]),
                "side": r.get("side", ""),
            })
        except (ValueError, KeyError):
            continue
    return list(reversed(out))  # newest first


def _latest(datatype: str, symbol: str, ratio_type: str | None = None) -> dict | None:
    p = _latest_csv(datatype, symbol)
    if not p:
        return None
    # long_short_ratio interleaves three series per poll (global, top_trader_account,
    # top_trader_position), so a blind last-row grab returns whichever was written
    # last. When a ratio_type is requested, read a small tail and pick the newest
    # row of that series.
    rows = _tail_rows(p, 12 if ratio_type else 1)
    if ratio_type:
        rows = [r for r in rows if r.get("ratio_type") == ratio_type]
    return rows[-1] if rows else None


@app.get("/api/symbol/{symbol}")
def symbol_detail(symbol: str):
    """Per-symbol drill-down: candles, trade tape, and latest derivatives metrics."""
    symbol = _check_symbol(symbol).lower()
    present = [s["key"] for s in STREAMS if _dir(s["key"], symbol).is_dir()]
    if not present:
        raise HTTPException(404, f"no data for {symbol.upper()}")

    candles = _candles(symbol, 180)
    price = candles[-1]["close"] if candles else None
    prev = candles[0]["close"] if candles else None
    change = ((price - prev) / prev * 100) if (price and prev) else None

    fund = _latest("funding_rates", symbol)
    oi = _latest("open_interest", symbol)
    ls = _latest("long_short_ratio", symbol, ratio_type="global")  # the global account ratio

    return {
        "symbol": symbol.upper(),
        "streams": present,
        "price": price,
        "change_pct": change,
        "change_span_min": len(candles),  # the % is over this many 1m candles, not 24h
        "candles": candles,
        "trades": _recent_trades(symbol, 60),
        "funding_rate": float(fund["funding_rate"]) if fund else None,
        "open_interest_usd": float(oi["oi_value"]) if oi else None,
        "long_short_ratio": float(ls["long_short_ratio"]) if ls else None,
    }


# ---------------------------------------------------------------------------
# Log files: list every log the collector has written (current + rotations)
# and tail or download a chosen one.
# ---------------------------------------------------------------------------
LOG_DIR = Path(os.environ.get("LOG_DIR", DATA_DIR.parent / "logs")).resolve()


def _log_files() -> list[Path]:
    if not LOG_DIR.is_dir():
        return []
    return sorted(LOG_DIR.glob("*.log"), key=lambda p: p.stat().st_mtime, reverse=True)


def _resolve_log(name: str | None) -> Path | None:
    files = _log_files()
    if not files:
        return None
    if not name:
        return files[0]
    # Accept only a bare filename that resolves to a file directly in LOG_DIR.
    candidate = (LOG_DIR / Path(name).name).resolve()
    if candidate.parent == LOG_DIR and candidate.is_file():
        return candidate
    return None


def _tail(path: Path, n: int) -> list[str]:
    """Last `n` lines without reading the whole file."""
    with path.open("rb") as f:
        f.seek(0, os.SEEK_END)
        size = f.tell()
        buf = b""
        while size > 0 and buf.count(b"\n") <= n:
            step = min(65536, size)
            size -= step
            f.seek(size)
            buf = f.read(step) + buf
    return buf.decode("utf-8", "replace").splitlines()[-n:]


_MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun",
           "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]


def _log_label(name: str) -> str:
    """A readable name for a log file: the active log is 'Current session', and a
    rotated log shows the time it was rolled (its filename timestamp)."""
    if "rCURRENT" in name:
        return "Current session"
    m = re.search(r"(\d{4})-(\d{2})-(\d{2})_(\d{2})-(\d{2})", name)
    if m:
        y, mo, d, hh, mm = m.groups()
        return f"{_MONTHS[int(mo) - 1]} {int(d)}, {y} at {hh}:{mm}"
    return name


@app.get("/api/logs/list")
def logs_list():
    files = [
        {"name": p.name, "label": _log_label(p.name),
         "size": p.stat().st_size, "mtime": int(p.stat().st_mtime)}
        for p in _log_files()
    ]
    return {"files": files}


@app.get("/api/logs")
def logs(file: str | None = None, lines: int = 300):
    lines = max(1, min(lines, 5000))
    path = _resolve_log(file)
    if not path:
        return {"file": None, "label": None, "lines": []}
    return {"file": path.name, "label": _log_label(path.name), "lines": _tail(path, lines)}


@app.get("/api/logs/download")
def logs_download(file: str | None = None):
    path = _resolve_log(file)
    if not path:
        raise HTTPException(status_code=404, detail="log not found")
    return FileResponse(path, media_type="text/plain", filename=path.name)


# ---------------------------------------------------------------------------
# Order-book reconstruction & data validation
#
# Replays a symbol's persisted depth from the latest snapshot forward, verifies
# update-ID continuity, and checks the reconstructed book never crosses. Also
# spot-checks trade-ID uniqueness/order and 1-minute kline continuity. This is
# the collector's integrity story made checkable against its own output.
# ---------------------------------------------------------------------------
def _orderbook_files(symbol: str, day: str | None = None) -> tuple[list[Path], list[Path]]:
    # Scope to one day. Default is the most recent day the symbol has data for;
    # reconstruction only needs the latest snapshot plus the diffs that follow it,
    # and a single day keeps the replay bounded regardless of run length.
    base = _dir("orderbooks", symbol)
    if not base.is_dir():
        return [], []
    if day:
        scan = base / day
        if not scan.is_dir():
            return [], []
    else:
        days = sorted(d for d in base.iterdir() if d.is_dir())
        scan = days[-1] if days else base
    updates = sorted(scan.rglob("*depth_updates*.csv"))
    snaps = sorted(scan.rglob("*depth_snapshot*.csv"))
    return updates, snaps


def _apply_level(book: dict, price: float, qty: float) -> None:
    # Key by float so the snapshot's "61573.599999999999" and the diff's "61573.6"
    # resolve to the same level (otherwise removals never match and levels pile up).
    if qty == 0.0:
        book.pop(price, None)
    else:
        book[price] = qty


def _reconstruct_orderbook(symbol: str, max_diffs: int = 200_000, day: str | None = None) -> dict:
    upd_files, snap_files = _orderbook_files(symbol, day)
    depth = {"rows": 0, "continuity_breaks": 0, "first_break": None,
             "first_ts": None, "last_ts": None}
    recon = {"snapshot_id": None, "diffs_applied": 0, "crossed_books": 0,
             "seed_crossed": False, "best_bid": None, "best_ask": None}

    # Base the rebuild on the newest snapshot by update id, not by filename. The
    # startup snapshot sorts last alphabetically but is the oldest by content;
    # choosing it would replay hours of moved-away levels that never get cleared,
    # leaving a permanently crossed book.
    bids: dict[float, float] = {}
    asks: dict[float, float] = {}
    by_id: dict[int, list[dict]] = {}
    for sf in snap_files:
        with sf.open() as f:
            for r in csv.DictReader(f):
                try:
                    by_id.setdefault(int(r["last_update_id"]), []).append(r)
                except (ValueError, KeyError):
                    continue
    snap_id = max(by_id) if by_id else None
    if snap_id is not None:
        for r in by_id[snap_id]:
            (bids if r["side"] == "bid" else asks)[float(r["price"])] = float(r["quantity"])
    recon["snapshot_id"] = snap_id
    # Audit the seed itself: a periodic snapshot can be stale-crossed after
    # severe stream gaps, and crossed_books only counts crossings after applied
    # diffs, so without this a crossed seed reads as clean.
    recon["seed_crossed"] = bool(bids and asks and max(bids) > min(asks))

    # One chronological pass over the day's diffs: verify update-id continuity
    # across the whole stream, and replay the levels recorded after the snapshot.
    prev_u = None
    applied = 0
    for uf in upd_files:
        with uf.open() as f:
            for r in csv.DictReader(f):
                depth["rows"] += 1
                # Captured tape span: lets the UI report how much of the day the
                # checks cover, so a partial tape isn't read as a clean full day.
                try:
                    ts = int(r["timestamp_ms"])
                    if depth["first_ts"] is None:
                        depth["first_ts"] = ts
                    depth["last_ts"] = ts
                except (ValueError, KeyError):
                    pass
                try:
                    pu, u = int(r["prev_update_id"]), int(r["last_update_id"])
                except (ValueError, KeyError):
                    continue
                if prev_u is not None and pu != prev_u:
                    depth["continuity_breaks"] += 1
                    if depth["first_break"] is None:
                        depth["first_break"] = {"expected_pu": prev_u, "got_pu": pu}
                prev_u = u
                if snap_id is None or u <= snap_id or applied >= max_diffs:
                    continue
                try:
                    for price, qty in json.loads(r["bids_json"]):
                        _apply_level(bids, float(price), float(qty))
                    for price, qty in json.loads(r["asks_json"]):
                        _apply_level(asks, float(price), float(qty))
                except (ValueError, KeyError):
                    continue
                applied += 1
                if bids and asks and max(bids) > min(asks):  # strict: a locked book (bid==ask) isn't crossed
                    recon["crossed_books"] += 1

    recon["diffs_applied"] = applied
    recon["best_bid"] = max(bids) if bids else None
    recon["best_ask"] = min(asks) if asks else None
    return {"depth_updates": depth, "reconstruction": recon}


def _rows(datatype: str, symbol: str, day: str | None = None) -> Iterator[dict]:
    """Stream a day file's rows lazily. A day of trades runs to 100s of MB, so
    validators single-pass this instead of materializing the whole file."""
    p = _latest_csv(datatype, symbol, day)
    if not p:
        return
    with p.open() as f:
        yield from csv.DictReader(f)


def _fnum(v) -> float | None:
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def _report(datatype: str, rows: int, checks: list[dict]) -> dict:
    return {"datatype": datatype, "rows": rows, "checks": checks,
            "ok": all(c["failures"] == 0 for c in checks)}


def _val_ohlc(datatype: str, symbol: str, with_volume: bool = False, day: str | None = None) -> dict:
    ts: list[int] = []
    seen: set[int] = set()
    dup = bad_ohlc = bad_vol = n = 0
    for r in _rows(datatype, symbol, day):
        n += 1
        t = _fnum(r.get("timestamp_ms"))
        o, h, low, c = (_fnum(r.get(k)) for k in ("open", "high", "low", "close"))
        if None in (t, o, h, low, c):
            continue
        t = int(t)
        ts.append(t)
        if t in seen:
            dup += 1
        seen.add(t)
        if not (h >= low and h >= o and h >= c and low <= o and low <= c):
            bad_ohlc += 1
        if with_volume:
            v = _fnum(r.get("volume"))
            if v is None or v < 0:
                bad_vol += 1
    ordered = sorted(set(ts))
    # Count MISSING minutes, not gap events: one 3h outage is 180 missing, not 1.
    missing = sum((b - a) // 60_000 - 1 for a, b in zip(ordered, ordered[1:]) if b - a > 60_000)
    checks = [
        {"name": "1-minute continuity (missing candles)", "failures": missing},
        {"name": "OHLC bounds (high=max, low=min)", "failures": bad_ohlc},
        {"name": "unique timestamps", "failures": dup},
    ]
    if with_volume:
        checks.append({"name": "volume >= 0", "failures": bad_vol})
    return _report(datatype, n, checks)


def _val_trades(symbol: str, day: str | None = None) -> dict:
    seen: set[str] = set()
    dup = ooo = bad_px = bad_side = n = 0
    last = None
    for r in _rows("trades", symbol, day):
        n += 1
        tid = r.get("trade_id", "")
        if tid in seen:
            dup += 1
        seen.add(tid)
        try:
            cur = int(tid)
            if last is not None and cur < last:
                ooo += 1
            last = cur
        except (TypeError, ValueError):
            pass
        px, qty = _fnum(r.get("price")), _fnum(r.get("quantity"))
        if px is None or qty is None or px <= 0 or qty <= 0:
            bad_px += 1
        if r.get("side") not in ("buy", "sell"):
            bad_side += 1
    return _report("trades", n, [
        {"name": "unique trade_id", "failures": dup},
        {"name": "monotonic trade_id", "failures": ooo},
        {"name": "price & qty > 0", "failures": bad_px},
        {"name": "side in {buy, sell}", "failures": bad_side},
    ])


def _val_liquidations(symbol: str, day: str | None = None) -> dict:
    bad_px = bad_val = n = 0
    for r in _rows("liquidations", symbol, day):
        n += 1
        px, qty, val = (_fnum(r.get(k)) for k in ("price", "quantity", "value"))
        if px is None or qty is None or px <= 0 or qty <= 0:
            bad_px += 1
        elif val is None or abs(val - px * qty) > max(1e-6, abs(px * qty) * 1e-4):
            bad_val += 1  # a missing value column is a failure, not a pass
    return _report("liquidations", n, [
        {"name": "price & qty > 0", "failures": bad_px},
        {"name": "value = price x qty", "failures": bad_val},
    ])


def _val_open_interest(symbol: str, day: str | None = None) -> dict:
    bad_oi = bad_val = n = 0
    for r in _rows("open_interest", symbol, day):
        n += 1
        oi, px, val = (_fnum(r.get(k)) for k in ("open_interest", "mark_price", "oi_value"))
        if oi is None or oi < 0:
            bad_oi += 1
        elif px is None or val is None or abs(val - oi * px) > max(1.0, abs(oi * px) * 1e-3):
            bad_val += 1  # a missing mark_price / oi_value is a failure, not a pass
    return _report("open_interest", n, [
        {"name": "open_interest >= 0", "failures": bad_oi},
        {"name": "oi_value = OI x mark_price", "failures": bad_val},
    ])


def _val_funding(symbol: str, day: str | None = None) -> dict:
    bad_rate = n = 0
    for r in _rows("funding_rates", symbol, day):
        n += 1
        rate = _fnum(r.get("funding_rate"))
        # Binance funding is a fraction ~0.0001 with a cap near 0.0075 (0.75%);
        # 3% is a generous envelope that still catches a mis-scaled/corrupt rate.
        if rate is None or abs(rate) > 0.03:
            bad_rate += 1
    return _report("funding_rates", n, [
        {"name": "funding_rate finite & bounded (|r|<=3%)", "failures": bad_rate},
    ])


def _val_long_short(symbol: str, day: str | None = None) -> dict:
    present: set = set()
    bad_sum = bad_ratio = n = 0
    for r in _rows("long_short_ratio", symbol, day):
        n += 1
        present.add(r.get("ratio_type"))
        lr, sr, rt = (_fnum(r.get(k)) for k in ("long_ratio", "short_ratio", "long_short_ratio"))
        if lr is None or sr is None:
            continue
        if abs((lr + sr) - 1.0) > 0.02:  # all three ratio types' long+short sum to 1
            bad_sum += 1
        if rt is not None and sr > 0 and abs(rt - lr / sr) > max(1e-3, rt * 1e-2):
            bad_ratio += 1
    expected = {"global", "top_trader_account", "top_trader_position"}
    return _report("long_short_ratio", n, [
        {"name": "all 3 ratio types present", "failures": len(expected - present)},
        {"name": "long + short ~= 1", "failures": bad_sum},
        {"name": "ratio = long / short", "failures": bad_ratio},
    ])


def _val_taker(symbol: str, day: str | None = None) -> dict:
    bad_vol = bad_ratio = n = 0
    for r in _rows("taker_buy_sell_ratio", symbol, day):
        n += 1
        b, s, rt = (_fnum(r.get(k)) for k in ("buy_volume", "sell_volume", "buy_sell_ratio"))
        if b is None or s is None or b < 0 or s < 0:
            bad_vol += 1
        elif rt is not None and s > 0 and abs(rt - b / s) > max(1e-3, rt * 1e-2):
            bad_ratio += 1
    return _report("taker_buy_sell_ratio", n, [
        {"name": "volumes >= 0", "failures": bad_vol},
        {"name": "ratio = buy / sell", "failures": bad_ratio},
    ])


def _datetime_check(datatype: str, symbol: str, day: str | None = None) -> dict:
    """Verify the datetime_utc column is present and agrees with timestamp_ms."""
    bad = 0
    for r in _rows(datatype, symbol, day):
        try:
            ts = int(r["timestamp_ms"])
            parsed = _parse_iso(r["datetime_utc"])
            if abs(int(parsed.timestamp() * 1000) - ts) > 1:
                bad += 1
        except (AttributeError, KeyError, ValueError, TypeError):
            bad += 1
    return {"name": "datetime_utc matches timestamp_ms", "failures": bad}


VALIDATORS = {
    "klines": lambda s, d: _val_ohlc("klines", s, with_volume=True, day=d),
    "mark_price_klines": lambda s, d: _val_ohlc("mark_price_klines", s, day=d),
    "index_price_klines": lambda s, d: _val_ohlc("index_price_klines", s, day=d),
    "premium_index_klines": lambda s, d: _val_ohlc("premium_index_klines", s, day=d),
    "trades": _val_trades,
    "liquidations": _val_liquidations,
    "open_interest": _val_open_interest,
    "funding_rates": _val_funding,
    "long_short_ratio": _val_long_short,
    "taker_buy_sell_ratio": _val_taker,
}


def _boundary_row(path: Path, last: bool) -> dict | None:
    """First or last data row of a CSV, without reading the whole file."""
    try:
        with path.open("rb") as f:
            header = f.readline()
            if last:
                size = f.seek(0, 2)
                f.seek(max(len(header), size - 65_536))
                tail = [ln for ln in f.read().splitlines() if ln.strip()]
                line = tail[-1] if tail else None
                if line is None or line == header.strip():
                    return None
            else:
                line = f.readline().strip()
                if not line:
                    return None
        hdr = next(csv.reader([header.decode()]))
        row = next(csv.reader([line.decode()]))
        if len(row) != len(hdr):
            return None
        return dict(zip(hdr, row))
    except (OSError, StopIteration, UnicodeDecodeError):
        return None


def _midnight_handoff(symbol: str, day: str) -> str | None:
    """Cross-day order-book continuity: the first diff of `day` must chain
    (pu == u) from the last diff of the previous day. None when either side
    has no capture: a first day has nothing to hand off from."""
    try:
        prev = (datetime.strptime(day, "%Y-%m-%d") - timedelta(days=1)).strftime("%Y-%m-%d")
    except ValueError:
        return None
    prev_dir = _dir("orderbooks", symbol) / prev
    day_dir = _dir("orderbooks", symbol) / day
    prev_files = sorted(prev_dir.glob("*depth_updates*.csv")) if prev_dir.is_dir() else []
    day_files = sorted(day_dir.glob("*depth_updates*.csv")) if day_dir.is_dir() else []
    if not prev_files or not day_files:
        return None
    last = _boundary_row(prev_files[-1], last=True)
    first = _boundary_row(day_files[0], last=False)
    if not last or not first:
        return None
    try:
        return "ok" if int(first["prev_update_id"]) == int(last["last_update_id"]) else "break"
    except (KeyError, ValueError):
        return None


@app.get("/api/dates/{symbol}")
def dates(symbol: str):
    """Every UTC day this symbol has data for (any stream), newest first."""
    symbol = _check_symbol(symbol).lower()
    days: set[str] = set()
    for s in STREAMS:
        base = _dir(s["key"], symbol)
        if base.is_dir():
            days.update(d.name for d in base.iterdir() if d.is_dir())
    return {"dates": sorted(days, reverse=True)}


@app.get("/api/validate/{symbol}")
def validate(symbol: str, date: str | None = None):
    symbol = _check_symbol(symbol).lower()
    _check_date(date)

    def has(key: str) -> bool:
        base = _dir(key, symbol)
        return (base / date).is_dir() if date else base.is_dir()

    present = [s["key"] for s in STREAMS if has(s["key"])]
    if not present:
        where = f"{symbol.upper()} on {date}" if date else symbol.upper()
        raise HTTPException(404, f"no data for {where}")

    orderbook = _reconstruct_orderbook(symbol, day=date) if "orderbooks" in present else None
    if orderbook:
        ob_base = _dir("orderbooks", symbol)
        days_dirs = sorted(d.name for d in ob_base.iterdir() if d.is_dir()) if ob_base.is_dir() else []
        eff_day = date or (days_dirs[-1] if days_dirs else None)
        orderbook["depth_updates"]["midnight_handoff"] = (
            _midnight_handoff(symbol, eff_day) if eff_day else None
        )
    types = []
    for k in VALIDATORS:
        if k not in present:
            continue
        rep = VALIDATORS[k](symbol, date)
        rep["checks"].append(_datetime_check(k, symbol, date))
        rep["ok"] = all(c["failures"] == 0 for c in rep["checks"])
        types.append(rep)

    ob_ok = orderbook is None or (
        orderbook["depth_updates"]["continuity_breaks"] == 0
        and orderbook["reconstruction"]["crossed_books"] == 0
        and not orderbook["reconstruction"]["seed_crossed"]
        and orderbook["depth_updates"]["midnight_handoff"] != "break"
    )
    clean = ob_ok and all(t["ok"] for t in types)
    return {
        "symbol": symbol.upper(),
        "date": date,
        "verdict": "clean" if clean else "issues",
        "orderbook": orderbook,
        "types": types,
    }


# ---------------------------------------------------------------------------
# Data export
#
# Slice the CSV tree by time in two shapes:
#   • bundle: one native CSV per (symbol, datatype), zipped. Faithful and
#     lossless; every file keeps its own schema.
#   • merged: one time-aligned CSV on a 1-minute grid. Periodic series join by
#     timestamp, the order book contributes derived top-of-book columns, and the
#     trade/liquidation streams are aggregated per bucket. The feature-matrix view.
# Plus a point-in-time order-book export: the reconstructed book as of an instant.
# ---------------------------------------------------------------------------
_MINUTE = 60_000
# Cap export windows so a client-supplied range can't allocate an unbounded grid
# (one minute cell per row) or walk an unbounded span of day files.
_MAX_EXPORT_SPAN_MS = 31 * 24 * 60 * 60 * 1000  # 31 days
_MAX_EXPORT_SYMBOLS = 50  # the merged grid holds every symbol's columns at once


def _check_span(from_ms: int, to_ms: int) -> None:
    if from_ms > to_ms:
        raise HTTPException(400, "from must be <= to")
    if to_ms - from_ms > _MAX_EXPORT_SPAN_MS:
        raise HTTPException(400, "range too large (max 31 days)")


# Sanity bounds for client-supplied instants: 2015-01-01 .. 2100-01-01 (the
# exchange launched in 2017; nothing here is scheduled past 2100). Anything
# outside is a typo or probe, and huge values overflow datetime → 500.
_MIN_TS_MS = 1_420_070_400_000
_MAX_TS_MS = 4_102_444_800_000


def _parse_ms(s: str) -> int:
    # Accept epoch millis, epoch seconds, or ISO-8601 (optionally with a Z).
    # These come from client query params (?ts=/?from=/?to=), so a malformed
    # value is a 400, never an unhandled 500.
    s = s.strip()
    if s.isdigit():
        if len(s) > 13:  # longest valid epoch-ms this side of year 2100
            raise HTTPException(400, "timestamp out of range")
        v = int(s)
        v = v if v > 10_000_000_000 else v * 1000
    else:
        try:
            dt = datetime.fromisoformat(s.replace("Z", "+00:00"))
        except ValueError:
            raise HTTPException(400, "invalid timestamp (expected epoch ms/s or ISO-8601)")
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        try:
            v = int(dt.timestamp() * 1000)
        except (OverflowError, OSError, ValueError):
            raise HTTPException(400, "timestamp out of range")
    if not _MIN_TS_MS <= v <= _MAX_TS_MS:
        raise HTTPException(400, "timestamp out of range")
    return v


def _minute(ms: int) -> int:
    return ms - (ms % _MINUTE)


def _date_dirs_in_range(base: Path, from_ms: int, to_ms: int) -> list[Path]:
    lo = datetime.fromtimestamp(from_ms / 1000, timezone.utc).date()
    hi = datetime.fromtimestamp(to_ms / 1000, timezone.utc).date()
    out = []
    for d in sorted(base.iterdir()):
        if not d.is_dir():
            continue
        try:
            day = datetime.strptime(d.name, "%Y-%m-%d").date()
        except ValueError:
            continue
        if lo <= day <= hi:
            out.append(d)
    return out


def _iter_rows(datatype, symbol, from_ms, to_ms, name_sub=None):
    """Yield (timestamp_ms, row) for a datatype/symbol within [from,to], in order."""
    base = _dir(datatype, symbol.lower())
    if not base.is_dir():
        return
    for day in _date_dirs_in_range(base, from_ms, to_ms):
        for path in sorted(day.glob("*.csv")):
            if name_sub and name_sub not in path.name:
                continue
            with path.open() as f:
                for r in csv.DictReader(f):
                    try:
                        ts = int(r["timestamp_ms"])
                    except (ValueError, KeyError, TypeError):
                        continue
                    if from_ms <= ts <= to_ms:
                        yield ts, r


def _slice_to_csv(dst, datatype, symbol, from_ms, to_ms, name_sub=None) -> int:
    """Write header + in-range rows for one datatype/symbol to a text stream."""
    base = _dir(datatype, symbol.lower())
    if not base.is_dir():
        return 0
    writer = None
    n = 0
    for day in _date_dirs_in_range(base, from_ms, to_ms):
        for path in sorted(day.glob("*.csv")):
            if name_sub and name_sub not in path.name:
                continue
            with path.open() as f:
                reader = csv.DictReader(f)
                if not reader.fieldnames:
                    continue
                if writer is None:
                    # extrasaction='ignore': if a later day-file added a column,
                    # drop the extra rather than 500 on the fixed header.
                    writer = csv.DictWriter(dst, fieldnames=reader.fieldnames, extrasaction="ignore")
                    writer.writeheader()
                for r in reader:
                    try:
                        ts = int(r["timestamp_ms"])
                    except (ValueError, KeyError, TypeError):
                        continue
                    if from_ms <= ts <= to_ms:
                        writer.writerow(r)
                        n += 1
    return n


# The order book lives in two schemas; the bundle exports each on its own.
_BUNDLE_PARTS = {
    "orderbooks": [("orderbook_updates", "depth_updates"),
                   ("orderbook_snapshots", "depth_snapshot")],
}


def _selection(symbols: str, types: str) -> tuple[list[str], list[str]]:
    syms = [_check_symbol(s.strip()).lower() for s in symbols.split(",") if s.strip()]
    tps = [t.strip() for t in types.split(",") if t.strip() in STREAM_KEYS]
    if not syms or not tps:
        raise HTTPException(400, "symbols and types are required")
    if len(syms) > _MAX_EXPORT_SYMBOLS:
        raise HTTPException(400, f"too many symbols (max {_MAX_EXPORT_SYMBOLS})")
    return syms, tps


@app.get("/api/export/bundle")
def export_bundle(
    symbols: str,
    types: str,
    start: str = Query(alias="from"),
    end: str = Query(alias="to"),
):
    syms, tps = _selection(symbols, types)
    from_ms, to_ms = _parse_ms(start), _parse_ms(end)
    _check_span(from_ms, to_ms)

    tmp = tempfile.NamedTemporaryFile(suffix=".zip", delete=False)
    written = 0
    try:
        with zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED) as z:
            for sym in syms:
                for t in tps:
                    for name, sub in _BUNDLE_PARTS.get(t, [(t, None)]):
                        # Slice through an on-disk temp file so a day of trades
                        # (100s of MB) never lives in a StringIO. A plain text
                        # tempfile is used instead of SpooledTemporaryFile +
                        # TextIOWrapper, which raises on Python < 3.11.
                        part = tempfile.TemporaryFile(mode="w+", encoding="utf-8", newline="")
                        try:
                            if _slice_to_csv(part, t, sym, from_ms, to_ms, sub):
                                part.seek(0)
                                with z.open(f"{sym}/{name}.csv", "w") as dst:
                                    while True:
                                        chunk = part.read(1 << 20)
                                        if not chunk:
                                            break
                                        dst.write(chunk.encode("utf-8"))
                                written += 1
                        finally:
                            part.close()
        tmp.close()
        # Don't hand back a valid-but-empty zip; the user can't tell "no data" apart.
        if written == 0:
            os.unlink(tmp.name)
            raise HTTPException(404, "no data for that selection and window")
    except BaseException:
        tmp.close()
        if os.path.exists(tmp.name):
            os.unlink(tmp.name)
        raise
    fname = f"binance_export_{from_ms}_{to_ms}.zip"
    return FileResponse(tmp.name, media_type="application/zip", filename=fname,
                        background=BackgroundTask(os.unlink, tmp.name))


# --- merged grid helpers ---------------------------------------------------
def _ffill(grid, rows, extract, put):
    rows = sorted(rows, key=lambda x: x[0])
    i, n, last = 0, len(rows), None
    for m in grid:
        while i < n and rows[i][0] <= m:
            last = rows[i][1]
            i += 1
        if last is not None:
            put(m, extract(last))


def _book_series(grid, symbol, from_ms, to_ms, put):
    # Best bid/ask from the nearest periodic snapshot at or before each minute.
    snaps: dict[int, dict] = {}
    for ts, r in _iter_rows("orderbooks", symbol, from_ms - 3_600_000, to_ms, "depth_snapshot"):
        try:
            price = float(r["price"])
        except (KeyError, ValueError):
            continue
        b = snaps.setdefault(ts, {"bid": None, "ask": None})
        if r.get("side") == "bid":
            b["bid"] = price if b["bid"] is None else max(b["bid"], price)
        else:
            b["ask"] = price if b["ask"] is None else min(b["ask"], price)
    series = sorted(snaps.items())
    i, n, last = 0, len(series), None
    for m in grid:
        while i < n and series[i][0] <= m:
            last = series[i][1]
            i += 1
        if last and last["bid"] is not None and last["ask"] is not None:
            bid, ask = last["bid"], last["ask"]
            put(m, {"best_bid": bid, "best_ask": ask,
                    "mid": round((bid + ask) / 2, 8), "spread": round(ask - bid, 8)})


def _agg_trades(grid, symbol, from_ms, to_ms, put):
    buckets: dict[int, list] = {}
    for ts, r in _iter_rows("trades", symbol, from_ms, to_ms):
        try:
            price, qty = float(r["price"]), float(r["quantity"])
        except (KeyError, ValueError):
            continue
        b = buckets.setdefault(_minute(ts), [0, 0.0, 0.0])
        b[0] += 1
        b[1] += price * qty
        b[2] += qty
    for m, (c, pv, q) in buckets.items():
        put(m, {"trade_count": c, "trade_vwap": round(pv / q, 8) if q else ""})


def _agg_liquidations(grid, symbol, from_ms, to_ms, put):
    buckets: dict[int, list] = {}
    for ts, r in _iter_rows("liquidations", symbol, from_ms, to_ms):
        try:
            val = float(r.get("value") or 0)
        except ValueError:
            val = 0.0
        b = buckets.setdefault(_minute(ts), [0, 0.0])
        b[0] += 1
        b[1] += val
    for m, (c, v) in buckets.items():
        put(m, {"liq_count": c, "liq_value": round(v, 4)})


def _merge_one(t, symbol, grid, from_ms, to_ms, put) -> list[str]:
    if t == "klines":
        names = ["open", "high", "low", "close", "volume"]
        for ts, r in _iter_rows(t, symbol, from_ms, to_ms):
            put(_minute(ts), {k: r.get(k, "") for k in names})
        return names
    if t in ("mark_price_klines", "index_price_klines", "premium_index_klines"):
        tag = {"mark_price_klines": "mark_close", "index_price_klines": "index_close",
               "premium_index_klines": "premium_close"}[t]
        for ts, r in _iter_rows(t, symbol, from_ms, to_ms):
            put(_minute(ts), {tag: r.get("close", "")})
        return [tag]
    if t == "funding_rates":
        _ffill(grid, _iter_rows(t, symbol, from_ms - 8 * 3_600_000, to_ms),
               lambda r: {"funding_rate": r.get("funding_rate", "")}, put)
        return ["funding_rate"]
    if t == "open_interest":
        _ffill(grid, _iter_rows(t, symbol, from_ms - 3_600_000, to_ms),
               lambda r: {"open_interest": r.get("open_interest", "")}, put)
        return ["open_interest"]
    if t == "taker_buy_sell_ratio":
        _ffill(grid, _iter_rows(t, symbol, from_ms - 3_600_000, to_ms),
               lambda r: {"taker_buy_sell_ratio": r.get("buy_sell_ratio", "")}, put)
        return ["taker_buy_sell_ratio"]
    if t == "long_short_ratio":
        rows = ((ts, r) for ts, r in _iter_rows(t, symbol, from_ms - 3_600_000, to_ms)
                if r.get("ratio_type") == "global")
        _ffill(grid, rows, lambda r: {"long_short_ratio": r.get("long_short_ratio", "")}, put)
        return ["long_short_ratio"]
    if t == "orderbooks":
        _book_series(grid, symbol, from_ms, to_ms, put)
        return ["best_bid", "best_ask", "mid", "spread"]
    if t == "trades":
        _agg_trades(grid, symbol, from_ms, to_ms, put)
        return ["trade_count", "trade_vwap"]
    if t == "liquidations":
        _agg_liquidations(grid, symbol, from_ms, to_ms, put)
        return ["liq_count", "liq_value"]
    return []


@app.get("/api/export/merged")
def export_merged(
    symbols: str,
    types: str,
    start: str = Query(alias="from"),
    end: str = Query(alias="to"),
):
    syms, tps = _selection(symbols, types)
    from_ms, to_ms = _parse_ms(start), _parse_ms(end)
    _check_span(from_ms, to_ms)

    multi = len(syms) > 1
    grid = list(range(_minute(from_ms), to_ms + 1, _MINUTE))
    cells = {m: {} for m in grid}
    columns: list[str] = []
    for sym in syms:
        prefix = f"{sym}_" if multi else ""

        def put(minute, mapping, _p=prefix):
            c = cells.get(minute)
            if c is not None:
                for k, v in mapping.items():
                    c[_p + k] = v

        for t in tps:
            columns.extend(prefix + n for n in _merge_one(t, sym, grid, from_ms, to_ms, put))

    # Consistent with the bundle: an empty result (an unknown symbol, or a window
    # with no collected data) is a 404, not a valid-but-empty grid the caller can't
    # tell apart from "the market was silent".
    if not any(cells[m] for m in grid):
        raise HTTPException(404, "no data for that selection and window")

    header = ["timestamp_ms", "datetime_utc"] + columns

    def gen():
        buf = io.StringIO()
        w = csv.writer(buf)
        w.writerow(header)
        yield buf.getvalue()
        buf.seek(0), buf.truncate(0)
        for m in grid:
            row = cells[m]
            # Trailing-Z form to match every native per-type CSV's datetime column.
            iso = datetime.fromtimestamp(m / 1000, timezone.utc).isoformat().replace("+00:00", "Z")
            w.writerow([m, iso] + [row.get(c, "") for c in columns])
            yield buf.getvalue()
            buf.seek(0), buf.truncate(0)

    fname = f"binance_merged_{from_ms}_{to_ms}.csv"
    return StreamingResponse(gen(), media_type="text/csv",
                             headers={"Content-Disposition": f'attachment; filename="{fname}"'})


def _reconstruct_at(symbol, at_ms):
    """Rebuild the book as of `at_ms`: newest snapshot at/before it, then the
    diffs up to it. Independent of the validation helper, which is day-scoped."""
    base = _dir("orderbooks", symbol.lower())
    if not base.is_dir():
        return {}, {}, None, None, False
    days = _date_dirs_in_range(base, at_ms - 86_400_000, at_ms)
    snap_files, upd_files = [], []
    for d in days:
        snap_files += sorted(d.glob("*depth_snapshot*.csv"))
        upd_files += sorted(d.glob("*depth_updates*.csv"))

    by: dict[tuple, list] = {}
    for sf in snap_files:
        with sf.open() as f:
            for r in csv.DictReader(f):
                try:
                    ts, sid = int(r["timestamp_ms"]), int(r["last_update_id"])
                except (KeyError, ValueError):
                    continue
                if ts <= at_ms:
                    by.setdefault((ts, sid), []).append(r)
    if not by:
        return {}, {}, None, None, False
    snap_ts, snap_id = max(by)
    bids, asks = {}, {}
    for r in by[(snap_ts, snap_id)]:
        (bids if r["side"] == "bid" else asks)[float(r["price"])] = float(r["quantity"])

    # Only rows after the snapshot and at/before `at_ms` matter, and the hourly
    # file names say what they hold, so skip whole files outside that window
    # instead of parsing a day's worth of diffs per seek.
    def overlaps(p: Path) -> bool:
        m = re.search(r"_(\d{4}-\d{2}-\d{2})_(\d{2})00-to-", p.name)
        if not m:
            return True
        start = int(datetime.strptime(m.group(1), "%Y-%m-%d")
                    .replace(tzinfo=timezone.utc).timestamp() * 1000) + int(m.group(2)) * 3_600_000
        return start <= at_ms and start + 3_600_000 > snap_ts

    last_u = None  # set by the first applied diff; then the pu chain is checked
    gap = False    # a broken pu chain (missed diffs after a reconnect) makes the book suspect
    for uf in upd_files:
        if not overlaps(uf):
            continue
        with uf.open() as f:
            for r in csv.DictReader(f):
                try:
                    ts, u, pu = int(r["timestamp_ms"]), int(r["last_update_id"]), int(r["prev_update_id"])
                except (KeyError, ValueError):
                    continue
                if ts > at_ms or u <= snap_id:
                    continue
                if last_u is not None and pu != last_u:
                    gap = True
                last_u = u
                try:
                    for price, qty in json.loads(r["bids_json"]):
                        _apply_level(bids, float(price), float(qty))
                    for price, qty in json.loads(r["asks_json"]):
                        _apply_level(asks, float(price), float(qty))
                except (ValueError, KeyError):
                    continue
    return bids, asks, snap_id, snap_ts, gap


@app.get("/api/export/orderbook")
def export_orderbook_at(symbol: str, at: str):
    sym = _check_symbol(symbol).lower()
    at_ms = _parse_ms(at)
    bids, asks, snap_id, _, _ = _reconstruct_at(sym, at_ms)
    if snap_id is None:
        raise HTTPException(404, f"no order-book snapshot for {symbol.upper()} at/before that time")

    def gen():
        buf = io.StringIO()
        w = csv.writer(buf)
        w.writerow(["side", "price", "quantity"])
        yield buf.getvalue()
        buf.seek(0), buf.truncate(0)
        for price in sorted(bids, reverse=True):
            w.writerow(["bid", price, bids[price]])
            yield buf.getvalue()
            buf.seek(0), buf.truncate(0)
        for price in sorted(asks):
            w.writerow(["ask", price, asks[price]])
            yield buf.getvalue()
            buf.seek(0), buf.truncate(0)

    stamp = datetime.fromtimestamp(at_ms / 1000, timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    fname = f"{sym}_orderbook_{stamp}.csv"
    return StreamingResponse(gen(), media_type="text/csv",
                             headers={"Content-Disposition": f'attachment; filename="{fname}"'})


# ---------------------------------------------------------------------------
# Live order book (depth ladder)
#
# Rebuild the current book cheaply enough to poll: latest snapshot in the newest
# day + only the last one or two hourly diff files (snapshots are ~10 min apart,
# so the diffs that follow the newest snapshot are in the current hour's file).
# ---------------------------------------------------------------------------
def _current_book(symbol: str):
    base = _dir("orderbooks", symbol.lower())
    if not base.is_dir():
        return None
    days = sorted(d for d in base.iterdir() if d.is_dir())
    if not days:
        return None
    day = days[-1]
    snap_files = sorted(day.glob("*depth_snapshot*.csv"))
    upd_files = sorted(day.glob("*depth_updates*.csv"))[-2:]

    by_id: dict[int, list] = {}
    for sf in snap_files:
        with sf.open() as f:
            for r in csv.DictReader(f):
                try:
                    by_id.setdefault(int(r["last_update_id"]), []).append(r)
                except (KeyError, ValueError):
                    continue
    if not by_id:
        return None
    snap_id = max(by_id)
    bids: dict[float, float] = {}
    asks: dict[float, float] = {}
    for r in by_id[snap_id]:
        (bids if r["side"] == "bid" else asks)[float(r["price"])] = float(r["quantity"])

    for uf in upd_files:
        with uf.open() as f:
            for r in csv.DictReader(f):
                try:
                    if int(r["last_update_id"]) <= snap_id:
                        continue
                    for p, q in json.loads(r["bids_json"]):
                        _apply_level(bids, float(p), float(q))
                    for p, q in json.loads(r["asks_json"]):
                        _apply_level(asks, float(p), float(q))
                except (KeyError, ValueError):
                    continue
    return bids, asks


@app.get("/api/orderbook/{symbol}")
def orderbook_now(symbol: str, levels: int = 20):
    _check_symbol(symbol)
    book = _current_book(symbol)
    if book is None:
        raise HTTPException(404, f"no order book for {symbol.upper()}")
    bids, asks = book
    levels = max(1, min(levels, 200))
    top_bids = sorted(bids.items(), key=lambda kv: -kv[0])[:levels]
    top_asks = sorted(asks.items(), key=lambda kv: kv[0])[:levels]

    def rows(levs):
        out, cum = [], 0.0
        for price, qty in levs:
            cum += qty
            out.append({"price": price, "qty": round(qty, 8), "cumulative": round(cum, 8)})
        return out

    bb = top_bids[0][0] if top_bids else None
    ba = top_asks[0][0] if top_asks else None
    return {
        "symbol": symbol.upper(),
        "best_bid": bb,
        "best_ask": ba,
        "mid": round((bb + ba) / 2, 8) if (bb is not None and ba is not None) else None,
        "spread": round(ba - bb, 8) if (bb is not None and ba is not None) else None,
        "bids": rows(top_bids),
        "asks": rows(top_asks),
    }


# ---------------------------------------------------------------------------
# Collector configuration
#
# Read and write the subset of config.toml worth editing from the UI. The symbol
# whitelist and the collection cadence. Writes go through tomlkit so the file's
# comments and layout survive. Applying changes needs a collector restart, and the
# dashboard never spawns or kills a process: it drops a restart-request sentinel
# that a supervisor (supervise.sh, systemd, or Docker) acts on.
# ---------------------------------------------------------------------------
CONFIG_FILE = Path(os.environ.get("COLLECTOR_CONFIG", DATA_DIR.parent / "config.toml")).resolve()
CONTROL_DIR = DATA_DIR / "control"
_REST_KEYS = [
    "funding_rates_seconds", "open_interest_seconds", "long_short_ratio_seconds",
    "taker_buy_sell_ratio_seconds", "mark_price_klines_seconds",
    "index_price_klines_seconds", "premium_index_klines_seconds",
]
_SPEEDS = (100, 250, 500)


class ConfigPatch(BaseModel):
    collect_all: bool
    symbols: list[str]
    update_speed_ms: int
    rest_intervals: dict[str, int]


@app.get("/api/config")
def get_config():
    if not CONFIG_FILE.is_file():
        raise HTTPException(404, f"config not found at {CONFIG_FILE.name}")
    doc = tomlkit.parse(CONFIG_FILE.read_text())
    syms = [str(s) for s in doc.get("collection", {}).get("symbols", [])]
    ob = doc.get("orderbook", {})
    ri = doc.get("rest_intervals", {})
    return {
        "path": CONFIG_FILE.name,
        "collect_all": len(syms) == 0,
        "symbols": syms,
        "update_speed_ms": int(ob.get("update_speed_ms", 500)),
        "rest_intervals": {k: int(ri.get(k, 0)) for k in _REST_KEYS},
    }


@app.post("/api/config", dependencies=[Depends(_require_custom_header)])
def set_config(patch: ConfigPatch):
    if patch.update_speed_ms not in _SPEEDS:
        raise HTTPException(400, f"update_speed_ms must be one of {_SPEEDS}")
    if not CONFIG_FILE.is_file():
        raise HTTPException(404, f"config not found at {CONFIG_FILE.name}")
    # Same shape rule as path symbols: these strings land in config.toml and
    # come back as filesystem lookups, so reject anything non-alphanumeric.
    whitelist = sorted({_check_symbol(s.strip()).lower() for s in patch.symbols if s.strip()})
    # An empty symbol list means "collect all" to the loader, so refuse to save
    # whitelist-mode with nothing selected; otherwise it silently captures the
    # entire universe, the opposite of what the user asked for.
    if not patch.collect_all and not whitelist:
        raise HTTPException(400, "whitelist mode requires at least one symbol")
    doc = tomlkit.parse(CONFIG_FILE.read_text())
    doc.setdefault("collection", tomlkit.table())["symbols"] = [] if patch.collect_all else whitelist
    doc.setdefault("orderbook", tomlkit.table())["update_speed_ms"] = patch.update_speed_ms
    ri = doc.setdefault("rest_intervals", tomlkit.table())
    for k, v in patch.rest_intervals.items():
        if k in _REST_KEYS and isinstance(v, int) and v > 0:
            ri[k] = v
    CONFIG_FILE.write_text(tomlkit.dumps(doc))
    return {"ok": True, "path": CONFIG_FILE.name}


@app.post("/api/restart", dependencies=[Depends(_require_custom_header)])
def restart():
    # The dashboard never manages the process itself. It records intent and lets
    # a supervisor act. Safe on any host; a no-op if nothing is supervising.
    CONTROL_DIR.mkdir(parents=True, exist_ok=True)
    (CONTROL_DIR / "restart.request").write_text(str(int(time.time())))
    return {"requested": True, "sentinel": "restart.request"}


# ---------------------------------------------------------------------------
# Replay: rebuild any captured instant.
#
# One clock interrogates every stream: the order book is rebuilt from the
# nearest snapshot plus diffs (periodic snapshots every ~10 min bound each
# rebuild to a few thousand rows, so seeks are fast), the tape shows the trades
# leading up to the instant, the slow REST series answer "as of then", and
# liquidations mark the timeline.
# ---------------------------------------------------------------------------
def _day_of(ts_ms: int) -> str:
    return datetime.fromtimestamp(ts_ms / 1000, timezone.utc).strftime("%Y-%m-%d")


@lru_cache(maxsize=8)
def _cached_rows(path: str, _mtime: float) -> tuple:
    """A day file's rows, cache-keyed by mtime so a growing file re-reads.
    Only the smaller lanes come through here (REST series, klines, liquidations
    (trades/orderbooks stream via _iter_rows/_bisect_csv), but a klines day
    still runs tens of MB as dicts, so cap the cache at 8 days to bound RAM."""
    with open(path, newline="") as f:
        return tuple(csv.DictReader(f))


def _day_rows(datatype: str, symbol: str, day: str) -> tuple:
    p = _latest_csv(datatype, symbol, day)
    if not p:
        return ()
    return _cached_rows(str(p), p.stat().st_mtime)


def _asof_row(datatype: str, symbol: str, at_ms: int) -> dict | None:
    """Last row at/before `at_ms`, looking back to the previous day if needed
    (funding settles 8-hourly, so the freshest row can be yesterday's)."""
    for day_ms in (at_ms, at_ms - 86_400_000):
        best = None
        for r in _day_rows(datatype, symbol, _day_of(day_ms)):
            try:
                ts = int(r["timestamp_ms"])
            except (KeyError, ValueError):
                continue
            if ts <= at_ms and (best is None or ts > int(best["timestamp_ms"])):
                best = r
        if best:
            return best
    return None


def _line_start_at(f, off: int, header_end: int) -> int:
    """Snap a byte offset to the start of the next full line."""
    if off <= header_end:
        return header_end
    f.seek(off - 1)
    f.readline()
    return f.tell()


def _bisect_csv(path: Path, ts_ms: int) -> int:
    """Byte offset of the first data row with timestamp_ms >= ts_ms. Rows are
    chronological, so binary-search byte offsets and snap to line starts,
    O(log n) seeks even on a multi-million-row trades file."""
    size = path.stat().st_size
    with path.open("rb") as f:
        header_end = len(f.readline())
        lo, hi = header_end, size
        while lo < hi:
            mid = (lo + hi) // 2
            start = _line_start_at(f, mid, header_end)
            f.seek(start)
            line = f.readline()
            ts = None
            if line:
                try:
                    ts = int(line.split(b",", 4)[3])  # trades layout only
                except (IndexError, ValueError):
                    ts = None
            if ts is None or ts >= ts_ms:  # EOF/garbage → search left
                hi = mid
            else:
                lo = mid + 1
        return _line_start_at(f, lo, header_end)


def _tape_before(symbol: str, at_ms: int, n: int) -> list[dict]:
    """The last `n` trades at/before `at_ms`, widening the lookback until the
    window catches prints (quiet alts can print nothing for minutes)."""
    p = _latest_csv("trades", symbol, _day_of(at_ms))
    if not p:
        return []
    with p.open(newline="") as probe:
        fields = next(csv.reader(probe))
    for lookback in (60_000, 900_000, 86_400_000):
        off = _bisect_csv(p, at_ms - lookback)
        out: deque = deque(maxlen=n)
        with p.open(newline="") as f:
            f.seek(off)
            for r in csv.DictReader(f, fieldnames=fields):
                try:
                    ts = int(r["timestamp_ms"])
                except (KeyError, ValueError, TypeError):
                    continue
                if ts > at_ms:
                    break
                out.append({"t": ts, "price": float(r["price"]),
                            "qty": float(r["quantity"]), "side": r.get("side", "")})
        if len(out) >= n:  # only stop once the tape is actually full
            return list(out)
    # No window filled `n`; the widest (last) pass holds the most we could gather.
    return list(out)


@app.get("/api/replay/{symbol}/day")
def replay_day(symbol: str, date: str):
    """The scrubber's map of one UTC day: 1-minute closes + volume, every
    liquidation as a timeline event, and whether the day has book capture."""
    sym = _check_symbol(symbol).lower()
    _check_date(date)
    klines = _day_rows("klines", sym, date)
    price = []
    for r in klines:
        try:
            price.append({"t": int(r["timestamp_ms"]), "c": float(r["close"]),
                          "v": float(r["volume"])})
        except (KeyError, ValueError):
            continue
    if not price:
        raise HTTPException(404, f"no capture for {symbol.upper()} on {date}")
    events = []
    for r in _day_rows("liquidations", sym, date):
        try:
            events.append({"t": int(r["timestamp_ms"]), "side": r.get("side", ""),
                           "price": float(r["price"]), "qty": float(r["quantity"]),
                           "value": float(r["value"])})
        except (KeyError, ValueError):
            continue
    return {
        "symbol": symbol.upper(),
        "date": date,
        "price": price,
        "events": events,
        "has_book": (_dir("orderbooks", sym) / date).is_dir(),
        "range": {"from": price[0]["t"], "to": price[-1]["t"] + _MINUTE},
    }


@app.get("/api/replay/{symbol}/at")
def replay_at(symbol: str, ts: str, levels: int = 25, tape: int = 30):
    """Everything the capture knows about one instant."""
    sym = _check_symbol(symbol).lower()
    at_ms = _parse_ms(ts)
    bids, asks, snap_id, snap_ts, book_gap = _reconstruct_at(sym, at_ms)
    if snap_id is None:
        raise HTTPException(404, "no order-book snapshot at or before that instant "
                                 "step forward past the day's first snapshot")

    def side(levels_map: dict, reverse: bool) -> list[dict]:
        out, cum = [], 0.0
        for px in sorted(levels_map, reverse=reverse)[:levels]:
            cum += levels_map[px]
            out.append({"price": px, "qty": levels_map[px], "cumulative": cum})
        return out

    top_bids, top_asks = side(bids, True), side(asks, False)
    best_bid = top_bids[0]["price"] if top_bids else None
    best_ask = top_asks[0]["price"] if top_asks else None

    candle = _asof_row("klines", sym, at_ms)
    funding = _asof_row("funding_rates", sym, at_ms)
    oi = _asof_row("open_interest", sym, at_ms)
    ls = None
    # Look back to the previous day too (early in a UTC day the freshest global
    # sample can be yesterday's), mirroring _asof_row.
    for day_ms in (at_ms, at_ms - 86_400_000):
        for r in reversed(_day_rows("long_short_ratio", sym, _day_of(day_ms))):
            try:
                if r.get("ratio_type") == "global" and int(r["timestamp_ms"]) <= at_ms:
                    ls = r
                    break
            except (KeyError, ValueError):
                continue
        if ls:
            break

    events = []
    for r in _day_rows("liquidations", sym, _day_of(at_ms)):
        try:
            t = int(r["timestamp_ms"])
        except (KeyError, ValueError):
            continue
        if at_ms - 3_000 < t <= at_ms:
            events.append({"t": t, "side": r.get("side", ""), "price": float(r["price"]),
                           "qty": float(r["quantity"]), "value": float(r["value"])})

    return {
        "symbol": symbol.upper(),
        "ts": at_ms,
        "book": {
            "bids": top_bids, "asks": top_asks,
            "best_bid": best_bid, "best_ask": best_ask,
            "mid": (best_bid + best_ask) / 2 if best_bid and best_ask else None,
            "spread": (best_ask - best_bid) if best_bid and best_ask else None,
            "snapshot_id": snap_id, "snapshot_ts": snap_ts, "gap": book_gap,
        },
        "tape": _tape_before(sym, at_ms, tape),
        "asof": {
            "close": float(candle["close"]) if candle else None,
            "funding_rate": float(funding["funding_rate"]) if funding else None,
            "oi_value": float(oi["oi_value"]) if oi else None,
            "long_short": float(ls["long_short_ratio"]) if ls else None,
        },
        "events": events,
    }


# --- Serve the built dashboard UI (Docker / `setup.sh` native run) -----------
# Guarded by SERVE_STATIC=1 so the Vite dev server (which proxies /api here) is
# never shadowed in development. Mounted LAST so every /api route matches first.
if os.environ.get("SERVE_STATIC") == "1":
    from fastapi.staticfiles import StaticFiles

    _DIST = Path(__file__).resolve().parents[1] / "frontend" / "dist"
    if _DIST.is_dir():
        app.mount("/", StaticFiles(directory=str(_DIST), html=True), name="ui")
