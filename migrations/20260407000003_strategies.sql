-- Strategy catalog table.
--
-- This table is metadata only for *strategy logic and parameters* — it does
-- NOT drive runtime behavior. The trading engine still loads enabled
-- strategies, modes, and parameters from `config/default.toml`
-- (`[[strategies]]`). The table exists so the UI can:
--   1. Render a strategy dropdown when creating/editing paper accounts
--      (instead of a free-text field that lets users typo strategy names).
--   2. Show a read-only "戦略一覧" page describing each strategy and its
--      algorithm.
--
-- The catalog IS authoritative for "what names are referenceable". Both
-- the API (`POST /paper-accounts`) and a startup drift check enforce that
-- `paper_accounts.strategy` only points at rows in this table; the FK
-- below additionally protects against TOCTOU races and direct-SQL writes.
--
-- Adding a new strategy therefore requires both a Rust impl in
-- `crates/strategy/src/` AND a row in this table (or a follow-up migration).

CREATE TABLE strategies (
    name TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    -- 'fx' / 'crypto' — used by the UI to filter the dropdown to strategies
    -- compatible with the account's exchange.
    category TEXT NOT NULL,
    -- One-line summary shown in the dropdown and the catalog list.
    description TEXT NOT NULL,
    -- Long-form algorithm explanation (Markdown). Rendered on the catalog
    -- detail view.
    algorithm TEXT NOT NULL,
    -- Reference parameters as documented in config/default.toml. The engine
    -- still uses config values; this is for display only. The CHECK
    -- enforces an object shape so the UI (which calls
    -- `Object.keys(default_params)`) can rely on it.
    default_params JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT strategies_category_check CHECK (category IN ('fx', 'crypto')),
    CONSTRAINT strategies_default_params_object
        CHECK (jsonb_typeof(default_params) = 'object')
);

-- Seed the three strategies that exist in code today. Descriptions and
-- algorithm text are derived from the actual implementations in
-- crates/strategy/src/{trend_follow,crypto_trend,swing_llm}.rs.
INSERT INTO strategies (name, display_name, category, description, algorithm, default_params)
VALUES
    (
        'trend_follow_v1',
        'トレンドフォロー v1 (FX)',
        'fx',
        'SMA ゴールデンクロス / デッドクロスを RSI でフィルタする FX 向けトレンドフォロー戦略。',
        $md$
## ロジック
- 各ローソク確定時に短期 SMA (`ma_short`) と長期 SMA (`ma_long`) を計算
- **ゴールデンクロス** (短期 ≤ 長期 → 短期 > 長期) かつ **RSI(14) < rsi_threshold** で **ロング** シグナル
- **デッドクロス** (短期 ≥ 長期 → 短期 < 長期) かつ **RSI(14) > 100 - rsi_threshold** で **ショート** シグナル
- SL: 50 pips、TP: 100 pips（リスクリワード 1:2）
- pip サイズはペアごと: JPY ペア 0.01 / その他 0.0001

## 適性
- ボラティリティが中程度で、明確な方向感のある相場
- 取引時間帯のロンドン・NY セッションが主戦場
$md$,
        '{"ma_short": 20, "ma_long": 50, "rsi_threshold": 70}'::jsonb
    ),
    (
        'crypto_trend_v1',
        'クリプトトレンド v1 (BTC)',
        'crypto',
        'SMA クロス + RSI を BTC/JPY 向けに短期化したトレンドフォロー戦略。',
        $md$
## ロジック
- M5 ローソクで短期 SMA (`ma_short`) と長期 SMA (`ma_long`) を計算
- **ゴールデンクロス + RSI(14) < rsi_threshold** で **ロング**
- **デッドクロス + RSI(14) > 100 - rsi_threshold** で **ショート**
- SL: エントリー価格の **2%**、TP: **4%**（リスクリワード 1:2）
- bitFlyer Crypto CFD のみで動作

## FX 戦略との違い
- MA 期間を短く（暗号資産はトレンド転換が速い）
- SL/TP を絶対 pip でなく価格パーセントで指定（暗号資産は価格レンジが広い）
- 24/365 稼働
$md$,
        '{"ma_short": 8, "ma_long": 21, "rsi_threshold": 75}'::jsonb
    ),
    (
        'swing_llm_v1',
        'スイング LLM v1 (FX)',
        'fx',
        'Vegapunk + Gemini で 4 時間ごとに方向判断を行う FX スイング戦略（実験中）。',
        $md$
## ロジック
- 4 時間ごとに各ペアの最新価格と過去のトレード履歴を Vegapunk で検索
- 取得したコンテキストを Gemini に渡し、ロング / ショート / ノーアクションを判定
- マクロ更新（経済指標・ニュース）も入力に含める
- LLM 連続失敗時は exponential backoff で頻度を下げる
- `holding_days_max` は LLM プロンプトのコンテキストとしてのみ使用（強制クローズは未実装）

## ステータス
- Phase 0 では検証中。`config/default.toml` で `enabled = false` 推奨
- Vegapunk と Gemini API キー両方が必要
$md$,
        '{"holding_days_max": 14}'::jsonb
    );

-- Auto-import any strategy names that pre-existing paper_accounts already
-- reference but the seed above didn't cover, so the FK below can be added
-- without rejecting historical rows. New deployments hit zero rows; this is
-- a safety net for environments where unknown strategy names crept in via
-- the old free-text input.
INSERT INTO strategies (name, display_name, category, description, algorithm)
SELECT DISTINCT
    pa.strategy,
    pa.strategy || ' (auto-imported)',
    CASE WHEN pa.exchange = 'oanda' THEN 'fx' ELSE 'crypto' END,
    'Auto-imported from an existing paper_account at migration time. Update with proper documentation.',
    'Auto-imported placeholder. See crates/strategy/ for the actual implementation if any.'
FROM paper_accounts pa
LEFT JOIN strategies s ON s.name = pa.strategy
WHERE s.name IS NULL AND pa.strategy <> '';

-- Foreign key from paper_accounts.strategy to strategies.name. Closes the
-- TOCTOU window between the API's `strategy_exists` check and the actual
-- INSERT/UPDATE, and protects against direct-SQL writes that bypass the
-- API entirely. ON DELETE RESTRICT prevents removing a strategy that any
-- account still references — operators must reassign accounts first.
--
-- Note: paper_accounts.strategy was created with a `DEFAULT ''` in
-- 20260406000001 for DDL backfill purposes; that default is intentionally
-- left in place because no row should ever still hold '' (the seed always
-- writes a real name and the API rejects empty values upstream). If a
-- legacy row with '' exists, this migration will fail loudly so the
-- operator can clean it up before retrying.
ALTER TABLE paper_accounts
    ADD CONSTRAINT paper_accounts_strategy_fk
    FOREIGN KEY (strategy) REFERENCES strategies(name)
    ON DELETE RESTRICT
    ON UPDATE CASCADE;
