import { useState } from 'react'
import { motion } from 'framer-motion'
import { ShieldCheck, ShieldAlert, Play, Check, X } from 'lucide-react'
import { useSymbols, useValidate, useDates, type Validation as Report, type ValType } from '../lib/api'

const rise = { initial: { opacity: 0, y: 10 }, animate: { opacity: 1, y: 0 } }

const LABELS: Record<string, string> = {
  trades: 'Trades',
  klines: 'Klines 1m',
  liquidations: 'Liquidations',
  funding_rates: 'Funding rates',
  open_interest: 'Open interest',
  long_short_ratio: 'Long / short ratio',
  taker_buy_sell_ratio: 'Taker ratio',
  mark_price_klines: 'Mark-price klines',
  index_price_klines: 'Index-price klines',
  premium_index_klines: 'Premium-index klines',
}

function Metric({ label, value, ok }: { label: string; value: React.ReactNode; ok?: boolean }) {
  const tone = ok === undefined ? 'text-text' : ok ? 'text-good' : 'text-alert'
  return (
    <div className="rounded-lg border border-line bg-raised/50 px-4 py-3">
      <div className="eyebrow">{label}</div>
      <div className={`tnum mt-1 break-all font-mono text-[17px] leading-tight ${tone}`}>{value}</div>
    </div>
  )
}

const hhmm = (ms: number) => new Date(ms).toISOString().slice(11, 16)

function OrderBook({ ob }: { ob: NonNullable<Report['orderbook']> }) {
  const d = ob.depth_updates
  const r = ob.reconstruction
  const fb = d.first_break
  const spreadNum = r.best_bid != null && r.best_ask != null ? r.best_ask - r.best_bid : null
  const spread = spreadNum != null ? spreadNum.toLocaleString(undefined, { maximumFractionDigits: 6 }) : '—'
  // How much of the UTC day the tape actually covers. A clean 59-minute tape
  // must not read like a clean full day.
  const spanPct =
    d.first_ts != null && d.last_ts != null ? Math.min(100, ((d.last_ts - d.first_ts) / 86_400_000) * 100) : null
  // Grade coverage only on a past (completed) UTC day, which should span the full
  // day. Today's in-progress day stops at "now" and is expected to be short, so it
  // renders neutral. Comparing the tape's UTC date to today distinguishes an
  // in-progress day from a past day that genuinely stopped early; the latter is a
  // real fault and still flags red.
  const isToday =
    d.last_ts != null &&
    new Date(d.last_ts).toISOString().slice(0, 10) === new Date().toISOString().slice(0, 10)
  const span =
    d.first_ts != null && d.last_ts != null
      ? `${hhmm(d.first_ts)}–${hhmm(d.last_ts)} · ${spanPct! < 1 ? '<1' : Math.round(spanPct!)}% of day`
      : '—'
  return (
    <section className="panel p-4">
      <h2 className="mb-3 font-display text-[14px] font-medium">Order book: reconstruct &amp; verify</h2>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        <Metric label="Diff rows" value={d.rows.toLocaleString()} />
        <Metric label="Captured span (UTC)" value={span} ok={spanPct == null || isToday ? undefined : spanPct > 95} />
        <Metric label="Continuity breaks" value={d.continuity_breaks} ok={d.continuity_breaks === 0} />
        <Metric label="Diffs applied" value={r.diffs_applied.toLocaleString()} />
        <Metric label="Crossed books (since last snapshot)" value={r.crossed_books} ok={r.crossed_books === 0} />
        <Metric label="Snapshot seed" value={r.seed_crossed ? 'crossed' : 'clean'} ok={!r.seed_crossed} />
        <Metric label="Snapshot ID" value={r.snapshot_id ?? '—'} />
        <Metric label="Best bid" value={r.best_bid != null ? r.best_bid.toLocaleString() : '—'} />
        <Metric label="Best ask" value={r.best_ask != null ? r.best_ask.toLocaleString() : '—'} />
        <Metric label="Spread" value={spread} ok={spreadNum == null ? undefined : spreadNum >= 0} />
        {d.midnight_handoff && (
          <Metric
            label="Midnight handoff"
            value={d.midnight_handoff === 'ok' ? 'chained' : 'broken'}
            ok={d.midnight_handoff === 'ok'}
          />
        )}
      </div>
      {fb && (
        <div className="mt-2 rounded-lg border border-alert/30 bg-alert/5 px-4 py-3">
          <div className="flex items-baseline justify-between gap-3">
            <span className="eyebrow text-alert">First continuity gap</span>
            <span className="tnum font-mono text-[12px] text-muted">
              {Math.abs(fb.got_pu - fb.expected_pu).toLocaleString()} updates skipped
            </span>
          </div>
          <div className="mt-1.5 break-all font-mono text-[11px] text-faint">
            expected prev-update-id <span className="text-muted">{fb.expected_pu}</span> · got{' '}
            <span className="text-muted">{fb.got_pu}</span>
          </div>
          <p className="mt-1.5 font-mono text-[10.5px] leading-relaxed text-faint">
            A single gap is almost always a collector restart; the periodic snapshot re-anchors the book
            right after. Many gaps would mean the stream is dropping messages.
          </p>
        </div>
      )}
    </section>
  )
}

