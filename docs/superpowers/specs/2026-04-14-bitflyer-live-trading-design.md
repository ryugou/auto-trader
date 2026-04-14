# bitFlyer Live Trading 設計書

- 作成日: 2026-04-14
- ステータス: 承認済み（ユーザー指示により実装詳細は実装者判断）
- 対象: 仮想通貨自動売買システムの本番（実資金）運用化

## 1. 背景と目的

現在ペーパートレードで運用中のシステムを bitFlyer Lightning FX/CFD（`FX_BTC_JPY`）で実資金運用できる状態にする。既存のペーパートレード経路には一切手を入れず、live 経路を追加する。

既存コードベースには `OrderExecutor` trait の実装が `PaperTrader` のみで、bitFlyer Private API クライアントも存在しない。UI と DB スキーマだけ live 対応していたが、実取引は物理的に不可能な状態だった。

## 2. スコープ

### In-scope
- bitFlyer Lightning FX/CFD (`FX_BTC_JPY`) での実取引
- `TradeMode::Live` アカウントの新設（初期資金 30,000〜50,000 JPY、レバレッジ 2、戦略 `donchian_trend_v1`）
- dry_run モード（API キー無しでも live 経路を空撃ち検証可能）
- Kill Switch / WebSocket 健全性 / 二重発注防止 / 起動時リコンシリエーション
- Slack Webhook 通知
- DB の pending/inconsistent 状態追加

### Out-of-scope
- OANDA FX の live 取引（FX はペーパーすら未完）
- 戦略ロジックの変更
- 複数戦略の同時 live 投入（段階投入ルールで PR 分離）
- 他取引所（Binance 等）対応

## 3. 運用方針の判断結果

| 項目 | 決定 |
|------|------|
| 初期資金 | 30,000〜50,000 JPY |
| Kill Switch | 通常口座の設定を踏襲。日次損失上限 = 初期残高の 5% |
| 対象戦略 | `donchian_trend_v1`（通常口座と同じ）のみ。1戦略ずつ段階投入 |
| 通知 | Slack Webhook |
| 注文形式 | 戦略が `Signal.order_type` で決定。既存4戦略は全て `Market`（成行） |
| dry_run 期間 | 実装完了後 **最低 1 週間** dry_run 稼働、問題ゼロ確認後に解除 |

## 4. アーキテクチャ概観

```
StrategyEngine
  └─ Signal { ..., order_type: Market | Limit { price } }  ← 新設フィールド
        ↓
   RiskGate (新設: paper/live 共通の前段ガード)
     - 日次損失チェック（Kill Switch）
     - 価格 tick 鮮度チェック（> 60s なら拒否）
     - 二重発注チェック（pending/open 重複防止）
        ↓
   Executor Dispatcher (main.rs 改修)
     - account.account_type で分岐
        ↓                                ↓
   PaperTrader (既存・無変更)    LiveTrader (新設)
                                    ↓
                             dry_run == true なら log only で return
                                    ↓
                             BitflyerPrivateApi (新設)
                                    ↓
                             bitFlyer 取引所

並行タスク:
  - ExecutionPollingTask: pending → open への遷移
  - ReconcilerTask: 起動時 + 5分毎に DB vs 取引所 突合
  - BalanceSyncTask: getcollateral で current_balance 同期
  - Notifier: Slack Webhook 送信
```

## 5. 新規コンポーネント詳細

### 5.1. `BitflyerPrivateApi` (`crates/market/src/bitflyer_private.rs`)

bitFlyer Private REST API の薄いラッパー。

**認証:**
- ヘッダ: `ACCESS-KEY`, `ACCESS-TIMESTAMP`, `ACCESS-SIGN`
- 署名: `ACCESS-SIGN = HMAC-SHA256(secret, timestamp + method + path + body)`

