import { motion } from 'framer-motion'
import { Activity, CheckCircle2, Database, FileStack, Gauge, Layers, Radio, Zap } from 'lucide-react'
import { useHealth, useStreams, useCoverage, type Health, type Stream, type Coverage } from '../lib/api'
import AnimatedNumber from '../components/AnimatedNumber'
import ThroughputChart, { useSeries } from '../components/ThroughputChart'
import SymbolExplorer from '../components/SymbolExplorer'
import { fmtAgo } from '../lib/format'

/* ------------------------------------------------------------------ format */
const nf = (n?: number | null) => (n == null ? '—' : Math.round(n).toLocaleString('en-US'))
const compact = (n?: number | null) =>
  n == null ? '—' : n >= 1e6 ? (n / 1e6).toFixed(2) + 'M' : n >= 1e3 ? (n / 1e3).toFixed(1) + 'k' : String(Math.round(n))
const fmtUptime = (s?: number) => {
  if (s == null) return '—'
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = Math.floor(s % 60)
  return h ? `${h}h ${m}m` : m ? `${m}m ${sec}s` : `${sec}s`
}

const EASE: [number, number, number, number] = [0.22, 1, 0.36, 1]
const rise = { initial: { opacity: 0, y: 12 }, animate: { opacity: 1, y: 0, transition: { duration: 0.5, ease: EASE } } }
const container = { animate: { transition: { staggerChildren: 0.07 } } }

/* -------------------------------------------------------------------- head */
function Masthead({ h }: { h?: Health }) {
  const live = !!h?.live
  const fresh = h?.pending_new_listings ?? []
  return (
    <header className="flex flex-wrap items-center justify-between gap-4">
      <div className="flex items-center gap-3">
        <div className="grid h-9 w-9 place-items-center rounded-lg border border-line bg-raised">
          <Radio size={17} className={live ? 'text-live' : 'text-faint'} />
        </div>
        <div>
          <div className="flex items-baseline gap-2">
            <h1 className="font-display text-[20px] font-bold leading-none tracking-tightest">Capture Desk</h1>
            {/* Which version is running. Without this, "am I up to date?" is
                unanswerable from the UI and the user has to go digging. */}
            {h?.version && (
              <span
                className="rounded border border-line px-1.5 py-0.5 font-mono text-[10px] text-faint"
                title="Installed version. Compare against the latest release on GitHub."
              >
                v{h.version}
              </span>
            )}
          </div>
          <p className="mt-1 font-mono text-[11px] text-faint">binance usdt-m futures · raw stream capture</p>
        </div>
      </div>
      {fresh.length > 0 && (
        <span
          className="inline-flex items-center gap-1.5 rounded-full border border-live/50 bg-live/10 px-3 py-1 font-mono text-[11px] text-live"
          title={fresh.map((l) => `${l.symbol.toUpperCase()} · detected ${l.detected_at.slice(11, 19)} UTC`).join('\n')}
        >
          <span className="lamp breathe h-1.5 w-1.5" style={{ background: '#FFB224', boxShadow: '0 0 8px #FFB224' }} />
          new pair{fresh.length > 1 ? 's' : ''} · {fresh.map((l) => l.symbol.toUpperCase()).join(', ')} · restart to capture
        </span>
      )}
      <div className="flex items-center gap-2.5 rounded-full border border-line bg-panel px-3.5 py-1.5">
        <span className={`lamp h-2 w-2 ${live ? 'breathe' : ''}`}
          style={{ background: live ? '#FFB224' : '#5C6675', boxShadow: live ? '0 0 10px #FFB224' : 'none' }} />
        <span className="font-mono text-[11.5px]">
          <span className={live ? 'text-live' : 'text-faint'}>{live ? 'LIVE' : 'IDLE'}</span>
          {/* On a fresh install there is no heartbeat yet, and "seen never ago" is
              the first thing a new user reads. Say what is actually happening. */}
          <span className="text-faint">
            {live
              ? ` · ${fmtUptime(h?.uptime_seconds)}`
              : h?.age_seconds == null
                ? ' · waiting for the collector'
                : ` · seen ${fmtAgo(h.age_seconds)} ago`}
          </span>
        </span>
      </div>
    </header>
  )
}

