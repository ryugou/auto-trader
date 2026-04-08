import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'
import TradeTable from '../components/TradeTable'
import type { PaperAccount } from '../api/types'

const JST_OFFSET_MS = 9 * 60 * 60 * 1000

// JST (UTC+9) 基準の YYYY-MM-DD を返す
function jstDateString(date: Date): string {
  return new Date(date.getTime() + JST_OFFSET_MS).toISOString().slice(0, 10)
}

function periodToRange(period: string): { from?: string; to?: string } {
  if (!period) return {}
  const now = new Date()
  const to = jstDateString(now)
  if (period === 'today') return { from: to, to }
  if (period === '1w') {
    const d = new Date(now)
    d.setUTCDate(d.getUTCDate() - 7)
    return { from: jstDateString(d), to }
  }
  if (period === '1m') {
    const d = new Date(now)
    d.setUTCMonth(d.getUTCMonth() - 1)
    return { from: jstDateString(d), to }
  }
  return {}
}

type ExchangeGroupKey = 'crypto' | 'fx' | 'other'

// Classify an exchange string into our two visual groups. Unknown
// exchanges fall into `other` so that a typo or a newly added venue
// still renders somewhere instead of being silently dropped.
function exchangeGroup(exchange: string): ExchangeGroupKey {
  if (exchange.startsWith('bitflyer')) return 'crypto'
  if (exchange === 'oanda') return 'fx'
  return 'other'
}

interface AccountGroup {
  key: ExchangeGroupKey
  label: string
  accounts: PaperAccount[]
}

// Build the ordered group list. Crypto first, then FX, then anything
// unclassified (so we notice during manual QA).
function buildGroups(accounts: PaperAccount[]): AccountGroup[] {
  const crypto: PaperAccount[] = []
  const fx: PaperAccount[] = []
  const other: PaperAccount[] = []
  for (const a of accounts) {
    const g = exchangeGroup(a.exchange)
    if (g === 'crypto') crypto.push(a)
    else if (g === 'fx') fx.push(a)
    else other.push(a)
  }
  const byName = (a: PaperAccount, b: PaperAccount) =>
    a.name.localeCompare(b.name, 'ja')
  const groups: AccountGroup[] = []
  if (crypto.length) {
    groups.push({ key: 'crypto', label: '暗号資産', accounts: crypto.sort(byName) })
  }
  if (fx.length) {
    groups.push({ key: 'fx', label: 'FX', accounts: fx.sort(byName) })
  }
  if (other.length) {
    groups.push({ key: 'other', label: 'その他', accounts: other.sort(byName) })
  }
  return groups
}

export default function Trades() {
  const [period, setPeriod] = useState<string>('')
  const range = useMemo(() => periodToRange(period), [period])

  const { data: accounts, isLoading } = useQuery({
    queryKey: ['accounts'],
    queryFn: () => api.accounts.list(),
  })

  const groups = useMemo(
    () => buildGroups(accounts ?? []),
    [accounts],
  )

  const selectClass =
    'bg-gray-800 border border-gray-700 text-gray-100 text-sm rounded px-3 py-1.5 focus:outline-none focus:border-blue-500'
  const labelClass = 'text-xs text-gray-400 mr-1'

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-bold">トレード履歴</h2>

      <div className="bg-gray-900 rounded p-3 flex flex-wrap items-center gap-3">
        <div className="flex items-center gap-2">
          <span className={labelClass}>期間</span>
          <select
            value={period}
            onChange={(e) => setPeriod(e.target.value)}
            className={selectClass}
          >
            <option value="">全期間</option>
            <option value="today">今日</option>
            <option value="1w">1週間</option>
            <option value="1m">1ヶ月</option>
          </select>
        </div>
      </div>

      {isLoading ? (
        <div className="bg-gray-900 rounded p-8 text-center text-gray-500">
          読み込み中...
        </div>
      ) : groups.length === 0 ? (
        <div className="bg-gray-900 rounded p-8 text-center text-gray-500">
          口座が登録されていません
        </div>
      ) : (
        groups.map((group) => (
          <section key={group.key} className="space-y-3">
            <h3 className="text-sm font-semibold text-gray-400 uppercase tracking-wider">
              {group.label}
            </h3>
            <div className="space-y-4">
              {group.accounts.map((account) => (
                <TradeTable
                  key={account.id}
                  account={account}
                  from={range.from}
                  to={range.to}
                />
              ))}
            </div>
          </section>
        ))
      )}
    </div>
  )
}