**提供メソッド:**
```rust
async fn send_child_order(&self, req: SendChildOrderRequest) -> Result<ChildOrderAcceptanceId>;
async fn get_child_orders(&self, order_id: &str) -> Result<Vec<ChildOrder>>;
async fn get_executions(&self, order_id: &str) -> Result<Vec<Execution>>;
async fn get_positions(&self, product_code: &str) -> Result<Vec<ExchangePosition>>;
async fn get_collateral(&self) -> Result<Collateral>;
async fn cancel_child_order(&self, order_id: &str) -> Result<()>;
```

**レート制限:** Private API は IP/アカウント単位で 200 req / 5 min。`governor` クレートでトークンバケット実装。超過時は自動で待機。

**依存追加:** `hmac = "0.12"`, `sha2 = "0.10"`, `hex = "0.4"`, `governor = "0.6"`

**テスト:** `wiremock` で bitFlyer レスポンスを mock。以下を必ず検証:
- 署名文字列のスナップショット（リファレンス実装と一致すること）
- エラーレスポンス (`status: -205` insufficient funds 等) の分類
- レート制限超過時の待機挙動

### 5.2. `LiveTrader` (`crates/executor/src/live.rs`)

`OrderExecutor` trait 実装。`PaperTrader` と同じインターフェースを維持。

```rust
pub struct LiveTrader {
    api: Arc<BitflyerPrivateApi>,
    pool: PgPool,
    notifier: Arc<Notifier>,
    dry_run: bool,
}

impl OrderExecutor for LiveTrader {
    async fn execute(&self, signal: Signal, account: &PaperAccount) -> Result<Trade> {
        // 1. trade を status='pending' で DB insert
        // 2. dry_run == true ならログ + trade を status='open' で close (paper 相当)
        // 3. api.send_child_order() 呼び出し
        // 4. レスポンスの child_order_acceptance_id を trade に保存
        // 5. 返却（実約定は ExecutionPollingTask が後で埋める）
    }

    async fn close_position(&self, trade: &Trade, reason: ExitReason) -> Result<()> {
        // 1. 反対売買の成行注文発行（quantity = trade.quantity）
        // 2. 約定待ち（max 30s タイムアウト、dry_run 時は即座に signal 価格で close）
        // 3. 実約定価格で pnl 計算、status='closed' に遷移
        // 4. 約定待ち時間超過時は warn 通知 + status='inconsistent'
    }
}
```

### 5.3. `RiskGate` (`crates/executor/src/risk_gate.rs`)

Signal を Executor に渡す前のガード層。paper/live 両方に適用。

```rust
pub enum GateDecision {
    Pass,
    Reject(RejectReason),
}

pub enum RejectReason {
    DailyLossLimitExceeded { loss: Decimal, limit: Decimal },
    PriceTickStale { age_secs: u64 },
    DuplicatePosition { existing_trade_id: Uuid },
    KillSwitchActive { until: DateTime<Utc> },
}

impl RiskGate {
    pub async fn check(&self, signal: &Signal, account: &PaperAccount) -> GateDecision;
}
```

**Kill Switch 発動ロジック:**
- 本日 (JST) クローズ済みトレードの `pnl_amount` 合計 + 現在の含み損益 <= `-(initial_balance × 0.05)` なら発動
- 発動時: `risk_halts` テーブルに `halted_until = 翌日 0:00 JST` で登録
- 当該アカウントからの新規エントリーを `halted_until` まで全拒否
- Slack 通知: 発動・解除両方

**二重発注防止:**
- DB に partial unique index を張る（5.7 参照）
- アプリ層でも `RiskGate` で事前チェックして即座にレスポンス返す

### 5.4. Signal への `order_type` 追加

```rust
#[derive(Debug, Clone)]
pub enum OrderType {
    Market,
    Limit { price: Decimal },
}

pub struct Signal {
    // ...既存フィールド
    pub order_type: OrderType,
}
```

既存4戦略は全て `OrderType::Market` を返すよう修正（挙動変更なし）。

### 5.5. `Notifier` (新 crate `crates/notify`)

既存の `db/notifications.rs` は UI 内通知専用。外部通知は別 crate として独立。

