# GMO FX Private API + Leverage Validation 設計

- 作成日: 2026-05-15
- ステータス: brainstorming 完了、未実装
- 目的: 「paper = live 契約」を成立させるための 9 項目のうち、(1) GMO FX Private API 実装 と (8) leverage 規制 validation をまとめて 1 PR に
- 関連: `~/.claude/projects/-Users-ryugo-Developer-src-personal-auto-trader/memory/feedback_paper_equals_live_in_unified_design.md`、本セッションで実施した paper=live 監査の Critical C1 と C2

## 背景

`Unified Trader (PR #42)` の契約: paper と live は同じコード経路、`dry_run=true` フラグだけが違う。ペーパーで動く=live で動く。

ところが現状:
- **`crates/market/src/gmo_fx.rs`** は Public Ticker REST polling のみ。Private API (注文/ポジション/口座/約定/キャンセル) が無い。
- **`crates/app/src/main.rs:75-128`** の `exchange_apis` registry に `Exchange::GmoFx` キー無し。
- **`crates/app/src/main.rs:1338-1359`** で live 口座 + `Exchange::GmoFx` の signal は "no ExchangeApi registered" として silent skip。
- **結果**: USD_JPY 戦略は paper では動くが、live=true にすると signal が消える。 → 契約違反。

加えて:
- **`crates/db/src/trading_accounts.rs:60`** の CHECK 制約は `leverage >= 1` のみ、上限無し。
- 日本の金融規制: 個人 FX 最大 25 倍、暗号資産 2 倍。 paper でも live でも同じ規制を踏まないと paper の試算が live と乖離する。

## ゴール

1. `Exchange::GmoFx` で live 口座が **silent skip ではなく実発注**されるようにする
2. open / close を bitFlyer の「opposite-side 新規発注」パターンに依存せず、GMO の `/v1/closeOrder` 経由で正しく実装する
3. `Trade.exchange_position_id` を追加し、open 時に取得した GMO positionId を保存、close 時に再利用
4. leverage 規制を accounts API レイヤで強制 (paper でも live でも有効)
5. mock テストで full open→close フローを担保

## 非ゴール (この PR では扱わない)

- 残りの 7 項目 (slippage / GMO swap / bitFlyer SFD / commission 経路 / EUR_USD 換算 / paper ロスカット / 時間軸 gate)
- bitFlyer 側の挙動変更 (既存 close_position_id=None で従来通り動かす)
- live 専用安全装置 (Kill Switch / 残高同期 / orphan handler 等 → memory feedback で禁止)
- OANDA に関する一切の変更 (memory feedback で禁止)
- GMO の指値 / OCO / IFD 注文 (market 注文のみで開始)

## 外部仕様 (GMO Coin Forex FX Private API)

