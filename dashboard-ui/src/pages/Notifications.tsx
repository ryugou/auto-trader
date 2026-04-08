import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'
import type { Notification } from '../api/types'

const JST_OFFSET_MS = 9 * 60 * 60 * 1000

function periodToRange(period: string): { from?: string; to?: string } {
  if (!period) return {}
  // Build a JST-anchored Date by shifting +9h, then do all date math
  // on its UTC fields. This keeps the calendar arithmetic in JST so
  // crossing JST midnight does not bleed an extra day off the front
  // of "1w" / "1m" the way subtracting from a UTC-anchored Date did.
  const nowJst = new Date(Date.now() + JST_OFFSET_MS)
  const to = nowJst.toISOString().slice(0, 10)
  if (period === 'today') return { from: to, to }
  if (period === '1w') {
    nowJst.setUTCDate(nowJst.getUTCDate() - 7)
    return { from: nowJst.toISOString().slice(0, 10), to }
  }
  if (period === '1m') {
    nowJst.setUTCMonth(nowJst.getUTCMonth() - 1)
    return { from: nowJst.toISOString().slice(0, 10), to }
  }
  return {}
}

function formatDateJst(iso: string): string {
  return new Date(iso).toLocaleString('ja-JP', {
    timeZone: 'Asia/Tokyo',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function formatNum(value: string | null): string {
  if (value == null) return '-'
  const n = Number(value)
  if (Number.isNaN(n)) return value
  return Math.round(n).toLocaleString()
}

function formatSignedInt(value: string | null): string {
  if (value == null) return '-'
  const n = Number(value)
  if (Number.isNaN(n)) return value
  const sign = n > 0 ? '+' : ''
  return `${sign}${Math.round(n).toLocaleString()}`
}

function kindLabel(kind: Notification['kind']): string {
  return kind === 'trade_opened' ? 'OPEN' : 'CLOSE'
}

const PER_PAGE = 50

export default function Notifications() {
  const [period, setPeriod] = useState<string>('')
  const [kind, setKind] = useState<'' | 'trade_opened' | 'trade_closed'>('')
  const [page, setPage] = useState(1)
  const range = useMemo(() => periodToRange(period), [period])

  const { data, isLoading } = useQuery({
    queryKey: ['notifications', { page, period, kind }],
    queryFn: () =>
      api.notifications.list({
        page: String(page),
        limit: String(PER_PAGE),
        from: range.from,
        to: range.to,
        kind: kind || undefined,
      }),
  })

  const total = data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PER_PAGE))
  const rangeStart = total === 0 ? 0 : (page - 1) * PER_PAGE + 1
  const rangeEnd = Math.min(page * PER_PAGE, total)

  const selectClass =
    'bg-gray-800 border border-gray-700 text-gray-100 text-sm rounded px-3 py-1.5 focus:outline-none focus:border-blue-500'
  const labelClass = 'text-xs text-gray-400 mr-1'

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-bold">通知履歴</h2>

      <div className="bg-gray-900 rounded p-3 flex flex-wrap items-center gap-3">
        <div className="flex items-center gap-2">
          <span className={labelClass}>期間</span>
          <select
            value={period}
            onChange={(e) => {
              setPeriod(e.target.value)
              setPage(1)
            }}
            className={selectClass}
          >
            <option value="">全期間</option>
            <option value="today">今日</option>
            <option value="1w">1週間</option>
            <option value="1m">1ヶ月</option>
          </select>
        </div>

        <div className="flex items-center gap-2">
          <span className={labelClass}>種別</span>
          <select
            value={kind}
            onChange={(e) => {
              setKind(e.target.value as '' | 'trade_opened' | 'trade_closed')
              setPage(1)
            }}
            className={selectClass}
          >
            <option value="">すべて</option>
            <option value="trade_opened">OPEN</option>
            <option value="trade_closed">CLOSE</option>
          </select>
        </div>
      </div>

      <div className="bg-gray-900 rounded-lg shadow overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-800">
                <th className="px-3 py-2 text-left text-gray-400 font-medium">日時</th>
                <th className="px-3 py-2 text-left text-gray-400 font-medium">種別</th>
                <th className="px-3 py-2 text-left text-gray-400 font-medium">戦略</th>
                <th className="px-3 py-2 text-left text-gray-400 font-medium">ペア</th>
                <th className="px-3 py-2 text-left text-gray-400 font-medium">方向</th>
                <th className="px-3 py-2 text-right text-gray-400 font-medium">価格</th>
                <th className="px-3 py-2 text-right text-gray-400 font-medium">PnL</th>
                <th className="px-3 py-2 text-left text-gray-400 font-medium">exit_reason</th>
              </tr>
            </thead>
            <tbody>
              {isLoading ? (
                <tr>
                  <td colSpan={8} className="px-3 py-8 text-center text-gray-500">
                    読み込み中...
                  </td>
                </tr>
              ) : !data || data.items.length === 0 ? (
                <tr>
                  <td colSpan={8} className="px-3 py-8 text-center text-gray-500">
                    通知はありません
                  </td>
                </tr>
              ) : (
                data.items.map((n) => (
                  <tr
                    key={n.id}
                    className={`border-b border-gray-800/50 ${
                      n.read_at == null ? 'bg-sky-950/30' : ''
                    }`}
                  >
                    <td className="px-3 py-2 text-gray-300 whitespace-nowrap">
                      {formatDateJst(n.created_at)}
                    </td>
                    <td className="px-3 py-2 font-mono">
                      <span
                        className={
                          n.kind === 'trade_opened' ? 'text-sky-400' : 'text-amber-400'
                        }
                      >
                        {kindLabel(n.kind)}
                      </span>
                    </td>
                    <td className="px-3 py-2 text-gray-300">{n.strategy_name}</td>
                    <td className="px-3 py-2 text-gray-300">{n.pair}</td>
                    <td className="px-3 py-2">
                      <span
                        className={
                          n.direction === 'long' ? 'text-emerald-400' : 'text-red-400'
                        }
                      >
                        {n.direction.toUpperCase()}
                      </span>
                    </td>
                    <td className="px-3 py-2 text-right font-mono text-gray-300">
                      {formatNum(n.price)}
                    </td>
                    <td className="px-3 py-2 text-right font-mono">
                      {n.pnl_amount == null ? (
                        <span className="text-gray-500">-</span>
                      ) : (
                        <span
                          className={
                            Number(n.pnl_amount) >= 0
                              ? 'text-emerald-400'
                              : 'text-red-400'
                          }
                        >
                          {formatSignedInt(n.pnl_amount)}
                        </span>
                      )}
                    </td>
                    <td className="px-3 py-2 text-gray-400 text-xs">
                      {n.exit_reason ?? '-'}
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>

        {totalPages > 1 && (
          <div className="flex items-center justify-between gap-2 px-4 py-2 border-t border-gray-800 text-xs text-gray-400">
            <span>
              {total} 件中 {rangeStart}-{rangeEnd} 件
            </span>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setPage((p) => Math.max(1, p - 1))}
                disabled={page <= 1}
                className="px-3 py-1 bg-gray-800 rounded hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                前へ
              </button>
              <button
                type="button"
                onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
                disabled={page >= totalPages}
                className="px-3 py-1 bg-gray-800 rounded hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                次へ
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
