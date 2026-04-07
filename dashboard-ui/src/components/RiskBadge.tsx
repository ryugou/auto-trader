import { useCallback, useMemo } from 'react'
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
 * when `riskLevel` is unavailable (e.g. because the catalog lookup is
 * still loading or the strategy is not found in the lookup map).
 */
export function RiskBadge({ riskLevel }: { riskLevel?: Strategy['risk_level'] }) {
  if (!riskLevel) return null
  return (
    <span className={`text-xs px-1.5 py-0.5 rounded ${RISK_CLASS[riskLevel]}`}>
      {RISK_LABEL[riskLevel]}
    </span>
  )
}

/**
 * Shared catalog query for the strategies table. Centralized so every
 * consumer (Strategies catalog page, AccountForm dropdown via category
 * filter, RiskBadge lookup, …) shares the same staleTime and queryKey
 * shape — no surprise refetches due to inconsistent options on the same
 * cache entry.
 */
export function useStrategyCatalogQuery() {
  return useQuery({
    queryKey: ['strategies', 'all'],
    queryFn: () => api.strategies.list(),
    staleTime: 5 * 60 * 1000,
    // Strategy catalog is quasi-static (only changes when a developer
    // adds a new strategy + migration). Opt out of the global 15-second
    // polling — fresh data on mount + on-window-focus is enough.
    refetchInterval: false,
  })
}

/**
 * Hook returning a `(strategyName) => risk_level | undefined` lookup
 * function backed by the catalog. The lookup map and the resolver
 * function are both memoized so they keep stable identities across
 * re-renders — important when callers pass the resolver into row
 * renderers that would otherwise re-render unnecessarily.
 */
export function useStrategyRiskLookup(): (
  name: string,
) => Strategy['risk_level'] | undefined {
  const { data } = useStrategyCatalogQuery()
  const map = useMemo(() => {
    const m = new Map<string, Strategy['risk_level']>()
    for (const s of data ?? []) {
      m.set(s.name, s.risk_level)
    }
    return m
  }, [data])
  return useCallback((name: string) => map.get(name), [map])
}
