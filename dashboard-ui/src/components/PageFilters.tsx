import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'

export interface PageFilterValue {
  exchange?: string
  paper_account_id?: string
  from?: string
  to?: string
}

interface PageFiltersProps {
  value: PageFilterValue
  onChange: (next: PageFilterValue) => void
}

function periodToRange(period: string): { from?: string; to?: string } {
  if (!period) return {}
  const now = new Date()
  const to = now.toISOString().slice(0, 10)
  if (period === 'today') return { from: to, to }
  if (period === '1w') {
    const d = new Date(now)
    d.setDate(d.getDate() - 7)
    return { from: d.toISOString().slice(0, 10), to }
  }
  if (period === '1m') {
    const d = new Date(now)
    d.setMonth(d.getMonth() - 1)
    return { from: d.toISOString().slice(0, 10), to }
  }
  return {}
}

export default function PageFilters({ value, onChange }: PageFiltersProps) {
  const [period, setPeriod] = useState<string>('')

  const { data: accounts } = useQuery({
    queryKey: ['accounts'],
    queryFn: () => api.accounts.list(),
  })

  const handleExchange = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const v = e.target.value || undefined
    onChange({ ...value, exchange: v })
  }

  const handleAccount = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const v = e.target.value || undefined
    onChange({ ...value, paper_account_id: v })
  }

  const handlePeriod = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const next = e.target.value
    setPeriod(next)
    const range = periodToRange(next)
    onChange({ ...value, from: range.from, to: range.to })
  }

  const selectClass =
    'bg-gray-800 border border-gray-700 text-gray-100 text-sm rounded px-3 py-1.5 focus:outline-none focus:border-blue-500'
  const labelClass = 'text-xs text-gray-400 mr-1'

  return (
    <div className="bg-gray-900 rounded p-3 mb-4 flex flex-wrap items-center gap-3">
      <div className="flex items-center gap-2">
        <span className={labelClass}>投資種別</span>
        <select
          value={value.exchange ?? ''}
          onChange={handleExchange}
          className={selectClass}
        >
          <option value="">全体</option>
          <option value="oanda">FX</option>
          <option value="bitflyer_cfd">暗号資産</option>
        </select>
      </div>

      <div className="flex items-center gap-2">
        <span className={labelClass}>口座</span>
        <select
          value={value.paper_account_id ?? ''}
          onChange={handleAccount}
          className={selectClass}
        >
          <option value="">全口座</option>
          {accounts?.map((a) => (
            <option key={a.id} value={a.id}>
              {a.name}
            </option>
          ))}
        </select>
      </div>

      <div className="flex items-center gap-2">
        <span className={labelClass}>期間</span>
        <select
          value={period}
          onChange={handlePeriod}
          className={selectClass}
        >
          <option value="">全期間</option>
          <option value="today">今日</option>
          <option value="1w">1週間</option>
          <option value="1m">1ヶ月</option>
        </select>
      </div>
    </div>
  )
}
