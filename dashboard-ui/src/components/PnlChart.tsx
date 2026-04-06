import { useQuery } from '@tanstack/react-query'
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from 'recharts'
import { api } from '../api/client'
import { useFilters } from '../contexts/FilterContext'

export default function PnlChart() {
  const { dashboardFilter } = useFilters()
  const { data, isLoading } = useQuery({
    queryKey: ['pnl-history', dashboardFilter],
    queryFn: () => api.dashboard.pnlHistory(dashboardFilter),
  })

  if (isLoading) {
    return (
      <div className="bg-gray-900 rounded-lg p-4 shadow h-80 animate-pulse" />
    )
  }

  const chartData = (data ?? []).map((row) => ({
    date: row.date,
    pnl: Number(row.cumulative_pnl),
  }))

  const hasNegative = chartData.some((d) => d.pnl < 0)
  const hasPositive = chartData.some((d) => d.pnl > 0)

  return (
    <div className="bg-gray-900 rounded-lg p-4 shadow">
      <h3 className="text-sm text-gray-400 mb-3">累積損益推移</h3>
      {chartData.length === 0 ? (
        <p className="text-gray-500 text-center py-16">データがありません</p>
      ) : (
        <ResponsiveContainer width="100%" height={300}>
          <AreaChart data={chartData}>
            <defs>
              <linearGradient id="pnlGrad" x1="0" y1="0" x2="0" y2="1">
                {hasPositive && (
                  <stop offset="0%" stopColor="#10b981" stopOpacity={0.3} />
                )}
                {hasNegative && (
                  <stop offset="100%" stopColor="#ef4444" stopOpacity={0.3} />
                )}
                {!hasNegative && (
                  <stop offset="100%" stopColor="#10b981" stopOpacity={0.05} />
                )}
                {!hasPositive && (
                  <stop offset="0%" stopColor="#ef4444" stopOpacity={0.05} />
                )}
              </linearGradient>
            </defs>
            <CartesianGrid strokeDasharray="3 3" stroke="#374151" />
            <XAxis
              dataKey="date"
              tick={{ fill: '#9ca3af', fontSize: 12 }}
              tickLine={false}
            />
            <YAxis
              tick={{ fill: '#9ca3af', fontSize: 12 }}
              tickLine={false}
              tickFormatter={(v: number) => `${Math.round(v).toLocaleString()}`}
            />
            <Tooltip
              contentStyle={{
                backgroundColor: '#1f2937',
                border: '1px solid #374151',
                borderRadius: '0.5rem',
                color: '#f3f4f6',
              }}
              formatter={(value) => [
                `${Math.round(Number(value)).toLocaleString()} 円`,
                '累積PnL',
              ]}
            />
            <Area
              type="monotone"
              dataKey="pnl"
              stroke="#3b82f6"
              fill="url(#pnlGrad)"
              strokeWidth={2}
            />
          </AreaChart>
        </ResponsiveContainer>
      )}
    </div>
  )
}
