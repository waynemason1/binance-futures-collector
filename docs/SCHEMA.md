# CSV schema

One file per symbol, per data type, per UTC day, at
`data/futures/<datatype>/<symbol>/<date>/<symbol>_<datatype>_<date>.csv` (order
book depth diffs rotate hourly). The header is written on the first line.

Every row starts with the same six columns:

| Column | Meaning |
|---|---|
| `exchange` | always `binance` |
| `market` | always `futures` |
| `datatype` | the data type (matches the directory) |
| `timestamp_ms` | the row's instant, **UTC milliseconds**; see each type for what the instant refers to |
| `datetime_utc` | the same instant as `timestamp_ms`, ISO-8601 UTC (e.g. `2026-07-03T13:00:00.348Z`) |
| `symbol` | lowercase, e.g. `btcusdt` |

`timestamp_ms` and `datetime_utc` always encode the same instant; the second is a
convenience for reading raw files. What that instant *means* differs by type
(bar-open, event time, or sample time) and is noted below, worth knowing before
joining types on time.

`side` is lowercase `buy` / `sell` for trades and liquidations, and `bid` / `ask`
for the order book.

---

## orderbooks: depth diffs (`depth_updates`)

The live diff stream. Reconstruct a book by applying these in `last_update_id`
order on top of a snapshot. `timestamp_ms` is the event time.

| Column | Meaning |
|---|---|
| `first_update_id` | `U`, the first update ID in the event |
| `last_update_id` | `u`, the last update ID in the event |
| `prev_update_id` | `pu`, the `u` of the previous event (continuity check) |
| `bids_json` | JSON array of `[price, qty]`; `qty = 0` removes the level |
| `asks_json` | JSON array of `[price, qty]` |

A break in the `prev_update_id → last_update_id` chain marks a discontinuity.

## orderbooks: snapshots (`depth_snapshot`)

Point-in-time book state (initial sync, bridge attach, and periodic). One row per
price level; `timestamp_ms` is when the snapshot was taken.

| Column | Meaning |
|---|---|
| `event_type` | `depth_snapshot_initial`, `depth_snapshot`, `depth_snapshot_bridge` |
| `side` | `bid` or `ask` |
| `price` | level price |
| `quantity` | level size |
| `last_update_id` | book version this level belongs to |

## trades

Aggregated trades, deduplicated by `trade_id`. `timestamp_ms` is the trade time.

| Column | Meaning |
|---|---|
| `trade_id` | aggregate trade ID (monotonic per symbol) |
| `side` | `buy` / `sell` (aggressor side) |
| `price` | fill price |
| `quantity` | size |
| `is_buyer_maker` | `1` if the buyer was the maker, else `0` |
| `first_update_id` | first aggregated trade ID |
| `last_update_id` | last aggregated trade ID |

Note: `first_update_id` / `last_update_id` are a carried-over misnomer. They hold
the aggTrade `f` / `l` fields (first/last **trade** ID aggregated into this row),
not order-book update IDs.

## klines

1-minute candles from the WebSocket stream. `timestamp_ms` is the candle **open**
time.

| Column | Meaning |
|---|---|
| `open` `high` `low` `close` | OHLC |
| `volume` | base-asset volume |
| `quote_volume` | quote-asset volume |
| `trade_count` | number of trades in the bucket |
| `taker_buy_base` | taker buy volume, base |
| `taker_buy_quote` | taker buy volume, quote |

## liquidations

Forced-order events. Binance serves these on the market-wide `!forceOrder@arr`
stream, i.e. the *entire* futures market (USDC-margined, coin-margined, dated
delivery, equity perps), not just the symbols we collect. The collector filters
them to its USDT-M universe, so only the collected symbols land on disk (one file
per symbol). `timestamp_ms` is the event time.

| Column | Meaning |
|---|---|
| `side` | `buy` / `sell` of the liquidated position |
| `price` | fill price |
| `quantity` | size |
| `value` | `price × quantity` |

## funding_rates

Sampled periodically; `timestamp_ms` is the sample time.

| Column | Meaning |
|---|---|
| `funding_rate` | current funding rate |
| `next_funding_time` | next settlement, UTC ms |

## open_interest

Sampled periodically; `timestamp_ms` is the sample time.

| Column | Meaning |
|---|---|
| `mark_price` | mark price at sample time |
| `open_interest` | open interest, base units |
| `oi_value` | open interest in quote terms (`open_interest × mark_price`) |

## long_short_ratio

Three related series in one file, distinguished by `ratio_type`. `timestamp_ms` is
the start of the `period` bucket.

| Column | Meaning |
|---|---|
| `period` | bucket granularity, e.g. `5m` |
| `ratio_type` | `global`, `top_trader_account`, or `top_trader_position` |
| `long_ratio` | long share |
| `short_ratio` | short share |
| `long_short_ratio` | long / short |

## taker_buy_sell_ratio

`timestamp_ms` is the start of the `period` bucket.

| Column | Meaning |
|---|---|
| `period` | bucket granularity |
| `buy_volume` | taker buy volume |
| `sell_volume` | taker sell volume |
| `buy_sell_ratio` | buy / sell |

## mark_price_klines, index_price_klines, premium_index_klines

1-minute OHLC for the mark price, index price, and premium index respectively.
`timestamp_ms` is the candle open time.

| Column | Meaning |
|---|---|
| `open` `high` `low` `close` | OHLC |
