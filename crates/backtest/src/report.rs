use auto_trader_core::types::{Trade, TradeStatus};
use rust_decimal::Decimal;

/// Result of a backtest run: the closed trades and the statistics derived from them.
pub struct BacktestReport {
    /// Number of closed trades.
    pub total_trades: usize,
    /// Number of closed trades with a positive profit.
    pub wins: usize,
    /// Number of closed trades that are not wins.
    pub losses: usize,
    /// Number of entries that the simulated trader could not open.
    pub execution_failures: usize,
    /// Fraction of closed trades that are wins, `0.0` when there are none.
    pub win_rate: f64,
    /// Sum of the profit and loss of the closed trades.
    pub total_pnl: Decimal,
    /// Largest fall of the balance from a previous peak while closing the trades in order.
    pub max_drawdown: Decimal,
    /// Balance at the start of the run.
    pub initial_balance: Decimal,
    /// Balance at the end of the run.
    pub final_balance: Decimal,
    /// Gross profit divided by gross loss: infinite when there is profit and no loss, `0.0` when there is neither.
    pub profit_factor: f64,
    /// Trades that the report was built from.
    pub trades: Vec<Trade>,
}

impl BacktestReport {
    /// Builds a report from the trades of a run, counting only closed trades in the statistics.
    pub fn from_trades_with_failures(
        trades: Vec<Trade>,
        initial_balance: Decimal,
        final_balance: Decimal,
        execution_failures: usize,
    ) -> Self {
        let closed: Vec<&Trade> = trades
            .iter()
            .filter(|t| t.status == TradeStatus::Closed)
            .collect();

        let total_trades = closed.len();
        let wins = closed
            .iter()
            .filter(|t| t.pnl_amount.unwrap_or_default() > Decimal::ZERO)
            .count();
        let losses = total_trades - wins;
        let win_rate = if total_trades > 0 {
            wins as f64 / total_trades as f64
        } else {
            0.0
        };

        let total_pnl = closed.iter().filter_map(|t| t.pnl_amount).sum::<Decimal>();

        let mut peak = initial_balance;
        let mut max_dd = Decimal::ZERO;
        let mut equity = initial_balance;
        for t in &closed {
            equity += t.pnl_amount.unwrap_or_default();
            if equity > peak {
                peak = equity;
            }
            let dd = peak - equity;
            if dd > max_dd {
                max_dd = dd;
            }
        }

        let gross_profit: Decimal = closed
            .iter()
            .filter_map(|t| t.pnl_amount)
            .filter(|p| *p > Decimal::ZERO)
            .sum();
        let gross_loss: Decimal = closed
            .iter()
            .filter_map(|t| t.pnl_amount)
            .filter(|p| *p < Decimal::ZERO)
            .map(|p| p.abs())
            .sum();
        let profit_factor = if gross_loss > Decimal::ZERO {
            (gross_profit / gross_loss)
                .to_string()
                .parse()
                .unwrap_or(0.0)
        } else if gross_profit > Decimal::ZERO {
            f64::INFINITY
        } else {
            0.0
        };

        Self {
            total_trades,
            wins,
            losses,
            execution_failures,
            win_rate,
            total_pnl,
            max_drawdown: max_dd,
            initial_balance,
            final_balance,
            profit_factor,
            trades,
        }
    }

    /// Prints the statistics of the report to standard output.
    pub fn print_summary(&self) {
        println!("=== Backtest Report ===");
        println!(
            "Trades: {} (W:{} L:{} Err:{})",
            self.total_trades, self.wins, self.losses, self.execution_failures
        );
        println!("Win Rate: {:.1}%", self.win_rate * 100.0);
        println!("Total PnL: {}", self.total_pnl);
        println!("Max Drawdown: {}", self.max_drawdown);
        println!("Profit Factor: {:.2}", self.profit_factor);
        println!("Balance: {} → {}", self.initial_balance, self.final_balance);
    }
}
