import { useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import PageFilters, { type PageFilterValue } from '../components/PageFilters'
import { RiskBadge, useStrategyRiskLookup } from '../components/RiskBadge'
import type { MarketPrice, PositionResponse } from '../api/types'

function formatInt(value: string | null | undefined): string {
  if (value == null) return '-'
  const n = Number(value)
  if (Number.isNaN(n)) return '-'
  return Math.round(n).toLocaleString()
}

function formatSignedInt(n: number): string {
  if (Number.isNaN(n)) return '-'
  const sign = n > 0 ? '+' : ''
  return `${sign}${Math.round(n).toLocaleString()}`
}

function pnlColor(n: number | null): string {
  if (n == null) return 'text-gray-500'
  if (n > 0) return 'text-emerald-400'
  if (n < 0) return 'text-red-400'
  return 'text-gray-400'
}

// (current - entry) × quantity for LONG, sign-flipped for SHORT.
// Returns null when we can't compute (no price observed yet, or no
// quantity recorded on the trade row). The banner already alerts on
// missing prices, so we don't try to be clever about the cell here.
function computeUnrealizedPnl(
  position: PositionResponse,
  priceMap: Map<string, MarketPrice>,
): number | null {
  if (position.quantity == null) return null
  const key = `${position.exchange}:${position.pair}`
  const price = priceMap.get(key)
  if (!price) return null
  const current = Number(price.price)
  const entry = Number(position.entry_price)
  const qty = Number(position.quantity)
  if (Number.isNaN(current) || Number.isNaN(entry) || Number.isNaN(qty)) return null
  const diff = position.direction === 'long' ? current - entry : entry - current
  return diff * qty
}

// 純損益 = 含み損益 - 累計 fees. If gross PnL cannot be computed,
// net is also unavailable (there is no meaningful "unknown minus
// known fees" display).
function computeNetPnl(
  position: PositionResponse,
  gross: number | null,
): number | null {
  if (gross == null) return null
  const fees = Number(position.fees ?? '0')
  if (Number.isNaN(fees)) return gross
  return gross - fees
}

export default function Positions() {
  const queryClient = useQueryClient()
  const [filters, setFilters] = useState<PageFilterValue>({})

  const { data: positions, isLoading } = useQuery({
    queryKey: ['positions'],
    queryFn: () => api.positions.list(),
  })

  const { data: pricesData } = useQuery({
    queryKey: ['market-prices'],
    queryFn: () => api.market.prices(),
  })

  const priceMap = useMemo(() => {
    const m = new Map<string, MarketPrice>()
    pricesData?.prices.forEach((p) => m.set(`${p.exchange}:${p.pair}`, p))
    return m
  }, [pricesData])

  const lookupRisk = useStrategyRiskLookup()

  const handleReload = () => {
    queryClient.invalidateQueries({ queryKey: ['positions'] })
    queryClient.invalidateQueries({ queryKey: ['market-prices'] })
  }

  const JST_OFFSET_MS = 9 * 60 * 60 * 1000
  const toJstDateString = (iso: string) =>
    new Date(new Date(iso).getTime() + JST_OFFSET_MS)
      .toISOString()
      .slice(0, 10)

  const filtered = (positions ?? []).filter((p) => {
    if (filters.exchange && p.exchange !== filters.exchange) return false
    if (
      filters.paper_account_id &&
      p.paper_account_id !== filters.paper_account_id
    )
      return false
    if (filters.from) {
      const entry = toJstDateString(p.entry_at)
      if (entry < filters.from) return false
    }
    if (filters.to) {
      const entry = toJstDateString(p.entry_at)
      if (entry > filters.to) return false
    }
    return true
  })

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold">保有ポジション</h2>
        <button
          onClick={handleReload}
          className="bg-gray-700 hover:bg-gray-600 text-gray-200 text-sm font-medium px-4 py-2 rounded transition"
        >
          リロード
        </button>
      </div>

      <PageFilters value={filters} onChange={setFilters} />

      <div className="bg-gray-900 rounded-lg shadow overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-800">
                <th className="px-4 py-2 text-left text-gray-400 font-medium">戦略</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">ペア</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">取引所</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">方向</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">エントリー価格</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">数量</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">含み損益</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">純損益</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">損切りライン</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">利確ライン</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">エントリー日時</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">口座</th>
              </tr>
            </thead>
            <tbody>
              {isLoading ? (
                <tr>
                  <td colSpan={12} className="px-4 py-8 text-center text-gray-500">
                    読み込み中...
                  </td>
                </tr>
              ) : !filtered.length ? (
                <tr>
                  <td colSpan={12} className="px-4 py-8 text-center text-gray-500">
                    保有ポジションはありません
                  </td>
                </tr>
              ) : (
                filtered.map((p) => {
                  const gross = computeUnrealizedPnl(p, priceMap)
                  const net = computeNetPnl(p, gross)
                  return (
                    <tr
                      key={p.trade_id}
                      className="border-b border-gray-800/50 hover:bg-gray-800/30"
                    >
                      <td className="px-4 py-2">
                        <div className="flex items-center gap-2">
                          <RiskBadge riskLevel={lookupRisk(p.strategy_name)} />
                          <span>{p.strategy_name}</span>
                        </div>
                      </td>
                      <td className="px-4 py-2">{p.pair}</td>
                      <td className="px-4 py-2 text-gray-300">{p.exchange}</td>
                      <td className="px-4 py-2">
                        <span
                          className={
                            p.direction === 'long'
                              ? 'text-emerald-400'
                              : 'text-red-400'
                          }
                        >
                          {p.direction.toUpperCase()}
                        </span>
                      </td>
                      <td className="px-4 py-2 text-right font-mono">
                        {formatInt(p.entry_price)}
                      </td>
                      <td className="px-4 py-2 text-right font-mono">
                        {formatInt(p.quantity)}
                      </td>
                      <td className={`px-4 py-2 text-right font-mono ${pnlColor(gross)}`}>
                        {gross == null ? '-' : formatSignedInt(gross)}
                      </td>
                      <td className={`px-4 py-2 text-right font-mono ${pnlColor(net)}`}>
                        {net == null ? '-' : formatSignedInt(net)}
                      </td>
                      <td className="px-4 py-2 text-right font-mono">
                        {formatInt(p.stop_loss)}
                      </td>
                      <td className="px-4 py-2 text-right font-mono">
                        {formatInt(p.take_profit)}
                      </td>
                      <td className="px-4 py-2 text-gray-300">
                        {new Date(p.entry_at).toLocaleString('ja-JP', {
                          // Pin to JST so the entry time matches what
                          // the trader logs on the server (which is
                          // also JST-scheduled).
                          timeZone: 'Asia/Tokyo',
                          month: '2-digit',
                          day: '2-digit',
                          hour: '2-digit',
                          minute: '2-digit',
                        })}
                      </td>
                      <td className="px-4 py-2 text-gray-300">
                        {p.paper_account_name || '-'}
                      </td>
                    </tr>
                  )
                })
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  )
}
