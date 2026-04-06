import KpiCards from '../components/KpiCards'
import PnlChart from '../components/PnlChart'
import BalanceChart from '../components/BalanceChart'

export default function Overview() {
  return (
    <div className="space-y-10">
      <h2 className="text-xl font-bold">概要</h2>

      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-gray-200">通常口座</h3>
        <KpiCards filters={{ account_type: 'live' }} />
        <PnlChart filters={{ account_type: 'live' }} />
        <BalanceChart accountType="live" />
      </section>

      <section className="space-y-4">
        <h3 className="text-lg font-semibold text-gray-200">ペーパートレード</h3>
        <KpiCards filters={{ account_type: 'paper' }} />
        <PnlChart filters={{ account_type: 'paper' }} />
        <BalanceChart accountType="paper" />
      </section>
    </div>
  )
}