/* ---------------------------------------------------------------- on disk */
function OnDisk({ c }: { c?: Coverage }) {
  const gapFree = c && c.gaps === 0
  const Item = ({ icon, label, value, tone }: { icon: React.ReactNode; label: string; value: string; tone?: 'good' }) => (
    <div className="flex items-center gap-3.5 px-5 py-4">
      <div className="grid h-9 w-9 place-items-center rounded-lg bg-raised text-muted">{icon}</div>
      <div>
        <div className="eyebrow">{label}</div>
        <div className={`tnum font-mono text-[22px] font-medium leading-tight ${tone === 'good' ? 'text-good' : 'text-text'}`}>{value}</div>
      </div>
    </div>
  )
  return (
    <motion.section variants={rise} className="panel grid grid-cols-1 divide-y divide-line sm:grid-cols-3 sm:divide-x sm:divide-y-0">
      <Item icon={<Database size={17} />} label="Symbols on disk" value={nf(c?.symbols_total)} />
      <Item icon={<FileStack size={17} />} label="CSV files" value={nf(c?.files_total)} />
      <Item icon={<CheckCircle2 size={17} className={gapFree ? 'text-good' : 'text-alert'} />}
        label={gapFree ? 'Gaps · continuous' : 'Gaps'} value={nf(c?.gaps)} tone={gapFree ? 'good' : undefined} />
    </motion.section>
  )
}

/* ------------------------------------------------------------------- hero */
function Hero({ h, series }: { h?: Health; series: { i: number; v: number }[] }) {
  const Pill = ({ icon, children }: { icon: React.ReactNode; children: React.ReactNode }) => (
    <span className="inline-flex items-center gap-1.5 font-mono text-[11px] text-muted">
      <span className="text-faint">{icon}</span>{children}
    </span>
  )
  return (
    <motion.section variants={rise} className="panel overflow-hidden">
      <div className="grid gap-4 md:grid-cols-[minmax(200px,1fr)_2.2fr]">
        <div className="border-b border-line p-5 md:border-b-0 md:border-r">
          <div className="flex items-center gap-2 eyebrow"><Activity size={13} /> Throughput</div>
          <div className="mt-2 flex items-baseline gap-2">
            <AnimatedNumber value={h?.messages_per_sec ?? 0} className="tnum font-mono text-[52px] font-semibold leading-none text-accent" />
            <span className="font-mono text-[13px] text-faint">msg/s</span>
          </div>
          <div className="mt-4 flex flex-wrap gap-x-4 gap-y-2">
            <Pill icon={<Layers size={12} />}>{nf(h?.active_connections)} conn</Pill>
            <Pill icon={<Zap size={12} />}>{h?.ws_latency_ms != null ? h.ws_latency_ms.toFixed(2) : '—'} ms proc lag</Pill>
            <Pill icon={<Gauge size={12} />}>{h?.connection_status ?? '—'}</Pill>
          </div>
        </div>
        <div className="h-[168px] w-full p-2 pr-3">
          <ThroughputChart data={series} />
        </div>
      </div>
    </motion.section>
  )
}

/* --------------------------------------------------------------- counters */
function Counters({ h }: { h?: Health }) {
  const Cell = ({ label, value }: { label: string; value: string }) => (
    <div className="px-5 py-3.5">
      <div className="eyebrow">{label}</div>
      <div className="tnum mt-1 font-mono text-[19px] text-text">{value}</div>
    </div>
  )
  return (
    <motion.section variants={rise} className="panel grid grid-cols-1 divide-y divide-line sm:grid-cols-3 sm:divide-x sm:divide-y-0">
      <Cell label="Messages" value={compact(h?.messages_total)} />
      <Cell label="Trades written" value={compact(h?.trades_written)} />
      {/* All three cells are lifetime counters, compacted for a consistent row.
          "Retrying now" read as a live count of streams currently down. It is
          actually the cumulative reconnect total, which sits at its ramp-up
          value for days. */}
      <Cell label="Reconnects" value={compact(h?.total_reconnects)} />
    </motion.section>
  )
}

