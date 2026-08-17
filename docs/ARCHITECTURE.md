# Architecture

The collector is a set of independent asynchronous tasks (Tokio) feeding a
single shared writer. Nothing shares mutable state except through concurrent
maps and a bounded message channel, so each stage can be reasoned about, and to fail,
on its own.

```
                         ┌──────────────────────────────────────────┐
   Binance WebSocket ───▶│ ConnectionManager                        │
   (depth / aggTrade /   │   batched routed connections,            │
    kline / forceOrder)  │   per-symbol ring buffers, reconnect     │
                         └───────────────┬──────────────────────────┘
                                         │ WsMessage
                          ┌──────────────▼───────────────┐
                          │ MessageHandler                │──▶ trades / klines /
                          │   trade dedup, routing        │    liquidations
                          └──────────────┬────────────────┘
                                         │ depth diffs
                          ┌──────────────▼───────────────┐
   Binance REST ─────────▶│ OrderbookManager              │──▶ order-book
   (depth snapshot)       │   snapshot → bridge → apply   │    snapshots
                          │   → trim → periodic re-save   │
                          └──────────────┬────────────────┘
                                         │
   Binance REST ──▶ RestClient ──▶ RestCollector ─────────┼──▶ funding / OI /
   (/fapi, /futures/data)   two limiters + breaker         │    ratios / klines
                                         │                 │
                                         ▼                 ▼
                                    CsvWriter  ───────▶  data/futures/**.csv
                                    GapRegistry ──────▶  data/gaps/*.json
                                    StatsExporter ────▶  data/stats/stats.json
```

## Startup

`main` raises the process file-descriptor limit (the collector keeps one open
handle per symbol × data type, thousands at full scale), initialises logging,
loads config, and discovers the active symbol set. It then brings symbols online
in staged batches: a single connection storm across ~500 symbols would breach
the exchange's rate limits, so batches are spaced and each is allowed to
stabilise before the next. Full startup takes ~30–40 minutes; steady state is
indefinite.

Shutdown flushes buffered data before exit.

## Symbol discovery

`SymbolDiscovery` pulls `exchangeInfo` and keeps only `TRADING`, `PERPETUAL`,
USDT-quoted contracts, minus an optional exclusion list. Multiplier tickers
(`1000SHIB`, `10000SATS`) are normalised to their base asset for exclusion
matching.

## Symbol lifecycle: listings and delistings mid-run

The exchange's universe changes while the collector runs, so discovery re-pulls
`exchangeInfo` every `symbol_discovery_interval_minutes` (default 10) and diffs
the result against the tracked set. (A configured whitelist pins the universe
instead, so discovery, and with it all lifecycle handling below, is skipped.) The
two directions are deliberately handled differently:

**Newly listed pairs** are detected within one interval, logged, and surfaced
on the dashboard: the console shows a "new pair · XUSDT · restart to capture"
alert until it's picked up. Capture begins at the next restart, one click on
the dashboard's Restart (a graceful flush-and-relaunch through the supervisor,
after which discovery re-runs and includes the new pair). This is a design
decision, not an accident: attaching a symbol mid-run would need new WebSocket
connections (stream subscriptions are baked into each connection URL),
order-book snapshot initialisation, and registration with every REST poller. A
full mid-run attach was considered and deliberately deferred. New listings are
rare, the new market is thin in its first hours, and the stability of
five-hundred-plus running symbols outweighs same-day capture of one; the
operator chooses the restart moment instead.

