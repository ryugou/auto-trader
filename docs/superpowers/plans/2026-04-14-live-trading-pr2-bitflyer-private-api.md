# PR 2: BitflyerPrivateApi (HMAC-SHA256 REST クライアント)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** bitFlyer Lightning の Private REST API を Rust から安全に呼び出す `BitflyerPrivateApi` ラッパーを新設する。HMAC-SHA256 署名、6 エンドポイント (`sendchildorder` / `getchildorders` / `getexecutions` / `getpositions` / `getcollateral` / `cancelchildorder`)、レート制限 (200 req / 5 min)、エラー分類、wiremock 統合テストまで通す。

**Architecture:** 単一モジュール `crates/market/src/bitflyer_private.rs` に API クライアントを閉じる。署名は pure function として単体テストし、HTTP 呼び出しは reqwest + wiremock で全シナリオを mock。レート制限は `governor` で トークンバケットを張り、超過時は automatic sleep で待機。LiveTrader (PR 3) からはこの型をそのまま `Arc<BitflyerPrivateApi>` で共有する。

**Tech Stack:** Rust workspace edition 2024。`hmac = "0.12"`, `sha2 = "0.10"`, `hex = "0.4"`, `governor = "0.6"`, `reqwest` (既存), `wiremock = "0.6"` (dev)。

**ブランチ:** `feat/bitflyer-private-api` (既に切り替え済み、main から派生)

**参照スペック:** `docs/superpowers/specs/2026-04-14-bitflyer-live-trading-design.md` 5.1 節

---

## 0. スコープ

**本 PR で実装する:**
- PR 1 follow-up 2 件 (先に片付ける):
  - `#[cfg(test)] impl Default for Trade` (test helper、以後の Trade リテラル散在を解消)
  - `OrderType::Limit { price }` の `price > 0` バリデーション
- bitFlyer Private API HTTP クライアント (HMAC-SHA256 + 6 エンドポイント)
- bitFlyer API エラー JSON (`{"status": -205, ...}`) の enum 分類
- レート制限トークンバケット (200 req / 5 min)
- wiremock 統合テスト (署名スナップショット + 各メソッド正常系 + 異常系)
- `.env.example` コメント更新 (既に `BITFLYER_API_KEY` / `BITFLYER_API_SECRET` は定義済み)

**本 PR で実装しない (PR 3 以降):**
- `LiveTrader` (`OrderExecutor` 実装)
- `ExecutionPollingTask`
- `main.rs` の配線
- API キーが存在しないときの起動時バリデーション

---

## File Structure

**新規作成:**
- `crates/market/src/bitflyer_private.rs` — クライアント本体 + 型定義
- `crates/market/tests/bitflyer_private_test.rs` — wiremock 統合テスト

**変更:**
- `Cargo.toml` (workspace) — `hmac`, `sha2`, `hex`, `governor` を `[workspace.dependencies]` に追加
- `crates/market/Cargo.toml` — 上記依存を `[dependencies]` に、`wiremock` を `[dev-dependencies]` に追加
- `crates/market/src/lib.rs` — `pub mod bitflyer_private;` 追加
- `crates/core/src/types.rs` — `OrderType::Limit` の `price > 0` バリデート関数追加 + `#[cfg(any(test, feature = "testing"))] impl Default for Trade`

---

## Task 1: PR 1 follow-up — Trade 用 test helper と OrderType::Limit バリデート

**目的:** PR 1 のコードレビューで挙がった 2 つの follow-up を先に片付ける。PR 2 以降で Trade リテラルに新フィールドが増えたときに全テストファイルを修正する痛みを軽減し、指値注文の価格安全性を型側で担保する。

**Files:**
- Modify: `crates/core/src/types.rs`

- [ ] **Step 1: `OrderType::Limit` 価格バリデートの失敗テストを書く**

`crates/core/src/types.rs` の `mod tests` 末尾に追加:

```rust
    #[test]
    fn order_type_limit_new_accepts_positive_price() {
        let ot = OrderType::limit(dec!(100.5)).unwrap();
        assert!(matches!(ot, OrderType::Limit { price } if price == dec!(100.5)));
    }

    #[test]
    fn order_type_limit_new_rejects_zero() {
        assert!(OrderType::limit(Decimal::ZERO).is_err());
    }

    #[test]
    fn order_type_limit_new_rejects_negative() {
        assert!(OrderType::limit(dec!(-1)).is_err());
    }
```

- [ ] **Step 2: 失敗を確認**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
cargo test -p auto-trader-core order_type_limit_new 2>&1 | tail -10
```

Expected: コンパイルエラー (`no function or associated item named 'limit' found for enum 'OrderType'`)

- [ ] **Step 3: バリデート付きコンストラクタを追加**

`crates/core/src/types.rs` の `impl Default for OrderType {...}` の直後 (= `#[derive(Default)]` を外して手書き Default にする必要はない。`limit()` ファクトリを追加する) に以下を追加:

```rust
impl OrderType {
    /// 指値注文を構築する。price が 0 以下の場合は Err を返し、
    /// 戦略側の計算バグ / 取引所側の異常レスポンスを型境界で弾く。
    /// `unreachable!()` / `todo!()` で済ませない理由は PR 1 Batch A
    /// レビューの FOLLOWUP 参照。
    pub fn limit(price: Decimal) -> Result<Self, InvalidOrderTypeError> {
        if price <= Decimal::ZERO {
            return Err(InvalidOrderTypeError::NonPositiveLimitPrice(price));
        }
        Ok(OrderType::Limit { price })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InvalidOrderTypeError {
    #[error("limit order price must be > 0, got {0}")]
    NonPositiveLimitPrice(Decimal),
}
```

`crates/core/Cargo.toml` に `thiserror` が入っているか確認 (PR 1 で既に入っているはず)。入っていなければ workspace dep として追加。

- [ ] **Step 4: テストをパスさせる**

```bash
cargo test -p auto-trader-core order_type_limit_new 2>&1 | tail -10
```

Expected: `test result: ok. 3 passed`

- [ ] **Step 5: Trade 用 test helper のテストを書く**

`crates/core/src/types.rs` の `mod tests` 末尾に追加:

```rust
    #[test]
    fn trade_default_produces_paper_open_with_none_order_ids() {
        let t = Trade::default();
        assert_eq!(t.mode, TradeMode::Paper);
        assert_eq!(t.status, TradeStatus::Open);
        assert!(t.child_order_acceptance_id.is_none());
        assert!(t.child_order_id.is_none());
        assert_eq!(t.direction, Direction::Long);
        assert_eq!(t.exchange, Exchange::BitflyerCfd);
    }
```

- [ ] **Step 6: 失敗を確認**

```bash
cargo test -p auto-trader-core trade_default 2>&1 | tail -10
```

Expected: コンパイルエラー (`trait 'Default' not implemented for 'Trade'`)

- [ ] **Step 7: `#[cfg(any(test, feature = "testing"))] impl Default for Trade` を追加**

`crates/core/src/types.rs` の `pub struct Trade { ... }` 定義の直後に以下を追加:

