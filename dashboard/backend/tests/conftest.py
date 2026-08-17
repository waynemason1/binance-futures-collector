"""Test harness for the dashboard backend.

Builds a synthetic capture tree (same layout + headers the Rust collector
writes) in a temp DATA_DIR, points the server at it via env vars, and exposes
row builders so each test can seed clean or deliberately-corrupted data.
"""
from __future__ import annotations

import csv
import json
import os
import shutil
import sys
from datetime import datetime, timezone
from pathlib import Path

import pytest

BACKEND = Path(__file__).resolve().parents[1]
REPO = BACKEND.parents[1]
sys.path.insert(0, str(BACKEND))

# The server resolves its paths at import time, so env must be set first.
_TMP = Path(os.environ.get("PYTEST_DATA_DIR", "")) if os.environ.get("PYTEST_DATA_DIR") else None


def pytest_configure(config):
    global _TMP
    if _TMP is None:
        import tempfile

        _TMP = Path(tempfile.mkdtemp(prefix="capture-desk-tests-"))
    (_TMP / "futures").mkdir(parents=True, exist_ok=True)
    (_TMP / "logs").mkdir(exist_ok=True)
    cfg = _TMP / "config.toml"
    shutil.copy(REPO / "config.example.toml", cfg)
    os.environ["DATA_DIR"] = str(_TMP)
    os.environ["LOG_DIR"] = str(_TMP / "logs")
    os.environ["COLLECTOR_CONFIG"] = str(cfg)


DAY = "2026-01-15"
# 2026-01-15T05:00:00Z; hour 05 so order-book files span 0500-to-0600.
T0 = int(datetime(2026, 1, 15, 5, 0, tzinfo=timezone.utc).timestamp() * 1000)
MIN = 60_000


def iso(ts_ms: int) -> str:
    dt = datetime.fromtimestamp(ts_ms / 1000, tz=timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{ts_ms % 1000:03d}Z"


def prefix(datatype: str, symbol: str, ts_ms: int, *, bad_iso: bool = False) -> list:
    when = "1999-01-01T00:00:00.000Z" if bad_iso else iso(ts_ms)
    return ["binance", "futures", datatype, ts_ms, when, symbol]


@pytest.fixture(scope="session")
def data_dir() -> Path:
    return _TMP


@pytest.fixture(scope="session")
def client():
    from fastapi.testclient import TestClient

    import server

    return TestClient(server.app)


# --------------------------------------------------------------------- writers
def write_csv(datatype: str, symbol: str, header: str, rows: list[list], *, day: str = DAY,
              filename: str | None = None) -> Path:
    d = _TMP / "futures" / datatype / symbol / day
    d.mkdir(parents=True, exist_ok=True)
    name = filename or f"{symbol}_{datatype}_{day}.csv"
    p = d / name
    with p.open("w", newline="") as f:
        w = csv.writer(f)
        w.writerow(header.split(","))
        w.writerows(rows)
    return p


H_KLINES = ("exchange,market,datatype,timestamp_ms,datetime_utc,symbol,"
            "open,high,low,close,volume,quote_volume,trade_count,taker_buy_base,taker_buy_quote")
H_TRADES = ("exchange,market,datatype,timestamp_ms,datetime_utc,symbol,"
            "trade_id,side,price,quantity,is_buyer_maker,first_update_id,last_update_id")
H_LIQ = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,side,price,quantity,value"
H_FUNDING = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,funding_rate,next_funding_time"
H_OI = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,mark_price,open_interest,oi_value"
H_LS = ("exchange,market,datatype,timestamp_ms,datetime_utc,symbol,"
        "period,ratio_type,long_ratio,short_ratio,long_short_ratio")
H_TAKER = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,period,buy_volume,sell_volume,buy_sell_ratio"
H_SNAP = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,event_type,side,price,quantity,last_update_id"
H_UPD = ("exchange,market,datatype,timestamp_ms,datetime_utc,symbol,"
         "first_update_id,last_update_id,prev_update_id,bids_json,asks_json")


def kline_row(symbol: str, ts: int, o=100.0, h=101.0, low=99.0, c=100.5, vol=10.0, **kw) -> list:
    return prefix("klines", symbol, ts, **kw) + [o, h, low, c, vol, vol * c, 5, vol / 2, vol * c / 2]


def clean_klines(symbol: str, n: int = 10, datatype: str = "klines") -> Path:
    rows = [prefix(datatype, symbol, T0 + i * MIN) + [100, 101, 99, 100.5, 10, 1005, 5, 5, 502.5]
            for i in range(n)]
    return write_csv(datatype, symbol, H_KLINES, rows)


def trade_row(symbol: str, ts: int, tid: int, side="buy", px=100.0, qty=1.0, **kw) -> list:
    return prefix("trades", symbol, ts, **kw) + [tid, side, px, qty, 0 if side == "buy" else 1, tid, tid]


def snapshot_rows(symbol: str, ts: int, snap_id: int, bids: list, asks: list) -> list[list]:
    rows = []
    for px, q in bids:
        rows.append(prefix("orderbook_snapshots", symbol, ts)
                    + ["depth_snapshot", "bid", px, q, snap_id])
    for px, q in asks:
        rows.append(prefix("orderbook_snapshots", symbol, ts)
                    + ["depth_snapshot", "ask", px, q, snap_id])
    return rows


def update_row(symbol: str, ts: int, u: int, pu: int, bids: list, asks: list) -> list:
    jb = json.dumps([[str(p), str(q)] for p, q in bids])
    ja = json.dumps([[str(p), str(q)] for p, q in asks])
    return prefix("orderbook_updates", symbol, ts) + [u, u, pu, jb, ja]


def write_snapshot(symbol: str, rows: list[list], *, filename: str | None = None) -> Path:
    return write_csv("orderbooks", symbol, H_SNAP, rows,
                     filename=filename or f"{symbol}_depth_snapshot_{DAY}.csv")


def write_updates(symbol: str, rows: list[list]) -> Path:
    return write_csv("orderbooks", symbol, H_UPD, rows,
                     filename=f"{symbol}_depth_updates_{DAY}_0500-to-0600.csv")


def validate(client, symbol: str) -> dict:
    r = client.get(f"/api/validate/{symbol}?date={DAY}")
    assert r.status_code == 200, r.text
    return r.json()


def checks_of(report: dict, datatype: str) -> dict[str, int]:
    t = next(t for t in report["types"] if t["datatype"] == datatype)
    return {c["name"]: c["failures"] for c in t["checks"]}
