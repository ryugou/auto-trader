import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api } from '../api/client'
import type { Notification } from '../api/types'

interface NotificationDropdownProps {
  open: boolean
}

function formatRelativeTime(iso: string): string {
  const now = Date.now()
  const then = new Date(iso).getTime()
  const diffSec = Math.max(0, Math.floor((now - then) / 1000))
  if (diffSec < 60) return `${diffSec}秒前`
  const diffMin = Math.floor(diffSec / 60)
  if (diffMin < 60) return `${diffMin}分前`
  const diffHour = Math.floor(diffMin / 60)
  if (diffHour < 24) return `${diffHour}時間前`
  const diffDay = Math.floor(diffHour / 24)
  return `${diffDay}日前`
}

function formatAbsoluteJst(iso: string): string {
  return new Date(iso).toLocaleString('ja-JP', {
    timeZone: 'Asia/Tokyo',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function formatSignedInt(value: string | null): string {
  if (value == null) return ''
  const n = Number(value)
  if (Number.isNaN(n)) return value
  const sign = n > 0 ? '+' : ''
  return `${sign}${Math.round(n).toLocaleString()}`
}

function renderBody(n: Notification, accountName: string): React.ReactNode {
  const dir = n.direction.toUpperCase()
  const price = Number(n.price).toLocaleString()
  if (n.kind === 'trade_opened') {
    return (
      <span>
        <span className="text-gray-300 font-medium">{accountName}</span>{' '}
        <span className="text-sky-400 font-mono">OPEN</span>{' '}
        {n.pair}{' '}
        <span className={n.direction === 'long' ? 'text-emerald-400' : 'text-red-400'}>
          {dir}
        </span>{' '}
        @ {price}
      </span>
    )
  }
  const pnlNum = Number(n.pnl_amount ?? '0')
  const pnlClass = pnlNum >= 0 ? 'text-emerald-400' : 'text-red-400'
  return (
    <span>
      <span className="text-gray-300 font-medium">{accountName}</span>{' '}
      <span className="text-amber-400 font-mono">CLOSE</span>{' '}
      {n.pair}{' '}
      <span className={n.direction === 'long' ? 'text-emerald-400' : 'text-red-400'}>
        {dir}
      </span>{' '}
      <span className={`font-mono ${pnlClass}`}>{formatSignedInt(n.pnl_amount)}</span>
    </span>
  )
}

export default function NotificationDropdown({ open }: NotificationDropdownProps) {
  const { data, isLoading } = useQuery({
    queryKey: ['notifications', { limit: 20 }],
    queryFn: () => api.notifications.list({ limit: '20', page: '1' }),
    enabled: open,
  })

  const { data: accounts } = useQuery({
    queryKey: ['accounts'],
    queryFn: () => api.accounts.list(),
  })

  const accountMap = useMemo(() => {
    const m = new Map<string, string>()
    accounts?.forEach((a) => m.set(a.id, a.name))
    return m
  }, [accounts])

  if (!open) return null

  return (
    <div className="absolute right-0 top-full mt-2 w-96 bg-gray-900 border border-gray-800 rounded-lg shadow-xl overflow-hidden z-50">
      <div className="px-4 py-2 border-b border-gray-800 text-sm font-semibold text-gray-100">
        通知
      </div>
      <div className="max-h-96 overflow-y-auto">
        {isLoading ? (
          <div className="px-4 py-6 text-center text-xs text-gray-500">読み込み中...</div>
        ) : !data || data.items.length === 0 ? (
          <div className="px-4 py-6 text-center text-xs text-gray-500">通知はありません</div>
        ) : (
          data.items.map((n) => {
            const accountName = accountMap.get(n.paper_account_id ?? '') ?? '-'
            return (
              <div
                key={n.id}
                className={`px-4 py-2 border-b border-gray-800/60 last:border-b-0 ${
                  n.read_at == null ? 'bg-sky-950/40' : ''
                }`}
              >
                <div className="text-xs text-gray-100">{renderBody(n, accountName)}</div>
                <div
                  className="text-[10px] text-gray-500 mt-0.5"
                  title={formatAbsoluteJst(n.created_at)}
                >
                  {formatRelativeTime(n.created_at)}
                </div>
              </div>
            )
          })
        )}
      </div>
      <div className="px-4 py-2 border-t border-gray-800 text-right">
        <Link to="/notifications" className="text-xs text-sky-400 hover:text-sky-300">
          すべて見る →
        </Link>
      </div>
    </div>
  )
}