WebFetch 実機確認済 (https://api.coin.z.com/fxdocs/en/):

- Base URL: `https://forex-api.coin.z.com/private`
- 認証: HMAC-SHA256、headers = `API-KEY` / `API-TIMESTAMP` (Unix ms) / `API-SIGN`
- 署名対象: `timestamp + method + path + requestBody` (path は `/v1/...`、`/private` は含まない、GET は body 空文字列)
- 主要 endpoint:
  - `POST /v1/order` — 新規注文 (symbol, side, executionType=MARKET, size 等)
  - `POST /v1/closeOrder` — 決済注文 (symbol, side=opposite, executionType, settlePosition: [{positionId, size}])
  - `POST /v1/cancelOrder` — キャンセル
  - `GET /v1/openPositions` — 建玉一覧 (positionId 取得)
  - `GET /v1/positionSummary` — 建玉サマリ
  - `GET /v1/executions` — 約定履歴
  - `GET /v1/account/assets` — 口座資産 (equity / margin / available)
- Symbol 形式: `USD_JPY`、`EUR_JPY` 等 (本リポジトリの `Pair("USD_JPY")` と一致)
- Rate limit: **POST 1 req/sec、GET 6 req/sec** (bitFlyer の 200/5min より厳しい)
- エラー: response の `status` フィールド + `cancelType` で失敗種別を返す

## アーキテクチャ

### 既存 trait の最小拡張

`crates/market/src/exchange_api.rs`:

```rust
use crate::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse,
};

#[async_trait]
pub trait ExchangeApi: Send + Sync {
    async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse>;

    async fn get_child_orders(...) -> ...;
    async fn get_executions(...) -> ...;
    async fn get_positions(...) -> ...;
    async fn get_collateral(...) -> ...;
    async fn cancel_child_order(...) -> ...;

    /// open 約定後に、新しく作られた exchange-side position の識別子を返す。
    /// bitFlyer は概念的に position に id を持たないので `None` を返す。
    /// GMO FX は `/v1/openPositions` から最新分を取得して `Some(positionId)` を返す。
    /// `after` は open の send_child_order を打った時刻。GMO 側 `openTimestamp >= after`
    /// で絞り込み、複数該当時は最新を採用。
    async fn resolve_position_id(
        &self,
        product_code: &str,
        after: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Option<String>>;
}
```

`crates/market/src/bitflyer_private.rs`:

```rust
pub struct SendChildOrderRequest {
    pub product_code: String,
    pub child_order_type: ChildOrderType,
    pub side: Side,
    pub size: Decimal,
    pub price: Option<Decimal>,
    pub minute_to_expire: Option<u32>,
    pub time_in_force: Option<TimeInForce>,
    /// `Some(positionId)` のとき、この注文は既存 position の close を意図する。
    /// - bitFlyer 実装: 無視 (opposite-side 新規発注として処理)
    /// - GMO FX 実装: `/v1/closeOrder` に dispatch、positionId を `settlePosition` に詰める
    /// 既存呼び出しは `close_position_id: None` で従来通り動く。
    pub close_position_id: Option<String>,
}
```

### `crates/market/src/gmo_fx_private.rs` (新規)

`bitflyer_private.rs` (~700 LOC) と同じ構造を踏襲:

```
GmoFxApiError (enum, anyhow 経由)
  ├ InvalidApiKey / InvalidSignature / Unauthorized
  ├ InsufficientMargin / InvalidOrderSize / PositionNotFound
  └ NetworkError / TransportError / Other(status)

GmoOrderRequest (POST /v1/order body)
  { symbol, side: BUY|SELL, executionType: MARKET, size: String, ... }

GmoCloseRequest (POST /v1/closeOrder body)
  { symbol, side, executionType, settlePosition: [{positionId, size}] }

GmoOrderResponse / GmoExecution / GmoOpenPosition / GmoAccountAssets
  (公式 response shape に従う、Decimal は文字列でやってくるので serde-with-str)

pub struct GmoFxPrivateApi {
    api_url: String,                    // "https://forex-api.coin.z.com"
    api_key: String,
    api_secret: String,
    http: reqwest::Client,
    post_limiter: Arc<RateLimiter>,     // 1 req/sec
    get_limiter: Arc<RateLimiter>,      // 6 req/sec
}

impl GmoFxPrivateApi {
    pub fn new(api_key: String, api_secret: String) -> Self { ... }
    pub fn with_post_limiter(self, lim: Arc<RateLimiter>) -> Self { ... }
    pub fn with_get_limiter(self, lim: Arc<RateLimiter>) -> Self { ... }
    pub fn with_api_url(self, url: String) -> Self { ... }   // テスト用 mock URL override

    async fn signed_request<T: DeserializeOwned>(&self, method, path, body) -> Result<T>
        // wait limiter → build headers (API-KEY/API-TIMESTAMP/API-SIGN)
        // → send → parse status field → decode T
}

#[async_trait]
impl ExchangeApi for GmoFxPrivateApi {
    async fn send_child_order(&self, req: SendChildOrderRequest) -> ... {
        match req.close_position_id {
            None => self.post_open_order(req).await,
            Some(pid) => self.post_close_order(req, pid).await,
        }
    }
    async fn resolve_position_id(&self, product_code, after) -> Option<String> {
        let positions = self.signed_request_get("/v1/openPositions?symbol={}").await?;
        // openTimestamp >= after でフィルタ、size match、最新を返す
    }
    async fn get_executions(...) -> Vec<Execution> {
        // /v1/executions?orderId=... → GmoExecution → Execution (bitFlyer-shaped) に変換
    }
    async fn get_positions(...) -> Vec<ExchangePosition> {
        // /v1/openPositions → GmoOpenPosition → ExchangePosition に変換 (commission/sfd 等は 0)
    }
    async fn get_collateral(&self) -> Collateral {
        // /v1/account/assets → GmoAccountAssets → Collateral に変換
    }
    async fn cancel_child_order(...) -> () {
        // /v1/cancelOrder { orderId }
    }
    async fn get_child_orders(...) -> Vec<ChildOrder> {
        // /v1/orders?orderId=... → GmoOrder → ChildOrder に変換
    }
}

fn sign(api_secret: &str, timestamp: i64, method: &str, path: &str, body: &str) -> String
    // HMAC-SHA256(secret, timestamp + method + path + body) → hex
```

### Trade に `exchange_position_id` 追加

migration `migrations/20260515000001_add_exchange_position_id_to_trades.sql`:

```sql
ALTER TABLE trades ADD COLUMN exchange_position_id TEXT;
COMMENT ON COLUMN trades.exchange_position_id IS
  'Exchange-side position identifier (used by GMO FX /v1/closeOrder). NULL for exchanges that net positions implicitly (bitFlyer).';
```

`crates/core/src/types.rs` の `Trade`:

```rust
pub struct Trade {
    ... existing ...
    pub exchange_position_id: Option<String>,
}
```

`crates/db/src/trades.rs` の INSERT / UPDATE / TradeRow mapping にカラム追加。

### trader.rs の wiring

`crates/executor/src/trader.rs`:

```rust
// open path (live, 既存) — fill_open 後:
let exchange_position_id = if self.dry_run {
    None
} else {
    self.api.resolve_position_id(&signal.pair.0, entry_at).await.ok().flatten()
};
let trade = Trade { ..., exchange_position_id, ... };
// DB INSERT

// close path:
let req = SendChildOrderRequest {
    product_code,
    side: opposite(trade.direction),
    size: trade.quantity,
    close_position_id: trade.exchange_position_id.clone(),  // None for bitFlyer, Some for GMO
    ...
};
self.api.send_child_order(req).await?;
```

dry_run (paper) は `resolve_position_id` を呼ばず常に None。paper では close_position_id=None でも close ロジックが完結する (paper は実 API を叩かないので)。

### registry 登録

`crates/app/src/main.rs:75-128` の bitFlyer 登録ブロックの直後に追加:

```rust
// GMO FX ExchangeApi — registered when GMO_API_KEY + GMO_API_SECRET present.
let gmo_api_key = std::env::var("GMO_API_KEY").ok().filter(|s| !s.trim().is_empty());
let gmo_api_secret = std::env::var("GMO_API_SECRET").ok().filter(|s| !s.trim().is_empty());
match (gmo_api_key, gmo_api_secret) {
    (Some(key), Some(secret)) => {
        let gmo_api: Arc<dyn ExchangeApi> = Arc::new(GmoFxPrivateApi::new(key, secret));
        exchange_apis.insert(Exchange::GmoFx, gmo_api);
        tracing::info!("GMO FX ExchangeApi registered");
    }
    _ => tracing::info!(
        "GMO FX ExchangeApi not registered (needs GMO_API_KEY + GMO_API_SECRET env)"
    ),
}
```

### leverage validation

`crates/db/src/trading_accounts.rs` (関数追加):

```rust
/// Returns Err with a human message when leverage exceeds the regulatory cap
/// for the exchange. Regulatory caps:
///   - bitflyer_cfd (crypto): 2x (Japan FSA limit for crypto)
///   - gmo_fx (FX): 25x (Japan FSA limit for retail FX)
/// Unknown exchanges pass through (return Ok) so adding a new exchange
/// requires conscious code change to set its cap.
pub fn validate_leverage_for_exchange(exchange: &str, leverage: i32) -> Result<(), String> {
    let cap = match exchange {
        "bitflyer_cfd" => 2,
        "gmo_fx" => 25,
        _ => return Ok(()),
    };
    if leverage > cap {
        Err(format!(
            "leverage {leverage} exceeds regulatory cap {cap} for {exchange}"
        ))
    } else {
        Ok(())
    }
}
```

`crates/app/src/api/accounts.rs` の create / update で呼び出し、validation error は 400 で返す。

### Testing

#### Unit

- `gmo_fx_private::sign_matches_known_hmac_sha256_vector` — 公式サンプル値 (なければ Python `hmac.new(secret.encode(), (ts+method+path+body).encode(), hashlib.sha256).hexdigest()` で生成して固定値テスト)
- `gmo_fx_private::open_order_json_serialization` — GmoOrderRequest → JSON が公式仕様通り
- `gmo_fx_private::close_order_json_serialization` — GmoCloseRequest → JSON
- `trading_accounts::validate_leverage_for_exchange_*` — 各 exchange × {cap-1, cap, cap+1} の境界

#### Integration

新規 `crates/integration-tests/src/mocks/gmo_fx_private_server.rs`:
- 既存 `gmo_fx_server.rs` (Public Ticker mock) と同様 wiremock ベース
- `POST /v1/order` → canned response (orderId, status=0)
- `POST /v1/closeOrder` → canned
- `GET /v1/openPositions` → 設定可能な positions リスト返却 (positionId/openTimestamp/symbol/side/size)
- `GET /v1/executions?orderId=X` → canned executions
- `GET /v1/account/assets` → canned

統合テスト:
1. `phase4_gmo_fx_open_flow` — open signal → mock /v1/order が叩かれる、resolve_position_id で /v1/openPositions が叩かれて trade.exchange_position_id が set される
2. `phase4_gmo_fx_close_flow` — open 済みの trade を close → /v1/closeOrder が positionId 込みで叩かれる、bitFlyer のように /v1/order が叩かれないことを確認
3. `phase4_gmo_fx_unauthorized` — 401 系エラーで proper error 伝播
4. `phase4_gmo_fx_rate_limit` — 連続 POST がレート制限される (governor 動作確認)
5. `phase3_accounts_api_leverage_validation` — POST /api/trading-accounts で leverage=30 / exchange=gmo_fx が 400 で reject、leverage=25 は 201、leverage=3 / exchange=bitflyer_cfd は 400 で reject

#### Existing phase3_swing_llm 系等への影響

`SendChildOrderRequest` の field 追加だけなので既存 caller (bitFlyer 経由) は build-break しない (`close_position_id: None` を渡せばよい)。明示的に bitFlyer test を全件パスさせる。

## エラーハンドリング

- HTTP 4xx/5xx は `GmoFxApiError::Http { status, body }` で wrap、最上位で anyhow へ
- GMO 独自エラーコード (`status != 0`) は `messages` 配列を parse して `GmoFxApiError::Api { code, message }`
- Network error (timeout/connect failure) は retry 3 回 (250ms exponential backoff、bitFlyer と同じ)
- Rate limit (HTTP 429 もしくは status 内 rate-limit) は governor で事前防止、検出時は警告ログ + 1 秒 sleep + 1 回 retry

## Out of scope (重要)

以下は memory feedback で禁止されているため、本 PR で扱わない:
- Kill Switch / 残高同期 / orphan handler / 起動時 API key 空検証 / live セッション専用 reconcile
- OANDA に関する一切の変更
- 残り 7 項目 (slippage / swap / SFD / commission / EUR_USD / liquidation / 時間軸 gate)

## マイグレーション順序

1. migration `add_exchange_position_id_to_trades.sql`
2. `Trade` struct + `db::trades` 更新
3. `SendChildOrderRequest.close_position_id` 追加 + bitFlyer impl 無視
4. `ExchangeApi::resolve_position_id` 追加 + bitFlyer は None 返却
5. `gmo_fx_private.rs` 実装 + unit test
6. mock server 拡張 + integration test
7. `main.rs` registry 登録
8. `trader.rs` の wiring (open 後 resolve_position_id、close 時 close_position_id 引き継ぎ)
9. leverage validation + accounts API 統合
10. `scripts/test-all.sh` 全段階 green 確認後 commit + push + PR

## 完了条件

- `Exchange::GmoFx` の live 口座で open / close が実 API endpoint に到達する (integration test で確認)
- `Trade.exchange_position_id` が GMO の場合に set され、close でそれが closeOrder に渡される
- accounts API が gmo_fx で leverage > 25、bitflyer_cfd で leverage > 2 を reject
- 既存 bitFlyer integration test 全件 green、新規 GMO FX integration test 全件 green
- `./scripts/test-all.sh` 全段階 green