```rust
pub struct Notifier {
    slack_webhook: Option<String>, // SLACK_WEBHOOK_URL env
    http: reqwest::Client,
}

pub enum NotifyEvent {
    OrderFilled { account, trade_id, pair, side, qty, price },
    OrderFailed { account, signal, reason },
    PositionClosed { account, trade_id, pnl, reason },
    KillSwitchTriggered { account, daily_loss, limit, halted_until },
    KillSwitchReleased { account },
    WebSocketDisconnected { duration_secs },
    StartupReconciliationDiff { orphan_db: Vec<Uuid>, orphan_exchange: Vec<String> },
    BalanceDrift { account, db_balance, exchange_balance, diff_pct },
    DryRunOrder { account, signal },  // dry_run 時に発注予定を通知
}

impl Notifier {
    pub async fn send(&self, event: NotifyEvent);
}
```

- Webhook 失敗時はリトライ 3 回、それでも失敗したらログのみ（通知失敗で本処理を止めない）
- `SLACK_WEBHOOK_URL` 未設定時は no-op（ログのみ）

### 5.6. `ExecutionPollingTask` (`crates/app/src/tasks/execution_poller.rs`)

Live アカウントの pending トレードを監視し、bitFlyer 約定を検出して open に遷移させる。

```rust
// 実行間隔: 3 秒
async fn tick(&self) {
    let pending_trades = db::trades::list_pending_live().await?;
    for trade in pending_trades {
        let executions = api.get_executions(&trade.child_order_acceptance_id).await?;
        if executions.is_empty() {
            // pending から 60 秒経過 → 約定しなかったと判定、inconsistent へ
            if trade.created_at.age() > 60s {
                mark_inconsistent(trade, "no execution in 60s").await?;
                notifier.send(OrderFailed { .. }).await;
            }
            continue;
        }
        let avg_price = weighted_average(&executions);
        let actual_qty = executions.iter().map(|e| e.size).sum();
        db::trades::promote_pending_to_open(trade.id, avg_price, actual_qty).await?;
        notifier.send(OrderFilled { .. }).await;
    }
}
```

### 5.7. DB マイグレーション (`migrations/20260414000001_live_trading_support.sql`)

```sql
-- trade_status に pending, inconsistent を追加
ALTER TYPE trade_status ADD VALUE IF NOT EXISTS 'pending';
ALTER TYPE trade_status ADD VALUE IF NOT EXISTS 'inconsistent';

-- bitFlyer 注文 ID を保持
ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS child_order_acceptance_id TEXT,
    ADD COLUMN IF NOT EXISTS child_order_id TEXT;

-- 二重発注防止: 同一 account × strategy × pair で pending/open は1件まで
CREATE UNIQUE INDEX IF NOT EXISTS trades_one_active_per_strategy_pair
    ON trades (paper_account_id, strategy_name, pair)
    WHERE status IN ('pending', 'open');

-- Kill Switch 発動記録
CREATE TABLE IF NOT EXISTS risk_halts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    paper_account_id UUID NOT NULL REFERENCES paper_accounts(id),
    reason TEXT NOT NULL,
    triggered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    halted_until TIMESTAMPTZ NOT NULL,
    released_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS risk_halts_account_active
    ON risk_halts (paper_account_id, halted_until)
    WHERE released_at IS NULL;
```

### 5.8. `ReconcilerTask` (`crates/app/src/tasks/reconciler.rs`)

起動時 + 5 分毎に DB と取引所の建玉を突合。Live アカウントのみ対象。

- DB の `status IN ('pending', 'open')` トレードを取得
- `api.get_positions("FX_BTC_JPY")` で取引所側建玉を取得
- 差分検出:
  - **DB にあるが取引所に無い** → `status='inconsistent'` に遷移、通知
  - **取引所にあるが DB に無い** → warn 通知（手動対処。DB に勝手に作らない）
  - **数量不一致** → warn 通知

