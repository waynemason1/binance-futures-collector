import { useQuery } from '@tanstack/react-query'

const j = async (url: string) => {
  const r = await fetch(url)
  if (!r.ok) throw new Error(`${url} → ${r.status}`)
  return r.json()
}

export interface Health {
  version?: string
  collector: string
  uptime_seconds: number
  messages_total: number
  connection_status: string
  active_connections: number
  total_reconnects: number
  trades_written: number
  ws_latency_ms: number | null
  messages_per_sec: number | null
  age_seconds: number
  live: boolean
  rss_mb?: number | null
  cpu_pct?: number | null
  fd_count?: number | null
  sys_mem_used_mb?: number | null
  sys_mem_total_mb?: number | null
  sys_cpu_cores?: number | null
  // Listings discovery has detected since the collector started; the console
  // alerts on these; capture begins at the next restart.
  pending_new_listings?: { symbol: string; detected_at: string }[]
}

export interface Stream {
  key: string
  label: string
  transport: 'ws' | 'rest'
  symbols: number
  live_symbols: number
  last_written: number | null
  active: boolean
  // Event-driven WS lanes only (trades/klines/liquidations): symbols outside
  // the freshness window split into connected-but-market-silent vs truly stale
  // (order book dead too). Absent for order book and REST lanes.
  quiet_symbols?: number
  stale_symbols?: number
}

export interface Coverage {
  symbols_total: number
  files_total: number
  gaps: number
  streams: { key: string; label: string; transport: 'ws' | 'rest'; symbols: number; files: number }[]
}

// The pulse refetches fast so the desk feels live.
export const useHealth = () =>
  useQuery<Health>({ queryKey: ['health'], queryFn: () => j('/api/health'), refetchInterval: 2000 })

export const useStreams = () =>
  useQuery<{ streams: Stream[] }>({ queryKey: ['streams'], queryFn: () => j('/api/streams'), refetchInterval: 3000 })

export const useCoverage = () =>
  useQuery<Coverage>({ queryKey: ['coverage'], queryFn: () => j('/api/coverage'), refetchInterval: 5000 })

export interface Candle { time: number; open: number; high: number; low: number; close: number; volume: number }
export interface Trade { t: number; price: number; qty: number; side: string }
export interface SymbolDetail {
  symbol: string
  streams: string[]
  price: number | null
  change_pct: number | null
  change_span_min?: number
  candles: Candle[]
  trades: Trade[]
  funding_rate: number | null
  open_interest_usd: number | null
  long_short_ratio: number | null
}

export interface SymbolRow { symbol: string; id: string; streams: string[]; active: boolean; last_kline: number | null }

export const useSymbols = () =>
  useQuery<{ symbols: SymbolRow[] }>({
    queryKey: ['symbols'], queryFn: () => j('/api/symbols'), refetchInterval: 10_000,
  })

export const useSymbolDetail = (id?: string) =>
  useQuery<SymbolDetail>({
    queryKey: ['symbol', id], queryFn: () => j(`/api/symbol/${id}`), enabled: !!id, refetchInterval: 3000,
  })

export interface ValCheck { name: string; failures: number }
export interface ValType { datatype: string; rows: number; checks: ValCheck[]; ok: boolean }
export interface Validation {
  symbol: string
  date: string | null
  verdict: 'clean' | 'issues'
  orderbook: {
    depth_updates: { rows: number; continuity_breaks: number; first_break: { expected_pu: number; got_pu: number } | null; midnight_handoff?: 'ok' | 'break' | null; first_ts?: number | null; last_ts?: number | null }
    reconstruction: { snapshot_id: number | null; diffs_applied: number; crossed_books: number; seed_crossed?: boolean; best_bid: number | null; best_ask: number | null }
  } | null
  types: ValType[]
}

// Validation is a full replay, so it runs on demand rather than on a poll.
export const useValidate = (id?: string, date?: string) =>
  useQuery<Validation>({
    queryKey: ['validate', id, date],
    queryFn: () => j(`/api/validate/${id}${date ? `?date=${date}` : ''}`),
    enabled: !!id, staleTime: Infinity, gcTime: 5 * 60_000, retry: false,
  })

export const useDates = (id?: string) =>
  useQuery<{ dates: string[] }>({
    queryKey: ['dates', id], queryFn: () => j(`/api/dates/${id}`),
    enabled: !!id, staleTime: 60_000,
  })

export interface LogTail { file: string | null; label: string | null; lines: string[] }
export interface LogFile { name: string; label: string; size: number; mtime: number }

