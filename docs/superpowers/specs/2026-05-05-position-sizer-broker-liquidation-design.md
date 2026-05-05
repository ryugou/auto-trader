# PositionSizer: Broker-Specific Liquidation-Aware Sizing

Date: 2026-05-05

## 背景

`crates/executor/src/position_sizer.rs` が `maintenance_margin_rate = 0.50` を関数内ローカル定数として持ち、**全 Exchange に対して 50% を一律適用**している。コメントは "bitFlyer CFD maintenance margin rate" としているが、二重に問題がある:

1. **値の根拠不明**: 0.50 は bitFlyer Crypto CFD 公式のロスカット維持率閾値 (50%) と数値こそ一致するが、後述の通り式の構造が実際の維持率ロスカット条件を表現していないため、bitFlyer の仕様を写したものとは言えない。
2. **gmo_fx (GMOコイン外国為替FX) のロスカット閾値 100% を全く反映していない**。

加えて式の構造そのものが正しくない:

```rust
// 現状（誤）
let max_alloc = (Decimal::ONE - maintenance_margin_rate) / (leverage * stop_loss_pct);
// = 0.5 / (L × s)
```

これは「SL ヒット時の損失が残高の (1 − 維持率) を超えない」という rule を表しているように見えるが、**実際の維持率 = 評価額 / 必要証拠金 のロスカット条件と一致していない**。

実害:
- gmo_fx の paper account がフルベットで running し、`FX 攻め` は開いた瞬間維持率 100.04% でほぼ即時 LC 圏。実際 PnL −29,989 円で残高 11 円まで持っていかれた。
- bitflyer_cfd の `安全` (BB) は新規取引が常時最大張り込み。lev=2 ・SL ≤ 3% の組合せでは LC 閾値 50% よりはるか手前で SL が機能するため結果オーライだが、設計としては誤った式に依存している。

## 仕様

「**SL に到達した瞬間に維持率がちょうど取引業者のロスカット閾値となる、最大の allocation**」を計算する。SL に到達する前の逆行は維持率が閾値より上で耐え、SL ヒットでちょうど LC ライン。SL より大きい逆行が起きる前に SL を機能させる。

### 数式

定義:
- `b` = 残高 (account current_balance)
- `L` = システム上のレバレッジ (account.leverage)
- `a` = allocation (求める変数)
- `s` = stop_loss_pct (Signal.stop_loss_pct)
- `Y` = 取引業者のロスカット閾値 (証拠金維持率の下限。例: gmo_fx=1.0、bitflyer_cfd=0.5)

ポジションを開いた直後の状態:
- 必要証拠金 `m = b × a`
- ポジション建値 `n = m × L = b × a × L`
- 評価額 `e₀ = b`
- 維持率 `e₀ / m = 1 / a × 100%`

SL ヒット時の状態:
- 含み損 = `n × s = b × a × L × s`
- 評価額 `e₁ = b × (1 − a × L × s)`
- 維持率 `e₁ / m = (1 − a × L × s) / a × 100%`

`SL ヒット時維持率 ≥ Y` の条件:

```
(1 − a × L × s) / a ≥ Y
⇔ 1 ≥ a × (Y + L × s)
⇔ a ≤ 1 / (Y + L × s)
```

したがって:

```
max_alloc = 1 / (Y + leverage × stop_loss_pct)
```

最終 allocation = `min(max_alloc, signal.allocation_pct)`（`signal.allocation_pct` は事前検証で `(0, 1]` の範囲が保証されるので、暗黙的に 1.0 で頭打ちとなる）。

### 数値検証

残高 30,000 円 / gmo_fx (lev=10, Y=1.0) / SL=2% の場合:

| 項目 | 計算 | 値 |
|---|---|---|
| max_alloc | `1 / (1.0 + 10 × 0.02)` | 0.8333 |
| 必要証拠金 | `30,000 × 0.8333` | 24,990 円 |
| 残金 | `30,000 − 24,990` | 5,010 円 |
| ポジション建値 | `24,990 × 10` | 249,900 円 |
| SL ヒット時の含み損 | `249,900 × 0.02` | −4,998 円 |
| SL ヒット時の評価額 | `30,000 − 4,998` | 25,002 円 |
| **SL ヒット時の維持率** | `25,002 / 24,990` | **100.05% (≈ Y=1.00)** |

→ 仕様通り、SL に到達した瞬間に LC ラインちょうど。

