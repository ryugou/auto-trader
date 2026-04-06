import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
  ResponsiveContainer,
} from 'recharts'
import { api } from '../api/client'

interface Props {
  accountType: 'paper' | 'live'
}

// Fixed color palette, cycled across accounts
const COLORS = [
  '#3b82f6', // blue
  '#10b981', // emerald
  '#f59e0b', // amber
  '#ef4444', // red
  '#8b5cf6', // violet
  '#06b6d4', // cyan
  '#ec4899', // pink
  '#84cc16', // lime
]

export default function BalanceChart({ accountType }: Props) {
  const { data, isLoading } = useQuery({
    queryKey: ['balance-history', accountType],
    queryFn: () =>
      api.dashboard.balanceHistory({ account_type: accountType }),
  })

  // Merge all accounts into a single date-keyed dataset: { date, [accName]: balance }
  const chartData = useMemo(() => {
    if (!data?.accounts?.length) return []
    const byDate = new Map<string, Record<string, number | string>>()
    for (const acc of data.accounts) {
      for (const p of acc.data) {
        const existing = byDate.get(p.date) ?? { date: p.date }
        existing[acc.account_name] = Number(p.balance)
        byDate.set(p.date, existing)
      }
    }
    return Array.from(byDate.values()).sort((a, b) =>
      String(a.date).localeCompare(String(b.date)),
    )
  }, [data])

  const accountNames = useMemo(
    () => data?.accounts.map((a) => a.account_name) ?? [],
    [data],
  )

  if (isLoading) {
    return (
      <div className="bg-gray-900 rounded-lg p-4 shadow h-80 animate-pulse" />
    )
  }

  return (
    <div className="bg-gray-900 rounded-lg p-4 shadow">
      <h3 className="text-sm text-gray-400 mb-3">口座残高推移</h3>
      {chartData.length === 0 ? (
        <p className="text-gray-500 text-center py-16">データがありません</p>
      ) : (
        <ResponsiveContainer width="100%" height={300}>
          <LineChart data={chartData}>
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
              formatter={(value, name) => [
                `${Math.round(Number(value ?? 0)).toLocaleString()} 円`,
                String(name),
              ]}
            />
            <Legend wrapperStyle={{ color: '#9ca3af', fontSize: 12 }} />
            {accountNames.map((name, i) => (
              <Line
                key={name}
                type="monotone"
                dataKey={name}
                stroke={COLORS[i % COLORS.length]}
                strokeWidth={2}
                dot={false}
              />
            ))}
          </LineChart>
        </ResponsiveContainer>
      )}
    </div>
  )
}