```rust
/// テスト専用の Default 実装。
/// 本番コードからは呼ばれないよう `#[cfg(test)]` でガード済み。
/// (PR 2 以降で Trade にフィールドを足しても、戦略 / backtest /
/// paper のテスト固有 Trade リテラルを全書き換えしないで済むよう、
/// ベースライン Trade を用意する。)
#[cfg(any(test, feature = "testing"))]
impl Default for Trade {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            strategy_name: String::from("test_strategy"),
            pair: Pair::new("FX_BTC_JPY"),
            exchange: Exchange::BitflyerCfd,
            direction: Direction::Long,
            entry_price: rust_decimal::Decimal::ZERO,
            exit_price: None,
            stop_loss: rust_decimal::Decimal::ZERO,
            take_profit: rust_decimal::Decimal::ZERO,
            quantity: None,
            leverage: rust_decimal::Decimal::ONE,
            fees: rust_decimal::Decimal::ZERO,
            paper_account_id: None,
            entry_at: chrono::DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            exit_at: None,
            pnl_pips: None,
            pnl_amount: None,
            exit_reason: None,
            mode: TradeMode::Paper,
            status: TradeStatus::Open,
            max_hold_until: None,
            child_order_acceptance_id: None,
            child_order_id: None,
        }
    }
}
```

- [ ] **Step 8: テストをパスさせる**

```bash
cargo test -p auto-trader-core trade_default 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed`

- [ ] **Step 9: ワークスペース全体で既存テストが壊れていないことを確認**

```bash
cargo test --workspace 2>&1 | grep -E 'FAILED|error\[' | head -5
```

Expected: 空 (FAILED / error なし)

- [ ] **Step 10: clippy + fmt**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo fmt --all -- --check 2>&1; echo "FMT EXIT: $?"
```

Expected: clippy 警告なし、FMT EXIT: 0

- [ ] **Step 11: コミット**

```bash
git add crates/core/src/types.rs
git commit -m "$(cat <<'EOF'
feat(core): OrderType::limit() validator + #[cfg(test)] Default for Trade

PR 1 review follow-ups:

- OrderType::limit(price) factory returns InvalidOrderTypeError
  when price <= 0. Strategies can now build Limit orders through
  a validated entry point instead of constructing the enum directly.
  Prevents garbage price from reaching the bitFlyer API in PR 2.
- Trade gains a test-only Default impl (gated by
  cfg(any(test, feature = "testing"))). Future PRs that add new
  Trade fields won't have to touch the 8+ Trade literals scattered
  across strategy / backtest / paper tests — each literal can fall
  back to `..Trade::default()`. Production code cannot see this
  impl so the guard from PR 1 (assert_valid_for_mode) stays intact.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: workspace 依存と `crates/market` crate 依存を追加する

**目的:** 後続タスクで使う `hmac`, `sha2`, `hex`, `governor`, `wiremock` を workspace 単位で宣言し、`crates/market` に実際に取り込む。本タスクはまだ新モジュールを実装しないので、コード上は `pub mod bitflyer_private;` のスタブのみ。

**Files:**
- Modify: `Cargo.toml` (workspace)
- Modify: `crates/market/Cargo.toml`
- Modify: `crates/market/src/lib.rs`
- Create: `crates/market/src/bitflyer_private.rs` (空骨格)

- [ ] **Step 1: workspace の `[workspace.dependencies]` に追加**

`Cargo.toml` の `[workspace.dependencies]` セクションに以下を追記:

```toml
hmac = "0.12"
sha2 = "0.10"
hex = "0.4"
governor = "0.6"
```

`wiremock = "0.6"` は PR 1 で既に登録済みなので触らない。

- [ ] **Step 2: `crates/market/Cargo.toml` に追加**

```toml
[dependencies]
# ... 既存 ...
hmac = { workspace = true }
sha2 = { workspace = true }
hex = { workspace = true }
governor = { workspace = true }

[dev-dependencies]
rust_decimal_macros = "1"
wiremock = { workspace = true }
tokio = { workspace = true }
```

- [ ] **Step 3: 空の `bitflyer_private.rs` を作る**

`crates/market/src/bitflyer_private.rs`:

```rust
//! bitFlyer Lightning Private REST API クライアント。
//!
//! 認証: `ACCESS-SIGN = HMAC-SHA256(secret, timestamp + method + path + body)`
//! レート制限: 200 req / 5 min (IP 単位)。
//!
//! 本モジュールは HTTP 境界までを閉じる。ドメインオブジェクト
//! (Trade, Signal 等) への変換は呼び出し側 (`LiveTrader` in PR 3)
//! が担う。
```

- [ ] **Step 4: lib.rs にモジュールを追加**

`crates/market/src/lib.rs`:

```rust
pub mod bitflyer;
pub mod bitflyer_private;
pub mod candle_builder;
pub mod indicators;
pub mod monitor;
pub mod oanda;
pub mod provider;
```

- [ ] **Step 5: ビルド確認**

```bash
cargo build --workspace 2>&1 | tail -5
```

Expected: 成功

- [ ] **Step 6: コミット**

```bash
git add Cargo.toml crates/market/Cargo.toml crates/market/src/lib.rs crates/market/src/bitflyer_private.rs Cargo.lock
git commit -m "$(cat <<'EOF'
chore(market): add hmac/sha2/hex/governor deps + empty bitflyer_private module

Prepares the crate for the BitflyerPrivateApi client implementation.
No code yet — just module registration so subsequent TDD commits
can layer behavior incrementally without carrying a giant
dependency-addition diff alongside the first test.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: HMAC-SHA256 署名 pure function

**目的:** `ACCESS-SIGN = HMAC-SHA256(secret, timestamp + method + path + body)` を計算する `fn sign(...)` を副作用なしに実装し、bitFlyer 公式サンプルとのスナップショット一致を保証する。

**Files:**
- Modify: `crates/market/src/bitflyer_private.rs`

- [ ] **Step 1: 署名テストを書く**

bitFlyer 公式 API リファレンスで紹介されている HMAC-SHA256 の入出力例をベースにテストを作る。直接一致する公式スナップショットがないため、`hmac` クレートの既知の挙動に基づく固定入出力で自己完結的な snapshot を持つ (HMAC-SHA256 自体は RFC 4231 で定義済み)。

`crates/market/src/bitflyer_private.rs` の末尾に `#[cfg(test)] mod tests { ... }` を追加:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_matches_known_hmac_sha256_vector() {
        // bitFlyer ドキュメント準拠の例:
        //   timestamp = "1234567890"
        //   method    = "GET"
        //   path      = "/v1/me/getpermissions"
        //   body      = ""
        //   secret    = "test_secret"
        // 上記に対する HMAC-SHA256 の hex 出力。Python 等の
        // 他実装で生成した期待値を固定し、rustfmt/rand シードに
        // 依存しない決定的スナップショットとする。
        let sig = sign(
            "test_secret",
            "1234567890",
            "GET",
            "/v1/me/getpermissions",
            "",
        );
        // 期待値は `python3 -c "import hmac,hashlib;
        //   print(hmac.new(b'test_secret',
        //     b'1234567890GET/v1/me/getpermissions',
        //     hashlib.sha256).hexdigest())"` で生成。
        assert_eq!(
            sig,
            "4cc8d1cbf7fe6a1d6a84c9b664d26bde7ecbbf0d5a79dd9ae98c17cb3f9cf4ff"
        );
    }

    #[test]
    fn sign_includes_post_body() {
        let with_body = sign(
            "secret",
            "1000",
            "POST",
            "/v1/me/sendchildorder",
            r#"{"product_code":"FX_BTC_JPY"}"#,
        );
        let without_body = sign("secret", "1000", "POST", "/v1/me/sendchildorder", "");
        assert_ne!(
            with_body, without_body,
            "body must affect the signature"
        );
    }
}
```

**注:** Step 1 の期待値は実装前に Python 等で事前計算しておく。Plan 実行者は以下を実機で実行して得た hex を assert_eq! に差し込むこと:

```bash
python3 -c "
import hmac, hashlib
msg = b'1234567890GET/v1/me/getpermissions'
key = b'test_secret'
print(hmac.new(key, msg, hashlib.sha256).hexdigest())
"
```

Plan 執筆時点で計算した `4cc8d1cbf7fe6a1d6a84c9b664d26bde7ecbbf0d5a79dd9ae98c17cb3f9cf4ff` は参考値。実機出力と一致しなければ実機出力を採用する。

- [ ] **Step 2: 失敗を確認**

```bash
cargo test -p auto-trader-market sign_ 2>&1 | tail -10
```

Expected: コンパイルエラー (`cannot find function 'sign' in this scope`)

- [ ] **Step 3: `sign` を実装**

`crates/market/src/bitflyer_private.rs` の既存 doc コメント直後に追加:

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// bitFlyer Private API の `ACCESS-SIGN` ヘッダを計算する。
///
/// 仕様:
///   ACCESS-SIGN = HMAC-SHA256(api_secret, timestamp + method + path + body)
///
/// - `timestamp`: Unix 秒を 10 進数文字列で表したもの
/// - `method`: 大文字 HTTP メソッド ("GET" / "POST")
/// - `path`: クエリ文字列含むパス (例: "/v1/me/getchildorders?count=100")
/// - `body`: POST 本体 (GET の場合は空文字列 "")
///
/// 返り値は 小文字 16 進数 64 文字。
pub(crate) fn sign(
    api_secret: &str,
    timestamp: &str,
    method: &str,
    path: &str,
    body: &str,
) -> String {
    let mut mac = HmacSha256::new_from_slice(api_secret.as_bytes())
        .expect("HMAC key size is never rejected by Hmac");
    mac.update(timestamp.as_bytes());
    mac.update(method.as_bytes());
    mac.update(path.as_bytes());
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
```

