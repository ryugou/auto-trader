import { useQuery } from '@tanstack/react-query'
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
  Cell,
} from 'recharts'
import { api } from '../api/client'
import type { DashboardFilter } from '../api/types'

interface PairChartProps {
  filters?: DashboardFilter
}

export default function PairChart({ filters = {} }: PairChartProps) {
  const { data, isLoading } = useQuery({
    queryKey: ['pairs', filters],
    queryFn: () => api.dashboard.pairs(filters),
  })

  if (isLoading) {
    return <div className="bg-gray-900 rounded-lg p-4 shadow h-80 animate-pulse" />
  }

  const chartData = (data ?? []).map((p) => ({
    name: p.pair,
    pnl: Number(p.total_pnl),
    trades: p.trade_count,
  }))

  return (
    <div className="bg-gray-900 rounded-lg p-4 shadow">
      <h3 className="text-sm text-gray-400 mb-3">ペア別損益</h3>
      {chartData.length === 0 ? (
        <p className="text-gray-500 text-center py-16">データがありません</p>
      ) : (
        <ResponsiveContainer width="100%" height={300}>
          <BarChart data={chartData}>
            <CartesianGrid strokeDasharray="3 3" stroke="#374151" />
            <XAxis
              dataKey="name"
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
                'PnL',
              ]}
            />
            <Bar dataKey="pnl" radius={[4, 4, 0, 0]}>
              {chartData.map((entry, idx) => (
                <Cell
                  key={idx}
                  fill={entry.pnl >= 0 ? '#10b981' : '#ef4444'}
                />
              ))}
            </Bar>
          </BarChart>
        </ResponsiveContainer>
      )}
    </div>
  )
}
