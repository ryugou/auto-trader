import type {
  SummaryResponse,
  PnlHistoryRow,
  StrategyStats,
  PairStats,
  HourlyWinrate,
  TradesResponse,
  PositionResponse,
  PaperAccount,
  CreatePaperAccount,
  UpdatePaperAccount,
  DashboardFilter,
} from './types'

const BASE = ''

function authHeaders(): Record<string, string> {
  const token = localStorage.getItem('api_token')
  return token ? { Authorization: `Bearer ${token}` } : {}
}

function qs(params: Record<string, string | undefined> | DashboardFilter): string {
  const entries = Object.entries(params).filter(([, v]) => v !== undefined)
  if (entries.length === 0) return ''
  return '?' + new URLSearchParams(entries as [string, string][]).toString()
}

async function get<T>(path: string): Promise<T> {
  const res = await fetch(BASE + path, { headers: { ...authHeaders() } })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return res.json()
}

async function post<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(BASE + path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify(body),
  })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return res.json()
}

async function put<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(BASE + path, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify(body),
  })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return res.json()
}

async function del(path: string): Promise<void> {
  const res = await fetch(BASE + path, { method: 'DELETE', headers: { ...authHeaders() } })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
}

export const api = {
  dashboard: {
    summary: (f: DashboardFilter = {}) =>
      get<SummaryResponse>(`/api/dashboard/summary${qs(f)}`),
    pnlHistory: (f: DashboardFilter = {}) =>
      get<PnlHistoryRow[]>(`/api/dashboard/pnl-history${qs(f)}`),
    strategies: (f: DashboardFilter = {}) =>
      get<StrategyStats[]>(`/api/dashboard/strategies${qs(f)}`),
    pairs: (f: DashboardFilter = {}) =>
      get<PairStats[]>(`/api/dashboard/pairs${qs(f)}`),
    hourlyWinrate: (f: DashboardFilter = {}) =>
      get<HourlyWinrate[]>(`/api/dashboard/hourly-winrate${qs(f)}`),
  },
  trades: {
    list: (params: Record<string, string | undefined> = {}) =>
      get<TradesResponse>(`/api/trades${qs(params)}`),
  },
  positions: {
    list: () => get<PositionResponse[]>('/api/positions'),
  },
  accounts: {
    list: () => get<PaperAccount[]>('/api/paper-accounts'),
    get: (id: string) => get<PaperAccount>(`/api/paper-accounts/${id}`),
    create: (data: CreatePaperAccount) =>
      post<PaperAccount>('/api/paper-accounts', data),
    update: (id: string, data: UpdatePaperAccount) =>
      put<PaperAccount>(`/api/paper-accounts/${id}`, data),
    delete: (id: string) => del(`/api/paper-accounts/${id}`),
  },
}
