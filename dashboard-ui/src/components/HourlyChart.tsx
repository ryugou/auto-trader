import { useQuery } from '@tanstack/react-query'
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from 'recharts'
import { api } from '../api/client'
import { useFilters } from '../contexts/FilterContext'

export default function HourlyChart() {
  const { dashboardFilter } = useFilters()
  const { data, isLoading } = useQuery({
    queryKey: ['hourly-winrate', dashboardFilter],
    queryFn: () => api.dashboard.hourlyWinrate(dashboardFilter),
  })

  if (isLoading) {
    return <div className="bg-gray-900 rounded-lg p-4 shadow h-80 animate-pulse" />
  }

  // Ensure all 24 hours are represented
  const hourMap = new Map((data ?? []).map((h) => [h.hour, h]))
  const chartData = Array.from({ length: 24 }, (_, i) => {
    const h = hourMap.get(i)
    const winRate =
      h && h.trade_count > 0
        ? (h.win_count / h.trade_count) * 100
        : 0
    return {
      hour: `${String(i).padStart(2, '0')}時`,
      winRate: Number(winRate.toFixed(1)),
      trades: h?.trade_count ?? 0,
    }
  })

  return (
    <div className="bg-gray-900 rounded-lg p-4 shadow">
      <h3 className="text-sm text-gray-400 mb-3">時間帯別勝率</h3>
      <ResponsiveContainer width="100%" height={300}>
        <BarChart data={chartData}>
          <CartesianGrid strokeDasharray="3 3" stroke="#374151" />
          <XAxis
            dataKey="hour"
            tick={{ fill: '#9ca3af', fontSize: 11 }}
            tickLine={false}
            interval={1}
          />
          <YAxis
            tick={{ fill: '#9ca3af', fontSize: 12 }}
            tickLine={false}
            domain={[0, 100]}
            tickFormatter={(v: number) => `${v}%`}
          />
          <Tooltip
            contentStyle={{
              backgroundColor: '#1f2937',
              border: '1px solid #374151',
              borderRadius: '0.5rem',
              color: '#f3f4f6',
            }}
            formatter={(value, _name, props) => {
              const trades = (props.payload as { trades: number }).trades
              return [`${value}% (${trades}件)`, '勝率']
            }}
          />
          <Bar dataKey="winRate" fill="#3b82f6" radius={[2, 2, 0, 0]} />
        </BarChart>
      </ResponsiveContainer>
    </div>
  )
}
