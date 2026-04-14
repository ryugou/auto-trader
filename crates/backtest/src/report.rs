use auto_trader_core::types::{Trade, TradeStatus};
use rust_decimal::Decimal;

pub struct BacktestReport {
    pub total_trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub execution_failures: usize,
    pub win_rate: f64,
    pub total_pnl: Decimal,
    pub max_drawdown: Decimal,
    pub initial_balance: Decimal,
    pub final_balance: Decimal,
    pub profit_factor: f64,
}

impl BacktestReport {
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

        // Max drawdown from equity curve
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
        }
    }

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
