import {
  createContext,
  useContext,
  useState,
  useMemo,
  type ReactNode,
} from 'react'
import type { DashboardFilter } from '../api/types'

interface FilterState {
  exchange?: string
  paper_account_id?: string
  from?: string
  to?: string
}

interface FilterContextValue {
  filters: FilterState
  setFilters: React.Dispatch<React.SetStateAction<FilterState>>
  dashboardFilter: DashboardFilter
}

const FilterContext = createContext<FilterContextValue | null>(null)

export function FilterProvider({ children }: { children: ReactNode }) {
  const [filters, setFilters] = useState<FilterState>({})

  const dashboardFilter = useMemo<DashboardFilter>(() => {
    const f: DashboardFilter = {}
    if (filters.exchange) f.exchange = filters.exchange
    if (filters.paper_account_id) f.paper_account_id = filters.paper_account_id
    if (filters.from) f.from = filters.from
    if (filters.to) f.to = filters.to
    return f
  }, [filters])

  return (
    <FilterContext.Provider value={{ filters, setFilters, dashboardFilter }}>
      {children}
    </FilterContext.Provider>
  )
}

export function useFilters(): FilterContextValue {
  const ctx = useContext(FilterContext)
  if (!ctx) throw new Error('useFilters must be used within FilterProvider')
  return ctx
}
