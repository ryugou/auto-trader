#![doc = "Order execution: position sizing, risk gate checks and the trader that places and closes orders."]

/// Order quantity calculation and trigger price rounding.
pub mod position_sizer;
/// Pure checks that decide whether an order may be placed.
pub mod risk_gate;
/// Trader that places and closes orders on an exchange and records the resulting trades.
pub mod trader;
