import { useHealthCoverage } from '../lib/api'

const fmtAge = (s: number | null): string =>
  s == null ? '—' : s < 90 ? `${Math.round(s)}s` : s < 3600 ? `${Math.round(s / 60)}m` : `${(s / 3600).toFixed(1)}h`

export default function Coverage() {
  const { data, isError } = useHealthCoverage()
  const streams = data?.streams ?? []
  const attention = data?.attention ?? []
  const coverage = data?.coverage ?? {}
  // Contiguous UTC dates from the earliest present day to the latest (capped 14,
  // newest first): a whole-collector-outage day then shows as an empty column
  // instead of silently vanishing from the union of present days.
  // Drop any malformed day key so one bad value can't crash the whole tab.
  const present = [...new Set(Object.values(coverage).flat())]
    .filter((k) => !Number.isNaN(new Date(k + 'T00:00:00Z').getTime()))
    .sort()
  const days: string[] = []
  if (present.length) {
    const d = new Date(present[present.length - 1] + 'T00:00:00Z')
    while (days.length < 14) {
      const s = d.toISOString().slice(0, 10)
      days.push(s)
      if (s <= present[0]) break
      d.setUTCDate(d.getUTCDate() - 1)
    }
  }

  return (
    <div className="mx-auto min-h-full max-w-[1080px] px-6 py-8">
      <header className="mb-6">
        <h1 className="font-display text-[20px] font-bold leading-none tracking-tightest">Health &amp; Coverage</h1>
        <p className="mt-1.5 font-mono text-[11px] text-faint">
          which UTC days each stream has captured, and any symbols that have gone quiet · live receiving counts live on the Console
        </p>
      </header>

      {isError && (
        <div className="panel mb-5 border-alert/30 p-4 font-mono text-[12px] text-alert">
          couldn&apos;t reach the collector API, so coverage is unavailable. retrying…
        </div>
      )}

      {/* Capture coverage heatmap */}
      <section className="panel mb-5 p-5">
        <h2 className="mb-1 font-display text-[14px] font-medium">Capture coverage</h2>
        <p className="mb-4 font-mono text-[10.5px] text-faint">a filled cell = that stream has data for that UTC day</p>
        <div className="overflow-x-auto">
          <table className="border-separate border-spacing-1">
            <thead>
              <tr>
                <th />
                {days.map((d) => (
                  <th key={d} className="whitespace-nowrap px-0 pb-1 text-center font-mono text-[9px] font-normal text-faint">
                    {d.slice(5)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {streams.map((s) => (
                <tr key={s.key}>
                  <td className="whitespace-nowrap pr-3 text-right font-mono text-[11px] text-muted">
                    {s.label} <span className="text-faint">· {s.symbols}</span>
                  </td>
                  {days.map((d) => {
                    const has = (coverage[s.key] ?? []).includes(d)
                    return (
                      <td
                        key={d}
                        role="img"
                        aria-label={`${s.label} · ${d} · ${has ? 'captured' : 'no data'}`}
                        title={`${s.label} · ${d}${has ? '' : ' · none'}`}
                        className="h-6 w-9 rounded"
                        style={{ background: has ? 'rgba(45,212,191,0.5)' : 'rgba(255,255,255,0.04)' }}
                      />
                    )
                  })}
                </tr>
              ))}
              {!streams.length && (
                <tr>
                  <td className="font-mono text-[12px] text-faint">loading…</td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </section>

      {/* Needs attention */}
      <section className="panel p-5">
        <h2 className="mb-1 font-display text-[14px] font-medium">Needs attention</h2>
        <p className="mb-4 font-mono text-[10.5px] text-faint">
          symbols whose order book has gone quiet (stalest first). A live symbol streams constant depth
        </p>
        {attention.length ? (
          <div className="flex flex-wrap gap-2">
            {attention.map((a) => (
              <span
                key={a.symbol}
                className="rounded-md border border-alert/30 bg-alert/5 px-2.5 py-1 font-mono text-[11px] text-alert"
              >
                {a.symbol} <span className="text-faint">{fmtAge(a.age)}</span>
              </span>
            ))}
          </div>
        ) : (
          <div className="font-mono text-[12px] text-good">All symbols streaming, nothing stale.</div>
        )}
      </section>
    </div>
  )
}
