// Crypto prices span ~8 orders of magnitude (BTC ~$60k, meme coins ~$0.000001).
// A fixed 2dp rounds every sub-dollar alt to "0.01", so pick decimals from the
// magnitude: ~5 significant figures for anything under a dollar, tighter above.
export const decimalsFor = (p?: number | null): number => {
  if (p == null || !isFinite(p) || p <= 0) return 2
  if (p >= 1000) return 2
  if (p >= 1) return 4
  return Math.min(8, Math.max(2, 4 - Math.floor(Math.log10(p))))
}

export const fmtPrice = (p?: number | null): string =>
  p == null
    ? '—'
    : p.toLocaleString('en-US', {
        minimumFractionDigits: decimalsFor(p),
        maximumFractionDigits: decimalsFor(p),
      })

// USD magnitude: $x.xxB / $x.xxM / $x.xk / $x, em dash for nullish.
export const usd = (n?: number | null): string =>
  n == null ? '—'
  : n >= 1e9 ? '$' + (n / 1e9).toFixed(2) + 'B'
  : n >= 1e6 ? '$' + (n / 1e6).toFixed(2) + 'M'
  : n >= 1e3 ? '$' + (n / 1e3).toFixed(1) + 'k'
  : '$' + n.toFixed(0)

// Compact "time since" in a single unit (s/m/h/d). The sentinel differs by
// call site, so it's a parameter.
export const fmtAgo = (s?: number | null, whenNull = 'never'): string =>
  s == null ? whenNull
  : s < 60 ? `${Math.round(s)}s`
  : s < 3600 ? `${Math.floor(s / 60)}m`
  : s < 86400 ? `${Math.floor(s / 3600)}h`
  : `${Math.floor(s / 86400)}d`