- [ ] **Step 4: テストパス確認**

```bash
cargo test -p auto-trader-market sign_ 2>&1 | tail -10
```

Expected: `test result: ok. 2 passed`

もし snapshot 値がズレていたら:

```bash
python3 -c "
import hmac, hashlib
print(hmac.new(b'test_secret', b'1234567890GET/v1/me/getpermissions', hashlib.sha256).hexdigest())
"
```

で得た hex をテストの `assert_eq!` に更新する。

- [ ] **Step 5: コミット**

```bash
git add crates/market/src/bitflyer_private.rs
git commit -m "$(cat <<'EOF'
feat(market): HMAC-SHA256 sign() for bitFlyer ACCESS-SIGN header

Pure function implementing bitFlyer's spec:
  ACCESS-SIGN = HMAC-SHA256(secret, timestamp + method + path + body)

Two unit tests:
- known-vector snapshot (RFC 4231-compatible output generated from
  Python stdlib hmac, used as reference for regression testing)
- body-affects-signature invariant (POST body must contribute)

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: API リクエスト / レスポンス型とエラー enum

**目的:** HTTP JSON を受けるドメイン型を先に定義する。bitFlyer 公式ドキュメントに出てくる全フィールドは型に載せない (YAGNI) ものの、`Trade` / `Signal` へのマッピングで必要になるものは網羅する。

**Files:**
- Modify: `crates/market/src/bitflyer_private.rs`

- [ ] **Step 1: 型と error enum のデシリアライズテストを書く**

テスト末尾に追加 (`mod tests` 内):

```rust
    use rust_decimal_macros::dec;

    #[test]
    fn deserialize_send_child_order_response() {
        let json = r#"{"child_order_acceptance_id":"JRF20150707-050237-639234"}"#;
        let r: SendChildOrderResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.child_order_acceptance_id, "JRF20150707-050237-639234");
    }

    #[test]
    fn deserialize_child_order() {
        let json = r#"{
            "id": 138398,
            "child_order_id": "JOR20150707-084555-022523",
            "product_code": "BTC_JPY",
            "side": "BUY",
            "child_order_type": "LIMIT",
            "price": "30000",
            "average_price": "30000",
            "size": "0.1",
            "child_order_state": "COMPLETED",
            "expire_date": "2015-07-14T07:25:47",
            "child_order_date": "2015-07-07T08:45:47",
            "child_order_acceptance_id": "JRF20150707-084555-022523",
            "outstanding_size": "0",
            "cancel_size": "0",
            "executed_size": "0.1",
            "total_commission": "0"
        }"#;
        let o: ChildOrder = serde_json::from_str(json).unwrap();
        assert_eq!(o.child_order_id, "JOR20150707-084555-022523");
        assert_eq!(o.side, "BUY");
        assert_eq!(o.child_order_state, ChildOrderState::Completed);
        assert_eq!(o.size, dec!(0.1));
    }

    #[test]
    fn deserialize_execution() {
        let json = r#"{
            "id": 37233,
            "child_order_id": "JOR20150707-084555-022523",
            "side": "BUY",
            "price": "30000",
            "size": "0.1",
            "commission": "0",
            "exec_date": "2015-07-07T09:57:40.397",
            "child_order_acceptance_id": "JRF20150707-084555-022523"
        }"#;
        let e: Execution = serde_json::from_str(json).unwrap();
        assert_eq!(e.id, 37233);
        assert_eq!(e.size, dec!(0.1));
    }

    #[test]
    fn deserialize_collateral() {
        let json = r#"{
            "collateral": "100000",
            "open_position_pnl": "-715",
            "require_collateral": "19857",
            "keep_rate": "5.0"
        }"#;
        let c: Collateral = serde_json::from_str(json).unwrap();
        assert_eq!(c.collateral, dec!(100000));
        assert_eq!(c.open_position_pnl, dec!(-715));
    }

    #[test]
    fn deserialize_exchange_position() {
        let json = r#"{
            "product_code": "FX_BTC_JPY",
            "side": "BUY",
            "price": "36000",
            "size": "10",
            "commission": "0",
            "swap_point_accumulate": "-35",
            "require_collateral": "120000",
            "open_date": "2015-11-03T10:04:45.011",
            "leverage": "3",
            "pnl": "965",
            "sfd": "0"
        }"#;
        let p: ExchangePosition = serde_json::from_str(json).unwrap();
        assert_eq!(p.product_code, "FX_BTC_JPY");
        assert_eq!(p.size, dec!(10));
    }

    #[test]
    fn deserialize_bitflyer_api_error() {
        let json = r#"{"status": -205, "error_message": "Insufficient fund", "data": null}"#;
        let e: BitflyerErrorBody = serde_json::from_str(json).unwrap();
        assert_eq!(e.status, -205);
        assert_eq!(e.error_message, "Insufficient fund");
    }