bitflyer_cfd (lev=2, Y=0.5) / SL=2%:

| 項目 | 計算 | 値 |
|---|---|---|
| max_alloc | `1 / (0.5 + 2 × 0.02)` | 1.852 → cap 1.0 |
| 必要証拠金 (alloc=1.0) | 30,000 | 30,000 円 |
| ポジション建値 | `30,000 × 2` | 60,000 円 |
| SL ヒット時の含み損 | `60,000 × 0.02` | −1,200 円 |
| SL ヒット時の評価額 | `30,000 − 1,200` | 28,800 円 |
| SL ヒット時の維持率 | `28,800 / 30,000` | 96% |

→ Y=50% よりはるか手前で SL が機能。lev=2 ・SL≤3% の組合せでは max_alloc が常に 1.0 で頭打ち、これは実質的に `min(max_alloc, signal.allocation_pct)` の挙動として正しい。

## 設定 (Configuration)

`config/default.toml` に取引業者ごとの LC 閾値を追加。

```toml
[exchange_margin.bitflyer_cfd]
liquidation_margin_level = 0.50  # 維持率 50% 未満で即時ロスカット (公式)

[exchange_margin.gmo_fx]
liquidation_margin_level = 1.00  # 維持率 100% 未満でロスカット (公式)
```

値の出典:
- bitflyer_cfd: bitFlyer Crypto CFD 公式 FAQ — 維持率 50% で即時ロスカット。
- gmo_fx: GMOコイン外国為替FX 公式サポート — 維持率 100% 未満でロスカット (必要証拠金 = 取引金額 × 4%、追証 125%、ロスカット 100%)。

OANDA は使用しないため設定対象外。

