"""Order-book reconstruction: the collector's core correctness claim.

Covers the clean replay, update-id continuity breaks, crossed-book detection,
and the newest-snapshot-by-update-id rule (regression: picking snapshots by
filename once replayed stale levels and produced a permanently crossed book).
"""
from conftest import (DAY, T0, snapshot_rows, update_row, validate,
                      write_snapshot, write_updates)


def test_clean_chain_reconstructs_book(client):
    sym = "obokusdt"
    write_snapshot(sym, snapshot_rows(sym, T0, 100,
                                      bids=[(99.0, 1.0), (98.0, 2.0)],
                                      asks=[(101.0, 1.5), (102.0, 2.5)]))
    write_updates(sym, [
        update_row(sym, T0 + 1000, 101, 100, bids=[(99.5, 0.4)], asks=[]),
        update_row(sym, T0 + 2000, 102, 101, bids=[], asks=[(101.0, 0.0)]),  # lift the touch
        update_row(sym, T0 + 3000, 103, 102, bids=[(98.0, 0.0)], asks=[(100.8, 0.9)]),
    ])

    ob = validate(client, sym)["orderbook"]
    assert ob["depth_updates"]["rows"] == 3
    assert ob["depth_updates"]["continuity_breaks"] == 0
    r = ob["reconstruction"]
    assert r["snapshot_id"] == 100
    assert r["diffs_applied"] == 3
    assert r["crossed_books"] == 0
    assert r["best_bid"] == 99.5
    assert r["best_ask"] == 100.8


def test_update_id_gap_is_a_continuity_break(client):
    sym = "obgapusdt"
    write_snapshot(sym, snapshot_rows(sym, T0, 100, bids=[(99.0, 1.0)], asks=[(101.0, 1.0)]))
    write_updates(sym, [
        update_row(sym, T0 + 1000, 101, 100, bids=[(99.1, 1.0)], asks=[]),
        update_row(sym, T0 + 2000, 105, 104, bids=[(99.2, 1.0)], asks=[]),  # pu 104 ≠ 101
        update_row(sym, T0 + 3000, 106, 105, bids=[], asks=[(100.9, 1.0)]),
    ])

    rep = validate(client, sym)
    d = rep["orderbook"]["depth_updates"]
    assert d["continuity_breaks"] == 1
    assert d["first_break"] == {"expected_pu": 101, "got_pu": 104}
    assert rep["verdict"] == "issues"


def test_crossed_book_is_flagged(client):
    sym = "obxusdt"
    write_snapshot(sym, snapshot_rows(sym, T0, 100, bids=[(99.0, 1.0)], asks=[(101.0, 1.0)]))
    # A bid strictly above the standing ask crosses the book.
    write_updates(sym, [update_row(sym, T0 + 1000, 101, 100, bids=[(102.0, 1.0)], asks=[])])

    rep = validate(client, sym)
    assert rep["orderbook"]["reconstruction"]["crossed_books"] >= 1
    assert rep["verdict"] == "issues"


def test_crossed_snapshot_seed_is_flagged(client):
    # Regression: a periodic snapshot is a dump of the collector's in-memory
    # book, which can freeze CROSSED after severe stream gaps (observed live:
    # spread -180.5 with zero appliable diffs). The seed itself must be audited
    # crossed_books only counts crossings after applied diffs.
    sym = "obseedusdt"
    write_snapshot(sym, snapshot_rows(sym, T0, 300,
                                      bids=[(102.0, 1.0)],   # stale bid ABOVE the ask
                                      asks=[(101.0, 1.0)]))
    # Every diff predates the snapshot id -> nothing applies, seed stands alone.
    write_updates(sym, [update_row(sym, T0 - 1000, 299, 298, bids=[(100.0, 1.0)], asks=[])])

    rep = validate(client, sym)
    r = rep["orderbook"]["reconstruction"]
    assert r["diffs_applied"] == 0
    assert r["seed_crossed"] is True
    assert r["crossed_books"] == 0  # unchanged semantics: counts applied-diff crossings only
    assert rep["verdict"] == "issues"


def test_tape_span_reports_partial_day_coverage(client):
    # A clean 2-second tape is still only a 2-second tape; the report must say
    # what it covered so a mostly-missing day can't read as a clean full day.
    sym = "obspanusdt"
    write_snapshot(sym, snapshot_rows(sym, T0, 100, bids=[(99.0, 1.0)], asks=[(101.0, 1.0)]))
    write_updates(sym, [
        update_row(sym, T0 + 1000, 101, 100, bids=[(99.1, 1.0)], asks=[]),
        update_row(sym, T0 + 3000, 102, 101, bids=[], asks=[(100.9, 1.0)]),
    ])

    d = validate(client, sym)["orderbook"]["depth_updates"]
    assert d["first_ts"] == T0 + 1000
    assert d["last_ts"] == T0 + 3000