### 5.9. `BalanceSyncTask` (`crates/app/src/tasks/balance_sync.rs`)

Live アカウント専用。`getcollateral` で実残高を取得し `paper_accounts.current_balance` に同期。

- 実行間隔: 5 分
- 差分が 1% 以上なら `BalanceDrift` 通知
- Paper アカウントには適用しない（ペーパーは自前計算）

### 5.10. 手数料モデル分岐

`account_type == 'live'` のトレードに対して：
- `main.rs:1332-1381` の自前 overnight fee 課金を**スキップ**
- `ExecutionPollingTask` で bitFlyer executions API から取得した `commission` を `trades.fees` に加算

Paper アカウントは現状の 0.04%/日 自前課金を維持。

## 6. 設定・環境変数

### 6.1. `.env.example` に追加

```bash
# bitFlyer Private API (live trading)
BITFLYER_API_KEY=
BITFLYER_API_SECRET=

# Slack 通知
SLACK_WEBHOOK_URL=

# Live 発注のドライラン（true: 発注直前で no-op、false: 実発注）
LIVE_DRY_RUN=true
```

### 6.2. `config/default.toml` に追加

```toml
[risk]
daily_loss_limit_pct = 0.05      # 日次損失上限（初期残高比）
price_freshness_secs = 60        # price tick 鮮度閾値
kill_switch_release_jst_hour = 0 # Kill Switch 自動解除時刻 (JST)

[live]
enabled = false                  # true にしないと LiveTrader は起動しない
dry_run = true                   # true なら発注手前で no-op（LIVE_DRY_RUN env で上書き可）
execution_poll_interval_secs = 3
reconciler_interval_secs = 300
balance_sync_interval_secs = 300
```

### 6.3. 起動時バリデーション (`crates/app/src/main.rs`)

起動時に以下を検証し、違反すれば fatal で終了：

1. `[live].enabled = true` の時:
   - `account_type='live'` のアカウントが 1 件以上存在
   - `BITFLYER_API_KEY` / `BITFLYER_API_SECRET` が非空
   - `SLACK_WEBHOOK_URL` が非空（通知なしの live は許さない）
2. `account_type='live'` のアカウントが存在する時:
   - `[live].enabled = true` でなければ fatal
3. `[live].dry_run = false` の時:
   - ログに **「🔴 LIVE TRADING MODE - REAL MONEY AT RISK」** を大きく出力
   - 3 秒待機後に取引開始（人間が気付けるように）

## 7. UI 対応

### 7.1. Accounts ページ
- `account_type == 'live'` の行を赤枠 + 「LIVE」バッジで強調
- dry_run 中は黄枠 + 「DRY RUN」バッジ

### 7.2. Positions ページ
- `status == 'pending'` を別色（青系）で表示
- `status == 'inconsistent'` を赤色 + 警告アイコン
- bitFlyer 注文 ID を新規列で表示（live のみ）

### 7.3. ダッシュボード上部
- Kill Switch 発動中のアカウントがあれば赤バナー
- WebSocket 切断中は黄バナー

## 8. テスト戦略

### 8.1. 単体テスト
- `BitflyerPrivateApi` の HMAC 署名（スナップショット）
- `RiskGate` の各 RejectReason ケース
- `LiveTrader` の state machine（pending → open → closed）

### 8.2. 統合テスト
- `wiremock` で bitFlyer API をフル mock
- シナリオ:
  - 正常発注 → 約定 → クローズ
  - 注文失敗 (残高不足)
  - 約定遅延 (60s 超過で inconsistent)
  - WebSocket 切断中の新規エントリー拒否
  - Kill Switch 発動 → 解除
  - リコンシリエーション差分検出

### 8.3. dry_run 走行テスト
- 実装完了後、`LIVE_DRY_RUN=true` で **最低 1 週間** 稼働
- Slack 通知で「こう発注する予定」を全件記録
- 期間中、ペーパートレードとの一貫性を確認

## 9. 実装順序（PR 分割）

