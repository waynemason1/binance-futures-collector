import { useEffect, useMemo, useRef, useState } from 'react'
import { Download } from 'lucide-react'
import { useLogFiles, useLogs } from '../lib/api'

const toneOf = (l: string): string =>
  /error|panic|failed|too many open|os error/i.test(l) ? 'text-alert'
  : /warn|\b418\b|\b429\b|reconnect|disconnect/i.test(l) ? 'text-live'
  : 'text-muted'

const RANK: Record<string, number> = { DEBUG: 0, INFO: 1, WARN: 2, ERROR: 3 }

const LEVELS = ['ALL', 'INFO', 'WARN', 'ERROR'] as const
type Level = (typeof LEVELS)[number]

const fmtSize = (b: number): string =>
  b >= 1 << 20 ? `${(b / (1 << 20)).toFixed(1)} MB` : `${Math.max(1, Math.round(b / 1024))} KB`

export default function Logs() {
  const { data: list } = useLogFiles()
  const files = list?.files ?? []
  const [file, setFile] = useState<string | null>(null)
  const [level, setLevel] = useState<Level>('ALL')
  const { data } = useLogs(file, 600)

  const box = useRef<HTMLDivElement>(null)
  const stick = useRef(true)

  const shown = useMemo(() => {
    const lines = data?.lines ?? []
    if (level === 'ALL') return lines
    const min = RANK[level]
    // Multi-line entries (panics, error contexts) have no level token on their
    // continuation lines inherit the entry's level so filtering ERROR/WARN keeps
    // the detail of the very errors being filtered for, instead of dropping it.
    const out: string[] = []
    let cur = 1
    for (const l of lines) {
      const m = l.match(/\b(ERROR|WARN|INFO|DEBUG)\b/)
      if (m) cur = RANK[m[1]]
      if (cur >= min) out.push(l)
    }
    return out
  }, [data, level])

  // Stable row keys: a tailing, filterable list must not key by array index
  // (indices shift as lines append and the level filter changes). Key by content,
  // disambiguating repeated identical lines by their occurrence count.
  const rows = useMemo(() => {
    const seen = new Map<string, number>()
    return shown.map((line) => {
      const n = seen.get(line) ?? 0
      seen.set(line, n + 1)
      return { key: `${line}#${n}`, line }
    })
  }, [shown])

  // Follow the tail unless the reader has scrolled up.
  useEffect(() => {
    const el = box.current
    if (el && stick.current) el.scrollTop = el.scrollHeight
  }, [shown])

  const onScroll = () => {
    const el = box.current
    if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48
  }

  const active = data?.label ?? files[0]?.label ?? 'collector'

  return (
    <div className="mx-auto min-h-full max-w-[1080px] px-6 py-8">
      <header className="mb-4 flex items-center justify-between">
        <div>
          <h1 className="font-display text-[20px] font-bold leading-none tracking-tightest">Logs</h1>
          <p className="mt-1.5 font-mono text-[11px] text-faint">{active} · tailing live</p>
        </div>
        <span className="lamp breathe h-2 w-2" style={{ background: '#FFB224', boxShadow: '0 0 8px #FFB224' }} />
      </header>

      <div className="mb-3 flex flex-wrap items-center gap-2">
        <select
          value={file ?? ''}
          onChange={(e) => setFile(e.target.value || null)}
          className="panel h-8 rounded-md px-2 font-mono text-[11px] text-muted outline-none"
        >
          <option value="">Latest{files[0] ? ` · ${files[0].label}` : ''}</option>
          {files.map((f) => (
            <option key={f.name} value={f.name}>
              {f.label} · {fmtSize(f.size)}
            </option>
          ))}
        </select>

        <div className="flex overflow-hidden rounded-md border border-line">
          {LEVELS.map((lv) => (
            <button
              key={lv}
              onClick={() => setLevel(lv)}
              className={`h-8 px-2.5 font-mono text-[10.5px] tracking-wide transition-colors ${
                level === lv ? 'bg-line text-text' : 'text-faint hover:text-muted'
              }`}
            >
              {lv}
            </button>
          ))}
        </div>

        <a
          href={`/api/logs/download${file ? `?file=${encodeURIComponent(file)}` : ''}`}
          className="panel flex h-8 items-center gap-1.5 rounded-md px-2.5 font-mono text-[11px] text-muted hover:text-text"
        >
          <Download size={12} /> download
        </a>

        <span className="ml-auto font-mono text-[10.5px] text-faint">{shown.length} lines</span>
      </div>

      <div
        ref={box}
        onScroll={onScroll}
        className="panel h-[68vh] overflow-auto p-4 font-mono text-[11.5px] leading-[1.55]"
      >
        {rows.map((r) => (
          <div key={r.key} className={`whitespace-pre-wrap ${toneOf(r.line)}`}>{r.line || ' '}</div>
        ))}
        {!shown.length && (
          <div className="text-faint">no log output{level !== 'ALL' ? ` at ${level}+` : ' yet'}</div>
        )}
      </div>
    </div>
  )
}
