import type {
  SummaryResponse,
  PnlHistoryRow,
  StrategyStats,
  PairStats,
  HourlyWinrate,
  TradesResponse,
  TradeEventsResponse,
  PositionResponse,
  TradingAccount,
  CreateTradingAccount,
  UpdateTradingAccount,
  DashboardFilter,
  BalanceHistoryResponse,
  Strategy,
  NotificationsResponse,
  NotificationUnreadCountResponse,
  MarketPricesResponse,
  MarketFeedHealthResponse,
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
    balanceHistory: (f: DashboardFilter = {}) =>
      get<BalanceHistoryResponse>(`/api/dashboard/balance-history${qs(f)}`),
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
    events: (id: string) =>
      get<TradeEventsResponse>(`/api/trades/${id}/events`),
  },
  positions: {
    list: () => get<PositionResponse[]>('/api/positions'),
  },
  accounts: {
    list: () => get<TradingAccount[]>('/api/trading-accounts'),
    get: (id: string) => get<TradingAccount>(`/api/trading-accounts/${id}`),
    create: (data: CreateTradingAccount) =>
      post<TradingAccount>('/api/trading-accounts', data),
    update: (id: string, data: UpdateTradingAccount) =>
      put<TradingAccount>(`/api/trading-accounts/${id}`, data),
    delete: (id: string) => del(`/api/trading-accounts/${id}`),
  },
  strategies: {
    list: (category?: 'fx' | 'crypto') =>
      get<Strategy[]>(`/api/strategies${category ? `?category=${category}` : ''}`),
    // Strategy names can contain free-text characters auto-imported from
    // historical paper_accounts rows (see migrations/20260407000003), so
    // URL-encode the path segment to be safe.
    get: (name: string) =>
      get<Strategy>(`/api/strategies/${encodeURIComponent(name)}`),
  },
  notifications: {
    list: (params: Record<string, string | undefined> = {}) =>
      get<NotificationsResponse>(`/api/notifications${qs(params)}`),
    unreadCount: () =>
      get<NotificationUnreadCountResponse>(`/api/notifications/unread-count`),
    markAllRead: () =>
      post<{ marked: number }>(`/api/notifications/mark-all-read`, {}),
  },
  market: {
    prices: () => get<MarketPricesResponse>(`/api/market/prices`),
  },
  health: {
    marketFeed: () =>
      get<MarketFeedHealthResponse>(`/api/health/market-feed`),
  },
}