def test_newest_snapshot_wins_regardless_of_filename(client):
    # Regression: the startup snapshot sorts LAST alphabetically but is the
    # OLDEST by content. Basing the replay on it produced crossed books.
    sym = "obsnapusdt"
    stale = snapshot_rows(sym, T0 - 3_600_000, 50,
                          bids=[(105.0, 1.0)],  # stale level ABOVE today's asks
                          asks=[(106.0, 1.0)])
    write_snapshot(sym, stale,
                   filename=f"{sym}_depth_snapshot_initial_2026_01_15_040000_{T0 - 3_600_000}.csv")
    fresh = snapshot_rows(sym, T0, 200, bids=[(99.0, 1.0)], asks=[(101.0, 1.0)])
    write_snapshot(sym, fresh)  # daily file sorts BEFORE the *_initial_* file

    write_updates(sym, [update_row(sym, T0 + 1000, 201, 200, bids=[(99.5, 1.0)], asks=[])])

    r = validate(client, sym)["orderbook"]["reconstruction"]
    assert r["snapshot_id"] == 200  # newest by id, not by filename sort
    assert r["crossed_books"] == 0
    assert r["best_bid"] == 99.5
    assert r["best_ask"] == 101.0


def test_orderbook_endpoint_serves_sorted_ladder(client):
    sym = "obladusdt"
    write_snapshot(sym, snapshot_rows(sym, T0, 100,
                                      bids=[(99.0, 1.0), (98.5, 2.0), (98.0, 3.0)],
                                      asks=[(101.0, 1.0), (101.5, 2.0), (102.0, 3.0)]))
    write_updates(sym, [update_row(sym, T0 + 1000, 101, 100, bids=[(99.2, 0.5)], asks=[])])

    r = client.get(f"/api/orderbook/{sym}?levels=3")
    assert r.status_code == 200, r.text
    book = r.json()
    bid_px = [lvl["price"] for lvl in book["bids"]]
    ask_px = [lvl["price"] for lvl in book["asks"]]
    assert bid_px == sorted(bid_px, reverse=True)  # best bid first
    assert ask_px == sorted(ask_px)  # best ask first
    assert book["best_bid"] == 99.2
    assert book["best_ask"] == 101.0
    assert book["spread"] > 0
    # Cumulative sizes never decrease walking away from the touch.
    for side in ("bids", "asks"):
        cums = [lvl["cumulative"] for lvl in book[side]]
        assert cums == sorted(cums)


PREV = "2026-01-14"
T_PREV = T0 - 86_400_000  # same wall-clock hour, previous day


def _seed_two_days(sym: str, first_pu_of_day: int) -> None:
    from conftest import H_UPD, write_csv

    # Previous day: chain ends at u=200.
    write_csv("orderbooks", sym, H_UPD,
              [update_row(sym, T_PREV, 199, 198, bids=[(99.0, 1.0)], asks=[]),
               update_row(sym, T_PREV + 1_000, 200, 199, bids=[], asks=[(101.0, 1.0)])],
              day=PREV, filename=f"{sym}_depth_updates_{PREV}_0500-to-0600.csv")
    # Current day: snapshot + one diff whose pu is the handoff under test.
    write_snapshot(sym, snapshot_rows(sym, T0, first_pu_of_day,
                                      bids=[(99.0, 1.0)], asks=[(101.0, 1.0)]))
    write_updates(sym, [update_row(sym, T0 + 1_000, first_pu_of_day + 1, first_pu_of_day,
                                   bids=[(99.5, 1.0)], asks=[])])


def test_midnight_handoff_chained(client):
    sym = "obhandokusdt"
    _seed_two_days(sym, first_pu_of_day=200)  # pu(first of day) == u(last of prev)
    rep = validate(client, sym)
    assert rep["orderbook"]["depth_updates"]["midnight_handoff"] == "ok"
    assert rep["verdict"] == "clean"


def test_midnight_handoff_break_flags_issues(client):
    sym = "obhandgapusdt"
    _seed_two_days(sym, first_pu_of_day=205)  # gap: prev day ended at u=200
    rep = validate(client, sym)
    assert rep["orderbook"]["depth_updates"]["midnight_handoff"] == "break"
    assert rep["verdict"] == "issues"
