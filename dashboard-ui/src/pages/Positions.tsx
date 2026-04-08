import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import PageFilters, { type PageFilterValue } from '../components/PageFilters'
import { RiskBadge, useStrategyRiskLookup } from '../components/RiskBadge'

export default function Positions() {
  const queryClient = useQueryClient()
  const [filters, setFilters] = useState<PageFilterValue>({})

  const { data: positions, isLoading } = useQuery({
    queryKey: ['positions'],
    queryFn: () => api.positions.list(),
  })
  const lookupRisk = useStrategyRiskLookup()

  const handleReload = () => {
    queryClient.invalidateQueries({ queryKey: ['positions'] })
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
                <th className="px-4 py-2 text-right text-gray-400 font-medium">SL</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">TP</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">エントリー日時</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">口座</th>
              </tr>
            </thead>
            <tbody>
              {isLoading ? (
                <tr>
                  <td colSpan={10} className="px-4 py-8 text-center text-gray-500">
                    読み込み中...
                  </td>
                </tr>
              ) : !filtered.length ? (
                <tr>
                  <td colSpan={10} className="px-4 py-8 text-center text-gray-500">
                    保有ポジションはありません
                  </td>
                </tr>
              ) : (
                filtered.map((p) => (
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
                    <td className="px-4 py-2 text-right">
                      {Number(p.entry_price).toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-right">
                      {p.quantity ? Number(p.quantity).toLocaleString() : '-'}
                    </td>
                    <td className="px-4 py-2 text-right">
                      {Number(p.stop_loss).toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-right">
                      {Number(p.take_profit).toLocaleString()}
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
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  )
}
