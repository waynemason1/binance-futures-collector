# Order books: sync, recovery, and reconstruction

Order-book maintenance is the subtle part of any market-data collector, so this
document spells out exactly how this one keeps ~500 books in sync, what it writes
to disk, how it behaves across a WebSocket reconnect, and how you can verify all
of it offline against the raw output. File references point at the code so every
claim here is checkable.

If you only read one section, read [Reconnects](#3-reconnects--the-deliberate-design).
It is the design decision most people ask about, and the one most collectors get
wrong.

---

## Background: the Binance snapshot + diff model

Binance USDT-M depth is delivered as a **REST snapshot** plus a **WebSocket diff
stream**. Each diff carries three update IDs:

| Field | Wire | Meaning |
|---|---|---|
| `U`  | `first_update_id` | first update ID covered by this diff |
| `u`  | `last_update_id`  | final update ID covered by this diff |
| `pu` | `prev_update_id`  | `u` of the **previous** diff |

A correct book is *snapshot + every diff applied in order*, where each diff chains
onto the last (`pu` of a diff equals `u` of the diff before it). Miss a diff and
the chain breaks: you can no longer trust the book and must re-anchor on a fresh
snapshot. Binance does **not** replay missed diffs, so a gap cannot be back-filled;
it can only be detected and re-anchored past.

---

## Lifecycle

Each symbol moves through four states
(`OrderbookState`, `src/collectors/orderbook_manager.rs`):

```
  Initializing ──▶ FetchingSnapshot ──▶ Syncing ──▶ Live
       ▲                                              │
       └───────────── resync (init/bridge failure) ◀──┘
                       rate-limited; never from a plain reconnect
```

- **Initializing / FetchingSnapshot / Syncing**: the one-time bridge from a REST
  snapshot onto the live diff stream (below).
- **Live**: steady state, diffs applied as they arrive.
- The only path back to re-anchoring is a **resync**, and it is reserved for
  *bridge failures*, not for ordinary reconnects. See
  [Resync](#4-resync--when-it-happens-and-how-its-throttled).

---

## 1. Initial sync: buffer first, then bridge

The classic race is that a snapshot and a stream started independently leave a hole
between them. This collector avoids it by **buffering the diff stream before
fetching the snapshot**:

1. **Buffer.** WebSocket diffs land in a per-symbol ring buffer
   (`symbol_buffers`, a bounded `VecDeque`) as soon as the connection opens.
   Nothing between "stream started" and "snapshot fetched" is lost.
2. **Snapshot.** Fetch the REST depth snapshot; call its `lastUpdateId`
   `snapshot_u`. Written to disk as `event_type = depth_snapshot_initial`.
3. **Bridge.** Scan the buffered diffs for the first one that continues the
   snapshot (`bridge_and_apply`, `orderbook_manager.rs`):
   - **Rule 1, perfect continuity:** the first diff with `first_u <= snapshot_u + 1`.
   - **Rule 2, pu-based attach:** a diff with `pu <= snapshot_u < first_u`
     (the snapshot lands inside the diff's range).
   - If the *oldest* buffered diff is already past the snapshot
     (`snapshot_u < first_pu`), the snapshot is too old to attach to anything
     buffered → immediate resync. If the gap is implausibly large
     (`> 10_000`), the snapshot is treated as stale → immediate resync.
4. **Attach.** Apply diffs from the bridge point forward, verifying the chain at
   every step (`update.prev_update_id == last_u`). A gap mid-attach aborts and
   retries. On success the book is written as `event_type = depth_snapshot_bridge`
   and the symbol goes **Live**.

---

## 2. Steady state: the live loop

Once **Live** (`run_live_depth_update_loop`, `orderbook_manager.rs`):

- Every 500 ms, drain each live symbol's ring buffer and apply the diffs.
- **Absolute quantities.** A diff level is an *absolute* quantity at a price, not a
  delta: `qty == 0` deletes the level, anything else replaces it
  (`apply_update_to_book`). This detail is why the book **self-heals** after a gap
  (below): a level is made correct by its next update regardless of history.
- **Bounded memory.** After each diff the book is trimmed to `depth_limit` levels
  per side (`trim_book_to_depth`), so a book cannot grow without bound over days.
- **Periodic snapshots.** A separate task re-serializes each live book to disk
  every `snapshot_interval_seconds` (`event_type = depth_snapshot`), staggered
  across a 180 s window to smooth disk and CPU. These are re-serializations of the
  **in-memory** book, so they are anchor points for reconstruction, **not** fresh
  REST pulls.

---

## 3. Reconnects: the deliberate design

**A WebSocket reconnect does not trigger a resync, and that is intentional.**

When a connection drops, `connection_manager` reconnects with exponential backoff
and resumes buffering. It does **not** reset order-book state, and the live loop
does **not** check `pu` continuity or resnapshot on the first post-reconnect diff.

Why not: a dropped connection takes a **whole batch of symbols down together**. If
each symbol resnapshotted on the gap, the collector would fire *hundreds of REST
snapshots in a burst*, blowing Binance's request-weight budget, drawing bans, and
cascading into a resync storm that is far worse than the gap it was trying to fix.
(This is a real failure mode; the design exists specifically to avoid it.)

Instead a reconnect is absorbed three ways:

1. **The raw tape stays lossless, and the gap is recorded.** Every diff the
   collector receives is written to `depth_updates` independently of the in-memory
   book (`message_handler.rs`). A reconnect therefore leaves a visible `pu`-break in
   the raw tape, which the offline validator counts as `continuity_breaks`
   (below). The gap is **detected, not silent.**
2. **The in-memory book self-heals.** Because diffs carry absolute quantities,
   every price level is corrected the next time it updates. Active levels (at and
   near the touch) recover within milliseconds. The only residue is a level that
   changed *during* the outage and then goes quiet, and `trim_book_to_depth`
   eventually drops it if it is deep.
3. **Reconstruction re-anchors forward.** Offline rebuilds always anchor on the
   **newest** snapshot and replay from there (below), so any consumer picks up past
   the gap at the next snapshot automatically.

The net effect: the authoritative artifact (the raw diff tape) is complete and its
gaps are detectable; the live book is a self-healing convenience; and the collector
never storms Binance on a reconnect.

---

## 4. Resync: when it happens, and how it's throttled

A resync (fresh REST snapshot mid-run) is reserved for **bridge failures**, not
reconnects:

- snapshot too old to attach to any buffered diff,
- stale snapshot (bridge gap `> 10_000`),
- bridge-rule timeout (`attach_bridge_timeout_seconds`).

Every resync is **rate-limited per symbol**: a `resync_cooldown_seconds` floor,
then exponential backoff (`resync_backoff_factor`) up to
`max_resync_backoff_seconds`. Each one is recorded in the gap registry
(`data/gaps/<symbol>_gaps.json`) and surfaced by the dashboard's `/api/gaps`.

---

## 5. What's written to disk

Two datatypes carry the book (full field lists in [SCHEMA.md](SCHEMA.md)):

- **`depth_updates`**: the raw diff tape, one row per diff, rotated hourly. This
  is the lossless source of truth. Carries `U` / `u` / `pu` plus the bid/ask level
  arrays as exact exchange wire strings.
- **`depth_snapshot`**: anchor rows, tagged by `event_type`:
  - `depth_snapshot_initial`, the REST snapshot taken at first sync,
  - `depth_snapshot_bridge`, the book immediately after a successful attach,
  - `depth_snapshot`, periodic re-serialization of the live book.

---

## 6. Reconstructing a book offline

To rebuild the book as of any instant (`_reconstruct_at`) or validate a whole day
(`_reconstruct_orderbook`), both in `dashboard/backend/server.py`:

1. Choose the **newest snapshot by update ID** at or before the target, *not* the
   newest by filename. The startup snapshot sorts last alphabetically but is the
   oldest by content; anchoring on it would replay hours of moved-away levels that
   never clear, leaving a permanently crossed book.
2. Replay every diff with `u > snapshot_id` forward, applying absolute quantities.

This is exactly what the dashboard's **Replay** tab does when you scrub to a moment
and step the book. See [REPLAY.md](REPLAY.md).

---

## 7. Verifying integrity

The dashboard's **Validation** tab (`/api/validate/{symbol}`) checks the collector's
own output against itself:

- **`continuity_breaks`**: walks the day's `depth_updates` and counts every place
  the `pu` chain breaks (`pu != previous u`). Reconnect gaps show up here, with the
  first break's expected/actual IDs.
- **`crossed_books`**: replays the day and flags any point where the reconstructed
  `best_bid >= best_ask` (a corruption signal).
- **`midnight_handoff`**: confirms the first diff of a day chains from the last
  diff of the previous day, so nothing is lost across the UTC date rollover and the
  hourly-file boundary.

A day passes only when there are zero continuity breaks, zero crossed books, and
the midnight handoff is intact.

---

## Guarantees and limitations (the honest version)

**Guaranteed**

- The `depth_updates` tape contains every diff the collector received, byte-exact.
- Any missing diff (reconnect gap, exchange hiccup) is **detectable** from the tape
  alone via the `pu` chain, so you never have to trust the book blindly.

**Best-effort**

- The in-memory book and its periodic `depth_snapshot` rows can carry brief,
  self-healing drift in the moments right after a reconnect. Levels at and near the
  touch recover on their next update; a deep, quiet level may hold a stale value
  until it updates again or is trimmed.

**Not attempted (by design)**

- Back-filling missed diffs. Binance does not replay them, and resnapshotting on
  every gap causes the request-weight cascade described above. If you need a
  gap-free book across a reconnect boundary, re-anchor on the next snapshot, which
  reconstruction already does automatically.

If you are consuming this data, the rule of thumb is: **treat `depth_updates` as
truth, re-anchor on the nearest `depth_snapshot`, and check `continuity_breaks`
before trusting a window that spans a reconnect.**
