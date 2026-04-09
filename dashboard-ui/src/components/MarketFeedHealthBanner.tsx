import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'
import type { MarketFeedHealth } from '../api/types'

function formatAgeMinutes(secs: number | null): string {
  if (secs == null) return 'tick 未受信'
  if (secs < 60) return `最終 tick ${secs} 秒前`
  const mins = Math.floor(secs / 60)
  if (mins < 60) return `最終 tick ${mins} 分前`
  const hours = Math.floor(mins / 60)
  return `最終 tick ${hours} 時間前`
}

function describe(f: MarketFeedHealth): string {
  const detail = f.status === 'missing' ? 'tick 未受信' : formatAgeMinutes(f.last_tick_age_secs)
  return `${f.exchange} / ${f.pair} (${detail})`
}

export default function MarketFeedHealthBanner() {
  const { data } = useQuery({
    queryKey: ['market-feed-health'],
    queryFn: () => api.health.marketFeed(),
  })

  // While the very first request is in flight we have no data — do
  // not flash a banner. The next 15s tick will give us a real
  // answer; a brief blind window is better than a false-positive
  // alarm on every page load.
  if (!data) return null

  const degraded = data.feeds.filter((f) => f.status !== 'healthy')
  if (degraded.length === 0) return null

  return (
    <div
      role="alert"
      className="bg-red-700 text-white px-4 py-2 text-sm font-semibold border-b border-red-900"
    >
      <div className="max-w-7xl mx-auto flex flex-col gap-1">
        <div className="flex items-center gap-2">
          <span aria-hidden>⚠️</span>
          <span>市場フィード異常</span>
        </div>
        {degraded.map((f) => (
          <div key={`${f.exchange}:${f.pair}`} className="text-xs font-normal pl-6">
            {describe(f)}
          </div>
        ))}
      </div>
    </div>
  )
}
