import StrategyChart from '../components/StrategyChart'
import PairChart from '../components/PairChart'
import HourlyChart from '../components/HourlyChart'

export default function Analysis() {
  return (
    <div className="space-y-6">
      <h2 className="text-xl font-bold">分析</h2>
      <StrategyChart />
      <PairChart />
      <HourlyChart />
    </div>
  )
}