```

- [ ] **Step 2: 失敗を確認**

```bash
cargo test -p auto-trader-market bitflyer_private 2>&1 | tail -15
```

Expected: 複数の `cannot find type` エラー

- [ ] **Step 3: 型を実装**

`crates/market/src/bitflyer_private.rs` の `sign()` 関数の直前に追加:

```rust
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// `POST /v1/me/sendchildorder` リクエスト本体。
///
/// `time_in_force` / `minute_to_expire` はデフォルトのまま取引所任せに
/// したいケースが多いので `Option`。PR 3 の LiveTrader は常に
/// `time_in_force = None` (GTC 相当) で送る想定。
#[derive(Debug, Clone, Serialize)]
pub struct SendChildOrderRequest {
    pub product_code: String,
    pub child_order_type: ChildOrderType,
    pub side: Side,
    pub size: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minute_to_expire: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<TimeInForce>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChildOrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendChildOrderResponse {
    pub child_order_acceptance_id: String,
}

/// `GET /v1/me/getchildorders` 個別要素。
///
/// bitFlyer レスポンスはすべて数値を文字列で返すため、
/// `rust_decimal` の `serde-with-str` feature (workspace 既定) で
/// 文字列 → Decimal を直接パースする。
#[derive(Debug, Clone, Deserialize)]
pub struct ChildOrder {
    pub id: u64,
    pub child_order_id: String,
    pub product_code: String,
    pub side: String,
    pub child_order_type: String,
    pub price: Decimal,
    pub average_price: Decimal,
    pub size: Decimal,
    pub child_order_state: ChildOrderState,
    pub expire_date: String,
    pub child_order_date: String,
    pub child_order_acceptance_id: String,
    pub outstanding_size: Decimal,
    pub cancel_size: Decimal,
    pub executed_size: Decimal,
    pub total_commission: Decimal,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChildOrderState {
    Active,
    Completed,
    Canceled,
    Expired,
    Rejected,
}

/// `GET /v1/me/getexecutions` 個別要素。
#[derive(Debug, Clone, Deserialize)]
pub struct Execution {
    pub id: u64,
    pub child_order_id: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub commission: Decimal,
    pub exec_date: String,
    pub child_order_acceptance_id: String,
}

/// `GET /v1/me/getpositions` 個別要素 (FX/CFD 専用)。
#[derive(Debug, Clone, Deserialize)]
pub struct ExchangePosition {
    pub product_code: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub commission: Decimal,
    pub swap_point_accumulate: Decimal,
    pub require_collateral: Decimal,
    pub open_date: String,
    pub leverage: Decimal,
    pub pnl: Decimal,
    pub sfd: Decimal,
}

/// `GET /v1/me/getcollateral` レスポンス。
#[derive(Debug, Clone, Deserialize)]
pub struct Collateral {
    pub collateral: Decimal,
    pub open_position_pnl: Decimal,
    pub require_collateral: Decimal,
    pub keep_rate: Decimal,
}

/// bitFlyer がエラーレスポンスで返す JSON の raw 形。
/// `status` が負数で返り、`error_message` が日本語/英語の
/// 人間向け説明。`data` は null または詳細オブジェクト。
///
/// 後段の `BitflyerApiError` で分類する。
#[derive(Debug, Clone, Deserialize)]
pub struct BitflyerErrorBody {
    pub status: i32,
    pub error_message: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

/// bitFlyer Private API が返しうる失敗の分類。
///
/// - `InsufficientFunds`: 残高不足。LiveTrader はアカウントを halt 対象に。
/// - `InvalidApiKey` / `InvalidSignature`: 認証失敗。起動時に発火すれば fatal。
/// - `RateLimited`: HTTP 429。governor の前段ガードを通過してしまった場合
///   (= 他プロセスや bitFlyer 側の偏り) の防衛線。
/// - `OrderNotFound`: 存在しない注文 ID。reconciler で手動対処。
/// - `ApiError { status, message }`: 上記に当てはまらない bitFlyer エラー。
/// - `Http`: reqwest 層のエラー。必ず `without_url()` で URL を落とす
///   (PR 1 notify crate と同じ方針)。
/// - `InvalidResponse`: HTTP は成功したが JSON パース失敗。
#[derive(Debug, Error)]
pub enum BitflyerApiError {
    #[error("insufficient funds: {0}")]
    InsufficientFunds(String),
    #[error("invalid api key")]
    InvalidApiKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("rate limited")]
    RateLimited,
    #[error("order not found: {0}")]
    OrderNotFound(String),
    #[error("bitflyer api error: status={status} message={message}")]
    ApiError { status: i32, message: String },
    #[error("http error: {0}")]
    Http(reqwest::Error),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

impl From<reqwest::Error> for BitflyerApiError {
    fn from(e: reqwest::Error) -> Self {
        // Slack Webhook 同様、reqwest::Error の Display は URL を含むため
        // without_url() で落とす。api_secret/api_key 自体は header に入る
        // ため URL には載らないが、将来 query string に何らかの識別子を
        // 入れたとき防衛線になる。
        BitflyerApiError::Http(e.without_url())
    }
}

impl BitflyerApiError {
    /// bitFlyer の raw error body から型付きエラーへ分類する。
    pub fn from_body(body: BitflyerErrorBody) -> Self {
        match body.status {
            -200 | -205 => BitflyerApiError::InsufficientFunds(body.error_message),
            -201 => BitflyerApiError::InvalidApiKey,
            -207 => BitflyerApiError::InvalidSignature,
            -208 => BitflyerApiError::OrderNotFound(body.error_message),
            s => BitflyerApiError::ApiError {
                status: s,
                message: body.error_message,
            },
        }
    }
}
```

`sign()` に付いていた `pub(crate)` は変更しない。

**重要:** `chrono::{DateTime, Utc}` import は Execution/ChildOrder の `exec_date` / `child_order_date` を `String` のまま扱う場合は不要。ただし PR 3 で parse するので当面 String 保持で OK。Task 4 では import を削除する。

- [ ] **Step 4: chrono / DateTime import を削除 (未使用)**

`use chrono::{DateTime, Utc};` を削除。

- [ ] **Step 5: テストパス確認**

```bash
cargo test -p auto-trader-market bitflyer_private 2>&1 | tail -20
```

Expected: 全パス (sign テスト 2 本 + 型 6 本 = 8 本)

もし `rust_decimal` の serde パーサが文字列受付をしないエラーが出たら、workspace の `rust_decimal = {..., features = ["serde-with-str"]}` になっているか確認 (PR 1 で既定 ON のはず)。

- [ ] **Step 6: clippy + fmt**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo fmt --all -- --check; echo "FMT EXIT: $?"
```

Expected: 両方パス。`thiserror::Error` の未使用 import 等があれば整理。

- [ ] **Step 7: コミット**

```bash
git add crates/market/src/bitflyer_private.rs
git commit -m "$(cat <<'EOF'
feat(market): request/response types + BitflyerApiError classifier

Type layer for the bitFlyer Private REST API — pure data shapes
with serde derives, no HTTP yet.

- SendChildOrderRequest / Response
- ChildOrder / ChildOrderState (GET /v1/me/getchildorders)
- Execution (GET /v1/me/getexecutions)
- ExchangePosition (GET /v1/me/getpositions)
- Collateral (GET /v1/me/getcollateral)
- BitflyerErrorBody + BitflyerApiError (status → typed variant)
- From<reqwest::Error> redacts URL via without_url() (same pattern
  as PR 1 notify crate, prevents future API key leaks via Display)

Six deserialization tests cover the documented payload shapes and
error classification.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `BitflyerPrivateApi` 構造体と認証ヘッダ付き HTTP リクエスト関数

**目的:** クライアント本体。6 メソッドの共通インフラ (auth ヘッダ生成、リクエスト送信、レスポンス JSON パース、エラー分類) をここで固める。レート制限は Task 7 で別途。

**Files:**
- Modify: `crates/market/src/bitflyer_private.rs`

- [ ] **Step 1: 認証ヘッダ生成関数のテストを書く**

`mod tests` に追加:

```rust
    #[test]
    fn auth_headers_contain_key_timestamp_and_sign() {
        let api = BitflyerPrivateApi::new_for_test(
            "http://example.invalid".to_string(),
            "test-key".to_string(),
            "test-secret".to_string(),
        );
        let headers = api.auth_headers("1234567890", "GET", "/v1/me/getcollateral", "");
        assert_eq!(headers.get("ACCESS-KEY").unwrap(), "test-key");
        assert_eq!(headers.get("ACCESS-TIMESTAMP").unwrap(), "1234567890");
        // signature は Task 3 の sign() と一致するはず
        let expected =
            sign("test-secret", "1234567890", "GET", "/v1/me/getcollateral", "");
        assert_eq!(headers.get("ACCESS-SIGN").unwrap(), &expected);
    }
```

- [ ] **Step 2: 失敗を確認**

```bash
cargo test -p auto-trader-market auth_headers 2>&1 | tail -10
```

Expected: コンパイルエラー (`cannot find struct 'BitflyerPrivateApi'`)

- [ ] **Step 3: `BitflyerPrivateApi` を実装**

`crates/market/src/bitflyer_private.rs` の `BitflyerApiError::from_body(...)` 直後に追加:

```rust
use std::collections::HashMap;

/// bitFlyer Private REST API クライアント。
///
/// コンストラクタは `new` (本番) と `new_for_test` (wiremock / 単体
/// テスト) を分離し、テストが本番 URL を誤って叩かないよう型で
/// ガードする。
#[derive(Clone)]
pub struct BitflyerPrivateApi {
    base_url: String,
    api_key: String,
    api_secret: String,
    http: reqwest::Client,
}

impl BitflyerPrivateApi {
    /// 本番用コンストラクタ。`base_url` は "https://api.bitflyer.com" 固定想定。
    pub fn new(api_key: String, api_secret: String) -> Self {
        Self::with_base_url(
            "https://api.bitflyer.com".to_string(),
            api_key,
            api_secret,
        )
    }

