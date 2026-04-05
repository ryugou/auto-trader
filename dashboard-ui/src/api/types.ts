export interface PaperAccount {
  id: string
  name: string
  exchange: string
  initial_balance: string
  current_balance: string
  currency: string
  leverage: string
  strategy: string
  created_at: string
  updated_at: string
}

export interface CreatePaperAccount {
  name: string
  exchange: string
  initial_balance: number
  leverage: number
  strategy: string
  currency?: string
}

export interface UpdatePaperAccount {
  name?: string
  initial_balance?: number
  leverage?: number
  strategy?: string
}

export interface SummaryResponse {
  total_pnl: number
  net_pnl: number
  total_fees: number
  trade_count: number
  win_count: number
  loss_count: number
  win_rate: number
  expected_value: number
  max_drawdown: number
}

export interface PnlHistoryRow {
  date: string
  total_pnl: string
  cumulative_pnl: string
}

export interface StrategyStats {
  strategy_name: string
  trade_count: number
  win_count: number
  total_pnl: string
  max_drawdown: string
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
  take_profit: string
  quantity: string | null
  leverage: string
  fees: string
  pnl_amount: string | null
  pnl_pips: string | null
  entry_at: string
  exit_at: string | null
  exit_reason: string | null
  paper_account_id: string | null
  status: string
}

export interface TradesResponse {
  trades: TradeRow[]
  total: number
  page: number
  per_page: number
}

export interface PositionResponse {
  trade_id: string
  strategy_name: string
  pair: string
  exchange: string
  direction: string
  entry_price: string
  quantity: string | null
  stop_loss: string
  take_profit: string
  entry_at: string
  paper_account_id: string | null
  paper_account_name: string
}

export interface DashboardFilter {
  exchange?: string
  paper_account_id?: string
  strategy?: string
  pair?: string
  from?: string
  to?: string
}
