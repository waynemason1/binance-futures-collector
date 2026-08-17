import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { AnimatePresence, motion, useReducedMotion } from 'framer-motion'
import { Pause, Play, ZoomIn } from 'lucide-react'
import { SymbolPicker } from '../components/SymbolExplorer'
import { fmtPrice, usd } from '../lib/format'
import {
  useDates, useReplayAt, useReplayDay, useSymbols,
  type ReplayAt, type ReplayDay, type ReplayEvent,
} from '../lib/api'

const AMBER = '#FFB224'
const GOOD = '#34E5B0'
const ALERT = '#FB6B7E'

const utc = (ms: number) => new Date(ms).toISOString()
const hms = (ms: number) => utc(ms).slice(11, 19)

/* ------------------------------------------------------------ minimap */
function Minimap({ day, ts, from, to, onSeek }: { day: ReplayDay; ts: number; from: number; to: number; onSeek: (ms: number) => void }) {
  const ref = useRef<SVGSVGElement>(null)
  const lastSeek = useRef(0)
  const W = 1000
  const H = 180
  const x = (t: number) => ((t - from) / Math.max(1, to - from)) * W

  // Only the price points inside the visible window, so zooming rescales the band
  // to what's on screen instead of flattening it against the whole day's range.
  const vis = useMemo(() => day.price.filter((p) => p.t >= from && p.t <= to), [day, from, to])
  const path = useMemo(() => {
    if (!vis.length) return { line: '', area: '' }
    const cs = vis.map((p) => p.c)
    const lo = Math.min(...cs)
    const hi = Math.max(...cs)
    const pad = (hi - lo || 1) * 0.12
    const y = (c: number) => 8 + (1 - (c - lo + pad) / (hi - lo + 2 * pad)) * (H - 30)
    const pts = vis.map((p) => `${x(p.t).toFixed(1)},${y(p.c).toFixed(1)}`)
    const line = 'M' + pts.join(' L')
    const area = line + ` L${x(vis[vis.length - 1].t).toFixed(1)},${H - 14} L${x(vis[0].t).toFixed(1)},${H - 14} Z`
    return { line, area }
  }, [vis, from, to])

  // Time ticks along the bottom; the interval adapts to the zoom so the axis
  // stays legible from a 15-minute window up to a full day.
  const ticks = useMemo(() => {
    const span = to - from
    const step = span <= 30 * 60_000 ? 5 * 60_000
      : span <= 2 * 3_600_000 ? 15 * 60_000
      : span <= 8 * 3_600_000 ? 3_600_000
      : 4 * 3_600_000
    const out: { t: number; label: string }[] = []
    for (let t = Math.ceil(from / step) * step; t <= to; t += step) {
      out.push({ t, label: hms(t).slice(0, 5) })
    }
    return out
  }, [from, to])

  const seekFromPointer = useCallback(
    (e: { clientX: number }) => {
      const el = ref.current
      if (!el) return
      const r = el.getBoundingClientRect()
      const frac = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width))
      onSeek(Math.round((from + frac * (to - from)) / 1000) * 1000)
    },
    [from, to, onSeek],
  )

  return (
    <svg
      ref={ref}
      viewBox={`0 0 ${W} ${H}`}
      preserveAspectRatio="none"
      className="h-[180px] w-full cursor-crosshair select-none focus:outline-none"
      role="slider"
      aria-label="Scrub through the day"
      aria-valuemin={from}
      aria-valuemax={to}
      aria-valuenow={ts}
      tabIndex={0}
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId)
        seekFromPointer(e)
      }}
      onPointerMove={(e) => {
        if (!(e.buttons & 1)) return
        const now = performance.now()
        if (now - lastSeek.current < 100) return
        lastSeek.current = now
        seekFromPointer(e)
      }}
    >
      {/* price */}
      <path d={path.area} fill="rgba(45,212,191,0.07)" />
      <path d={path.line} fill="none" stroke="rgba(45,212,191,0.55)" strokeWidth="1.2" vectorEffect="non-scaling-stroke" />
      {/* time ticks */}
      {ticks.map((h) => (
        <g key={h.t}>
          <line x1={x(h.t)} x2={x(h.t)} y1={H - 14} y2={H - 10} stroke="#39414D" vectorEffect="non-scaling-stroke" />
          <text x={x(h.t)} y={H - 2} textAnchor="middle" fill="#5C6675" fontSize="8.5" fontFamily="IBM Plex Mono">
            {h.label}
          </text>
        </g>
      ))}
      {/* liquidations: ticks along the base, taller = bigger */}
      {day.events.filter((ev) => ev.t >= from && ev.t <= to).map((ev, i) => {
        const h = Math.min(22, 5 + Math.log10(Math.max(10, ev.value)) * 4)
        return (
          <line
            key={i}
            x1={x(ev.t)} x2={x(ev.t)} y1={H - 15} y2={H - 15 - h}
            stroke={ev.side === 'sell' ? ALERT : GOOD} strokeWidth="1.4"
            opacity="0.8" vectorEffect="non-scaling-stroke"
          >
            <title>{`${ev.side} liquidation ${usd(ev.value)} · ${hms(ev.t)}`}</title>
          </line>
        )
      })}
      {/* playhead */}
      <line x1={x(ts)} x2={x(ts)} y1={2} y2={H - 12} stroke={AMBER} strokeWidth="1.6" vectorEffect="non-scaling-stroke" />
      <polygon
        points={`${Math.min(W - 5, Math.max(5, x(ts))) - 4.5},2 ${Math.min(W - 5, Math.max(5, x(ts))) + 4.5},2 ${Math.min(W - 5, Math.max(5, x(ts)))},9`}
        fill={AMBER}
      />
    </svg>
  )
}