    /// ベース URL を明示するコンストラクタ。テスト・本番切り替え用。
    pub fn with_base_url(base_url: String, api_key: String, api_secret: String) -> Self {
        Self {
            base_url,
            api_key,
            api_secret,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client builder should not fail with basic config"),
        }
    }

    /// wiremock テスト専用のコンストラクタ。`#[cfg(test)]` ではなく
    /// `new_for_test` という名前で区別することで統合テスト (別 crate
    /// 境界をまたぐ `crates/market/tests/*.rs`) からも呼べるようにする。
    #[doc(hidden)]
    pub fn new_for_test(base_url: String, api_key: String, api_secret: String) -> Self {
        Self::with_base_url(base_url, api_key, api_secret)
    }

    /// 認証ヘッダ 3 本を生成する pure function (テスト可能)。
    pub(crate) fn auth_headers(
        &self,
        timestamp: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> HashMap<&'static str, String> {
        let sig = sign(&self.api_secret, timestamp, method, path, body);
        let mut h = HashMap::new();
        h.insert("ACCESS-KEY", self.api_key.clone());
        h.insert("ACCESS-TIMESTAMP", timestamp.to_string());
        h.insert("ACCESS-SIGN", sig);
        h
    }

    /// 現在時刻の Unix 秒を 10 進文字列で返す。テストで時刻を固定
    /// したい場合は呼び出し側で auth_headers を直接叩く。
    fn current_timestamp() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before 1970");
        now.as_secs().to_string()
    }

    /// 共通 HTTP リクエストラッパー。
    ///
    /// - `method`: "GET" / "POST"
    /// - `path`: 例 "/v1/me/getcollateral" (クエリ文字列を含むこと)
    /// - `body_json`: POST 本体 JSON 文字列 (GET は "")
    ///
    /// 成功時は bitFlyer の raw レスポンスを (2xx, body_string) で返す。
    /// HTTP ステータスが 2xx でも JSON body に `status: <負数>` が
    /// 入っていれば `BitflyerApiError::from_body` で分類する。
    async fn request(
        &self,
        method: &str,
        path: &str,
        body_json: &str,
    ) -> Result<String, BitflyerApiError> {
        let url = format!("{}{}", self.base_url, path);
        let ts = Self::current_timestamp();
        let headers = self.auth_headers(&ts, method, path, body_json);

        let mut req = match method {
            "GET" => self.http.get(&url),
            "POST" => self.http.post(&url),
            _ => {
                return Err(BitflyerApiError::InvalidResponse(format!(
                    "unsupported method: {method}"
                )));
            }
        };
        for (k, v) in &headers {
            req = req.header(*k, v);
        }
        if method == "POST" {
            req = req
                .header("Content-Type", "application/json")
                .body(body_json.to_string());
        }

        let resp = req.send().await?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| BitflyerApiError::Http(e.without_url()))?;

        if status.as_u16() == 429 {
            return Err(BitflyerApiError::RateLimited);
        }
        if !status.is_success() {
            // 非 2xx レスポンスは body に BitflyerErrorBody が載っている
            // ことが多い。パース失敗したら InvalidResponse に fallback。
            return match serde_json::from_str::<BitflyerErrorBody>(&text) {
                Ok(body) => Err(BitflyerApiError::from_body(body)),
                Err(_) => Err(BitflyerApiError::InvalidResponse(format!(
                    "non-2xx status {} body {}",
                    status.as_u16(),
                    text
                ))),
            };
        }

        // 2xx でも bitFlyer は `{"status":-200,...}` を返すことがある。
        // status フィールドを覗いて負数なら error として扱う。
        if let Ok(body) = serde_json::from_str::<BitflyerErrorBody>(&text) {
            if body.status < 0 {
                return Err(BitflyerApiError::from_body(body));
            }
        }

        Ok(text)
    }
}
```

- [ ] **Step 4: テストパス確認**

```bash
cargo test -p auto-trader-market auth_headers 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed`

- [ ] **Step 5: 既存の全テストが通ることを確認**

```bash
cargo test -p auto-trader-market 2>&1 | grep -E 'test result|FAILED'
```

Expected: 全パス

- [ ] **Step 6: clippy + fmt**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo fmt --all -- --check; echo "FMT EXIT: $?"
```

Expected: 両方 OK

- [ ] **Step 7: コミット**

```bash
git add crates/market/src/bitflyer_private.rs
git commit -m "$(cat <<'EOF'
feat(market): BitflyerPrivateApi core + auth header generation

The shared HTTP plumbing every endpoint method in the next commits
will use:

- new() for production, with_base_url() / new_for_test() for tests
- auth_headers() builds ACCESS-KEY / ACCESS-TIMESTAMP / ACCESS-SIGN
  (unit-tested against sign())
- request() dispatches GET/POST with signed headers, classifies
  2xx-with-negative-status as BitflyerApiError::from_body, maps 429
  to RateLimited, and surfaces unparseable non-2xx as InvalidResponse
- reqwest::Error is routed through the From<> impl from Task 4, so
  URL secrets (future query-string identifiers) cannot leak via ?

No endpoint methods yet — next commits layer send_child_order,
get_child_orders, etc. on top of this foundation.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `send_child_order` + wiremock 統合テスト

**目的:** 最初の API メソッド。成功時のレスポンスパース、403/401 認証エラー、-205 残高不足エラーの 3 ケースを wiremock でカバーする。

**Files:**
- Modify: `crates/market/src/bitflyer_private.rs`
- Create: `crates/market/tests/bitflyer_private_test.rs`

- [ ] **Step 1: `send_child_order` 成功テストを書く (統合テスト)**

`crates/market/tests/bitflyer_private_test.rs` を新規作成:

```rust
//! bitFlyer Private API の統合テスト (wiremock 使用)。
//!
//! `crates/market/src/bitflyer_private.rs` の単体テストでは sign() や
//! 型 deserialize を検証済み。ここでは HTTP 境界全体 (認証ヘッダ送信、
//! body シリアライズ、レスポンス分類) を bitFlyer ドキュメントに即した
//! ペイロードで確認する。

use auto_trader_market::bitflyer_private::{
    BitflyerApiError, BitflyerPrivateApi, ChildOrderType, SendChildOrderRequest, Side,
};
use rust_decimal_macros::dec;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client_for(server: &MockServer) -> BitflyerPrivateApi {
    BitflyerPrivateApi::new_for_test(
        server.uri(),
        "test-key".to_string(),
        "test-secret".to_string(),
    )
}

#[tokio::test]
async fn send_child_order_market_order_returns_acceptance_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/sendchildorder"))
        .and(header_exists("ACCESS-KEY"))
        .and(header_exists("ACCESS-TIMESTAMP"))
        .and(header_exists("ACCESS-SIGN"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"child_order_acceptance_id":"JRF20260414-050237-639234"}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let req = SendChildOrderRequest {
        product_code: "FX_BTC_JPY".to_string(),
        child_order_type: ChildOrderType::Market,
        side: Side::Buy,
        size: dec!(0.01),
        price: None,
        minute_to_expire: None,
        time_in_force: None,
    };
    let resp = api.send_child_order(req).await.unwrap();
    assert_eq!(resp.child_order_acceptance_id, "JRF20260414-050237-639234");
}

#[tokio::test]
async fn send_child_order_insufficient_funds_maps_to_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/sendchildorder"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                r#"{"status":-205,"error_message":"Insufficient fund","data":null}"#,
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let req = SendChildOrderRequest {
        product_code: "FX_BTC_JPY".to_string(),
        child_order_type: ChildOrderType::Market,
        side: Side::Buy,
        size: dec!(10),
        price: None,
        minute_to_expire: None,
        time_in_force: None,
    };
    let err = api.send_child_order(req).await.unwrap_err();
    match err {
        BitflyerApiError::InsufficientFunds(msg) => {
            assert!(msg.contains("Insufficient"), "unexpected msg: {msg}");
        }
        other => panic!("expected InsufficientFunds, got {other:?}"),
    }
}

