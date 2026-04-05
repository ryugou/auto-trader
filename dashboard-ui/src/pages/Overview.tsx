import KpiCards from '../components/KpiCards'
import PnlChart from '../components/PnlChart'

export default function Overview() {
  return (
    <div className="space-y-6">
      <h2 className="text-xl font-bold">概要</h2>
      <KpiCards />
      <PnlChart />
    </div>
  )
}
