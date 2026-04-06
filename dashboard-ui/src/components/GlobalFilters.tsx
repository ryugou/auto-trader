import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'
import { useFilters } from '../contexts/FilterContext'

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

export default function GlobalFilters() {
  const { filters, setFilters } = useFilters()

  const { data: accounts } = useQuery({
    queryKey: ['accounts'],
    queryFn: () => api.accounts.list(),
  })

  const handleExchange = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const val = e.target.value || undefined
    setFilters((prev) => ({ ...prev, exchange: val }))
  }

  const handleAccount = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const val = e.target.value || undefined
    setFilters((prev) => ({ ...prev, paper_account_id: val }))
  }

  const handlePeriod = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const range = periodToRange(e.target.value)
    setFilters((prev) => ({ ...prev, from: range.from, to: range.to }))
  }

  const selectClass =
    'bg-gray-800 border border-gray-700 text-gray-100 text-sm rounded px-3 py-1.5 focus:outline-none focus:border-blue-500'

  return (
    <div className="flex flex-wrap items-center gap-3">
      <select
        value={filters.exchange ?? ''}
        onChange={handleExchange}
        className={selectClass}
      >
        <option value="">全体</option>
        <option value="oanda">FX (OANDA)</option>
        <option value="bitflyer_cfd">Crypto (bitFlyer)</option>
      </select>

      <select
        value={filters.paper_account_id ?? ''}
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

      <select
        defaultValue=""
        onChange={handlePeriod}
        className={selectClass}
      >
        <option value="">全期間</option>
        <option value="today">今日</option>
        <option value="1w">1週間</option>
        <option value="1m">1ヶ月</option>
      </select>
    </div>
  )
}