#[tokio::test]
async fn send_child_order_invalid_api_key_maps_to_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/sendchildorder"))
        .respond_with(ResponseTemplate::new(401).set_body_string(
            r#"{"status":-201,"error_message":"Invalid API key","data":null}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let req = SendChildOrderRequest {
        product_code: "FX_BTC_JPY".to_string(),
        child_order_type: ChildOrderType::Market,
        side: Side::Buy,
        size: dec!(0.01),
        price: None,
        minute_to_expire: None,
        time_in_force: None,
    };
    let err = api.send_child_order(req).await.unwrap_err();
    assert!(matches!(err, BitflyerApiError::InvalidApiKey));
}
```

- [ ] **Step 2: 失敗を確認**

```bash
cargo test -p auto-trader-market --test bitflyer_private_test 2>&1 | tail -10
```

Expected: コンパイルエラー (`no method named 'send_child_order'`)

- [ ] **Step 3: `send_child_order` を実装**

`crates/market/src/bitflyer_private.rs` の `impl BitflyerPrivateApi { ... }` 末尾の `}` 直前 (= `request()` 関数の直後) に追加:

```rust
    /// `POST /v1/me/sendchildorder` — 成行/指値注文を発行する。
    pub async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> Result<SendChildOrderResponse, BitflyerApiError> {
        let body = serde_json::to_string(&req)
            .map_err(|e| BitflyerApiError::InvalidResponse(format!("serialize: {e}")))?;
        let text = self
            .request("POST", "/v1/me/sendchildorder", &body)
            .await?;
        serde_json::from_str(&text)
            .map_err(|e| BitflyerApiError::InvalidResponse(format!("parse: {e}: {text}")))
    }
```

- [ ] **Step 4: テストパス確認**

```bash
cargo test -p auto-trader-market --test bitflyer_private_test 2>&1 | tail -15
```

Expected: 3 テスト全パス

- [ ] **Step 5: clippy + fmt**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo fmt --all -- --check; echo "FMT EXIT: $?"
```

Expected: OK

- [ ] **Step 6: コミット**

```bash
git add crates/market/src/bitflyer_private.rs crates/market/tests/bitflyer_private_test.rs
git commit -m "$(cat <<'EOF'
feat(market): send_child_order + wiremock coverage

First bitFlyer Private endpoint method. Three scenarios covered
by wiremock integration tests:

- happy path: POST body and signed headers reach the server, 200
  with child_order_acceptance_id parses cleanly
- insufficient funds (status=-205 in 200 body): maps to
  BitflyerApiError::InsufficientFunds
- invalid api key (401 + status=-201): maps to InvalidApiKey

The assertion on header_exists() guards against accidentally
dropping authentication later.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: `get_child_orders` + `get_executions` + wiremock

**目的:** 注文状態 / 約定照会を追加。配列レスポンスのパース、空配列、order_id クエリ文字列の組み立てを検証。

**Files:**
- Modify: `crates/market/src/bitflyer_private.rs`
- Modify: `crates/market/tests/bitflyer_private_test.rs`

- [ ] **Step 1: 統合テストを追加**

`crates/market/tests/bitflyer_private_test.rs` 末尾に:

```rust
use wiremock::matchers::query_param;

#[tokio::test]
async fn get_child_orders_returns_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getchildorders"))
        .and(query_param("product_code", "FX_BTC_JPY"))
        .and(query_param(
            "child_order_acceptance_id",
            "JRF20260414-050237-639234",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{
                "id": 1,
                "child_order_id": "JOR20260414-050237-639234",
                "product_code": "FX_BTC_JPY",
                "side": "BUY",
                "child_order_type": "MARKET",
                "price": "0",
                "average_price": "11500000",
                "size": "0.01",
                "child_order_state": "COMPLETED",
                "expire_date": "2026-05-14T07:25:47",
                "child_order_date": "2026-04-14T08:45:47",
                "child_order_acceptance_id": "JRF20260414-050237-639234",
                "outstanding_size": "0",
                "cancel_size": "0",
                "executed_size": "0.01",
                "total_commission": "0"
            }]"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let orders = api
        .get_child_orders("FX_BTC_JPY", "JRF20260414-050237-639234")
        .await
        .unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].child_order_id, "JOR20260414-050237-639234");
    assert_eq!(orders[0].executed_size, dec!(0.01));
}

#[tokio::test]
async fn get_child_orders_empty_list_is_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getchildorders"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let orders = api
        .get_child_orders("FX_BTC_JPY", "unknown_id")
        .await
        .unwrap();
    assert!(orders.is_empty());
}

#[tokio::test]
async fn get_executions_returns_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getexecutions"))
        .and(query_param("product_code", "FX_BTC_JPY"))
        .and(query_param(
            "child_order_acceptance_id",
            "JRF20260414-050237-639234",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{
                "id": 99,
                "child_order_id": "JOR20260414-050237-639234",
                "side": "BUY",
                "price": "11500000",
                "size": "0.01",
                "commission": "0",
                "exec_date": "2026-04-14T09:57:40.397",
                "child_order_acceptance_id": "JRF20260414-050237-639234"
            }]"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let execs = api
        .get_executions("FX_BTC_JPY", "JRF20260414-050237-639234")
        .await
        .unwrap();
    assert_eq!(execs.len(), 1);
    assert_eq!(execs[0].price, dec!(11500000));
}
```

- [ ] **Step 2: 失敗を確認**

```bash
cargo test -p auto-trader-market --test bitflyer_private_test get_ 2>&1 | tail -10
```

Expected: `no method named 'get_child_orders'` 等

- [ ] **Step 3: メソッドを実装**

`crates/market/src/bitflyer_private.rs` の `send_child_order` の直後に追加:

```rust
    /// `GET /v1/me/getchildorders` — `child_order_acceptance_id` で特定の
    /// 注文 (とその状態) を取得する。bitFlyer は複数件返すが、acceptance_id
    /// で絞ると通常 1 件。
    pub async fn get_child_orders(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> Result<Vec<ChildOrder>, BitflyerApiError> {
        let path = format!(
            "/v1/me/getchildorders?product_code={}&child_order_acceptance_id={}",
            product_code, child_order_acceptance_id
        );
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text)
            .map_err(|e| BitflyerApiError::InvalidResponse(format!("parse: {e}: {text}")))
    }

    /// `GET /v1/me/getexecutions` — 約定一覧を取得する。
    pub async fn get_executions(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> Result<Vec<Execution>, BitflyerApiError> {
        let path = format!(
            "/v1/me/getexecutions?product_code={}&child_order_acceptance_id={}",
            product_code, child_order_acceptance_id
        );
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text)
            .map_err(|e| BitflyerApiError::InvalidResponse(format!("parse: {e}: {text}")))
    }
```

- [ ] **Step 4: テスト確認**

```bash
cargo test -p auto-trader-market --test bitflyer_private_test 2>&1 | tail -15
```

Expected: 6 テスト (Task 6 の 3 + Task 7 の 3) 全パス

- [ ] **Step 5: clippy + fmt**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo fmt --all -- --check; echo "FMT EXIT: $?"
```

- [ ] **Step 6: コミット**

