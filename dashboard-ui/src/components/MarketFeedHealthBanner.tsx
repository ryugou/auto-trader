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
  const { data, isError, isLoading } = useQuery({
    queryKey: ['market-feed-health'],
    queryFn: () => api.health.marketFeed(),
  })

  // Distinguish three states carefully because this banner is the
  // whole point of the feature — silently hiding on a failed
  // request would defeat it:
  //   1. isLoading AND no data yet → brief blind window, render
  //      nothing (next 15s poll fills it in; better than
  //      false-positive on every first paint).
  //   2. isError → the monitoring API itself is unreachable. That
  //      is a louder failure than a single feed being stale and
  //      MUST be visible; otherwise the operator thinks everything
  //      is fine while the whole monitoring loop is dead.
  //   3. data present → walk the feed list; show banner only when
  //      at least one feed is non-healthy.
  if (isError) {
    return (
      <div
        role="alert"
        className="bg-red-700 text-white px-4 py-2 text-sm font-semibold border-b border-red-900"
      >
        <div className="max-w-7xl mx-auto flex items-center gap-2">
          <span aria-hidden>⚠️</span>
          <span>監視 API 到達不可 (/api/health/market-feed)</span>
        </div>
      </div>
    )
  }

  if (isLoading || !data) return null

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
