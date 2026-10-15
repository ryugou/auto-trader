use auto_trader_db::trades::OpenTradeWithAccount;

pub(crate) mod crypto_monitor;
pub(crate) mod engine_loop;
pub(crate) mod executor_loop;
pub(crate) mod exit_executor;
pub(crate) mod overnight;
pub(crate) mod recorder;
pub(crate) mod warmup;

pub(crate) struct TradeAccount {
    pub(crate) account_type: String,
    pub(crate) account_name: String,
}

pub(crate) fn resolve_trade_account(owned: &OpenTradeWithAccount) -> Option<TradeAccount> {
    let trade = &owned.trade;
    let Some(account_type) = owned.account_type.clone() else {
        tracing::warn!(
            trade_id = %trade.id,
            account_id = %trade.account_id,
            "invalid account: not found"
        );
        return None;
    };
    let account_name = owned
        .account_name
        .clone()
        .unwrap_or_else(|| trade.account_id.to_string());
    Some(TradeAccount {
        account_type,
        account_name,
    })
}
