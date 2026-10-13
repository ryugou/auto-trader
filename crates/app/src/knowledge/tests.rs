use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::types::{Direction, Exchange, Pair, Trade, TradeStatus};
use auto_trader_core::vegapunk_port::{SearchMode, SearchResults, VegapunkApi};
use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

use super::*;

struct CaptureApi {
    captured: std::sync::Mutex<Vec<(String, String, String, String)>>,
}

#[async_trait::async_trait]
impl VegapunkApi for CaptureApi {
    async fn ingest_raw(
        &self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<()> {
        self.captured.lock().unwrap().push((
            text.into(),
            source_type.into(),
            channel.into(),
            timestamp.into(),
        ));
        Ok(())
    }
    async fn search(&self, _: &str, _: SearchMode, _: i32) -> anyhow::Result<SearchResults> {
        Ok(SearchResults {
            hits: vec![],
            search_id: "sid".into(),
        })
    }
    async fn feedback(&self, _: &str, _: i32, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn merge(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

fn fixture_trade() -> Trade {
    Trade {
        id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        strategy_name: "test_strategy".to_string(),
        pair: Pair("USD_JPY".to_string()),
        exchange: Exchange::GmoFx,
        direction: Direction::Long,
        entry_price: Decimal::new(15000, 2),
        entry_at: Utc::now(),
        stop_loss: Decimal::new(14900, 2),
        take_profit: Some(Decimal::new(15200, 2)),
        quantity: Decimal::new(1000, 0),
        leverage: Decimal::ONE,
        status: TradeStatus::Open,
        pnl_amount: None,
        fees: Decimal::ZERO,
        exit_price: None,
        exit_at: None,
        exit_reason: None,
        max_hold_until: None,
        exchange_position_id: None,
        stop_order_id: None,
    }
}

#[tokio::test]
async fn record_trade_open_emits_ingest_with_correct_metadata() {
    let api = Arc::new(CaptureApi {
        captured: std::sync::Mutex::new(Vec::new()),
    });
    let store = VegapunkKnowledgeStore::new(api.clone() as Arc<dyn VegapunkApi>);

    let mut indicators = HashMap::new();
    indicators.insert("rsi".to_string(), Decimal::new(50, 0));
    let trade = fixture_trade();

    store
        .record_trade_open(&trade, &indicators, Some(Decimal::new(2, 2)))
        .await
        .unwrap();

    let captured = api.captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let (text, source_type, channel, _ts) = &captured[0];
    assert_eq!(source_type, "trade_signal");
    assert_eq!(channel, "usd_jpy-trades");
    assert!(text.contains("USD_JPY"), "text should include pair");
    assert!(text.contains("ロング"), "text should include direction");
}