### Configuration の型 (`crates/core/src/config.rs`)

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ExchangeMarginConfig {
    pub liquidation_margin_level: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    // ... 既存フィールド
    #[serde(default)]
    pub exchange_margin: HashMap<String, ExchangeMarginConfig>,
}
```

## コンポーネント変更

### `crates/executor/src/position_sizer.rs`

- 関数内ローカル `maintenance_margin_rate = 0.50` を**削除**。
- `calculate_quantity()` シグネチャに `liquidation_margin_level: Decimal` を追加。
- 計算ロジックを新式 `max_alloc = 1 / (Y + L × s)` に置換。
- 入力検証で `liquidation_margin_level <= 0` を reject (None を返す)。

```rust
pub fn calculate_quantity(
    &self,
    pair: &Pair,
    balance: Decimal,
    entry_price: Decimal,
    leverage: Decimal,
    allocation_pct: Decimal,
    stop_loss_pct: Decimal,
    liquidation_margin_level: Decimal,  // 新規
) -> Option<Decimal>
```

### `crates/executor/src/trader.rs`

- `Trader` 構造体に `liquidation_margin_level: Decimal` フィールドを追加（construction 時に決定）。
- `Trader::new` の引数を 1 つ追加してこの値を受け取る。
- `execute()` 内の `sizer.calculate_quantity(...)` 呼び出しに該当値を渡す。

### `crates/app/src/main.rs`

- 起動時に `config.exchange_margin` を読み、各 trading_account の `exchange` に対応する `liquidation_margin_level` を解決。
- **欠落していたら panic / 起動失敗** (fail-closed)。`gmo_fx` のアカウントが存在するのに `[exchange_margin.gmo_fx]` が未定義なら起動を中止し、ログに「設定漏れ: gmo_fx の liquidation_margin_level が未定義」と出力。
- 解決した値を `Trader::new` に渡す。

### Strategy 側

**変更なし**。各戦略は引き続き `allocation_pct = ALLOCATION_CAP (= 1.0)` を Signal に詰める。`PositionSizer` 側の `max_alloc` cap が LC 安全性を担保する。

## エラーハンドリングと境界条件

| ケース | 挙動 |
|---|---|
| `liquidation_margin_level <= 0` | `calculate_quantity` が None を返す（不正入力扱い） |
| 設定欠落 (config に exchange エントリ無し、対応 account 有り) | 起動時 panic (fail-closed) |
| `Y + L × s` が 0 以下 (理論上ありえないが防御的に) | None を返す |
| `max_alloc` が 1.0 を超える | `signal.allocation_pct` 側の cap (1.0) で頭打ち |
| `signal.allocation_pct < max_alloc` | strategy 側の小さい方を採用 (= `min(max_alloc, allocation_pct)`) |
| 既存の入力検証 (balance/price/leverage/SL/allocation_pct ≤ 0 等) | 現状通り None |

## スコープ外 (このスペックでやらないこと)

- **既存 open positions のリサイズ**: 5/1 から open のまま放置されている `通常`/`vegapunk連動` のトレード (qty=0.002) は本スペック対象外。新規発火するシグナルから新ロジックが適用される。既存トレードを直したい場合はユーザー操作で手動クローズする。
- **ロスカット閾値の動的取得**: 取引業者 API から閾値を引いてくる仕組みは作らない。TOML の静的設定のみ。
- **Slippage / gap 用の安全バッファ**: 数式上 `max_alloc` で SL ヒット時にちょうど Y にぴったり乗る。それ以上の余裕は積まない。必要が生じたら別スペックで `safety_buffer_margin_level` 等を追加する。
- **OANDA**: 削除済み。再導入時は別スペック。

## テスト計画 (TDD)

### Unit (`crates/executor/src/position_sizer.rs`)

新しいシグネチャ前提で書き直し:

1. `gmo_fx_full_allocation_with_loose_sl`
   - lev=10, SL=0.5%, Y=1.0, balance=30,000
   - 期待 alloc = `1/(1.0+0.05) ≈ 0.952`
   - qty = 30000 × 10 × 0.952 / entry_price (USD/JPY @ 157)

2. `gmo_fx_tight_sl_caps_at_one`
   - SL が極小で `1/(Y+L×s) > 1.0` のケース → alloc=1.0 で頭打ち。

3. `bitflyer_cfd_full_allocation_typical`
   - lev=2, SL=2%, Y=0.5, balance=30,000
   - max_alloc = `1/(0.5+0.04) ≈ 1.85` → cap 1.0
   - qty = 30000 × 2 / 12,500,000 = 0.0048 → truncated to 0.004

4. `lc_constraint_binds_at_high_leverage_and_wide_sl`
   - lev=10, SL=10%, Y=1.0
   - max_alloc = `1/(1.0+1.0) = 0.5`
   - qty が半分

5. `liquidation_margin_level_zero_rejected`
   - Y ≤ 0 → None

6. `post_sl_margin_level_equals_threshold_invariant`
   - 任意の (lev, SL, Y) で `max_alloc = 1/(Y+L×s)` を計算した結果を sizer に流すと、SL ヒット時の維持率が `Y` (許容誤差 < 1bp) になることを property test で検証。

7. **既存テストの書き直し**: 現在 `full_allocation_with_risk_limiting` 等のテストは旧式前提なので新シグネチャと数値で更新。

### Integration (`crates/integration-tests/`)

1. `phase3_sizing_no_liquidation_gmo_fx`
   - DB に gmo_fx account (lev=10, balance=30,000) を作成、SL=2% の signal を流し、Trader 経由で trade insert。
   - 検証: trade.quantity が `30000 × 10 × 0.833 / entry_price` 相当になっていること。

2. `phase3_sizing_no_liquidation_bitflyer_cfd`
   - bitflyer_cfd account で同様に検証。alloc=1.0 cap が効くこと。

3. `phase1_startup_fails_when_exchange_margin_missing`
   - config から `[exchange_margin.gmo_fx]` を削除し、gmo_fx account が存在する状態で起動。
   - 期待: panic / 起動失敗。

### 既存テストへの影響

- `crates/integration-tests/tests/phase3_*` のうち `PositionSizer::new` を直接呼んでいる箇所は新シグネチャに合わせる必要がある。
- `crates/executor/src/position_sizer.rs` のテストすべてが旧シグネチャ前提なので全面更新。

## 移行とロールアウト

1. PR 1 本でリリース可能。後方互換配慮なし (production の既存 paper account がそのまま稼働しているだけで、コード側に旧シグネチャ呼び出しは残らない)。
2. PR マージ後、Docker image 再ビルド → 再デプロイで効果発生。
3. デプロイ後の最初の新規取引から新ロジックが適用される。`通常`/`vegapunk連動` の旧 open ポジは手動クローズで再エントリすること。

## 参考

- bitFlyer Crypto CFD ロスカットルール: <https://bitflyer.com/ja-jp/faq/7-23>
- GMOコイン外国為替FX ロスカット: <https://support.coin.z.com/hc/ja/articles/17884183390105>
- 数式の導出: 上記「仕様 → 数式」セクション