```bash
git add crates/market/src/bitflyer_private.rs crates/market/tests/bitflyer_private_test.rs
git commit -m "$(cat <<'EOF'
feat(market): get_child_orders + get_executions + wiremock coverage

Two read endpoints needed by the ExecutionPollingTask (PR 3):

- get_child_orders(product, acceptance_id): order status + fill
  progression. Happy path + empty-list both tested to ensure the
  polling loop handles "order not yet placed" transparently.
- get_executions(product, acceptance_id): fill details for
  computing pnl against each partial execution.

Query-string assembly is pinned by query_param() matchers so the
next refactor can't silently drop a filter.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `get_positions` + `get_collateral` + `cancel_child_order` + wiremock

**目的:** 残り 3 メソッドを一括で追加する。それぞれ小さいのでまとめる。

**Files:**
- Modify: `crates/market/src/bitflyer_private.rs`
- Modify: `crates/market/tests/bitflyer_private_test.rs`

- [ ] **Step 1: 統合テストを追加**

`crates/market/tests/bitflyer_private_test.rs` 末尾:

```rust
#[tokio::test]
async fn get_positions_returns_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getpositions"))
        .and(query_param("product_code", "FX_BTC_JPY"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{
                "product_code": "FX_BTC_JPY",
                "side": "BUY",
                "price": "11500000",
                "size": "0.01",
                "commission": "0",
                "swap_point_accumulate": "0",
                "require_collateral": "57500",
                "open_date": "2026-04-14T10:04:45.011",
                "leverage": "2",
                "pnl": "0",
                "sfd": "0"
            }]"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let positions = api.get_positions("FX_BTC_JPY").await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].size, dec!(0.01));
    assert_eq!(positions[0].product_code, "FX_BTC_JPY");
}

#[tokio::test]
async fn get_collateral_returns_struct() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getcollateral"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{
                "collateral": "30000",
                "open_position_pnl": "-123",
                "require_collateral": "15000",
                "keep_rate": "2.0"
            }"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let c = api.get_collateral().await.unwrap();
    assert_eq!(c.collateral, dec!(30000));
    assert_eq!(c.open_position_pnl, dec!(-123));
}

#[tokio::test]
async fn cancel_child_order_returns_unit_on_200() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/cancelchildorder"))
        .and(header_exists("ACCESS-SIGN"))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    api.cancel_child_order("FX_BTC_JPY", "JRF20260414-050237-639234")
        .await
        .unwrap();
}

#[tokio::test]
async fn cancel_child_order_unknown_id_maps_to_order_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/cancelchildorder"))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            r#"{"status":-208,"error_message":"Order not found","data":null}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let err = api
        .cancel_child_order("FX_BTC_JPY", "bogus")
        .await
        .unwrap_err();
    assert!(matches!(err, BitflyerApiError::OrderNotFound(_)));
}
```

- [ ] **Step 2: 失敗を確認**

```bash
cargo test -p auto-trader-market --test bitflyer_private_test 2>&1 | tail -15
```

Expected: コンパイルエラー (`no method named 'get_positions'` 等)

- [ ] **Step 3: メソッド 3 本を実装**

`crates/market/src/bitflyer_private.rs` の `get_executions` の直後に追加:

```rust
    /// `GET /v1/me/getpositions` — 保有建玉一覧 (FX/CFD 専用)。
    pub async fn get_positions(
        &self,
        product_code: &str,
    ) -> Result<Vec<ExchangePosition>, BitflyerApiError> {
        let path = format!("/v1/me/getpositions?product_code={}", product_code);
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text)
            .map_err(|e| BitflyerApiError::InvalidResponse(format!("parse: {e}: {text}")))
    }

    /// `GET /v1/me/getcollateral` — 証拠金の現在状態。
    pub async fn get_collateral(&self) -> Result<Collateral, BitflyerApiError> {
        let text = self.request("GET", "/v1/me/getcollateral", "").await?;
        serde_json::from_str(&text)
            .map_err(|e| BitflyerApiError::InvalidResponse(format!("parse: {e}: {text}")))
    }

    /// `POST /v1/me/cancelchildorder` — 未約定注文をキャンセルする。
    /// 成功時は 2xx 空 body が返るため、型上は `()` を返す。
    pub async fn cancel_child_order(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> Result<(), BitflyerApiError> {
        #[derive(Serialize)]
        struct CancelRequest<'a> {
            product_code: &'a str,
            child_order_acceptance_id: &'a str,
        }
        let body = serde_json::to_string(&CancelRequest {
            product_code,
            child_order_acceptance_id,
        })
        .map_err(|e| BitflyerApiError::InvalidResponse(format!("serialize: {e}")))?;
        let _ = self
            .request("POST", "/v1/me/cancelchildorder", &body)
            .await?;
        Ok(())
    }
```

- [ ] **Step 4: テスト確認**

```bash
cargo test -p auto-trader-market --test bitflyer_private_test 2>&1 | tail -15
```

Expected: 10 テスト (Task 6-8 分) 全パス

- [ ] **Step 5: clippy + fmt**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo fmt --all -- --check; echo "FMT EXIT: $?"
```

- [ ] **Step 6: コミット**

```bash
git add crates/market/src/bitflyer_private.rs crates/market/tests/bitflyer_private_test.rs
git commit -m "$(cat <<'EOF'
feat(market): get_positions + get_collateral + cancel_child_order

The final three endpoints:

- get_positions(product_code): FX/CFD open positions, used by
  ReconcilerTask to diff against DB state in PR 5
- get_collateral(): account balance / pnl snapshot, used by
  BalanceSyncTask in PR 5
- cancel_child_order(product, acceptance_id): emergency-stop
  path; 404 + status=-208 maps to OrderNotFound so the reconciler
  can distinguish "already gone" from a real failure

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: レート制限 (governor)

**目的:** Private API の 200 req / 5 min 制限をクライアント側で事前に守る。`governor` のトークンバケットで超過時は `await` で待機させ、bitFlyer 側の 429 を誘発しないようにする。

**Files:**
- Modify: `crates/market/src/bitflyer_private.rs`
- Modify: `crates/market/tests/bitflyer_private_test.rs`

- [ ] **Step 1: governor の利用パターンを確認**

```bash
cargo doc -p governor --no-deps 2>&1 | tail -3
```

確認ポイント:
- `governor::RateLimiter::direct(Quota::per_minute(NonZeroU32::new(40).unwrap()))` で毎分 40 req のバケット
- `limiter.until_ready().await` で待機
- `Arc<RateLimiter>` は `Clone` 可能 (内部 `Arc`)

- [ ] **Step 2: テストを追加 (待機検証)**

レート制限は実時間に影響するので、短いバケットを専用テスト用に構築して確認する。`crates/market/tests/bitflyer_private_test.rs` 末尾:

```rust
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::test]
async fn rate_limit_waits_when_bucket_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getcollateral"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"collateral":"30000","open_position_pnl":"0","require_collateral":"0","keep_rate":"0"}"#,
        ))
        .mount(&server)
        .await;

    // 1 秒に 2 件しか許さない極小バケット。3 件目は必ず待機が発生する。
    let limiter = Arc::new(governor::RateLimiter::direct(governor::Quota::per_second(
        NonZeroU32::new(2).unwrap(),
    )));
    let api = BitflyerPrivateApi::new_for_test(server.uri(), "k".into(), "s".into())
        .with_rate_limiter(limiter);

    let start = Instant::now();
    api.get_collateral().await.unwrap();
    api.get_collateral().await.unwrap();
    api.get_collateral().await.unwrap();
    let elapsed = start.elapsed();

    // 3 件目は少なくとも ~500ms 待たされる (バケット 2/sec → 1 件 = 500ms)。
    // タイミング系テストは flake しやすいので、下限 400ms だけ確認する。
    assert!(
        elapsed >= Duration::from_millis(400),
        "rate limit did not apply; elapsed={elapsed:?}"
    );
}
```

- [ ] **Step 3: 失敗を確認**

```bash
cargo test -p auto-trader-market --test bitflyer_private_test rate_limit 2>&1 | tail -10
```

Expected: `no method named 'with_rate_limiter'`

- [ ] **Step 4: `with_rate_limiter` と `request()` に acquire を組み込む**

`crates/market/src/bitflyer_private.rs` の既存 `use` セクション (先頭付近) に:

```rust
use std::sync::Arc;

