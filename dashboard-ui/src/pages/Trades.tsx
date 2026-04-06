import { useState } from 'react'
import TradeTable from '../components/TradeTable'
import PageFilters, { type PageFilterValue } from '../components/PageFilters'

export default function Trades() {
  const [filters, setFilters] = useState<PageFilterValue>({})

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-bold">トレード履歴</h2>
      <PageFilters value={filters} onChange={setFilters} />
      <TradeTable filters={filters} />
    </div>
  )
}
