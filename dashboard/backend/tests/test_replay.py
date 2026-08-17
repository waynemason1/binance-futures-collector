"""Replay endpoints: the book at an instant, the tape before it, as-of context."""
from conftest import (DAY, MIN, T0, H_FUNDING, H_LIQ, H_TRADES, clean_klines, prefix,
                      snapshot_rows, trade_row, update_row, write_csv,
                      write_snapshot, write_updates)


def seed_replay_symbol(sym: str) -> None:
    clean_klines(sym, n=10)
    write_csv("trades", sym, H_TRADES,
              [trade_row(sym, T0 + i * 10_000, 500 + i, side="buy" if i % 2 else "sell",
                         px=100.0 + i) for i in range(12)])
    write_csv("liquidations", sym, H_LIQ,
              [prefix("liquidations", sym, T0 + 45_000) + ["sell", 104.0, 2.0, 208.0]])
    write_csv("funding_rates", sym, H_FUNDING,
              [prefix("funding_rates", sym, T0) + [0.0001, T0 + 8 * 3600 * 1000]])
    write_snapshot(sym, snapshot_rows(sym, T0, 100,
                                      bids=[(99.0, 1.0), (98.0, 2.0)],
                                      asks=[(101.0, 1.0), (102.0, 2.0)]))
    write_updates(sym, [
        update_row(sym, T0 + 1_000, 101, 100, bids=[(99.5, 0.5)], asks=[]),
        update_row(sym, T0 + 5_000, 102, 101, bids=[], asks=[(100.6, 0.7)]),
    ])


def test_replay_day_maps_the_scrubber(client):
    seed_replay_symbol("rpdayusdt")
    r = client.get(f"/api/replay/rpdayusdt/day?date={DAY}")
    assert r.status_code == 200, r.text
    d = r.json()
    assert len(d["price"]) == 10
    assert d["has_book"] is True
    assert d["range"]["from"] == T0
    assert d["range"]["to"] == T0 + 10 * MIN
    assert len(d["events"]) == 1 and d["events"][0]["t"] == T0 + 45_000


def test_replay_at_rebuilds_the_instant(client):
    seed_replay_symbol("rpatusdt")
    # T0+2s: first diff (u=101) applied, second (u=102, at T0+5s) NOT yet.
    r = client.get(f"/api/replay/rpatusdt/at?ts={T0 + 2_000}")
    assert r.status_code == 200, r.text
    d = r.json()
    book = d["book"]
    assert book["snapshot_id"] == 100
    assert book["best_bid"] == 99.5
    assert book["best_ask"] == 101.0  # the 100.6 ask arrives at T0+5s
    # Cumulative depth is monotonic outward from the touch.
    cums = [lvl["cumulative"] for lvl in book["bids"]]
    assert cums == sorted(cums)
    # Tape: only prints at/before the instant (trades at T0 and T0+10s exist;
    # at T0+2s exactly one qualifies).
    assert [t["t"] for t in d["tape"]] == [T0]
    assert d["asof"]["funding_rate"] == 0.0001


def test_replay_at_advances_with_the_clock(client):
    seed_replay_symbol("rpstepusdt")
    d = client.get(f"/api/replay/rpstepusdt/at?ts={T0 + 6_000}").json()
    assert d["book"]["best_ask"] == 100.6  # u=102 now applied
    assert len(d["tape"]) == 1  # trade at T0 only (next print is T0+10s)


def test_replay_liquidation_event_window(client):
    seed_replay_symbol("rpliqusdt")
    # Liq at T0+45s: visible in the (t-3s, t] window, absent right before.
    before = client.get(f"/api/replay/rpliqusdt/at?ts={T0 + 44_000}").json()
    assert before["events"] == []
    hit = client.get(f"/api/replay/rpliqusdt/at?ts={T0 + 45_500}").json()
    assert len(hit["events"]) == 1
    assert hit["events"][0]["value"] == 208.0


def test_replay_before_first_snapshot_is_404(client):
    seed_replay_symbol("rpearlyusdt")
    r = client.get(f"/api/replay/rpearlyusdt/at?ts={T0 - 60_000}")
    assert r.status_code == 404


def test_tape_bisect_finds_late_day_prints_fast(client):
    # A row deep in the file is found via byte bisection, not a full scan.
    sym = "rpbisectusdt"
    rows = [trade_row(sym, T0 + i * 1_000, 9000 + i) for i in range(600)]
    write_csv("trades", sym, H_TRADES, rows)
    import server
    p = server._latest_csv("trades", sym, DAY)
    off = server._bisect_csv(p, T0 + 500_000)
    with p.open() as f:
        f.seek(off)
        first = f.readline().split(",")
    assert int(first[3]) == T0 + 500_000