export const useLogFiles = () =>
  useQuery<{ files: LogFile[] }>({
    queryKey: ['logs-list'], queryFn: () => j('/api/logs/list'), refetchInterval: 15_000,
  })

export const useLogs = (file: string | null, lines = 500) =>
  useQuery<LogTail>({
    queryKey: ['logs', file, lines],
    queryFn: () =>
      j(`/api/logs?lines=${lines}${file ? `&file=${encodeURIComponent(file)}` : ''}`),
    refetchInterval: 3000,
  })

export interface CollectorConfig {
  path: string
  collect_all: boolean
  symbols: string[]
  update_speed_ms: number
  rest_intervals: Record<string, number>
}

export const useConfig = () =>
  useQuery<CollectorConfig>({ queryKey: ['config'], queryFn: () => j('/api/config'), staleTime: 30_000 })

const post = async (url: string, body?: unknown) => {
  const r = await fetch(url, {
    method: 'POST',
    // X-Capture-Desk is the backend's CSRF guard: a custom header can't be sent
    // cross-site without a CORS preflight, which same-origin policy rejects.
    headers: { 'Content-Type': 'application/json', 'X-Capture-Desk': '1' },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (!r.ok) throw new Error(`${url} → ${r.status}`)
  return r.json()
}

export const saveConfig = (patch: Omit<CollectorConfig, 'path'>) => post('/api/config', patch)
export const requestRestart = () => post('/api/restart')

export interface DepthLevel { price: number; qty: number; cumulative: number }
export interface OrderBook {
  symbol: string
  best_bid: number | null
  best_ask: number | null
  mid: number | null
  spread: number | null
  bids: DepthLevel[]
  asks: DepthLevel[]
}
export const useOrderBook = (id?: string, levels = 18) =>
  useQuery<OrderBook>({
    queryKey: ['orderbook', id, levels],
    queryFn: () => j(`/api/orderbook/${id}?levels=${levels}`),
    enabled: !!id, refetchInterval: 4000, retry: false,
  })

export interface StreamHealth {
  key: string; label: string; transport: 'ws' | 'rest'; symbols: number
}
export interface HealthCoverage {
  streams: StreamHealth[]
  attention: { symbol: string; age: number | null }[]
  coverage: Record<string, string[]>
}
export const useHealthCoverage = () =>
  useQuery<HealthCoverage>({
    queryKey: ['health-coverage'], queryFn: () => j('/api/health/coverage'), refetchInterval: 15_000,
  })

export interface GapEvent { event_type: string; reason: string | null; datetime_utc: string | null; timestamp: number | null }
export const useGaps = (id?: string) =>
  useQuery<{ symbol: string; events: GapEvent[] }>({
    queryKey: ['gaps', id], queryFn: () => j(`/api/gaps/${id}`), enabled: !!id, staleTime: 30_000,
  })

export interface ReplayPricePoint { t: number; c: number; v: number }
export interface ReplayEvent { t: number; side: string; price: number; qty: number; value: number }
export interface ReplayDay {
  symbol: string
  date: string
  price: ReplayPricePoint[]
  events: ReplayEvent[]
  has_book: boolean
  range: { from: number; to: number }
}
export interface ReplayAt {
  symbol: string
  ts: number
  book: {
    bids: DepthLevel[]
    asks: DepthLevel[]
    best_bid: number | null
    best_ask: number | null
    mid: number | null
    spread: number | null
    snapshot_id: number
    snapshot_ts: number
    gap?: boolean
  }
  tape: { t: number; price: number; qty: number; side: string }[]
  asof: { close: number | null; funding_rate: number | null; oi_value: number | null; long_short: number | null }
  events: ReplayEvent[]
}

export const useReplayDay = (id?: string, date?: string) =>
  useQuery<ReplayDay>({
    queryKey: ['replay-day', id, date],
    queryFn: () => j(`/api/replay/${id}/day?date=${date}`),
    enabled: !!id && !!date, staleTime: 60_000, retry: false,
  })

// Scrubbing hammers this; keep the previous instant on screen while the next
// one loads so the book morphs instead of flashing empty.
export const useReplayAt = (id?: string, ts?: number) =>
  useQuery<ReplayAt>({
    queryKey: ['replay-at', id, ts],
    queryFn: () => j(`/api/replay/${id}/at?ts=${ts}`),
    enabled: !!id && !!ts, staleTime: Infinity, gcTime: 60_000, retry: false,
    placeholderData: (prev) => prev,
  })
