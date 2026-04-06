import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'
import { useFilters } from '../contexts/FilterContext'

function formatJpy(value: number): string {
  const sign = value >= 0 ? '+' : ''
  return `${sign}${Math.round(value).toLocaleString()} 円`
}

function formatPercent(value: number): string {
  return `${value.toFixed(1)}%`
}

interface CardProps {
  label: string
  value: string
  color: string
}

function Card({ label, value, color }: CardProps) {
  return (
    <div className="bg-gray-900 rounded-lg p-4 shadow">
      <p className="text-sm text-gray-400 mb-1">{label}</p>
      <p className={`text-2xl font-bold ${color}`}>{value}</p>
    </div>
  )
}

export default function KpiCards() {
  const { dashboardFilter } = useFilters()
  const { data, isLoading } = useQuery({
    queryKey: ['dashboard-summary', dashboardFilter],
    queryFn: () => api.dashboard.summary(dashboardFilter),
  })

  if (isLoading || !data) {
    return (
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        {[...Array(4)].map((_, i) => (
          <div key={i} className="bg-gray-900 rounded-lg p-4 shadow animate-pulse h-20" />
        ))}
      </div>
    )
  }

  const pnlColor = data.net_pnl >= 0 ? 'text-emerald-400' : 'text-red-400'
  const ddColor = 'text-red-400'

  return (
    <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
      <Card label="総損益" value={formatJpy(data.net_pnl)} color={pnlColor} />
      <Card label="勝率" value={formatPercent(data.win_rate * 100)} color="text-gray-100" />
      <Card
        label="期待値"
        value={formatJpy(data.expected_value)}
        color={data.expected_value >= 0 ? 'text-emerald-400' : 'text-red-400'}
      />
      <Card label="最大DD" value={formatJpy(data.max_drawdown)} color={ddColor} />
    </div>
  )
}
