-- Strategy catalog v2: add risk_level + seed three new crypto strategies
-- and create three paper accounts to run them in parallel.
--
-- The user runs each strategy on its own isolated 30,000 JPY paper
-- account so the experiment is "find which algorithm works" — accounts
-- are independent, blow-ups are acceptable, iteration loops fast.

-- Step 1: risk_level column with CHECK enum.
-- Existing rows are updated immediately (NOT NULL DEFAULT 'medium').
ALTER TABLE strategies
    ADD COLUMN risk_level TEXT NOT NULL DEFAULT 'medium';

ALTER TABLE strategies
    ADD CONSTRAINT strategies_risk_level_check
    CHECK (risk_level IN ('low', 'medium', 'high'));

-- Step 2: backfill risk levels for the three previously-seeded
-- strategies so the badge is meaningful right away.
UPDATE strategies SET risk_level = 'medium' WHERE name = 'trend_follow_v1';
UPDATE strategies SET risk_level = 'medium' WHERE name = 'crypto_trend_v1';
UPDATE strategies SET risk_level = 'medium' WHERE name = 'swing_llm_v1';

-- Step 3: seed the three new crypto strategies. Algorithm text is
-- intentionally long-form Markdown so the catalog detail view can render
-- it without further lookup. References to the source studies are
-- inline so future readers can verify the design.
INSERT INTO strategies (name, display_name, category, risk_level, description, algorithm, default_params)
VALUES
    (
        'bb_mean_revert_v1',
        '慎重平均回帰 v1 (BB)',
        'crypto',
        'low',
        'Bollinger Bands 極値 + RSI 逆張り。レンジ相場で小さく取り続けるコツコツ型。',
        $md$
## 想定相場
レンジ相場・横ばい

## エントリー
- **Long**: 終値が BB(20, 2.5σ) の **下バンドより下** で確定 + **RSI(14) < 25** + 直前足が安値更新（投げ売り確認）
- **Short**: 終値が **上バンドより上** + **RSI(14) > 75** + 直前足が高値更新

## 損切
- ATR(14) ベース: `max(0.5×ATR, entry×0.5%)` 距離
- 30k 口座保護のため上限 **2%** にキャップ

## 利確（動的）
- **Long / Short** とも価格が **SMA20（BB ミドル）に到達** したら決済
- **24 時間** 経過しても未達なら強制決済（fail-safe）

なお invalidation は **SL に一任** している。レンジ離脱で価格が逆走した
場合は SL が拾うので、別途 RSI ベースの早利確ロジックは持たない。

## 出典
[Babypips Short-Term Bollinger Reversion](https://www.babypips.com/trading/system-rules-short-term-bollinger-reversion-strategy)
classic mean-reversion ruleset.

## 想定スペック
- 想定 R:R: **1:1.2 〜 1:1.5**
- 想定勝率: **60-65%**
- 1 トレードの想定リスク: 残高の 0.5〜1.5%
$md$,
        '{"bb_period": 20, "bb_stddev": 2.5, "rsi_period": 14, "atr_period": 14, "sl_max_pct": 0.02, "time_limit_hours": 24}'::jsonb
    ),
    (
        'donchian_trend_v1',
        '標準ブレイクアウト v1 (Donchian)',
        'crypto',
        'medium',
        '20 本ドンチャンチャネルブレイク + ATR フィルタ。中規模トレンドを最後まで取りに行くタートル流。',
        $md$
## 想定相場
中規模〜大規模トレンド

## エントリー
- **Long**: 終値が **直近 20 本高値** を上抜け + **ATR(14) > 直近 50 本平均 ATR**（低ボラ偽ブレイク除外）
- **Short**: ミラー（20 本安値下抜け + 同条件）

## 損切
- **2 × ATR(14)** 距離（タートル "N" stop）
- 30k 口座保護のため上限 **3%** にキャップ

## 利確（動的）
- **Long**: 終値が **直近 10 本安値を下回ったら** 決済
- **Short**: 終値が **直近 10 本高値を上回ったら** 決済
- **固定 TP は持たない**（タートル原典: "Fixed targets artificially limit your upside"）

## 出典
- [Original Turtle Trading Rules](https://oxfordstrat.com/coasdfASD32/uploads/2016/01/turtle-rules.pdf) (Richard Dennis, 1983)
- [Modern Turtle for Forex](https://fxnx.com/en/blog/the-modern-turtle-adapting-donchian-breakouts-for-2024-forex)

## 想定スペック
- 想定 R:R: **1:2 〜 1:5+**（トレンドが伸びれば青天井）
- 想定勝率: **35-45%**
- 1 トレードの想定リスク: 残高の 1〜2%
$md$,
        '{"entry_channel": 20, "exit_channel": 10, "atr_period": 14, "atr_baseline_bars": 50, "sl_atr_mult": 2, "sl_max_pct": 0.03}'::jsonb
    ),
    (
        'squeeze_momentum_v1',
        '攻めボラティリティ v1 (TTM Squeeze)',
        'crypto',
        'high',
        'BB が KC 内に縮小するスクイーズ後の爆発的ブレイクをモメンタム方向で取る。一発狙い型。',
        $md$
## 想定相場
急騰・急落・サポート/レジスタンス破綻・サプライズ

## エントリー
- **スクイーズ条件**: BB(20, 2σ) が KC(20, 1.5×ATR) の内側に **6 本連続** 以上収まっている
- **Long**: スクイーズが解除（BB が KC 外に出る）+ モメンタム（close - SMA20）が**正で増加中**
- **Short**: スクイーズ解除 + モメンタムが**負で減少中**

## 損切
- 直近 5 本のスイング安値（Long）/ 高値（Short）
- 30k 口座保護のため上限 **4%** にキャップ

## 利確（動的）
- **Long / Short** とも終値が **EMA(21) を逆方向に割ったら** 決済
- **48 時間** 経過したら強制決済（fail-safe）
- 固定 TP は持たない — トレーリングで伸ばし続ける

## 出典
- [TrendSpider TTM Squeeze guide](https://trendspider.com/learning-center/introduction-to-ttm-squeeze/)
- [EBC Financial: Mastering TTM Squeeze](https://www.ebc.com/forex/top-ways-to-master-the-ttm-squeeze-trading-strategy)
- John Carter, "Mastering the Trade"

## 想定スペック
- 想定 R:R: **1:3 〜 1:8**
- 想定勝率: **30-35%**
- 1 トレードの想定リスク: 残高の 1〜3%
- ※ クラッシュ局面で大きな短ポジが取れる設計
$md$,
        '{"bb_period": 20, "bb_stddev": 2, "kc_period": 20, "kc_atr_mult": 1.5, "atr_period": 14, "ema_trail_period": 21, "squeeze_bars": 6, "sl_max_pct": 0.04, "time_limit_hours": 48}'::jsonb
    );

-- Step 4: create three paper accounts to run the new strategies in
-- parallel, all starting at 30,000 JPY. The accounts are independent —
-- this is a 3-way parallel A/B test, not a portfolio. Existing
-- crypto_real / crypto_100k accounts (running crypto_trend_v1) are
-- left untouched as baselines.
INSERT INTO paper_accounts (id, name, exchange, initial_balance, current_balance, currency, leverage, strategy, account_type)
VALUES
    ('a0000000-0000-0000-0000-000000000010', 'crypto_safe_v1',       'bitflyer_cfd', 30000, 30000, 'JPY', 2, 'bb_mean_revert_v1',    'paper'),
    ('a0000000-0000-0000-0000-000000000011', 'crypto_normal_v1',     'bitflyer_cfd', 30000, 30000, 'JPY', 2, 'donchian_trend_v1',    'paper'),
    ('a0000000-0000-0000-0000-000000000012', 'crypto_aggressive_v1', 'bitflyer_cfd', 30000, 30000, 'JPY', 2, 'squeeze_momentum_v1',  'paper');