※ 各 PR は CI 全緑 + TDD + code-review スキル通過を必須とする

| PR | 内容 | 規模目安 |
|----|------|---------|
| **PR 1** | Signal に OrderType 追加、既存戦略を Market に修正、Notifier crate 新設 + Slack Webhook、DB マイグレーション (pending/inconsistent/risk_halts/unique index) | 中 |
| **PR 2** | BitflyerPrivateApi 実装 + wiremock 統合テスト | 大 |
| **PR 3** | LiveTrader 本体 + dry_run モード + ExecutionPollingTask | 大 |
| **PR 4** | RiskGate (Kill Switch + WS 健全性 + 二重発注ガード) | 中 |
| **PR 5** | ReconcilerTask + BalanceSyncTask + 手数料モデル分岐 | 中 |
| **PR 6** | main.rs dispatcher 配線 + env バリデーション + account_type 整合性検証 | 中 |
| **PR 7** | UI 対応（live/pending/inconsistent 視認性） | 小 |
| **PR 8** | E2E 統合テスト + ドキュメント (`specs/live-trading-operations.md`) | 中 |

**リリース手順:**
1. PR 1〜8 全てマージ、`LIVE_DRY_RUN=true` で本番ブランチに deploy
2. **最低 1 週間の dry_run 走行**、Slack 通知と DB 状態を監視
3. 問題ゼロ確認後、bitFlyer API キー発行（ユーザー作業）
4. `.env` に API キー設定、`LIVE_DRY_RUN=false` に変更
5. まず `donchian_trend_v1`（通常口座相当）1 戦略のみ live アカウント作成、初期資金 30,000 JPY
6. 1 週間問題なければ、他戦略の live 化を検討

## 10. リスクと対策

| リスク | 対策 |
|--------|------|
| API キー漏洩 | env 管理、git ignore 徹底、キーローテーション手順を docs に明記 |
| 重複発注 | DB partial unique index + RiskGate 事前チェック |
| 約定不一致 | ExecutionPollingTask + ReconcilerTask で検出、inconsistent 状態で手動対処 |
| WS 切断中の古い価格で発注 | RiskGate の price_freshness_secs チェック |
| 想定外の損失拡大 | Kill Switch（日次損失 -5% で 24h 停止） |
| 通知失敗で障害気付かず | Slack Webhook 送信失敗もログ + 起動時疎通確認 |
| dry_run と live の挙動ズレ | dry_run は「発注手前までは完全に live 経路を通る」設計 |
| DB と取引所の残高ズレ | BalanceSyncTask で 1% 超過を通知、手動差分確認 |

## 11. 段階的デプロイ・緊急停止手順

### 緊急停止（全 live ポジションをクローズ）

```bash
# 1. LIVE_DRY_RUN=true に変更して再起動（新規発注停止）
docker compose restart auto-trader

# 2. bitFlyer Web UI または API で手動クローズ
# (自動クローズは race condition を招くため提供しない)

# 3. DB の open/pending トレードを inconsistent に遷移
docker compose exec db psql -U auto-trader -d auto_trader \
  -c "UPDATE trades SET status='inconsistent' WHERE status IN ('open', 'pending') AND exchange='bitflyer_cfd';"
```

### キーローテーション

1. bitFlyer Web で新キー発行
2. `.env` を新キーに更新
3. `docker compose restart auto-trader`
4. 起動ログで `BitflyerPrivateApi` 疎通成功確認
5. bitFlyer Web で旧キー無効化

## 12. 受け入れ基準

- [ ] PR 1〜8 全てマージ済み
- [ ] CI 全緑、コードカバレッジが低下していない
- [ ] wiremock 統合テストで全シナリオパス
- [ ] `LIVE_DRY_RUN=true` で 1 週間以上の連続稼働実績
- [ ] Slack 通知が期待通り発火（発注/失敗/Kill Switch/WS 切断/リコン差分）
- [ ] 本ドキュメント (`specs/live-trading-operations.md`) 完成
- [ ] ユーザー最終承認
