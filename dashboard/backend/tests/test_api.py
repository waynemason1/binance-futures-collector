"""Health, dates, config editing, restart sentinel, and export bundle."""
import io
import json
import os
import time
import zipfile
from datetime import datetime, timezone

from conftest import DAY, H_TRADES, T0, trade_row, write_csv


def test_streams_split_quiet_from_stale(client):
    # Event-driven lanes: a symbol with stale trades but a fresh order book is
    # "quiet" (connected, market silent); stale in both means genuinely stale.
    import server

    old = time.time() - 600  # beyond both the trades (150s) and book (90s) windows

    def age(path):
        os.utime(path, (old, old))

    # quietsym: book fresh, trades stale -> quiet
    write_csv("orderbooks", "quietsym", H_TRADES, [])
    age(write_csv("trades", "quietsym", H_TRADES, []))
    # deadsym: both stale -> stale
    age(write_csv("orderbooks", "deadsym", H_TRADES, []))
    age(write_csv("trades", "deadsym", H_TRADES, []))
    # livesym: both fresh -> live
    write_csv("orderbooks", "livesym", H_TRADES, [])
    write_csv("trades", "livesym", H_TRADES, [])

    server._streams_cache.update(t=0.0, data=None)
    trades = next(s for s in client.get("/api/streams").json()["streams"] if s["key"] == "trades")
    assert trades["quiet_symbols"] == 1
    assert trades["stale_symbols"] == 1
    # REST lanes and the order book itself don't carry the split.
    book = next(s for s in client.get("/api/streams").json()["streams"] if s["key"] == "orderbooks")
    assert "quiet_symbols" not in book


def test_health_reports_dead_without_stats(client, data_dir):
    stats = data_dir / "stats" / "stats.json"
    if stats.exists():
        stats.unlink()
    body = client.get("/api/health").json()
    assert body["live"] is False


def test_health_goes_live_with_fresh_stats(client, data_dir):
    stats = data_dir / "stats" / "stats.json"
    stats.parent.mkdir(exist_ok=True)
    stats.write_text(json.dumps({
        "collector": "binance_futures",
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "uptime_seconds": 60,
        "messages_total": 1000,
        "connection_status": "connected",
    }))
    body = client.get("/api/health").json()
    assert body["live"] is True
    assert body["messages_total"] == 1000


def test_dates_lists_seeded_day(client):
    write_csv("trades", "datesusdt", H_TRADES, [trade_row("datesusdt", T0, 1)])
    days = client.get("/api/dates/datesusdt").json()["dates"]
    assert DAY in days


def test_config_roundtrip_preserves_comments(client, data_dir):
    cfg = data_dir / "config.toml"
    before = client.get("/api/config").json()
    assert before["update_speed_ms"] in (100, 250, 500)

    patch = {k: before[k] for k in ("collect_all", "symbols", "rest_intervals")}
    patch["update_speed_ms"] = 250
    r = client.post("/api/config", json=patch, headers={"X-Capture-Desk": "1"})
    assert r.status_code == 200, r.text

    assert client.get("/api/config").json()["update_speed_ms"] == 250
    text = cfg.read_text()
    assert "update_speed_ms = 250" in text
    # tomlkit editing must keep the file's comments intact.
    assert "keeps full microstructure" in text


def test_restart_writes_sentinel_only(client, data_dir):
    r = client.post("/api/restart", headers={"X-Capture-Desk": "1"})
    assert r.status_code == 200
    assert (data_dir / "control" / "restart.request").is_file()


def test_post_without_csrf_header_is_403(client):
    # State-changing endpoints require the custom header; a plain cross-site
    # form/fetch POST can't attach one, so this is the CSRF line of defense.
    assert client.post("/api/restart").status_code == 403
    assert client.post("/api/config", json={
        "collect_all": True, "symbols": [], "update_speed_ms": 250, "rest_intervals": {},
    }).status_code == 403


def test_config_rejects_malformed_symbols(client):
    r = client.post("/api/config", json={
        "collect_all": False, "symbols": ["btcusdt", "../../../etc/passwd"],
        "update_speed_ms": 250, "rest_intervals": {},
    }, headers={"X-Capture-Desk": "1"})
    assert r.status_code == 400


def test_export_bundle_slices_by_time(client):
    sym = "expusdt"
    rows = [trade_row(sym, T0 + i * 60_000, 1000 + i) for i in range(10)]
    write_csv("trades", sym, H_TRADES, rows)

    # Window covering only the first 5 minutes → 5 of the 10 rows.
    r = client.get("/api/export/bundle", params={
        "symbols": sym, "types": "trades",
        "from": str(T0), "to": str(T0 + 4 * 60_000),
    })
    assert r.status_code == 200, r.text
    z = zipfile.ZipFile(io.BytesIO(r.content))
    member = next(n for n in z.namelist() if "trades" in n and sym in n)
    lines = z.read(member).decode().strip().splitlines()
    assert lines[0].startswith("exchange,market,datatype,timestamp_ms,datetime_utc,symbol")
    assert len(lines) == 1 + 5  # header + the 5 in-window rows


def test_export_merged_builds_grid(client):
    sym = "mrgusdt"
    rows = [trade_row(sym, T0 + i * 60_000, 2000 + i, px=100.0 + i, qty=2.0) for i in range(5)]
    write_csv("trades", sym, H_TRADES, rows)

    r = client.get("/api/export/merged", params={
        "symbols": sym, "types": "trades",
        "from": str(T0), "to": str(T0 + 4 * 60_000),
    })
    assert r.status_code == 200, r.text
    lines = r.text.strip().splitlines()
    assert lines[0] == "timestamp_ms,datetime_utc,trade_count,trade_vwap"
    assert len(lines) == 1 + 5  # a 5-minute grid, one trade per minute
    cols = lines[1].split(",")
    assert cols[1].endswith("Z")   # datetime_utc is trailing-Z UTC
    assert cols[2] == "1"          # trade_count for that minute


def test_export_merged_empty_is_404(client):
    # An unknown symbol (valid format, no data) yields no cells, so merged must 404
    # like the bundle, not hand back an empty grid the caller can't interpret.
    r = client.get("/api/export/merged", params={
        "symbols": "ghostusdt", "types": "trades",
        "from": str(T0), "to": str(T0 + 60_000),
    })
    assert r.status_code == 404, r.text
