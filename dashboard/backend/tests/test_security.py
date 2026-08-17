"""Input-hardening regressions: path traversal and export-range bounds.

Both classes of bug let a client reach past the intended data: a crafted
symbol/date escaping the futures root, or a huge time range allocating an
unbounded grid. These lock the fixes in place.
"""
from conftest import DAY, H_TRADES, T0, trade_row, write_csv


# --- path traversal --------------------------------------------------------

def test_symbol_with_traversal_is_rejected(client):
    # A symbol carrying path syntax must never reach the filesystem. (Kept to a
    # single URL segment so the router doesn't 404 before the validator runs.)
    for bad in ("foo..bar", "sym.json", "foo$bar", "a" * 40):
        r = client.get(f"/api/symbol/{bad}")
        assert r.status_code == 400, f"{bad!r} should be rejected, got {r.status_code}"


def test_date_with_traversal_is_rejected(client):
    write_csv("trades", "secsym", H_TRADES, [trade_row("secsym", T0, 1)])
    r = client.get("/api/validate/secsym", params={"date": "../../../../etc"})
    assert r.status_code == 400
    # A well-formed date on the same endpoint is accepted (not a 400).
    ok = client.get("/api/validate/secsym", params={"date": DAY})
    assert ok.status_code != 400


def test_export_symbol_with_traversal_is_rejected(client):
    r = client.get("/api/export/merged", params={
        "symbols": "../../../../etc", "types": "klines", "from": "0", "to": "60000",
    })
    assert r.status_code == 400


def test_valid_symbol_still_works(client):
    # Guard against over-tightening: a normal ticker must still resolve.
    write_csv("trades", "goodusdt", H_TRADES, [trade_row("goodusdt", T0, 1)])
    assert client.get("/api/dates/goodusdt").status_code == 200


# --- export range bounds ---------------------------------------------------

def test_export_merged_rejects_oversized_range(client):
    # In-bounds instants, 32-day span: specifically the span cap, not the
    # timestamp range check.
    r = client.get("/api/export/merged", params={
        "symbols": "btcusdt", "types": "klines",
        "from": str(T0), "to": str(T0 + 32 * 86_400_000),
    })
    assert r.status_code == 400
    assert "too large" in r.json()["detail"]


def test_export_bundle_rejects_oversized_range(client):
    r = client.get("/api/export/bundle", params={
        "symbols": "btcusdt", "types": "klines",
        "from": str(T0), "to": str(T0 + 32 * 86_400_000),
    })
    assert r.status_code == 400


def test_out_of_range_timestamps_are_400(client):
    # Pre-2015, post-2100, and absurdly long digit strings must all be clean
    # 400s: a 300-digit epoch previously overflowed datetime into a 500.
    for frm in ("0", "9" * 300, str(5_000_000_000_000_000)):
        r = client.get("/api/export/merged", params={
            "symbols": "btcusdt", "types": "klines", "from": frm, "to": str(T0),
        })
        assert r.status_code == 400, frm
    r = client.get("/api/replay/btcusdt/at", params={"ts": "9999-01-01T00:00:00Z"})
    assert r.status_code == 400


def test_export_rejects_reversed_range(client):
    r = client.get("/api/export/merged", params={
        "symbols": "btcusdt", "types": "klines", "from": "60000", "to": "0",
    })
    assert r.status_code == 400


# --- malformed input returns 400, never an unhandled 500 -------------------

def test_bad_timestamp_is_400_not_500(client):
    # A malformed ?from= must be a clean 400, not a ValueError -> 500.
    r = client.get("/api/export/merged", params={
        "symbols": "btcusdt", "types": "klines", "from": "not-a-time", "to": "60000",
    })
    assert r.status_code == 400


def test_replay_at_bad_ts_is_400(client):
    r = client.get("/api/replay/btcusdt/at", params={"ts": "garbage"})
    assert r.status_code == 400


def test_health_survives_corrupt_stats(client, data_dir):
    # A stats.json caught mid-write (truncated JSON) must read as not-live, not 500.
    stats = data_dir / "stats" / "stats.json"
    stats.parent.mkdir(exist_ok=True)
    stats.write_text('{"messages_total": 123')  # truncated on purpose
    r = client.get("/api/health")
    assert r.status_code == 200
    assert r.json()["live"] is False


# --- state-changing guards -------------------------------------------------

def test_wrong_csrf_header_value_is_403(client):
    # The guard checks the exact value (!= "1"); a present-but-wrong header must
    # still be refused, not just a missing one.
    assert client.post("/api/restart", headers={"X-Capture-Desk": "0"}).status_code == 403


def test_resolve_log_rejects_traversal(data_dir):
    import server

    log_dir = data_dir / "logs"
    log_dir.mkdir(exist_ok=True)
    (log_dir / "collector.log").write_text("hello\n")
    ok = server._resolve_log("collector.log")
    assert ok is not None and ok.name == "collector.log"
    # config.toml exists one level up but must not be reachable via traversal.
    assert server._resolve_log("../config.toml") is None
    assert server._resolve_log("../../../../etc/passwd") is None


def test_bisect_csv_boundaries():
    import server

    rows = [trade_row("bisectusdt", T0 + i * 1000, 1000 + i) for i in range(5)]
    p = write_csv("trades", "bisectusdt", H_TRADES, rows)
    size = p.stat().st_size
    with p.open("rb") as f:
        header_end = len(f.readline())
    # A ts before the first row snaps to the first data row.
    assert server._bisect_csv(p, T0 - 10_000) == header_end
    # A ts after the last row lands at EOF.
    assert server._bisect_csv(p, T0 + 100_000) == size
