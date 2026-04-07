import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'
import type { Strategy } from '../api/types'

const RISK_LABEL: Record<Strategy['risk_level'], string> = {
  low: '慎重',
  medium: '標準',
  high: '攻め',
}

const RISK_CLASS: Record<Strategy['risk_level'], string> = {
  low: 'bg-green-900/40 text-green-300 border border-green-800',
  medium: 'bg-blue-900/40 text-blue-300 border border-blue-800',
  high: 'bg-red-900/40 text-red-300 border border-red-800',
}

/**
 * Shared chip rendering the risk-level for a strategy. Renders nothing
 * when the strategy doesn't yet have a `risk_level` (e.g. because the
 * row is still loading or has been deleted from the catalog).
 */
export function RiskBadge({ riskLevel }: { riskLevel?: Strategy['risk_level'] }) {
  if (!riskLevel) return null
  return (
    <span
      className={`text-xs px-1.5 py-0.5 rounded ${RISK_CLASS[riskLevel]}`}
    >
      {RISK_LABEL[riskLevel]}
    </span>
  )
}

/**
 * Hook returning a `(strategyName) => risk_level | undefined` lookup
 * function backed by the catalog. Components that need risk badges next
 * to many strategy names (positions table, accounts table) call this
 * once and pass the resolver into row renderers — single fetch per page,
 * O(1) lookups per row.
 */
export function useStrategyRiskLookup(): (
  name: string,
) => Strategy['risk_level'] | undefined {
  const { data } = useQuery({
    queryKey: ['strategies', 'all'],
    queryFn: () => api.strategies.list(),
    staleTime: 5 * 60 * 1000,
  })
  const map = new Map<string, Strategy['risk_level']>()
  for (const s of data ?? []) {
    map.set(s.name, s.risk_level)
  }
  return (name: string) => map.get(name)
}
