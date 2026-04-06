import { useState } from 'react'
import StrategyChart from '../components/StrategyChart'
import PairChart from '../components/PairChart'
import HourlyChart from '../components/HourlyChart'
import PageFilters, { type PageFilterValue } from '../components/PageFilters'

export default function Analysis() {
  const [filters, setFilters] = useState<PageFilterValue>({})

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-bold">分析</h2>
      <PageFilters value={filters} onChange={setFilters} />
      <StrategyChart filters={filters} />
      <PairChart filters={filters} />
      <HourlyChart filters={filters} />
    </div>
  )
}
