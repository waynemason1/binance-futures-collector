# Replay

The dashboard's Replay tab rebuilds a captured moment from the files on disk.
Pick a pair, a day, and a second; the order book, trade tape, and derivatives
context are reconstructed as they stood at that instant, and can then be
stepped or played forward.

The practical use is inspecting events after the fact: what the book looked
like around a wick, how a liquidation cascade ate through the bids. It also
serves as an ongoing end-to-end check: if an arbitrary second can be rebuilt,
the capture around it is whole. The Validation tab tests that property
formally; Replay lets you see it.

## One clock, every stream

Streams are not replayed equally, because they don't move equally.

| Stream | In the replay |
|---|---|
| Order book | rebuilt per second |
| Trades | tape of prints up to the instant |
| Liquidations | ticks on the timeline, plus a banner when the clock crosses one |
| Funding, open interest, long/short | value as of the instant |
| Klines | the day's price line, used as the navigation strip |

Funding changes every eight hours and open interest every few minutes, so
frame-replaying them would show nothing. They appear as "as of" values instead.

## Reading the screen

The timecode (amber, top right) is the current position, UTC. Amber means
recorded time throughout this view: timecode, playhead, mid-price marker.
Teal stays live data; green and red stay bid/ask and buy/sell.

The day strip is the navigation. The teal line is the day's price in 1-minute
closes. Ticks along the base are liquidations, one per event, at the moment it
happened: red for a sell liquidation (a long forced out), green for a buy (a
short forced out), taller for bigger notional. Hovering a tick shows side,
size, and time. Clusters of ticks mark the moments worth replaying, which
makes them useful as bookmarks. Drag the amber playhead, or click anywhere on
the strip, to seek.

Transport: -10s / -1s / play-pause / +1s / +10s, with playback at x1, x10, or
x60 (the clock advances that many seconds per wall second). Arrow keys step
1s, shift+arrows 10s, space toggles play. Moving the head never pauses
playback; only pause, or switching pair or day, stops the clock.

The depth profile is a mirrored cumulative-depth chart centred on the mid:
bids mass to the left in green, asks to the right in red, and the shape
changes as the clock moves. The figures on each shoulder are total visible
size per side. Asymmetry here is information, not a rendering problem. A
large green mass against a thin red sliver is a bid-wall regime, and the
horizontal spread of each side shows how tightly liquidity sits against the
touch.

"Book at HH:MM:SS" is the ladder: price, size, and cumulative size per level,
asks descending into the spread, bids below.

"Tape up to HH:MM:SS" is the other half of the story. The book is intent;
resting orders waiting to trade. The tape is action: the last ~30 executed
trades at or before the instant, newest first. Green prints mean the buyer
was the aggressor, red the seller. On a quiet pair the lookback widens
automatically (1 minute, then 15, then the day) so the panel shows the most
recent prints rather than sitting empty.

Below the panels: last close, funding rate, open interest, and global
long/short ratio as of the instant. Funding looks back to the previous day if
needed, since it settles 8-hourly. When the clock passes within 3 seconds of
a liquidation, a banner flashes across the panel with side, notional, and
price.

## How an instant is rebuilt

```
GET /api/replay/{symbol}/day?date=YYYY-MM-DD
GET /api/replay/{symbol}/at?ts=<ms|ISO>&levels=25&tape=30
```

`/day` returns the navigation payload: 1-minute closes and volumes, every
liquidation as an event, and the day's captured time range.

`/at` assembles the moment. The book starts from the newest depth snapshot at
or before `ts`, chosen by its recorded timestamp and update id rather than by
filename (the startup snapshot sorts last alphabetically but is the oldest by
content), then
applies every diff row with `u > snapshot_id` and `timestamp <= ts`. The
collector writes a snapshot every 10 minutes, so a rebuild replays at most
about 10 minutes of diffs no matter where in the day you seek. Hourly diff
files whose names place them entirely outside the snapshot-to-instant window
are skipped without being opened. In practice a seek costs about 0.2s on a
full-rate symbol.

The tape comes from the day's trades file, which is chronological, so the
endpoint binary-searches byte offsets (seek to a midpoint, snap to the next
line, compare timestamps) and then reads forward. No full scan, even at
millions of rows. The as-of values are the last row at or before `ts` in the
day's funding / open-interest / long-short files, which are small and cached
by file mtime. Events are liquidations in the trailing 3-second window.

Seeking before the day's first snapshot returns 404 with a hint to step
forward; there is nothing sound to rebuild a book from yet. Malformed inputs are
rejected up front with a 400: a symbol that isn't a plain ticker, a `date` that
isn't `YYYY-MM-DD`, or a `ts` that isn't epoch ms/s or ISO-8601.

## Limits

The UI steps at 1-second resolution; the API accepts milliseconds. Book
fidelity is bounded by the capture itself: diffs arrive on the configured
cadence (500ms by default), and the rebuilt book carries the snapshot's
levels plus every level the diffs have touched since. Days without
order-book capture show the price strip but no moment panel. The tape shows
aggregated trades (`aggTrade`), Binance's merge of same-price fills per
instant, which is what the collector stores.