/* -------------------------------------------------------- depth profile */
function DepthProfile({ at }: { at: ReplayAt }) {
  const still = useReducedMotion()
  const W = 1000
  const H = 130
  const { bids, asks, mid } = at.book

  const d = useMemo(() => {
    if (!bids.length || !asks.length || mid == null) return null
    const span = Math.max(mid - bids[bids.length - 1].price, asks[asks.length - 1].price - mid, 1e-12)
    const maxCum = Math.max(bids[bids.length - 1].cumulative, asks[asks.length - 1].cumulative)
    const px = (p: number) => W / 2 + ((p - mid) / span) * (W / 2 - 8)
    const py = (c: number) => H - 16 - (c / maxCum) * (H - 34)
    // Step outward from the touch on each side.
    const walk = (levels: typeof bids) => {
      let path = `M${px(levels[0].price).toFixed(1)},${H - 16}`
      let prevY = H - 16
      for (const l of levels) {
        const X = px(l.price).toFixed(1)
        path += ` L${X},${prevY.toFixed(1)}`
        prevY = py(l.cumulative)
        path += ` L${X},${prevY.toFixed(1)}`
      }
      return path + ` L${px(levels[levels.length - 1].price).toFixed(1)},${H - 16} Z`
    }
    return { bid: walk(bids), ask: walk(asks), midX: W / 2 }
  }, [bids, asks, mid])

  if (!d) return null
  const spring = still ? { duration: 0 } : { type: 'tween' as const, duration: 0.45, ease: [0.22, 1, 0.36, 1] as const }

  return (
    <div className="border-b border-line/60 px-2 pt-2">
      <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className="h-[120px] w-full">
        <motion.path animate={{ d: d.bid }} transition={spring} initial={false} fill="rgba(52,229,176,0.14)" stroke={GOOD} strokeWidth="1.1" strokeOpacity="0.7" vectorEffect="non-scaling-stroke" />
        <motion.path animate={{ d: d.ask }} transition={spring} initial={false} fill="rgba(251,107,126,0.12)" stroke={ALERT} strokeWidth="1.1" strokeOpacity="0.7" vectorEffect="non-scaling-stroke" />
        <line x1={d.midX} x2={d.midX} y1="6" y2={H - 16} stroke={AMBER} strokeWidth="1" strokeDasharray="3 4" opacity="0.7" vectorEffect="non-scaling-stroke" />
        <text x="10" y="16" fill="#5C6675" fontSize="10" fontFamily="IBM Plex Mono">
          {'\u03a3 '}{bids.length ? bids[bids.length - 1].cumulative.toFixed(2) : '0'}
        </text>
        <text x={W - 10} y="16" textAnchor="end" fill="#5C6675" fontSize="10" fontFamily="IBM Plex Mono">
          {'\u03a3 '}{asks.length ? asks[asks.length - 1].cumulative.toFixed(2) : '0'}
        </text>
      </svg>
      <div className="flex items-center justify-between pb-1.5 font-mono text-[9.5px] text-faint">
        <span>bid depth {fmtPrice(at.book.best_bid)}</span>
        <span className="text-live">mid {fmtPrice(at.book.mid)} · spread {fmtPrice(at.book.spread)}</span>
        <span>{fmtPrice(at.book.best_ask)} ask depth</span>
      </div>
      {at.book.gap && (
        <div className="pb-1 text-center font-mono text-[9.5px] text-alert">
          ⚠ update-id gap before this instant: book rebuilt across a capture break, treat with caution
        </div>
      )}
    </div>
  )
}

