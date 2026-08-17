import { useMemo, useState } from 'react'
import { Download, Package, Table2 } from 'lucide-react'
import { useSymbols } from '../lib/api'

const TYPES: { key: string; label: string }[] = [
  { key: 'orderbooks', label: 'Order book' },
  { key: 'trades', label: 'Trades' },
  { key: 'klines', label: 'Klines 1m' },
  { key: 'liquidations', label: 'Liquidations' },
  { key: 'funding_rates', label: 'Funding' },
  { key: 'open_interest', label: 'Open interest' },
  { key: 'long_short_ratio', label: 'Long / short' },
  { key: 'taker_buy_sell_ratio', label: 'Taker ratio' },
  { key: 'mark_price_klines', label: 'Mark price' },
  { key: 'index_price_klines', label: 'Index price' },
  { key: 'premium_index_klines', label: 'Premium index' },
]

const pad = (n: number) => String(n).padStart(2, '0')
// The whole app is UTC (CSV datetime_utc, stats.json), so the pickers are UTC too
// Format and parse the datetime-local strings as UTC, not the browser's tz.
const toUtcInput = (d: Date) =>
  `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}T${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}`
const ms = (v: string) => {
  const [d, t = '00:00'] = v.split('T')
  const [Y, M, D] = d.split('-').map(Number)
  const [h, mi, s] = t.split(':').map(Number)
  return Date.UTC(Y, M - 1, D, h || 0, mi || 0, s || 0)
}

function download(url: string) {
  const a = document.createElement('a')
  a.href = url
  a.rel = 'noopener'
  document.body.appendChild(a)
  a.click()
  a.remove()
}

// h-9 is load-bearing: a <select> and an <input type="datetime-local"> render at
// different intrinsic heights from identical padding, and the row is items-end,
// so without a shared height the shorter control drags its label out of line.
const fieldCls =
  'h-9 rounded-lg border border-line bg-raised px-3 font-mono text-[12px] text-text focus:border-accent focus:outline-none'
const selectCls = `${fieldCls} min-w-[10rem]`

function SymbolSelect({
  value, onChange, symbols,
}: {
  value: string
  onChange: (v: string) => void
  symbols: { id: string; symbol: string }[]
}) {
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)} className={selectCls}>
      {symbols.map((s) => (
        <option key={s.id} value={s.id}>{s.symbol}</option>
      ))}
    </select>
  )
}

