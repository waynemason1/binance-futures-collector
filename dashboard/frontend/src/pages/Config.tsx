import { useEffect, useMemo, useState } from 'react'
import { Save, RotateCw, Check } from 'lucide-react'
import { useConfig, useSymbols, saveConfig, requestRestart } from '../lib/api'

const REST_FIELDS: { key: string; label: string }[] = [
  { key: 'funding_rates_seconds', label: 'Funding' },
  { key: 'open_interest_seconds', label: 'Open interest' },
  { key: 'long_short_ratio_seconds', label: 'Long / short' },
  { key: 'taker_buy_sell_ratio_seconds', label: 'Taker ratio' },
  { key: 'mark_price_klines_seconds', label: 'Mark price' },
  { key: 'index_price_klines_seconds', label: 'Index price' },
  { key: 'premium_index_klines_seconds', label: 'Premium index' },
]
const SPEEDS = [100, 250, 500]

export default function Config() {
  const { data: cfg } = useConfig()
  const { data: symData } = useSymbols()
  const allSymbols = symData?.symbols ?? []

  const [collectAll, setCollectAll] = useState(true)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [speed, setSpeed] = useState(500)
  const [rest, setRest] = useState<Record<string, number>>({})
  const [filter, setFilter] = useState('')
  const [status, setStatus] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  // Seed the form from the file once it loads.
  useEffect(() => {
    if (!cfg) return
    setCollectAll(cfg.collect_all)
    setSelected(new Set(cfg.symbols))
    setSpeed(cfg.update_speed_ms)
    setRest(cfg.rest_intervals)
  }, [cfg])

  const filtered = useMemo(() => {
    const f = filter.trim().toLowerCase()
    return (f ? allSymbols.filter((s) => s.id.includes(f)) : allSymbols).slice(0, 500)
  }, [allSymbols, filter])

  const toggle = (id: string) =>
    setSelected((p) => {
      const n = new Set(p)
      n.has(id) ? n.delete(id) : n.add(id)
      return n
    })

  const run = async (fn: () => Promise<unknown>, ok: string) => {
    setBusy(true)
    setStatus(null)
    try {
      await fn()
      setStatus(ok)
    } catch (e) {
      setStatus(`Failed: ${(e as Error).message}`)
    } finally {
      setBusy(false)
    }
  }

  const save = () =>
    run(
      () =>
        saveConfig({
          collect_all: collectAll,
          symbols: [...selected],
          update_speed_ms: speed,
          rest_intervals: rest,
        }),
      'Saved to config.toml. Restart the collector to apply.',
    )

  return (
    <div className="mx-auto min-h-full max-w-[1080px] px-6 py-8">
      <header className="mb-6">
        <h1 className="font-display text-[20px] font-bold leading-none tracking-tightest">Config</h1>
        <p className="mt-1.5 font-mono text-[11px] text-faint">
          edit the collector's pairs and cadence · applied on the next restart
        </p>
      </header>

      {/* Symbols */}
      <section className="panel mb-5 p-5">
        <div className="mb-3 flex items-center justify-between">
          <h2 className="font-display text-[14px] font-medium">Pairs</h2>
          <button
            onClick={() => setCollectAll((v) => !v)}
            className={`rounded-full border px-3 py-1 font-mono text-[11px] transition-colors ${
              collectAll ? 'border-accent/50 bg-accent/10 text-accent' : 'border-line text-faint hover:text-muted'
            }`}
          >
            {collectAll ? 'Collecting all discovered' : 'Whitelist'}
          </button>
        </div>

        {collectAll ? (
          <p className="font-mono text-[11px] leading-relaxed text-faint">
            Every tradeable USDT-M perpetual is collected; new listings are noticed by discovery and captured from the next restart.
            Switch to a whitelist to restrict capture to chosen pairs.
          </p>
        ) : (
          <>
            <div className="mb-2 flex items-center gap-3">
              <input
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder="filter…"
                className="w-40 rounded-lg border border-line bg-raised px-3 py-1.5 font-mono text-[12px] text-text focus:border-accent focus:outline-none"
              />
              <span className="font-mono text-[11px] text-faint">{selected.size} selected</span>
              {selected.size > 0 && (
                <button onClick={() => setSelected(new Set())} className="font-mono text-[11px] text-faint hover:text-muted">
                  clear
                </button>
              )}
            </div>
            <div className="grid max-h-64 grid-cols-2 gap-x-4 gap-y-1 overflow-auto rounded-lg border border-line bg-raised/40 p-3 sm:grid-cols-3">
              {filtered.map((s) => {
                const on = selected.has(s.id)
                return (
                  <button
                    key={s.id}
                    onClick={() => toggle(s.id)}
                    className={`flex items-center gap-2 rounded px-1.5 py-1 text-left font-mono text-[11.5px] transition-colors ${
                      on ? 'text-accent' : 'text-faint hover:text-muted'
                    }`}
                  >
                    <span
                      className={`flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-sm border ${
                        on ? 'border-accent bg-accent/20' : 'border-line'
                      }`}
                    >
                      {on && <Check size={10} />}
                    </span>
                    {s.symbol}
                  </button>
                )
              })}
              {!filtered.length && <span className="col-span-full font-mono text-[11px] text-faint">no matches</span>}
            </div>
          </>
        )}
      </section>

      {/* Granularity */}
      <section className="panel mb-5 p-5">
        <h2 className="mb-4 font-display text-[14px] font-medium">Granularity</h2>

        <div className="mb-6 border-b border-line/60 pb-6">
          <div className="mb-1 flex items-baseline gap-2">
            <span className="eyebrow">WebSocket streams</span>
            <span className="font-mono text-[10px] text-faint">· live push · 4 datatypes</span>
          </div>
          <p className="mb-3 max-w-2xl font-mono text-[10.5px] leading-relaxed text-faint">
            Order book, trades, klines and liquidations push continuously, so there&apos;s no poll interval to set. The
            only tunable WebSocket rate is how often Binance sends a depth diff for the order book:
          </p>
          <div className="eyebrow mb-2">Order book update speed</div>
          <div className="flex overflow-hidden rounded-lg border border-line" style={{ width: 'fit-content' }}>
            {SPEEDS.map((ms) => (
              <button
                key={ms}
                onClick={() => setSpeed(ms)}
                className={`px-4 py-1.5 font-mono text-[12px] transition-colors ${
                  speed === ms ? 'bg-line text-text' : 'text-faint hover:text-muted'
                }`}
              >
                {ms} ms
              </button>
            ))}
          </div>
        </div>

        <div className="mb-1 flex items-baseline gap-2">
          <span className="eyebrow">REST API polls</span>
          <span className="font-mono text-[10px] text-faint">· fetched on a timer · 7 datatypes</span>
        </div>
        <p className="mb-3 max-w-2xl font-mono text-[10.5px] leading-relaxed text-faint">
          These seven have no live stream, so the collector polls Binance&apos;s REST API on the interval below
          (seconds). Long/short and taker share the per-IP <span className="text-muted">/futures/data</span> budget,
          so their achieved cadence stretches past the set target under load; each fetch pulls the last eight
          buckets, so nothing is missed.
        </p>
        <div className="grid grid-cols-2 gap-x-6 gap-y-2 sm:grid-cols-3">
          {REST_FIELDS.map((f) => (
            <label key={f.key} className="flex items-center justify-between gap-2">
              <span className="font-mono text-[11.5px] text-muted">{f.label}</span>
              <input
                type="number"
                min={1}
                value={rest[f.key] ?? ''}
                onChange={(e) => setRest((r) => ({ ...r, [f.key]: Math.max(1, Number(e.target.value) | 0) }))}
                className="w-20 rounded-lg border border-line bg-raised px-2 py-1 text-right font-mono text-[12px] text-text focus:border-accent focus:outline-none"
              />
            </label>
          ))}
        </div>
      </section>

      {/* Actions */}
      <section className="panel flex flex-wrap items-center gap-3 p-5">
        <button
          onClick={save}
          disabled={busy}
          className="inline-flex items-center gap-1.5 rounded-lg border border-accent/40 bg-accent/10 px-3.5 py-1.5 font-mono text-[12px] text-accent transition-colors hover:bg-accent/20 disabled:opacity-40"
        >
          <Save size={12} /> Save
        </button>
        <button
          onClick={() => run(requestRestart, 'Restart requested. Applies if the collector runs under a supervisor.')}
          disabled={busy}
          className="inline-flex items-center gap-1.5 rounded-lg border border-line px-3.5 py-1.5 font-mono text-[12px] text-muted transition-colors hover:text-text disabled:opacity-40"
        >
          <RotateCw size={12} /> Restart to apply
        </button>
        {status && <span className="font-mono text-[11px] text-faint">{status}</span>}
      </section>

      <p className="mt-3 px-1 font-mono text-[10.5px] leading-relaxed text-faint">
        Changes are written to <span className="text-muted">{cfg?.path ?? 'config.toml'}</span> and take effect when the
        collector restarts. The dashboard only records the request; a supervisor (the shipped <span className="text-muted">supervise.sh</span>,
        systemd, or Docker) performs the restart, so the UI never spawns or kills a process itself.
      </p>
    </div>
  )
}
