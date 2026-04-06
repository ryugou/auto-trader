import TradeTable from '../components/TradeTable'

export default function Trades() {
  return (
    <div className="space-y-6">
      <h2 className="text-xl font-bold">トレード履歴</h2>
      <TradeTable />
    </div>
  )
}