export default function Export() {
  const { data } = useSymbols()
  const symbols = data?.symbols ?? []

  const now = useMemo(() => new Date(), [])
  const hourAgo = useMemo(() => new Date(Date.now() - 3_600_000), [])

  const [sym, setSym] = useState('btcusdt')
  const [picked, setPicked] = useState<string[]>(['orderbooks', 'klines', 'premium_index_klines'])
  const [from, setFrom] = useState(toUtcInput(hourAgo))
  const [to, setTo] = useState(toUtcInput(now))
  const [format, setFormat] = useState<'bundle' | 'merged'>('merged')

  const [obSym, setObSym] = useState('btcusdt')
  const [at, setAt] = useState(toUtcInput(now))

  const toggle = (k: string) =>
    setPicked((p) => (p.includes(k) ? p.filter((x) => x !== k) : [...p, k]))

  // A cleared datetime-local input makes ms() return NaN, and NaN comparisons
  // are all false, so validity needs an explicit finite check, not just from<=to.
  const rangeValid = Number.isFinite(ms(from)) && Number.isFinite(ms(to)) && ms(from) <= ms(to)
  const atValid = Number.isFinite(ms(at))

  const runRange = () => {
    if (!picked.length || !rangeValid) return
    const base = format === 'bundle' ? '/api/export/bundle' : '/api/export/merged'
    download(`${base}?symbols=${sym}&types=${picked.join(',')}&from=${ms(from)}&to=${ms(to)}`)
  }
  const runOb = () => {
    if (!atValid) return
    download(`/api/export/orderbook?symbol=${obSym}&at=${ms(at)}`)
  }

  return (
    <div className="mx-auto min-h-full max-w-[1080px] px-6 py-8">
      <header className="mb-6">
        <h1 className="font-display text-[20px] font-bold leading-none tracking-tightest">Export</h1>
        <p className="mt-1.5 font-mono text-[11px] text-faint">
          slice the collected data by time: a research bundle or a time-aligned feature matrix
        </p>
      </header>

      {/* Range export */}
      <section className="panel mb-5 p-5">
        <h2 className="mb-4 font-display text-[14px] font-medium">Range export</h2>

        <div className="mb-4 flex flex-wrap items-end gap-4">
          <label className="flex flex-col gap-1.5">
            <span className="eyebrow">Symbol</span>
            <SymbolSelect value={sym} onChange={setSym} symbols={symbols} />
          </label>
          <label className="flex flex-col gap-1.5">
            <span className="eyebrow">From</span>
            <input type="datetime-local" value={from} onChange={(e) => setFrom(e.target.value)}
              className={fieldCls} style={{ colorScheme: 'dark' }} />
          </label>
          <label className="flex flex-col gap-1.5">
            <span className="eyebrow">To</span>
            <input type="datetime-local" value={to} onChange={(e) => setTo(e.target.value)}
              className={fieldCls} style={{ colorScheme: 'dark' }} />
          </label>
        </div>

        <div className="mb-4">
          <div className="eyebrow mb-2">Data types</div>
          <div className="flex flex-wrap gap-2">
            {TYPES.map((t) => {
              const on = picked.includes(t.key)
              return (
                <button
                  key={t.key}
                  onClick={() => toggle(t.key)}
                  className={`rounded-full border px-3 py-1 font-mono text-[11.5px] transition-colors ${
                    on ? 'border-accent/50 bg-accent/10 text-accent' : 'border-line text-faint hover:text-muted'
                  }`}
                >
                  {t.label}
                </button>
              )
            })}
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-3">
          <div className="flex overflow-hidden rounded-lg border border-line">
            {(['merged', 'bundle'] as const).map((f) => (
              <button
                key={f}
                onClick={() => setFormat(f)}
                className={`flex items-center gap-1.5 px-3 py-1.5 font-mono text-[11.5px] transition-colors ${
                  format === f ? 'bg-line text-text' : 'text-faint hover:text-muted'
                }`}
              >
                {f === 'merged' ? <Table2 size={12} /> : <Package size={12} />}
                {f === 'merged' ? 'Merged CSV' : 'Bundle ZIP'}
              </button>
            ))}
          </div>
          <button
            onClick={runRange}
            disabled={!picked.length || !rangeValid}
            title={!rangeValid ? 'Pick a valid From/To range' : undefined}
            className="inline-flex h-9 items-center gap-1.5 rounded-lg border border-accent/40 bg-accent/10 px-3.5 font-mono text-[12px] text-accent transition-colors hover:bg-accent/20 disabled:opacity-40"
          >
            <Download size={12} /> Download
          </button>
        </div>

        <p className="mt-3 font-mono text-[10.5px] leading-relaxed text-faint">
          {format === 'merged'
            ? 'Merged: one CSV on a 1-minute grid: periodic series join by timestamp, the order book as snapshot-sampled (~10 min) top-of-book columns forward-filled (best bid/ask/mid/spread), trades and liquidations aggregated per minute. Times are UTC.'
            : 'Bundle: a ZIP with one native CSV per data type, each sliced to the window and kept in its original schema. Times are UTC.'}
        </p>
      </section>

      {/* Point-in-time order book */}
      <section className="panel p-5">
        <h2 className="mb-1 font-display text-[14px] font-medium">Order book at a point in time</h2>
        <p className="mb-4 font-mono text-[10.5px] text-faint">
          Rebuild the full book as of an instant (nearest snapshot + diffs up to it) and download every level.
        </p>
        <div className="flex flex-wrap items-end gap-4">
          <label className="flex flex-col gap-1.5">
            <span className="eyebrow">Symbol</span>
            <SymbolSelect value={obSym} onChange={setObSym} symbols={symbols} />
          </label>
          <label className="flex flex-col gap-1.5">
            <span className="eyebrow">As of (UTC)</span>
            <input type="datetime-local" value={at} onChange={(e) => setAt(e.target.value)}
              className={fieldCls} style={{ colorScheme: 'dark' }} />
          </label>
          <button
            onClick={runOb}
            disabled={!atValid}
            title={!atValid ? 'Pick a valid time' : undefined}
            className="inline-flex h-9 items-center gap-1.5 rounded-lg border border-accent/40 bg-accent/10 px-3.5 font-mono text-[12px] text-accent transition-colors hover:bg-accent/20 disabled:opacity-40"
          >
            <Download size={12} /> Download book
          </button>
        </div>
      </section>
    </div>
  )
}