/* --------------------------------------------------------- stream monitor */
// An event-driven lane (trades/klines/liquidations) reports quiet_symbols; when
// nothing is book-dead it's connected-and-healthy even with no recent event, so
// a market-silent lane counts as live and reads "quiet" rather than "idle".
const laneConnected = (s: Stream) =>
  s.active || (s.quiet_symbols != null && (s.stale_symbols ?? 0) === 0)

function Channel({ s }: { s: Stream }) {
  const live = s.live_symbols ?? s.symbols
  // Event-driven lanes (trades/liquidations/klines) report quiet_symbols:
  // connected pairs with nothing to print right now (order book still fresh).
  // Those count as healthy; the denominator display is reserved for pairs
  // that are actually stale, so a sleepy alt doesn't read as a dropped stream.
  const quiet = s.quiet_symbols ?? 0
  const stale = s.stale_symbols ?? Math.max(0, s.symbols - live)
  const healthy = live + quiet
  const raw = s.symbols ? Math.round((healthy / s.symbols) * 100) : 0
  const pct = healthy > 0 ? Math.max(3, raw) : 0 // don't paint a 3% sliver when nothing is live
  const partial = stale > 0
  const connected = laneConnected(s)
  return (
    <div className="flex items-center gap-3 px-4 py-2.5">
      <span className={`lamp h-1.5 w-1.5 shrink-0 ${connected ? 'breathe' : ''}`}
        style={{ background: connected ? '#FFB224' : '#39414D', boxShadow: connected ? '0 0 8px rgba(255,178,36,0.6)' : 'none' }} />
      <span className="min-w-0 flex-1 truncate font-display text-[13.5px] sm:w-36 sm:flex-none">{s.label}</span>
      <div className="hidden h-[5px] flex-1 overflow-hidden rounded-full bg-raised sm:block">
        <div className="h-full rounded-full" style={{ width: `${pct}%`, background: connected ? '#2DD4BF' : '#2b3440' }} />
      </div>
      <span className="tnum w-16 shrink-0 text-right font-mono text-[13px] text-text">
        {partial ? healthy : s.symbols}
        {partial && <span className="text-alert">/{s.symbols}</span>}
      </span>
      <span className="w-16 shrink-0 whitespace-nowrap text-right font-mono text-[10.5px]">
        {!connected ? <span className="text-faint">idle {fmtAgo(s.last_written)}</span>
          : partial ? <span className="text-alert">{stale} stale</span>
          : quiet > 0 ? <span className="text-faint">{quiet} quiet</span>
          : <span className="text-live">live</span>}
      </span>
    </div>
  )
}

function StreamMonitor({ streams }: { streams?: Stream[] }) {
  if (!streams) return null
  const ws = streams.filter((s) => s.transport === 'ws')
  const rest = streams.filter((s) => s.transport === 'rest')
  const liveCount = streams.filter(laneConnected).length
  const Group = ({ title, sub, rows }: { title: string; sub: string; rows: Stream[] }) => (
    <div className="min-w-0">
      <div className="mb-0.5 flex items-baseline justify-between px-4 pt-1">
        <span className="eyebrow">{title}</span><span className="font-mono text-[10px] text-faint">{sub}</span>
      </div>
      <div className="divide-y divide-line/50">{rows.map((s) => <Channel key={s.key} s={s} />)}</div>
    </div>
  )
  return (
    <motion.section variants={rise}>
      <div className="mb-2.5 flex items-baseline justify-between">
        <h2 className="font-display text-[14px] font-medium">Stream monitor</h2>
        <span className="font-mono text-[11px] text-faint"><span className={liveCount ? 'text-live' : ''}>{liveCount}</span>/{streams.length} live</span>
      </div>
      <div className="panel grid gap-x-6 gap-y-4 py-3 lg:grid-cols-2">
        <Group title="WebSocket · live push" sub="continuous" rows={ws} />
        <Group title="REST · polled" sub="interval" rows={rest} />
      </div>
    </motion.section>
  )
}

