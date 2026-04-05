use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{MacroUpdate, Strategy};
use auto_trader_core::types::{Direction, Pair, Signal};
use auto_trader_vegapunk::client::VegapunkClient;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio::sync::Mutex;

pub struct SwingLLMv1 {
    name: String,
    pairs: Vec<Pair>,
    _holding_days_max: u32,
    vegapunk: Mutex<VegapunkClient>,
    gemini_api_url: String,
    gemini_api_key: String,
    gemini_model: String,
    last_check: HashMap<String, chrono::DateTime<chrono::Utc>>,
    check_interval: chrono::Duration,
    latest_macro: Option<String>,
}

impl SwingLLMv1 {
    pub fn new(
        name: String,
        pairs: Vec<Pair>,
        holding_days_max: u32,
        vegapunk: VegapunkClient,
        gemini_api_url: String,
        gemini_api_key: String,
        gemini_model: String,
    ) -> Self {
        Self {
            name,
            pairs,
            _holding_days_max: holding_days_max,
            vegapunk: Mutex::new(vegapunk),
            gemini_api_url,
            gemini_api_key,
            gemini_model,
            last_check: HashMap::new(),
            check_interval: chrono::Duration::hours(4),
            latest_macro: None,
        }
    }

    fn should_check(&self, pair: &str) -> bool {
        let now = chrono::Utc::now();
        match self.last_check.get(pair) {
            Some(last) => now - *last >= self.check_interval,
            None => true,
        }
    }

    async fn query_vegapunk_and_llm(
        &self,
        pair: &Pair,
        current_price: Decimal,
    ) -> anyhow::Result<Option<(Direction, Decimal, Decimal, Decimal, f64)>> {
        // 1. Search Vegapunk for similar patterns
        let query = format!(
            "{}の現在の市場状況とトレード判断。価格: {}",
            pair.0, current_price
        );
        let mut vp = self.vegapunk.lock().await;
        let search_result = vp.search(&query, "local", 5).await?;

        // 2. Build context from search results
        let context: Vec<String> = search_result
            .results
            .iter()
            .filter_map(|r| r.text.clone())
            .collect();

        drop(vp); // release lock before making Gemini API call

        // 3. Ask Gemini Flash for trade decision
        let macro_context = self.latest_macro.as_deref().unwrap_or("マクロ情報なし");
        let prompt = format!(
            "あなたはFXスイングトレードの判断AIです。以下の情報からトレード判断をしてください。\n\n\
             通貨ペア: {}\n現在価格: {}\n\n\
             過去の類似判断:\n{}\n\n\
             マクロ環境: {}\n\n\
             回答は必ず以下のJSON形式のみで返してください:\n\
             {{\"action\": \"long\" | \"short\" | \"none\", \"confidence\": 0.0-1.0, \
             \"sl_pips\": number, \"tp_pips\": number, \"reason\": \"string\"}}",
            pair.0,
            current_price,
            context.join("\n"),
            macro_context,
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            self.gemini_api_url, self.gemini_model
        );

        let body = serde_json::json!({
            "contents": [{"parts": [{"text": prompt}]}]
        });

        let resp: serde_json::Value = client
            .post(&url)
            .header("x-goog-api-key", &self.gemini_api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let text = resp["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("");

        // Strip markdown code fences if present
        let json_text = text
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let decision: serde_json::Value = serde_json::from_str(json_text)?;

        let action = decision["action"].as_str().unwrap_or("none");
        let confidence = decision["confidence"].as_f64().unwrap_or(0.0);
        let sl_pips = decision["sl_pips"].as_f64().unwrap_or(100.0);
        let tp_pips = decision["tp_pips"].as_f64().unwrap_or(200.0);

        if action == "none" || confidence < 0.6 {
            return Ok(None);
        }

        let direction = match action {
            "long" => Direction::Long,
            "short" => Direction::Short,
            _ => return Ok(None),
        };

        let pip_size = if pair.0.contains("JPY") {
            Decimal::new(1, 2) // JPY pairs: 0.01
        } else {
            Decimal::new(1, 4) // others: 0.0001
        };

        let sl = pip_size * Decimal::try_from(sl_pips)?;
        let tp = pip_size * Decimal::try_from(tp_pips)?;

        Ok(Some((direction, current_price, sl, tp, confidence)))
    }
}

#[async_trait::async_trait]
impl Strategy for SwingLLMv1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }

        let pair_key = event.pair.0.clone();
        if !self.should_check(&pair_key) {
            return None;
        }

        let result = self
            .query_vegapunk_and_llm(&event.pair, event.candle.close)
            .await;

        // Update last_check only after successful query to allow retry on failure
        if result.is_ok() {
            self.last_check.insert(pair_key, chrono::Utc::now());
        }

        match result {
            Ok(Some((direction, entry, sl, tp, confidence))) => {
                let (stop_loss, take_profit) = match direction {
                    Direction::Long => (entry - sl, entry + tp),
                    Direction::Short => (entry + sl, entry - tp),
                };
                Some(Signal {
                    strategy_name: self.name.clone(),
                    pair: event.pair.clone(),
                    direction,
                    entry_price: entry,
                    stop_loss,
                    take_profit,
                    confidence,
                    timestamp: event.timestamp,
                })
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("swing_llm decision failed for {}: {e}", event.pair);
                None
            }
        }
    }

    fn on_macro_update(&mut self, update: &MacroUpdate) {
        self.latest_macro = Some(update.summary.clone());
    }
}
