import { useEffect, useRef, useState } from 'react'
import { Area, AreaChart, ResponsiveContainer, Tooltip, YAxis } from 'recharts'

export interface Pt { i: number; v: number }

/** Rolling series: append the current value on every poll `tick` (dataUpdatedAt). */
export function useSeries(value: number | null | undefined, tick: number, max = 90): Pt[] {
  const [data, setData] = useState<Pt[]>([])
  const iRef = useRef(0)
  useEffect(() => {
    setData((d) => [...d, { i: iRef.current++, v: value ?? 0 }].slice(-max))
  }, [tick, value, max])
  return data
}

function TipCard({ active, payload }: any) {
  if (!active || !payload?.length) return null
  return (
    <div className="rounded-md border border-line bg-raised px-2.5 py-1.5 shadow-panel">
      <span className="tnum font-mono text-[12px] text-accent">{Math.round(payload[0].value)}</span>
      <span className="ml-1 font-mono text-[10px] text-faint">msg/s</span>
    </div>
  )
}

export default function ThroughputChart({ data }: { data: Pt[] }) {
  return (
    <ResponsiveContainer width="100%" height="100%">
      <AreaChart data={data} margin={{ top: 6, right: 2, left: 2, bottom: 0 }}>
        <defs>
          <linearGradient id="tpFill" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="#2DD4BF" stopOpacity={0.42} />
            <stop offset="100%" stopColor="#2DD4BF" stopOpacity={0} />
          </linearGradient>
        </defs>
        <YAxis hide domain={[0, (max: number) => Math.max(10, Math.ceil(max * 1.25))]} />
        <Tooltip content={<TipCard />} cursor={{ stroke: '#2DD4BF', strokeOpacity: 0.35, strokeDasharray: '3 3' }} />
        <Area
          type="monotone"
          dataKey="v"
          stroke="#2DD4BF"
          strokeWidth={2}
          fill="url(#tpFill)"
          dot={false}
          isAnimationActive={false}
        />
      </AreaChart>
    </ResponsiveContainer>
  )
}
