# Changelog

Notable changes to this project. Versions follow [semantic versioning](https://semver.org):
given `MAJOR.MINOR.PATCH`, a MAJOR bump means the CSV schema or config format changed
in a way that needs action from you, MINOR adds capability, PATCH fixes things.

## [1.0.0] - 2026-08-17

First public release. Extracted and hardened from a private trading-research
system; this is the capture layer, standalone.

### Capture

- Full Binance USDT-M perpetual futures tape: every symbol, every stream, eleven
  data types on disk, to plain CSV.
- Order books reconstructed from a REST snapshot bridged onto the buffered diff
  stream, with `U`/`u`/`pu` continuity verified at every step. Every discontinuity
  is recorded rather than hidden.
- A WebSocket reconnect deliberately does **not** trigger a re-snapshot. The raw
  tape stays lossless and the gap is detectable; re-snapshotting hundreds of
  symbols at once would breach the exchange's rate limits. See
  [`docs/ORDERBOOK.md`](docs/ORDERBOOK.md).
- Two rolling-window rate limiters behind one circuit breaker: request-weight over
  60 s for `/fapi/*`, request-count over 5 min for `/futures/data/*`. A `418` is
  never retried. Runs indefinitely from a single residential IP, no proxies.
- Symbol lifecycle handled live: delistings are torn down after two consecutive
  confirmations, and a discovery result that loses more than 10% of the universe is
  rejected as exchange maintenance.

### Dashboard

- Capture Desk: live throughput and per-stream health, on-disk coverage, a replay
  view that rebuilds any captured second from the CSVs, and a validation view that
  re-verifies a whole day.
- Validators are tested against seeded corruption, so "the data is clean" is a
  checked claim.
- Bound to `127.0.0.1` by default; it has no authentication (see
  [`SECURITY.md`](SECURITY.md)).

### Operations

- Memory bounded over multi-day runs: jemalloc with a background purge thread on
  Linux. A 6.2-day soak across 552 M messages is documented in
  [`docs/SOAK_TEST.md`](docs/SOAK_TEST.md), including the glibc baseline that
  motivated the change.
- `./setup.sh` installs either path (Docker or native) and checks prerequisites,
  disk headroom, the Docker daemon, and that Binance is reachable from your
  network before doing any work. `./setup.sh --check` runs those checks and exits.
- `--version` and `--help` on the collector binary; the running version is shown
  in the dashboard header.
- Graceful shutdown drains in-flight updates and flushes every open CSV buffer
  before exit, with `stop_grace_period` set high enough in Compose for a
  full-universe flush to finish.

[1.0.0]: https://github.com/waynemason1/binance-futures-collector/releases/tag/v1.0.0