/* ----------------------------------------------------------- book + tape */
function Ladder({ at }: { at: ReplayAt }) {
  const { bids, asks } = at.book
  const maxCum = Math.max(
    bids.length ? bids[bids.length - 1].cumulative : 0,
    asks.length ? asks[asks.length - 1].cumulative : 0,
    1e-9,
  )
  const Row = ({ price, qty, cumulative, side }: { price: number; qty: number; cumulative: number; side: 'bid' | 'ask' }) => (
    <div className="relative grid grid-cols-3 items-center py-[2px] font-mono text-[11px]">
      <div
        className={`absolute inset-y-0 right-0 transition-[width] duration-300 ${side === 'bid' ? 'bg-good/12' : 'bg-alert/12'}`}
        style={{ width: `${(cumulative / maxCum) * 100}%` }}
      />
      <span className={`tnum relative text-left ${side === 'bid' ? 'text-good' : 'text-alert'}`}>{fmtPrice(price)}</span>
      <span className="tnum relative text-center text-muted">{qty.toFixed(3)}</span>
      <span className="tnum relative text-right text-faint">{cumulative.toFixed(2)}</span>
    </div>
  )
  return (
    <div className="max-h-[300px] overflow-y-auto px-3 py-1">
      {[...asks.slice(0, 12)].reverse().map((l) => <Row key={`a${l.price}`} {...l} side="ask" />)}
      <div className="my-0.5 border-y border-line/60 py-0.5 text-center font-mono text-[10px] text-live">
        {fmtPrice(at.book.mid)}
      </div>
      {bids.slice(0, 12).map((l) => <Row key={`b${l.price}`} {...l} side="bid" />)}
    </div>
  )
}

function Tape({ at }: { at: ReplayAt }) {
  return (
    <div className="max-h-[300px] overflow-y-auto px-3 py-1">
      {[...at.tape].reverse().map((t, i) => (
        <div key={`${t.t}-${i}`} className="grid grid-cols-3 items-baseline whitespace-nowrap py-[3px] font-mono text-[11px]">
          <span className={`tnum text-left ${t.side === 'buy' ? 'text-good' : 'text-alert'}`}>{fmtPrice(t.price)}</span>
          <span className="tnum text-center text-muted">{t.qty.toFixed(3)}</span>
          <span className="tnum text-right text-faint">{hms(t.t)}</span>
        </div>
      ))}
      {!at.tape.length && <div className="py-6 text-center font-mono text-[11px] text-faint">no prints before this instant</div>}
    </div>
  )
}

