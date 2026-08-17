import { useEffect, useRef, useState } from 'react'
import { AnimatePresence, motion } from 'framer-motion'
import { ChevronDown, Search } from 'lucide-react'
import { Command } from 'cmdk'
import { createChart, ColorType, type IChartApi, type ISeriesApi } from 'lightweight-charts'
import { useSymbols, useSymbolDetail, useOrderBook, type Candle, type Trade, type SymbolRow, type OrderBook, type DepthLevel } from '../lib/api'

const EASE: [number, number, number, number] = [0.22, 1, 0.36, 1]
const rise = { initial: { opacity: 0, y: 12 }, animate: { opacity: 1, y: 0, transition: { duration: 0.5, ease: EASE } } }

import { decimalsFor, fmtPrice, usd } from '../lib/format'
// Tape times render in UTC: every timestamp in the app (CSVs, exports, pickers) is UTC.
const hhmm = (ms: number) => new Date(ms).toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit', second: '2-digit', timeZone: 'UTC' })
// compact "time since" for a symbol's last kline
const since = (s?: number | null) =>
  s == null ? '' : s < 90 ? `${Math.round(s)}s` : s < 5400 ? `${Math.round(s / 60)}m` : s < 129600 ? `${Math.round(s / 3600)}h` : `${Math.round(s / 86400)}d`

/* ---------------------------------------------------------- symbol picker */
function LiveDot({ on, dim }: { on: boolean; dim?: boolean }) {
  return (
    <span className={`lamp h-1.5 w-1.5 shrink-0 ${on ? 'breathe' : ''}`}
      style={{ background: on ? '#FFB224' : dim ? '#39414D' : '#5C6675', boxShadow: on ? '0 0 8px rgba(255,178,36,0.6)' : 'none' }} />
  )
}

function PickerItem({ s, selected, onPick }: { s: SymbolRow; selected: boolean; onPick: (id: string) => void }) {
  return (
    <Command.Item
      value={s.id}
      onSelect={() => onPick(s.id)}
      className="flex cursor-pointer items-center justify-between rounded-md px-2 py-1.5 font-mono text-[12.5px] text-muted aria-selected:bg-raised aria-selected:text-text"
    >
      <span className="flex items-center gap-2">
        <LiveDot on={s.active} dim />
        <span className={`tnum ${selected ? 'text-accent' : ''}`}>{s.symbol}</span>
      </span>
      {s.active
        ? <span className="text-[9.5px] uppercase tracking-wide text-live">live</span>
        : <span className="tnum text-[9.5px] text-faint">{since(s.last_kline)}</span>}
    </Command.Item>
  )
}

