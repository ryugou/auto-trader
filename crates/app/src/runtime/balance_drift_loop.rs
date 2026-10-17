use std::sync::Arc;

use auto_trader::balance_drift::{BalanceDriftContext, check_live_accounts};
use auto_trader::spawn_system_alerts;
use auto_trader_notify::Notifier;

pub(crate) async fn check_and_alert(ctx: &BalanceDriftContext, notifier: &Arc<Notifier>) {
    let alerts = check_live_accounts(ctx).await;
    spawn_system_alerts(notifier, alerts);
}

pub(crate) async fn run(ctx: BalanceDriftContext, notifier: Arc<Notifier>) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3600));
    loop {
        interval.tick().await;
        check_and_alert(&ctx, &notifier).await;
    }
}