/* --------------------------------------------------------------- page */
const STEPS = [-10_000, -1_000, 1_000, 10_000]
const SPEEDS = [1, 10, 60]
const ZOOMS: { label: string; span: number | null }[] = [
  { label: '15m', span: 15 * 60_000 },
  { label: '1h', span: 60 * 60_000 },
  { label: '6h', span: 6 * 60 * 60_000 },
  { label: 'Day', span: null },
]

export default function Replay() {
  const { data: syms } = useSymbols()
  const [sel, setSel] = useState('btcusdt')
  const { data: dates } = useDates(sel)
  const [day, setDay] = useState<string>()
  const [ts, setTs] = useState<number>()
  const [playing, setPlaying] = useState(false)
  const [speed, setSpeed] = useState(10)
  const [zoom, setZoom] = useState(ZOOMS[3]) // Day
  const [winStart, setWinStart] = useState<number | null>(null)
  const still = useReducedMotion()

  const withKlines = syms?.symbols.filter((s) => s.streams.includes('klines')) ?? []
  useEffect(() => {
    if (dates?.dates.length && (!day || !dates.dates.includes(day))) setDay(dates.dates[0])
  }, [dates, day])

  const dayQ = useReplayDay(sel, day)
  const range = dayQ.data?.range

  // Land one minute before the end of the day's capture.
  useEffect(() => {
    if (range) setTs((t) => (t && t >= range.from && t <= range.to ? t : range.to - 60_000))
  }, [range])

  // Zoom window for the tape: the full day, or a span that follows the playhead.
  // Kept as state (winStart) with hysteresis so scrubbing *inside* the window
  // doesn't make it pan; it only re-centers when the playhead leaves the window
  // (a step/play past the edge), which reads as the tape catching up.
  useEffect(() => {
    if (!range || zoom.span == null || zoom.span >= range.to - range.from) {
      setWinStart(null)
      return
    }
    const span = zoom.span
    const lo = range.from
    const hi = range.to - span
    const clamp = (v: number) => Math.min(Math.max(v, lo), hi)
    const cur = ts ?? range.to
    setWinStart((prev) =>
      prev != null && cur >= prev && cur <= prev + span ? clamp(prev) : clamp(cur - span / 2),
    )
  }, [zoom, range, ts])

  const win = useMemo(() => {
    if (!range) return null
    if (winStart == null || zoom.span == null) return range
    return { from: winStart, to: winStart + zoom.span }
  }, [range, winStart, zoom])

  const atQ = useReplayAt(sel, ts)
  const at = atQ.data

  const seek = useCallback(
    (ms: number) => {
      if (!range) return
      setTs(Math.min(range.to, Math.max(range.from, ms)))
    },
    [range],
  )

  // The deck: play advances the clock speed× each wall second.
  useEffect(() => {
    if (!playing || !ts || !range) return
    const id = setInterval(() => {
      setTs((t) => {
        const next = (t ?? 0) + 1_000 * speed
        if (next >= range.to) {
          setPlaying(false)
          return range.to
        }
        return next
      })
    }, 1_000)
    return () => clearInterval(id)
  }, [playing, speed, range, ts === undefined])

  // Deck keys: ← → step, shift = ×10, space = play/pause.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Don't hijack keys while a form control has focus (the symbol/date
      // <select> and any input use Space/Arrows natively).
      const tag = (e.target as HTMLElement)?.tagName
      if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') return
      if (e.key === 'ArrowLeft') { e.preventDefault(); seek((ts ?? 0) - (e.shiftKey ? 10_000 : 1_000)) }
      else if (e.key === 'ArrowRight') { e.preventDefault(); seek((ts ?? 0) + (e.shiftKey ? 10_000 : 1_000)) }
      else if (e.key === ' ') { e.preventDefault(); setPlaying((p) => !p) }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [seek, ts])

  const liq: ReplayEvent | undefined = at?.events[at.events.length - 1]

  return (
    <div className="mx-auto min-h-full max-w-[1080px] px-6 py-8">
      <header className="mb-6 flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="font-display text-[20px] font-bold leading-none tracking-tightest">Replay</h1>
          <p className="mt-1.5 font-mono text-[11px] text-faint">
            rebuild any captured instant · scrub the day, step the book, read the tape
          </p>
        </div>
        {/* the timecode: where in the recording you are */}
        <div className="text-right">
          <div className="font-mono text-[10px] uppercase tracking-widest text-faint">{day ?? '—'} · UTC</div>
          <div className="tnum font-mono text-[34px] font-semibold leading-none text-live" style={{ textShadow: '0 0 18px rgba(255,178,36,0.35)' }}>
            {ts ? hms(ts) : '--:--:--'}
          </div>
        </div>
      </header>

      {/* transport */}
      <section className="panel mb-5">
        <div className="flex flex-wrap items-center gap-3 border-b border-line/60 px-4 py-3">
          <SymbolPicker symbols={withKlines} value={sel} onChange={(id) => { setPlaying(false); setSel(id) }} />
          <select
            value={day ?? ''}
            onChange={(e) => { setPlaying(false); setDay(e.target.value) }}
            className="rounded-lg border border-line bg-raised px-2 py-1.5 font-mono text-[12px] text-text focus:border-accent focus:outline-none"
            aria-label="Day"
          >
            {(dates?.dates ?? []).map((d) => <option key={d}>{d}</option>)}
          </select>

          <div className="flex w-full flex-wrap items-center gap-1.5 sm:ml-auto sm:w-auto">
            {STEPS.map((ms) => (
              <button
                key={ms}
                onClick={() => seek((ts ?? 0) + ms)}
                className="rounded-lg border border-line px-2.5 py-1.5 font-mono text-[11px] text-muted transition-colors hover:text-text focus-visible:border-accent focus-visible:outline-none"
              >
                {ms > 0 ? `+${ms / 1000}s` : `${ms / 1000}s`}
              </button>
            ))}
            <button
              onClick={() => setPlaying((p) => !p)}
              aria-label={playing ? 'Pause' : 'Play'}
              className={`mx-1 grid h-9 w-9 place-items-center rounded-full border transition-colors focus-visible:outline-none ${
                playing ? 'border-live/60 bg-live/10 text-live' : 'border-line text-muted hover:text-text focus-visible:border-accent'
              }`}
            >
              {playing ? <Pause size={14} /> : <Play size={14} className="ml-0.5" />}
            </button>
            <div className="flex overflow-hidden rounded-lg border border-line">
              {SPEEDS.map((s) => (
                <button
                  key={s}
                  onClick={() => setSpeed(s)}
                  className={`px-2.5 py-1.5 font-mono text-[11px] transition-colors focus-visible:outline-none ${
                    speed === s ? 'bg-line text-text' : 'text-faint hover:text-muted'
                  }`}
                >
                  ×{s}
                </button>
              ))}
            </div>
            <div className="flex items-center overflow-hidden rounded-lg border border-line">
              <ZoomIn size={12} className="ml-2 mr-0.5 text-faint" aria-hidden />
              {ZOOMS.map((z) => (
                <button
                  key={z.label}
                  onClick={() => setZoom(z)}
                  title="Zoom the tape window (follows the playhead)"
                  className={`px-2 py-1.5 font-mono text-[11px] transition-colors focus-visible:outline-none ${
                    zoom.label === z.label ? 'bg-line text-text' : 'text-faint hover:text-muted'
                  }`}
                >
                  {z.label}
                </button>
              ))}
            </div>
          </div>
        </div>

        <div className="px-4 pb-2 pt-3">
          {dayQ.data ? (
            <Minimap
              day={dayQ.data}
              ts={ts ?? dayQ.data.range.from}
              from={(win ?? dayQ.data.range).from}
              to={(win ?? dayQ.data.range).to}
              onSeek={seek}
            />
          ) : (
            <div className="grid h-[180px] place-items-center font-mono text-[12px] text-faint">
              {dayQ.isError ? `no capture for ${sel.toUpperCase()} on ${day}` : 'loading the day…'}
            </div>
          )}
          <div className="mt-1 flex flex-wrap items-center justify-between gap-x-4 gap-y-1 font-mono text-[9.5px] text-faint">
            <span>
              <span className="text-accent/70">—</span> price, 1m closes
              <span className="mx-2 text-line">·</span>
              base ticks = liquidations: <span className="text-alert">longs forced out</span> /{' '}
              <span className="text-good">shorts forced out</span>, taller = bigger
            </span>
            <span>drag to scrub · ←/→ 1s · shift 10s · space plays</span>
          </div>
        </div>
      </section>

      {/* the instant */}
      {dayQ.data && !dayQ.data.has_book && (
        <div className="panel px-5 py-10 text-center font-mono text-[12px] text-muted">
          No order-book capture for {sel.toUpperCase()} on {day}. Pick another day or pair.
        </div>
      )}
      {atQ.isError && dayQ.data?.has_book && (
        <div className="panel px-5 py-10 text-center font-mono text-[12px] text-muted">
          Before the day&apos;s first snapshot. Step forward to where book capture begins.
        </div>
      )}

      {at && (
        <section className="panel relative overflow-hidden">
          <AnimatePresence>
            {liq && (
              <motion.div
                key={liq.t}
                initial={{ opacity: still ? 1 : 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className={`flex items-center justify-center gap-2 border-b px-4 py-1.5 font-mono text-[11px] ${
                  liq.side === 'sell' ? 'border-alert/30 bg-alert/10 text-alert' : 'border-good/30 bg-good/10 text-good'
                }`}
              >
                liquidation · {liq.side} {usd(liq.value)} @ {fmtPrice(liq.price)} · {hms(liq.t)}
              </motion.div>
            )}
          </AnimatePresence>

          <DepthProfile at={at} />

          <div className="grid lg:grid-cols-2">
            <div className="border-b border-line/60 lg:border-b-0 lg:border-r">
              <div className="flex items-baseline justify-between px-3 pt-2">
                <span className="eyebrow">Book at {hms(at.ts)}</span>
                <span className="font-mono text-[9px] text-faint">px · qty · cum</span>
              </div>
              <Ladder at={at} />
            </div>
            <div>
              <div className="flex items-baseline justify-between px-3 pt-2">
                <span className="eyebrow">Tape up to {hms(at.ts)}</span>
                <span className="font-mono text-[9px] text-faint">px · qty · time</span>
              </div>
              <Tape at={at} />
            </div>
          </div>

          <div className="grid grid-cols-2 gap-px border-t border-line/60 bg-line/40 sm:grid-cols-4">
            {[
              { label: 'Last close', value: at.asof.close != null ? fmtPrice(at.asof.close) : '—' },
              { label: 'Funding', value: at.asof.funding_rate != null ? (at.asof.funding_rate * 100).toFixed(4) + '%' : '—' },
              { label: 'Open interest', value: at.asof.oi_value != null ? usd(at.asof.oi_value) : '—' },
              { label: 'Long / short', value: at.asof.long_short != null ? at.asof.long_short.toFixed(2) : '—' },
            ].map((c) => (
              <div key={c.label} className="bg-panel px-4 py-2.5">
                <div className="eyebrow">{c.label}</div>
                <div className="tnum mt-0.5 font-mono text-[14px] text-text">{c.value}</div>
              </div>
            ))}
          </div>
        </section>
      )}
    </div>
  )
}