type RateLimiter = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;
```

`BitflyerPrivateApi` 構造体に `rate_limiter` フィールドを追加:

```rust
#[derive(Clone)]
pub struct BitflyerPrivateApi {
    base_url: String,
    api_key: String,
    api_secret: String,
    http: reqwest::Client,
    rate_limiter: Option<Arc<RateLimiter>>,
}
```

既存の `new`, `with_base_url`, `new_for_test` を更新して `rate_limiter: None` を入れる (本番の `new` だけはデフォルトで 200 req / 5 min のバケットを張る):

```rust
    pub fn new(api_key: String, api_secret: String) -> Self {
        // bitFlyer Private API は IP 単位で 200 req / 5 min (= 40 req/min)。
        // 安全側に 30 req/min としてバケットを張り、バースト 10 を許可する。
        let limiter = Arc::new(governor::RateLimiter::direct(
            governor::Quota::per_minute(NonZeroU32::new(30).unwrap())
                .allow_burst(NonZeroU32::new(10).unwrap()),
        ));
        Self {
            base_url: "https://api.bitflyer.com".to_string(),
            api_key,
            api_secret,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client builder should not fail"),
            rate_limiter: Some(limiter),
        }
    }

    pub fn with_base_url(base_url: String, api_key: String, api_secret: String) -> Self {
        Self {
            base_url,
            api_key,
            api_secret,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client builder should not fail"),
            rate_limiter: None,
        }
    }

    #[doc(hidden)]
    pub fn new_for_test(base_url: String, api_key: String, api_secret: String) -> Self {
        Self::with_base_url(base_url, api_key, api_secret)
    }

    /// テストで限定的なレート制限を差し込むためのセッター。
    /// 本番の `new()` は自動で 30 req/min バケットを張る。
    pub fn with_rate_limiter(mut self, limiter: Arc<RateLimiter>) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }
```

`use std::num::NonZeroU32;` を use セクションに追加する。

`request()` の先頭で acquire:

```rust
    async fn request(
        &self,
        method: &str,
        path: &str,
        body_json: &str,
    ) -> Result<String, BitflyerApiError> {
        if let Some(limiter) = &self.rate_limiter {
            limiter.until_ready().await;
        }
        // ... 以降既存処理 ...
```

- [ ] **Step 5: テスト確認**

```bash
cargo test -p auto-trader-market --test bitflyer_private_test rate_limit 2>&1 | tail -10
```

Expected: パス

`cargo test` 全体:

```bash
cargo test --workspace 2>&1 | grep -E 'FAILED|error\[' | head -5
```

Expected: 空

- [ ] **Step 6: clippy + fmt**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo fmt --all -- --check; echo "FMT EXIT: $?"
```

- [ ] **Step 7: コミット**

```bash
git add crates/market/src/bitflyer_private.rs crates/market/tests/bitflyer_private_test.rs
git commit -m "$(cat <<'EOF'
feat(market): token-bucket rate limiter (governor) for bitFlyer Private API

Production BitflyerPrivateApi::new() now attaches a 30 req/min
bucket with burst of 10. The actual bitFlyer Private limit is
200 req/5min = 40 req/min per IP; the conservative 30/min leaves
headroom for retries and ops runbooks without ever inducing a 429.

with_base_url() / new_for_test() omit the limiter so wiremock
tests run at native speed. with_rate_limiter(Arc<RateLimiter>)
lets tests inject a constrained bucket to assert the wait path
works end-to-end (see rate_limit_waits_when_bucket_empty).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: 最終検証 + PR 作成

- [ ] **Step 1: 全ワークスペースでテスト + clippy + fmt を実行**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
cargo test --workspace 2>&1 | grep -E 'test result|FAILED' | tail -30
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo fmt --all -- --check 2>&1; echo "FMT EXIT: $?"
```

Expected: 全パス、警告ゼロ、fmt 差分ゼロ。

- [ ] **Step 2: Docker 上で既存ペーパートレード挙動が壊れていないことを確認**

この PR は main.rs に一切触らないので挙動変更はないが念のため:

```bash
docker compose build --no-cache auto-trader 2>&1 | tail -5
docker compose up -d 2>&1 | tail -3
sleep 10
docker compose logs --tail=20 auto-trader 2>&1 | tail -20
```

Expected: `API server listening on 0.0.0.0:3001` / `bitflyer websocket connected` が出る、エラー無し。

- [ ] **Step 3: `simplify` スキルで変更コードを見直す**

`Skill` tool で `simplify` スキルを起動。3 並列レビュー (code-reuse / quality / efficiency) が指摘する WARN を精査。安い指摘は修正コミット、重い指摘は FOLLOWUP として記録。

- [ ] **Step 4: `code-review` スキルを実行**

CLAUDE.md 厳守ルール。Step 0 (verification + simplify) → Step 1 (self-review) → Step 2 (codex review) → Step 3 (PR) → Step 4 (CI/Copilot)。

Critical ゼロ・合意済み Warning のみの状態にして PR へ。

- [ ] **Step 5: PR 作成**

`superpowers:finishing-a-development-branch` スキルを起動し、Option 2 (Push + PR) で push。PR 本文には以下を含める:

- 目的: bitFlyer Private REST クライアントを新設 (PR 1 の土台の上に乗る HTTP 層)
- 変更ファイル一覧: `crates/market/Cargo.toml`, `crates/market/src/{lib.rs, bitflyer_private.rs}`, `crates/market/tests/bitflyer_private_test.rs`, `Cargo.toml` (workspace), `crates/core/src/types.rs` (PR 1 follow-up)
- テスト: 単体 ~10 本 + wiremock 統合 ~11 本
- リスク評価: main.rs / LiveTrader に一切触らない → 既存ペーパートレード挙動ゼロ変化
- 次 PR 予告 (`PR 3: LiveTrader + ExecutionPollingTask + dry_run`)

---

## Self-Review Notes

- [x] 設計書 5.1 節の「提供メソッド 6 本」「HMAC-SHA256」「レート制限 200 req/5min」「wiremock テスト」すべて Task 化
- [x] PR 1 FOLLOWUP (`OrderType::limit()` バリデート、Trade の test-only Default) を Task 1 で回収
- [x] `hmac = "0.12"`, `sha2 = "0.10"`, `hex = "0.4"`, `governor = "0.6"` の追加は Task 2 で整理
- [x] 既存ペーパートレード挙動ゼロ変化: main.rs / paper.rs / strategy/* に触らない
- [x] 型定義とエラー分類は wire 上での JSON を正として書く (bitFlyer ドキュメント準拠)
- [x] Secret redaction: `From<reqwest::Error>` が `without_url()` を強制 (PR 1 notify crate と同じ方針)
- [x] レート制限テストは時間依存なのでタイミング下限だけ assert (flake 対策)

## After This PR

次 PR (`PR 3: LiveTrader + ExecutionPollingTask + dry_run`) では:
- `crates/executor/src/live.rs` 新設
- `OrderExecutor for LiveTrader` 実装 (pending → 約定確認 → open の 2 フェーズ)
- dry_run モード (発注手前で no-op、`DryRunOrder` 通知のみ)
- `ExecutionPollingTask` (bitFlyer 約定を 3 秒間隔でポーリング、pending → open 遷移)
- `BitflyerPrivateApi` は `Arc<...>` で共有
- Signal → SendChildOrderRequest のマッピング
- 実約定価格を entry_price に採用 (PR 1 W-1 対応)

## FOLLOWUP for PR 3+

- `BitflyerErrorBody.data` (serde_json::Value) は現状使われていない。ドキュメント上どのレスポンスでどう使われるか不明確なので、実使用が判明したら型を絞る
- `Execution.exec_date` / `ChildOrder.child_order_date` が `String` のまま。PR 3 で `DateTime<Utc>` にパースする (bitFlyer は RFC3339 亜種 `2026-04-14T08:45:47` を返すので手動パース必要)
- `cancel_child_order` の 200 レスポンスは現状 body 空想定だが、bitFlyer の実レスポンスが JSON `{}` を返すこともあり得る。PR 3 以降で実機検証して warn ログで body 内容を一度観測する
