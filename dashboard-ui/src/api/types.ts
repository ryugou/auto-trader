export interface TradingAccount {
  id: string
  name: string
  exchange: string
  initial_balance: string
  current_balance: string
  currency: string
  leverage: string
  strategy: string
  account_type: 'paper' | 'live'
  created_at: string
  // Enriched fields from GET /api/trading-accounts
  unrealized_pnl?: string
  evaluated_balance?: string
}

export interface CreateTradingAccount {
  name: string
  exchange: string
  initial_balance: string
  leverage: string
  strategy: string
  account_type: 'paper' | 'live'
  currency?: string
}

export interface UpdateTradingAccount {
  name?: string
  leverage?: string
  strategy?: string
}

export interface Strategy {
  name: string
  display_name: string
  category: 'fx' | 'crypto'
  risk_level: 'low' | 'medium' | 'high'
  description: string | null
  algorithm: string | null
  default_params: Record<string, unknown> | null
  created_at: string
}

export interface SummaryResponse {
  total_pnl: string
  net_pnl: string
  total_fees: string
  trade_count: number
  win_count: number
  loss_count: number
  win_rate: number
  expected_value: number
  max_drawdown: string
}

export interface PnlHistoryRow {
  date: string
  daily_pnl: string
  cumulative_pnl: string
}

export interface StrategyStats {
  strategy_name: string
  trade_count: number
  win_count: number
  total_pnl: string
}

export interface PairStats {
  pair: string
  trade_count: number
  win_count: number
  total_pnl: string
}

export interface HourlyWinrate {
  hour: number
  trade_count: number
  win_count: number
}

export interface TradeRow {
  id: string
  strategy_name: string
  pair: string
  exchange: string
  direction: string
  entry_price: string
  exit_price: string | null
  stop_loss: string
  // null for dynamic-exit strategies (mean-revert / trailing channel etc.)
  take_profit: string | null
  // Required after the unified rewrite — every trade has an actual fill quantity.
  quantity: string
  leverage: string
  fees: string
  pnl_amount: string | null
  entry_at: string
  exit_at: string | null
  exit_reason: string | null
  account_id: string
  account_type: string | null
  status: string
}

export interface TradesResponse {
  trades: TradeRow[]
  total: number
  page: number
  per_page: number
}

export interface TradeEvent {
  kind: 'open' | 'overnight_fee' | 'close'
  occurred_at: string
  price: string | null
  quantity: string | null
  direction: string | null
  cash_delta: string | null
  pnl_amount: string | null
}

export interface TradeEventsResponse {
  events: TradeEvent[]
}

export interface PositionResponse {
  trade_id: string
  strategy_name: string
  pair: string
  exchange: string
  direction: string
  entry_price: string
  // Required after the unified rewrite — open trades always have a quantity.
  quantity: string
  stop_loss: string
  // null for dynamic-exit strategies.
  take_profit: string | null
  fees: string
  entry_at: string
  account_id: string
  account_name: string
}

export interface DashboardFilter {
  exchange?: string
  account_id?: string
  account_type?: string
  strategy?: string
  pair?: string
  from?: string
  to?: string
}

export interface BalanceHistoryPoint {
  date: string
  balance: string
}

export interface BalanceHistoryAccount {
  account_id: string
  account_name: string
  data: BalanceHistoryPoint[]
}

export interface BalanceHistoryResponse {
  accounts: BalanceHistoryAccount[]
}

export interface Notification {
  id: string
  kind: 'trade_opened' | 'trade_closed'
  trade_id: string
  account_id: string
  strategy_name: string
  pair: string
  direction: 'long' | 'short'
  price: string
  pnl_amount: string | null
  exit_reason: string | null
  created_at: string
  read_at: string | null
}

export interface NotificationsResponse {
  items: Notification[]
  total: number
  unread_count: number
  page: number
  limit: number
}

export interface NotificationUnreadCountResponse {
  count: number
}

export interface MarketPrice {
  exchange: string
  pair: string
  price: string
  ts: string
}

export interface MarketPricesResponse {
  prices: MarketPrice[]
}

export type MarketFeedStatus = 'healthy' | 'stale' | 'missing' | 'market_closed'

export interface MarketFeedHealth {
  exchange: string
  pair: string
  status: MarketFeedStatus
  last_tick_age_secs: number | null
}

export interface MarketFeedHealthResponse {
  feeds: MarketFeedHealth[]
}
