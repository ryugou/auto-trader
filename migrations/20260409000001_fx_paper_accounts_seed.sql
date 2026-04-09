-- FX paper accounts: 2 balances × 2 allocations = 4 accounts.
-- All run donchian_trend_fx_* on USD/JPY with leverage 25x.
-- UUID prefix b0000000-... to distinguish from crypto (a0000000-...).
--
-- ON CONFLICT DO NOTHING so this is safe to re-apply against an
-- environment where the rows were inserted via REST API instead.

-- Step 1: register the two new FX strategies in the strategies
-- catalog so paper_accounts can reference them. The strategies
-- catalog has an FK check enforced by the paper_accounts insert
-- below, so the catalog rows must exist first.
INSERT INTO strategies (name, display_name, category, risk_level, description, algorithm, default_params)
VALUES
    (
        'donchian_trend_fx_normal',
        'FX 標準ブレイクアウト 通常 (Donchian FX)',
        'fx',
        'medium',
        '20 本 Donchian ブレイク + ATR フィルタ。USD/JPY M15、allocation 50% で保守運用。',
        $md$
## 想定相場
中規模〜大規模トレンド (USD/JPY M15)

## エントリー
- **Long**: 終値が直近 20 本高値を上抜け + ATR(14) > 直近 50 本平均 ATR
- **Short**: ミラー (20 本安値下抜け + 同条件)

## 損切
- **2 × ATR(14)** 距離 (Turtle "N" stop)

## 利確 (動的)
- **Long**: 終値が直近 10 本安値を下抜けたら決済
- **Short**: 終値が直近 10 本高値を上抜けたら決済

## Allocation
- **0.50** (通常)。証拠金維持率 SL 後 200%、余裕を持って保守運用。

## Max hold
- **72 時間** (FX トレンドは crypto より長期)

## 想定スペック
- 想定 R:R: 1:2 以上 (トレイリング次第)
- 想定勝率: 35-45%
$md$,
        '{"entry_channel": 20, "exit_channel": 10, "atr_period": 14, "atr_sl_mult": 2.0, "allocation_pct": 0.50}'::jsonb
    ),
    (
        'donchian_trend_fx_aggressive',
        'FX 標準ブレイクアウト 攻め (Donchian FX)',
        'fx',
        'high',
        '20 本 Donchian ブレイク + ATR フィルタ。USD/JPY M15、allocation 80% で攻撃運用。',
        $md$
## 想定相場
中規模〜大規模トレンド (USD/JPY M15)

## エントリー・損切・利確・Max hold
`donchian_trend_fx_normal` と同じ。

## Allocation
- **0.80** (攻め)。証拠金維持率 SL 後 120%、フル寄り。ATR 拡大時には維持率低下リスクあり。

## 想定スペック
- 想定 R:R: 1:2 以上 (トレイリング次第)
- 想定勝率: 35-45%
- リスク: 通常版の 1.6 倍 (allocation 比)
$md$,
        '{"entry_channel": 20, "exit_channel": 10, "atr_period": 14, "atr_sl_mult": 2.0, "allocation_pct": 0.80}'::jsonb
    )
ON CONFLICT (name) DO NOTHING;

-- Step 2: seed the 4 paper accounts.
INSERT INTO paper_accounts (
    id, name, exchange, initial_balance, current_balance,
    currency, leverage, strategy, account_type, created_at, updated_at
) VALUES
    ('b0000000-0000-0000-0000-000000000010', 'fx_small_normal_v1',
     'oanda', 30000, 30000, 'JPY', 25,
     'donchian_trend_fx_normal', 'paper', NOW(), NOW()),
    ('b0000000-0000-0000-0000-000000000011', 'fx_small_aggressive_v1',
     'oanda', 30000, 30000, 'JPY', 25,
     'donchian_trend_fx_aggressive', 'paper', NOW(), NOW()),
    ('b0000000-0000-0000-0000-000000000012', 'fx_standard_normal_v1',
     'oanda', 100000, 100000, 'JPY', 25,
     'donchian_trend_fx_normal', 'paper', NOW(), NOW()),
    ('b0000000-0000-0000-0000-000000000013', 'fx_standard_aggressive_v1',
     'oanda', 100000, 100000, 'JPY', 25,
     'donchian_trend_fx_aggressive', 'paper', NOW(), NOW())
ON CONFLICT (id) DO NOTHING;