function TypeCard({ t }: { t: ValType }) {
  return (
    <section className="panel p-4">
      <div className="mb-3 flex items-baseline justify-between">
        <h2 className="font-display text-[13.5px] font-medium">{LABELS[t.datatype] ?? t.datatype}</h2>
        <span className="tnum font-mono text-[10.5px] text-faint">{t.rows.toLocaleString()} rows</span>
      </div>
      <div className="space-y-1.5">
        {t.checks.map((c) => (
          <div key={c.name} className="flex items-center justify-between font-mono text-[12px]">
            <span className="text-muted">{c.name}</span>
            <span className={`inline-flex items-center gap-1 ${c.failures === 0 ? 'text-good' : 'text-alert'}`}>
              {c.failures === 0 ? <Check size={12} /> : <><X size={12} />{c.failures}</>}
            </span>
          </div>
        ))}
      </div>
    </section>
  )
}

function ReportView({ r }: { r: Report }) {
  return (
    <motion.div variants={rise} initial="initial" animate="animate" className="space-y-5">
      {r.orderbook && <OrderBook ob={r.orderbook} />}
      <div className="grid gap-5 lg:grid-cols-2">
        {r.types.map((t) => <TypeCard key={t.datatype} t={t} />)}
      </div>
    </motion.div>
  )
}

export default function Validation() {
  const { data: syms } = useSymbols()
  const list = syms?.symbols.filter((s) => s.streams.includes('orderbooks')) ?? []
  const [sel, setSel] = useState('btcusdt')
  const [dateSel, setDateSel] = useState('')
  const [target, setTarget] = useState<string | undefined>(undefined)
  const [targetDate, setTargetDate] = useState<string | undefined>(undefined)
  const { data: datesResp } = useDates(sel)
  const dayList = datesResp?.dates ?? []
  const { data, isFetching, isError, refetch } = useValidate(target, targetDate)

  const run = () => {
    if (target === sel && targetDate === (dateSel || undefined)) refetch()
    else {
      setTarget(sel)
      setTargetDate(dateSel || undefined)
    }
  }
  const verdict = data?.verdict

  return (
    <div className="mx-auto min-h-full max-w-[1080px] px-6 py-8">
      <header className="mb-6">
        <h1 className="font-display text-[20px] font-bold leading-none tracking-tightest">Validation</h1>
        <p className="mt-1.5 font-mono text-[11px] text-faint">re-checks the captured files on disk · continuity, invariants, and an order-book rebuild from the latest snapshot</p>
      </header>

      <div className="panel mb-5 flex flex-wrap items-center gap-3 px-5 py-3.5">
        <select
          value={sel}
          onChange={(e) => { setSel(e.target.value); setDateSel('') }}
          className="tnum rounded-lg border border-line bg-raised py-1.5 pl-3 pr-8 font-mono text-[13px] text-text focus:border-accent focus:outline-none"
        >
          {list.map((s) => <option key={s.id} value={s.id}>{s.symbol}</option>)}
        </select>
        <select
          value={dateSel}
          onChange={(e) => setDateSel(e.target.value)}
          className="tnum rounded-lg border border-line bg-raised py-1.5 pl-3 pr-8 font-mono text-[13px] text-muted focus:border-accent focus:outline-none"
        >
          <option value="">Latest{dayList[0] ? ` · ${dayList[0]}` : ''}</option>
          {dayList.map((d) => <option key={d} value={d}>{d}</option>)}
        </select>
        <button
          onClick={run}
          disabled={isFetching}
          className="inline-flex items-center gap-1.5 rounded-lg border border-accent/40 bg-accent/10 px-3.5 py-1.5 font-mono text-[12px] text-accent transition-colors hover:bg-accent/20 disabled:opacity-50"
        >
          <Play size={12} /> {isFetching ? 'Validating…' : 'Validate'}
        </button>
        {verdict && (
          <span className={`inline-flex items-center gap-1.5 rounded-full border px-3 py-1 font-mono text-[11px] ${
            verdict === 'clean' ? 'border-good/40 bg-good/10 text-good' : 'border-alert/40 bg-alert/10 text-alert'
          }`}>
            {verdict === 'clean' ? <ShieldCheck size={13} /> : <ShieldAlert size={13} />}
            {verdict === 'clean' ? 'CLEAN' : 'ISSUES'}
          </span>
        )}
        {data && <span className="font-mono text-[11px] text-faint">{data.date ?? 'latest'}</span>}
      </div>

      {data ? (
        <ReportView r={data} />
      ) : (
        <div className="panel px-5 py-12 text-center font-mono text-[13px] text-faint">
          {isError ? 'No data for that symbol.' : isFetching ? 'Replaying every data type…' : 'Pick a symbol and run a full validation pass.'}
        </div>
      )}
    </div>
  )
}