**Delisted pairs are torn down without a restart.** A symbol must be absent from
two *consecutive* discovery results, checked a full interval apart (the ticker
uses delayed catch-up, so a stall can't collapse them together), i.e. typically
10–20 minutes of wall clock, before it counts as removed. One flaky
`exchangeInfo` response can never kill a live symbol; a failed discovery call
skips the cycle entirely rather than counting toward removal
(`SymbolSetTracker`, unit-tested); and a response that loses more than 10% of
the tracked universe at once is treated as a degraded feed or exchange
maintenance and ignored, so a halt window can never mass-teardown live symbols.
On confirmation the collector logs `N symbols left TRADING`, the symbol's
order-book task drops its state and exits (instead of retrying snapshots
against a dead market forever), and every per-symbol REST poller skips it, so
a dead pair stops consuming retries and rate-limit budget within minutes (the
bulk funding poll needs no skip: the exchange simply stops returning the
symbol). Its stream names remain in the WebSocket connection URLs until
restart, which is harmless: the exchange stops emitting them. As defence in
depth, a periodic sweep also drops in-memory book state for any symbol idle for
two hours, and idle CSV buffers are flushed and evicted after thirty minutes.

A pair that returns to `TRADING` is reported as a fresh listing: its REST
polling resumes immediately, and order-book capture rejoins at the next
restart, same as any new listing.

## WebSocket layer

Binance routes futures streams across two endpoints: depth on `/public`, and
`aggTrade` / `kline` / the global `forceOrder` liquidation feed on `/market`.
`ConnectionManager` opens one connection per endpoint per symbol batch, each
carrying every stream for that batch's symbols, and buffers incoming depth diffs
into a per-symbol ring with a hard capacity and time bound. Connections reconnect
with exponential backoff on drop or timeout.

`MessageHandler` is the single consumer: it deduplicates aggregated trades by
trade ID (with a bounded, self-evicting window) and routes each message to the
writer. Because `!forceOrder@arr` is market-wide, it also filters liquidations
down to the collected USDT-M universe, so no USDC-margined, coin-margined,
dated-delivery, or equity-perp symbols reach disk.

## Order book

The hard part. A symbol's book is only trustworthy once a REST snapshot is
stitched into the live diff stream:

1. Buffer diffs as they arrive.
2. Fetch a REST depth snapshot.
3. Find the diff whose range spans the snapshot's `lastUpdateId` and apply
   forward, verifying `prev_update_id` continuity across each subsequent diff.
4. If the snapshot is older than the buffer, or the chain can't be bridged,
   discard and re-snapshot with backoff.

Once live, diffs apply directly; the book is trimmed to the configured depth on
every update to bound memory. A periodic task writes the current book; recovery
events are recorded to the gap registry. Because every persisted depth row keeps
its `U`/`u`/`pu` IDs, the book is reconstructable, and any gap detectable,
entirely offline.

For the full lifecycle (the bridge rules, why a reconnect deliberately does
*not* resnapshot, how the book self-heals, and how to reconstruct and validate a
book from the raw output) see [`docs/ORDERBOOK.md`](ORDERBOOK.md).

## Rate limiting

`RestClient` fronts every REST call with two rolling-window limiters sharing one
circuit breaker:

- **`/fapi/*`**: a request-*weight* budget over 60 s (depth snapshots cost more
  than klines; weight is derived from the endpoint and its `limit`).
- **`/futures/data/*`**: a request-*count* budget over 5 minutes; Binance limits
  these endpoints separately and more tightly.

On any `429`/`418`, or when the `X-MBX-USED-WEIGHT-1M` header nears the ceiling,
the breaker trips and *every* request path pauses for the server-instructed
`Retry-After`. A `418` is never retried; retrying a banned IP only extends the
ban. This keeps a single home IP healthy indefinitely; there is no need for
proxies.

## REST collectors

`RestCollector` runs one poll loop per metric, each on its own cadence, batched
to spread load. For the bucketed series (klines, long/short and taker ratios),
each poll requests a window of historical buckets and writes every bucket newer
than the last one seen (deduplicated by the bucket's own timestamp), so poll rate
and data granularity are decoupled, so the endpoints can be polled slowly
without losing a single native bucket. Funding and open interest have no
history window on their endpoints; they are point-in-time metrics, sampled at
each poll.

## Storage

`CsvWriter` holds one buffered writer per open file in a concurrent map. Writes
are small and flushed on a one-second cadence; depth diffs are accumulated,
sorted by update ID, and written in order. Files rotate hourly and per day;
idle buffers are evicted. On Linux the collector runs on jemalloc with a
background purge thread (see the allocator declaration in `main.rs` and the
embedded configuration in `.cargo/config.toml`), so the pages freed by each
rotation's buffer eviction are returned to the OS on a decay timer and resident
memory tracks the live working set instead of ratcheting to the day's
high-water mark. glibc's arena allocator measurably crept ~15–20 MB/day under
this workload (see [`docs/SOAK_TEST.md`](SOAK_TEST.md)). Buffers flush on drop,
so rotation and cleanup can't strand in-flight rows. Headers are written once
and flushed immediately to survive a crash without duplication.

`GapRegistry` persists recovery events per symbol as JSON. `StatsExporter`
writes a heartbeat (`stats.json`) that the dashboard and any external monitor
can read.

## Module map

| Path (under `src/`) | Responsibility |
|---|---|
| `main.rs` | startup, fd limit, logging, task orchestration, shutdown |
| `config.rs` | typed configuration |
| `collectors/symbol_discovery.rs` | active-symbol resolution |
| `websocket/connection_manager.rs` | batched connections, ring buffers, reconnect |
| `websocket/message_handler.rs` | consumer, trade dedup, routing |
| `collectors/orderbook_manager.rs` | snapshot/diff reconstruction, resync |
| `rest/client.rs` | HTTP, rate limiters, circuit breaker |
| `collectors/rest_collector.rs` | metric poll loops |
| `utils/csv_writer.rs` | buffered CSV output, rotation |
| `utils/gap_registry.rs` | recovery-event log |
| `utils/stats_exporter.rs` | heartbeat |