/* ------------------------------------------------------ collector health */
const gb = (mb?: number | null) => (mb == null ? '—' : (mb / 1024).toFixed(1) + ' GiB')

function HCell({ label, value, sub }: { label: string; value: string; sub?: React.ReactNode }) {
  return (
    <div className="px-5 py-3.5">
      <div className="eyebrow">{label}</div>
      <div className="tnum mt-1 font-mono text-[18px] leading-tight text-text">{value}</div>
      {sub && <div className="mt-1 font-mono text-[10.5px] text-faint">{sub}</div>}
    </div>
  )
}

function CollectorHealth({ h }: { h: Health }) {
  const cores = h.sys_cpu_cores ?? null
  const memPct =
    h.sys_mem_total_mb && h.sys_mem_used_mb != null
      ? Math.round((h.sys_mem_used_mb / h.sys_mem_total_mb) * 100)
      : null
  // Collector CPU as a share of the whole machine. cpu_pct is percent-of-one-core,
  // so dividing by the core count gives percent-of-all-cores (100% = every core).
  const cpuPct = h.cpu_pct != null ? (cores ? h.cpu_pct / cores : h.cpu_pct) : null
  return (
    <motion.section variants={rise}>
      <div className="mb-2.5 flex items-baseline justify-between">
        <h2 className="font-display text-[14px] font-medium">Collector health</h2>
        <span className="font-mono text-[11px] text-faint">process · host</span>
      </div>
      <div className="panel grid gap-y-px md:grid-cols-2 md:divide-x md:divide-line">
        <div className="grid grid-cols-2 divide-x divide-line">
          <HCell label="Collector RSS" value={h.rss_mb != null ? Math.round(h.rss_mb).toLocaleString() + ' MiB' : '—'} sub="resident" />
          <HCell
            label="System memory"
            value={gb(h.sys_mem_used_mb) + ' / ' + gb(h.sys_mem_total_mb)}
            sub={
              memPct != null ? (
                <span className="flex items-center gap-1.5">
                  <span className="h-1 w-14 overflow-hidden rounded-full bg-raised">
                    <span className="block h-full rounded-full bg-accent/70" style={{ width: memPct + '%' }} />
                  </span>
                  {memPct}%
                </span>
              ) : undefined
            }
          />
        </div>
        <div className="grid grid-cols-2 divide-x divide-line">
          <HCell
            label="Collector CPU"
            value={cpuPct == null ? '—' : cpuPct > 0 && cpuPct < 1 ? '<1%' : Math.round(cpuPct) + '%'}
            sub={cores ? `of ${cores} cores` : 'of one core'}
          />
          <HCell label="Open FDs" value={h.fd_count != null ? h.fd_count.toLocaleString() : '—'} sub="descriptors" />
        </div>
      </div>
    </motion.section>
  )
}

/* -------------------------------------------------------------------- page */
export default function Console() {
  const health = useHealth()
  const h = health.data
  const series = useSeries(h?.messages_per_sec, health.dataUpdatedAt)
  const { data: streamsResp } = useStreams()
  const { data: c } = useCoverage()

  return (
    <div className="mx-auto min-h-full max-w-[1080px] px-6 py-8">
      <Masthead h={h} />
      {health.isError ? (
        <div className="panel mt-6 px-5 py-10 text-center font-mono text-[13px] text-muted">
          No heartbeat from the collector backend. Start it and this fills in.
        </div>
      ) : (
        <motion.div variants={container} initial="initial" animate="animate" className="mt-6 space-y-5">
          <OnDisk c={c} />
          <Hero h={h} series={series} />
          <Counters h={h} />
          {h?.rss_mb != null && <CollectorHealth h={h} />}
          <StreamMonitor streams={streamsResp?.streams} />
          <SymbolExplorer />
        </motion.div>
      )}
    </div>
  )
}