export function SymbolPicker({ symbols, value, onChange }: { symbols: SymbolRow[]; value: string; onChange: (id: string) => void }) {
  const [open, setOpen] = useState(false)
  const wrap = useRef<HTMLDivElement>(null)
  const current = symbols.find((s) => s.id === value)
  const live = symbols.filter((s) => s.active)
  const history = symbols.filter((s) => !s.active)

  useEffect(() => {
    const onDoc = (e: MouseEvent) => { if (wrap.current && !wrap.current.contains(e.target as Node)) setOpen(false) }
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); setOpen((o) => !o) }
      else if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', onDoc)
    document.addEventListener('keydown', onKey)
    return () => { document.removeEventListener('mousedown', onDoc); document.removeEventListener('keydown', onKey) }
  }, [])

  const pick = (id: string) => { onChange(id); setOpen(false) }
  const headingCls = '[&_[cmdk-group-heading]]:eyebrow [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:pb-1 [&_[cmdk-group-heading]]:pt-2'

  return (
    <div ref={wrap} className="relative">
      <button
        onClick={() => setOpen((o) => !o)}
        className="flex items-center gap-2 rounded-lg border border-line bg-raised py-1.5 pl-2.5 pr-2 font-mono text-[13px] text-text transition-colors hover:border-faint focus:border-accent focus:outline-none"
      >
        <LiveDot on={!!current?.active} />
        <span className="tnum">{current?.symbol ?? value.toUpperCase()}</span>
        <kbd className="ml-1 hidden rounded border border-line bg-panel px-1 py-px font-mono text-[9px] text-faint sm:inline">⌘K</kbd>
        <ChevronDown size={14} className="text-faint" />
      </button>

      <AnimatePresence>
        {open && (
          <motion.div
            initial={{ opacity: 0, y: -6, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -6, scale: 0.98 }}
            transition={{ duration: 0.14, ease: EASE }}
            className="absolute left-0 z-40 mt-2 w-[284px] overflow-hidden rounded-xl border border-line bg-panel shadow-2xl shadow-black/60"
          >
            <Command filter={(val, search) => (val.includes(search.toLowerCase().trim()) ? 1 : 0)}>
              <div className="flex items-center gap-2 border-b border-line px-3">
                <Search size={14} className="shrink-0 text-faint" />
                <Command.Input
                  autoFocus
                  placeholder="Search pairs…"
                  className="w-full bg-transparent py-2.5 font-mono text-[13px] text-text placeholder:text-faint focus:outline-none"
                />
              </div>
              <Command.List className="max-h-[302px] overflow-y-auto p-1.5">
                <Command.Empty className="py-6 text-center font-mono text-[12px] text-faint">No pair matches.</Command.Empty>
                {live.length > 0 && (
                  <Command.Group heading="Streaming now" className={headingCls}>
                    {live.map((s) => <PickerItem key={s.id} s={s} selected={s.id === value} onPick={pick} />)}
                  </Command.Group>
                )}
                {history.length > 0 && (
                  <Command.Group heading={`On disk · history (${history.length})`} className={`mt-1 ${headingCls}`}>
                    {history.map((s) => <PickerItem key={s.id} s={s} selected={s.id === value} onPick={pick} />)}
                  </Command.Group>
                )}
              </Command.List>
            </Command>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  )
}

/* ------------------------------------------------------------- candle chart */
function CandleChart({ candles, symbol }: { candles: Candle[]; symbol: string }) {
  const el = useRef<HTMLDivElement>(null)
  const legend = useRef<HTMLDivElement>(null)
  const chart = useRef<IChartApi | null>(null)
  const series = useRef<ISeriesApi<'Candlestick'> | null>(null)
  const volume = useRef<ISeriesApi<'Histogram'> | null>(null)
  const lastSym = useRef('')
  const lastTime = useRef(0)

  useEffect(() => {
    if (!el.current) return
    const c = createChart(el.current, {
      autoSize: true,
      layout: { background: { type: ColorType.Solid, color: 'transparent' }, textColor: '#7E8798', fontFamily: 'IBM Plex Mono', fontSize: 11, attributionLogo: false },
      grid: { vertLines: { color: 'rgba(33,40,52,0.6)' }, horzLines: { color: 'rgba(33,40,52,0.6)' } },
      rightPriceScale: { borderColor: '#212834', scaleMargins: { top: 0.08, bottom: 0.22 } },
      timeScale: { borderColor: '#212834', timeVisible: true, secondsVisible: false },
      crosshair: { mode: 1, vertLine: { color: '#2DD4BF', width: 1, style: 2 }, horzLine: { color: '#2DD4BF', width: 1, style: 2 } },
    })
    series.current = c.addCandlestickSeries({
      upColor: '#34E5B0', downColor: '#FB6B7E', borderVisible: false,
      wickUpColor: '#34E5B0', wickDownColor: '#FB6B7E',
    })
    volume.current = c.addHistogramSeries({ priceFormat: { type: 'volume' }, priceScaleId: 'vol', lastValueVisible: false, priceLineVisible: false })
    c.priceScale('vol').applyOptions({ scaleMargins: { top: 0.84, bottom: 0 } })

    // OHLC readout that follows the crosshair, written to the DOM directly so a
    // 60fps hover doesn't churn React state.
    c.subscribeCrosshairMove((param) => {
      const l = legend.current
      if (!l) return
      const bar = param.seriesData.get(series.current!) as { open: number; high: number; low: number; close: number } | undefined
      const vol = param.seriesData.get(volume.current!) as { value: number } | undefined
      if (!bar) { l.style.opacity = '0'; return }
      const d = decimalsFor(bar.close)
      const f = (x: number) => x.toLocaleString('en-US', { minimumFractionDigits: d, maximumFractionDigits: d })
      const up = bar.close >= bar.open
      l.style.opacity = '1'
      l.innerHTML =
        `<span style="color:#5C6675">O</span> ${f(bar.open)}` +
        `  <span style="color:#5C6675">H</span> ${f(bar.high)}` +
        `  <span style="color:#5C6675">L</span> ${f(bar.low)}` +
        `  <span style="color:#5C6675">C</span> <span style="color:${up ? '#34E5B0' : '#FB6B7E'}">${f(bar.close)}</span>` +
        (vol ? `  <span style="color:#5C6675">V</span> ${Math.round(vol.value).toLocaleString('en-US')}` : '')
    })
    chart.current = c
    // Reset the incremental-update refs on teardown so a remount (incl. React
    // StrictMode's dev double-mount) always reloads the full series, never
    // update()s onto a fresh empty chart.
    return () => {
      c.remove(); chart.current = null; series.current = null; volume.current = null
      lastSym.current = ''; lastTime.current = 0
    }
  }, [])

  useEffect(() => {
    if (!series.current || !candles.length) return
    const d = decimalsFor(candles[candles.length - 1].close)
    series.current.applyOptions({ priceFormat: { type: 'price', precision: d, minMove: Math.pow(10, -d) } })
    const bars = candles.map((k) => ({ time: k.time as any, open: k.open, high: k.high, low: k.low, close: k.close }))
    const vols = candles.map((k) => ({
      time: k.time as any, value: k.volume,
      color: k.close >= k.open ? 'rgba(52,229,176,0.32)' : 'rgba(251,107,126,0.32)',
    }))
    const lastT = candles[candles.length - 1].time
    if (lastSym.current !== symbol) {
      // New symbol (or first load): load the whole series and fit the view once.
      series.current.setData(bars)
      volume.current?.setData(vols)
      chart.current?.timeScale().fitContent()
      lastSym.current = symbol
    } else if (lastTime.current && lastT - lastTime.current > 120) {
      // Missed several candles (e.g. the tab was backgrounded): resync in full.
      series.current.setData(bars)
      volume.current?.setData(vols)
    } else {
      // Normal 3s poll: update only the latest bar. update() appends when the
      // minute rolls over and edits the forming candle otherwise, so the chart
      // follows real time while the crosshair and zoom stay put, with no re-fit,
      // and the accumulating history never falls outside a stale view range.
      series.current.update(bars[bars.length - 1])
      volume.current?.update(vols[vols.length - 1])
    }
    lastTime.current = lastT
  }, [candles, symbol])

  return (
    <div className="relative h-full w-full">
      <div ref={el} className="h-full w-full" />
      <div ref={legend} className="pointer-events-none absolute left-2 top-1.5 z-10 font-mono text-[10.5px] text-muted transition-opacity duration-100" style={{ opacity: 0 }} />
    </div>
  )
}

function Stat({ label, value, tone }: { label: string; value: string; tone?: 'up' | 'down' }) {
  return (
    <div className="rounded-lg border border-line bg-raised/60 px-3 py-2">
      <div className="eyebrow">{label}</div>
      <div className={`tnum mt-0.5 font-mono text-[15px] ${tone === 'up' ? 'text-good' : tone === 'down' ? 'text-alert' : 'text-text'}`}>{value}</div>
    </div>
  )
}

/* ------------------------------------------------------------------ export */
function DepthLadder({ book }: { book?: OrderBook }) {
  const midRef = useRef<HTMLDivElement>(null)
  // Center the view on the spread when the symbol changes (not on every poll,
  // which would fight the reader's scroll).
  useEffect(() => {
    midRef.current?.scrollIntoView({ block: 'center' })
  }, [book?.symbol])
  const asks = book?.asks ?? []
  const bids = book?.bids ?? []
  const maxCum = Math.max(
    asks.length ? asks[asks.length - 1].cumulative : 0,
    bids.length ? bids[bids.length - 1].cumulative : 0,
    1e-9,
  )
  const Row = ({ lvl, side }: { lvl: DepthLevel; side: 'bid' | 'ask' }) => (
    <div className="relative grid grid-cols-3 items-center py-[2px] font-mono text-[11px]">
      <div
        className={`absolute inset-y-0 right-0 ${side === 'bid' ? 'bg-good/12' : 'bg-alert/12'}`}
        style={{ width: `${(lvl.cumulative / maxCum) * 100}%` }}
      />
      <span className={`tnum relative text-left ${side === 'bid' ? 'text-good' : 'text-alert'}`}>
        {fmtPrice(lvl.price)}
      </span>
      <span className="tnum relative text-center text-muted">{lvl.qty.toFixed(3)}</span>
      <span className="tnum relative text-right text-faint">{lvl.cumulative.toFixed(2)}</span>
    </div>
  )
  return (
    <div className="mt-1 max-h-[236px] overflow-y-auto px-3 pb-2">
      {[...asks].reverse().map((l, i) => <Row key={`a${i}`} lvl={l} side="ask" />)}
      {book?.mid != null && (
        <div ref={midRef} className="my-0.5 flex items-center justify-between border-y border-line/60 py-1 font-mono text-[10.5px]">
          <span className="tnum text-text">{fmtPrice(book.mid)}</span>
          <span className="text-faint">spread {fmtPrice(book.spread)}</span>
        </div>
      )}
      {bids.map((l, i) => <Row key={`b${i}`} lvl={l} side="bid" />)}
      {!asks.length && !bids.length && (
        <div className="py-6 text-center font-mono text-[11px] text-faint">rebuilding book…</div>
      )}
    </div>
  )
}

export default function SymbolExplorer() {
  const { data: syms } = useSymbols()
  const [sel, setSel] = useState('btcusdt')
  const [view, setView] = useState<'tape' | 'depth'>('tape')
  const { data: d } = useSymbolDetail(sel)
  const { data: book } = useOrderBook(view === 'depth' ? sel : undefined)

  // Every pair with candles on disk. The collector marks which are live right
  // now (`active`) vs left over from an earlier run. The picker shows both,
  // clearly badged, so any pair stays reachable without a 500-row scroll.
  const withKlines = syms?.symbols.filter((s) => s.streams.includes('klines')) ?? []
  const liveCount = withKlines.filter((s) => s.active).length

  // Keep the selection valid; prefer a live pair when the current one drops out.
  useEffect(() => {
    if (withKlines.length && !withKlines.some((s) => s.id === sel)) {
      setSel(withKlines.find((s) => s.active)?.id ?? withKlines[0].id)
    }
  }, [withKlines, sel])

  const up = (d?.change_pct ?? 0) >= 0

  return (
    <motion.section variants={rise}>
      <div className="mb-2.5 flex items-baseline justify-between">
        <h2 className="font-display text-[14px] font-medium">Symbol explorer</h2>
        <span className="font-mono text-[11px] text-faint">
          {liveCount ? <><span className="text-live">{liveCount}</span> streaming · </> : null}1-minute candles · live tape
        </span>
      </div>

      <div className="panel overflow-hidden">
        {/* header: symbol picker + price */}
        <div className="flex flex-wrap items-center justify-between gap-4 border-b border-line px-5 py-3.5">
          <SymbolPicker symbols={withKlines} value={sel} onChange={setSel} />
          <div className="flex items-baseline gap-3">
            <span className="tnum font-mono text-[26px] font-semibold leading-none">{fmtPrice(d?.price)}</span>
            {d?.change_pct != null && (
              <span
                className={`tnum font-mono text-[13px] ${up ? 'text-good' : 'text-alert'}`}
                title={`over the last ${d.change_span_min ?? 0} one-minute candles on disk`}
              >
                {up ? '+' : ''}{d.change_pct.toFixed(2)}%
                <span className="ml-1 text-faint">
                  · {d.change_span_min && d.change_span_min >= 60 ? `${Math.round(d.change_span_min / 60)}h` : `${d.change_span_min ?? 0}m`}
                </span>
              </span>
            )}
          </div>
        </div>

        <div className="grid gap-4 p-4 lg:grid-cols-[2.4fr_1fr]">
          {/* candlestick chart */}
          <div className="h-[300px] w-full overflow-hidden rounded-lg border border-line bg-ink/40">
            {d?.candles?.length ? <CandleChart candles={d.candles} symbol={sel} /> :
              <div className="grid h-full place-items-center font-mono text-[12px] text-faint">no candles for this symbol</div>}
          </div>

          {/* right column: metrics + trade tape */}
          <div className="flex min-w-0 flex-col gap-3">
            <div className="grid grid-cols-3 gap-2">
              <Stat label="Funding" value={d?.funding_rate != null ? (d.funding_rate * 100).toFixed(4) + '%' : '—'}
                tone={d?.funding_rate != null ? (d.funding_rate >= 0 ? 'up' : 'down') : undefined} />
              <Stat label="Open Int" value={usd(d?.open_interest_usd)} />
              <Stat label="L/S ratio" value={d?.long_short_ratio != null ? d.long_short_ratio.toFixed(2) : '—'}
                tone={d?.long_short_ratio != null ? (d.long_short_ratio >= 1 ? 'up' : 'down') : undefined} />
            </div>

            <div className="min-h-0 flex-1 rounded-lg border border-line bg-raised/40">
              <div className="flex items-center justify-between px-3 pt-2">
                <div className="flex overflow-hidden rounded border border-line">
                  {(['tape', 'depth'] as const).map((v) => (
                    <button
                      key={v}
                      onClick={() => setView(v)}
                      className={`px-2 py-0.5 font-mono text-[9.5px] uppercase tracking-wide transition-colors ${
                        view === v ? 'bg-line text-text' : 'text-faint hover:text-muted'
                      }`}
                    >
                      {v === 'tape' ? 'Tape' : 'Depth'}
                    </button>
                  ))}
                </div>
                <span className="font-mono text-[9px] text-faint">{view === 'tape' ? 'px · qty · time' : 'px · qty · cum'}</span>
              </div>
              {view === 'tape' ? (
                <div className="mt-1 max-h-[236px] overflow-y-auto px-3 pb-2">
                  {(d?.trades ?? []).map((t: Trade, idx: number) => (
                    <div key={t.t + '-' + idx} className="grid grid-cols-3 items-baseline py-[3px] font-mono text-[11px] whitespace-nowrap">
                      <span className={`tnum text-left ${t.side === 'buy' ? 'text-good' : 'text-alert'}`}>{fmtPrice(t.price)}</span>
                      <span className="tnum text-center text-muted">{t.qty.toFixed(3)}</span>
                      <span className="tnum text-right text-faint">{hhmm(t.t)}</span>
                    </div>
                  ))}
                  {!d?.trades?.length && <div className="py-6 text-center font-mono text-[11px] text-faint">no recent trades</div>}
                </div>
              ) : (
                <DepthLadder book={book} />
              )}
            </div>
          </div>
        </div>
      </div>
    </motion.section>
  )
}
