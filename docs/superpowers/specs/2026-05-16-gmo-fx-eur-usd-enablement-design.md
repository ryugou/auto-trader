# GMO FX EUR_USD を 4 戦略で有効化する 設計

- 作成日: 2026-05-16
- ステータス: 設計完了 (実装は次の plan で着手)
- 目的: paper=live 監査の残課題 #3 (EUR_USD 等のマルチペア対応) を解消。
- 関連: PR #86 (paper=live 契約 1/N、GMO FX Private API)、PR #87 (paper=live 契約 2/N、commission)、本セッションでの再監査結果

## 背景

paper=live 監査の Explore agent は「`crates/market/src/gmo_fx.rs:49` で `pairs: vec![Pair::new("USD_JPY")]` hardcode、EUR_USD は market feed が起動しない」と判定したが、再検証の結果これは誤読だった。実際のコード:

- `crates/market/src/gmo_fx.rs:42-55` の `GmoFxFeed::new(pairs: Vec<Pair>, primary_timeframe)` は constructor で pair 配列を受け取る汎用設計
- `crates/app/src/main.rs:696-712` は `config.pairs.fx` から `Vec<Pair>` を生成して GmoFxFeed に渡す
- `config/default.toml [pairs] fx = ["USD_JPY", "EUR_USD"]` 両方既に登録
- `[pair_config.USD_JPY]` / `[pair_config.EUR_USD]` も両方 `price_unit` / `min_order_size` 設定済

つまり market feed と pair_config の側では既に EUR_USD を受け入れる準備が整っている。動かない唯一の理由は、各 `[[strategies]]` エントリの `pairs = ["FX_BTC_JPY", "USD_JPY"]` に `EUR_USD` が含まれていないため、戦略が EUR_USD の `PriceEvent` を `on_price` で early-return しているだけ。

これはコード変更不要の「設定漏れ」。`pairs` array に 1 文字列を足す PR で完結する。

## ゴール

`config/default.toml` の `[[strategies]]` 4 エントリの `pairs` に `"EUR_USD"` を加え、GMO FX の EUR_USD で paper トレードが走るようにする。

対象 4 戦略 (いずれも `mode = "paper"` で `enabled = true`):
- `bb_mean_revert_v1`
- `donchian_trend_v1`
- `donchian_trend_evolve_v1`
- `squeeze_momentum_v1`

## 非ゴール (この PR では触らない)

- `donchian_trend_evolve_v1` 用 GMO FX paper account の追加 (現状 GMO FX の paper account は `FX 安全 / FX 通常 / FX 攻め` の 3 つ = bb_mean_revert / donchian_trend / squeeze_momentum 3 戦略分のみ、evolve 用が無い。本 PR では設定上 EUR_USD を有効化するが、account 不在のため evolve signal は dispatch 段階で skip される ─ USD_JPY と同じ現状を維持。別タスクで REST API もしくは migration で追加する)
- live モード切替
- EUR_USD 専用 strategy パラメータ最適化 (現状 params は pair 非依存で動作する設計)
- EUR_USD historical candle のバックテスト再実行 (paper 運用ログで段階確認)

## Architecture

純粋な config 変更。コード変更なし。

```
[strategies]
  pairs: ["FX_BTC_JPY", "USD_JPY"]
                    ↓
  pairs: ["FX_BTC_JPY", "USD_JPY", "EUR_USD"]
```

各戦略の `on_price` は `event.pair` を見て `self.pairs.contains(&event.pair)` で early-return する pair-filter を持つ (既存)。`pairs` 配列に EUR_USD を加えれば即座に EUR_USD の PriceEvent も処理対象になる。

## Components

| File | 変更 |
|------|------|
| `config/default.toml` | 4 行修正 (各 `[[strategies]]` の `pairs` array に `"EUR_USD"` 追加) |

その他ファイル変更なし。

## 影響範囲・データフロー

1. **Market feed**: `GmoFxFeed` は config から両 pair を購読中 (現状から変化なし)。EUR_USD の ticker は既に PriceStore に到達している。
2. **Strategy engine**: 4 戦略の `on_price` が EUR_USD `PriceEvent` を受理 → 内部の indicator / signal logic を実行 (params 非依存)。
3. **Signal dispatch**: signal の `pair = EUR_USD` で発火 → `account.strategy == signal.strategy_name` で account 解決:
   - bb_mean_revert → `FX 安全` account でトレード
   - donchian_trend → `FX 通常` account でトレード
   - squeeze_momentum → `FX 攻め` account でトレード
   - donchian_trend_evolve_v1 → 該当 GMO FX account 無し、dispatch skip (USD_JPY と同じ既存挙動)
4. **Trade row**: paper 経路で `Trade.pair = EUR_USD` の row が `trades` テーブルに insert される。pnl 計算は既存式で動作 (price unit は `pair_config.EUR_USD.price_unit = 0.00001` で精度確保)。
5. **Position monitor / job**: pair 単位の loop で動くので EUR_USD trade も自動的に SL/TP/time-limit 監視対象になる。
6. **Warmup**: 起動時に donchian の 100 bars warmup が走る。CandleBuilder は pair-agnostic な loop で動くので EUR_USD の M5/H1 candle も DB から自動 warmup される (既存 candle 履歴が無ければ最初の H1 完成まで signal は出ない ─ これは現状の USD_JPY 初回起動と同じ挙動)。

## Testing

- 既存 unit / integration test に影響なし (config 文字列のみの変更)。
- `scripts/test-all.sh` で fmt / clippy / 全 test suite が green であること
- merge 後 24-48 時間の paper 運用ログで以下を確認 (本 PR の DoD ではなく後追い check):
  - bb_mean_revert / donchian_trend / squeeze_momentum 各戦略の EUR_USD signal 数
  - EUR_USD trade row が trades テーブルに記録されているか
  - エラーログに EUR_USD 関連の不正 (price 0 / pair_config missing 等) が無いこと

## Error handling

- EUR_USD candle 履歴が不足している場合 → strategy の warmup 期間中は signal なし (既存挙動と同じ、エラーではない)
- ticker API が EUR_USD を返さなくなった場合 → `mark_market_closed` で feed health が degrade 表示 (既存挙動)
- `pair_config.EUR_USD` が万一誤って削除された場合 → sizing 計算が panic ではなく明示エラーになる (既存挙動)

## マイグレーション・互換性

- DB schema 変更なし
- 既存 trades 行はそのまま (pair 列 string で柔軟)
- 既存 paper account の挙動は変更なし
- USD_JPY / FX_BTC_JPY 既存トレードは影響なし

## レビュー観点 (PR description に含める想定)

- 変更は `config/default.toml` 4 行のみ
- `donchian_trend_evolve_v1` の GMO FX account 不在については別タスクで対応する旨を明記
- merge 後の paper 運用観察ポイント (EUR_USD signal 数 / trade row / エラーログ) を README or PR description にメモ
