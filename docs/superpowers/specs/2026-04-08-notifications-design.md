# ダッシュボード通知機能

- 作成日: 2026-04-08
- 対象: ダッシュボード全体（バックエンド API + フロントエンド UI + DB スキーマ）

## 目的

トレード open / close イベントをユーザーが見逃さないよう、ダッシュボードのヘッダーにベルマーク + 未読バッジを置き、ドロップダウンで直近の通知を一覧できるようにする。長期の履歴は専用ページでページング閲覧できる。既読・未読は DB 永続化し、複数ブラウザ/タブ間で状態が共有される。

## スコープ

- 新規テーブル `notifications` と DB マイグレーション
- トレード open / close 時に同一トランザクションで通知を生成するバックエンド処理
- 通知 REST API（一覧取得 / 未読件数取得 / 全既読化）
- ヘッダーベル + ドロップダウン + 専用ページのフロントエンド UI
- 日次バッチでの既読 30 日超通知の物理削除

## 非スコープ

- プッシュ通知 / メール / Slack 連携等の外部通知チャネル
- 通知タイプの拡張（overnight_fee, balance_low 等）— 今回は `trade_opened` / `trade_closed` の 2 種類のみ
- 通知から対象トレードへのディープリンク（クリックは表示のみ）
- ユーザーごとの通知設定 / ミュート機能
- マルチユーザー対応

## ユーザー体験

### ヘッダー

- 既存のナビ右隣にベルアイコン 🔔 を配置
- 未読が存在する場合、右上に丸い赤バッジで未読件数を表示（1〜99、100 件以上は `99+`）
- 未読が 0 件のときはバッジを描画しない
- ベルクリックでドロップダウン開閉

### ドロップダウン

- ベル真下に固定幅（約 360px）のパネルをオーバーレイで開く
- パネル内には最新 20 件を `created_at DESC` で表示
- 未読と既読は同じリストに混在、未読は背景色（`bg-sky-950/40` 程度）で強調
- 各アイテムの表示:
  - **OPEN**: `🟢 OPEN BTC/JPY LONG @ 10,500,000 · 5 分前`
  - **CLOSE (profit)**: `🔴 CLOSE BTC/JPY LONG +8,400 (take_profit) · 1 時間前`
  - **CLOSE (loss)**: `🔴 CLOSE BTC/JPY LONG -3,200 (stop_loss) · 1 時間前`
- 時刻は相対表示（`〇 分前 / 〇 時間前 / 〇 日前`）、マウスオーバーで絶対時刻（JST）を title 属性で表示
- パネル下部に「すべて見る →」リンクで `/notifications` ページへ
- パネル外クリックで閉じる
- 通知が 1 件も無いときは `通知はありません` と中央に表示

### 既読化

- ドロップダウンを **開いた瞬間** にサーバー側で全未読を既読化する
- その直後に来た新しい通知は未読のまま残る（パネル表示中は 15 秒ごとに fetch 更新されるが、既読化はパネル open の 1 回だけ）
- 個別のクリックでの既読化や「既読/未読を切り替え」機能は持たない
- 誤クリックで即既読化される点は許容

### 通知ページ (`/notifications`)

- ナビタブには **追加しない**（ベル経由からのみ到達）
- 上部フィルタ:
  - 期間: `全期間 / 今日 / 1週間 / 1ヶ月`（Trades ページと同じ定義）
  - 種別: `すべて / OPEN / CLOSE`
- 本体: 50 件/ページの表形式、前へ/次へボタンのシンプルページング
- カラム: `日時 (絶対表示 JST) / 種別 / 戦略 / ペア / 方向 / 価格 / PnL / exit_reason`
- このページを単に開くだけでは追加の既読化は起きない（既に dropdown で既読化されているはずだが、仕様として明記）

## データモデル

### `notifications` テーブル

```sql
CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL CHECK (kind IN ('trade_opened', 'trade_closed')),
    trade_id UUID NOT NULL REFERENCES trades(id) ON DELETE CASCADE,
    -- 冗長保存される表示用フィールド（trades JOIN を避けて O(1) 読み出し）
    paper_account_id UUID NOT NULL,
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    direction TEXT NOT NULL,      -- 'long' | 'short'
    price NUMERIC NOT NULL,       -- open: entry_price / close: exit_price
    pnl_amount NUMERIC,           -- close のみ NOT NULL、open では NULL
    exit_reason TEXT,             -- close のみ NOT NULL、open では NULL
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    read_at TIMESTAMPTZ           -- NULL = 未読
);

CREATE INDEX idx_notifications_created_at ON notifications (created_at DESC);
CREATE INDEX idx_notifications_unread ON notifications (read_at) WHERE read_at IS NULL;
```

### 保持期間

- 未読通知は永続保持
- 既読通知は `read_at < NOW() - INTERVAL '30 days'` で物理削除
- 削除は日次バッチから 1 回実行

## バックエンド

### 通知生成

**ファイル:** `crates/executor/src/paper.rs`

- **open**: `PaperTrader::open_position` 内の既存トランザクション `tx` で `insert_trade_with_executor` を呼んだ直後に、同じ `tx` で `notifications::insert_trade_opened(&mut tx, &trade).await?` を呼ぶ
- **close**: `PaperTrader::close_position` 内の CAS UPDATE が `rows_affected == 1` を確認した直後、同じ `tx` で `notifications::insert_trade_closed(&mut tx, &closed_trade).await?` を呼ぶ

同一トランザクション内で実行することで、「トレードは作られたのに通知が抜けた」「通知だけ出てトレードがロールバックされた」という矛盾状態を防ぐ。notifications INSERT は単純で、失敗するならむしろトレードを止めるべき事態なのでトレードと運命を共にする方針を取る。

### 新規モジュール

**ファイル:** `crates/db/src/notifications.rs` (新規)

公開関数:

```rust
pub async fn insert_trade_opened<'e, E>(
    executor: E,
    trade: &Trade,
) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>;

pub async fn insert_trade_closed<'e, E>(
    executor: E,
    trade: &Trade,
) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>;

pub async fn list(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    unread_only: bool,
    kind_filter: Option<&str>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> anyhow::Result<(Vec<Notification>, i64)>; // (items, total)

pub async fn unread_count(pool: &PgPool) -> anyhow::Result<i64>;

pub async fn mark_all_read(pool: &PgPool) -> anyhow::Result<i64>; // returns rows affected

pub async fn purge_old_read(pool: &PgPool) -> anyhow::Result<u64>; // returns rows deleted
```

`Notification` 構造体は上記テーブルをそのままマップする serde-serializable な struct（`lib.rs` に pub 追加）。

### 日次バッチ

**ファイル:** `crates/app/src/main.rs`

既存の daily batch（`daily batch startup backfill` 等が走る位置）の近くに、`notifications::purge_old_read(&pool).await` を 1 回呼ぶ追加処理を入れる。呼び出し先のエラーは `warn!` でログするのみでバッチ全体を止めない。

## API

### 新規モジュール

**ファイル:** `crates/app/src/api/notifications.rs` (新規)

ルート登録は `crates/app/src/api/mod.rs` に 1 行追加。

### エンドポイント

#### `GET /api/notifications`

クエリパラメータ:

- `unread_only`: `bool`, default `false`
- `limit`: `int`, default `50`, max `200`
- `page`: `int`, default `1`
- `kind`: `'trade_opened' | 'trade_closed'`, optional
- `from`: JST date string `YYYY-MM-DD`, optional
- `to`: JST date string `YYYY-MM-DD`, optional

レスポンス:

```json
{
  "items": [
    {
      "id": "...",
      "kind": "trade_closed",
      "trade_id": "...",
      "paper_account_id": "...",
      "strategy_name": "bb_mean_revert_v1",
      "pair": "FX_BTC_JPY",
      "direction": "long",
      "price": "10500000",
      "pnl_amount": "-3200",
      "exit_reason": "stop_loss",
      "created_at": "2026-04-08T05:30:12Z",
      "read_at": "2026-04-08T05:35:00Z"
    }
  ],
  "total": 123,
  "unread_count": 4
}
```

`unread_count` は `items` のフィルタとは独立で、常に DB 全体の未読件数を返す（ベルバッジ用データの一貫性のため）。

#### `GET /api/notifications/unread-count`

レスポンス: `{ "count": 4 }`

ベルバッジのポーリング用の軽量エンドポイント。`SELECT COUNT(*) ... WHERE read_at IS NULL` だけなので高速。

#### `POST /api/notifications/mark-all-read`

リクエストボディ無し。

レスポンス: `{ "marked": 4 }`

SQL: `UPDATE notifications SET read_at = NOW() WHERE read_at IS NULL`

## フロントエンド

### 新規コンポーネント

#### `dashboard-ui/src/components/NotificationBell.tsx`

- ヘッダー `<NavBar />` の右隣に配置（`App.tsx` の header flex に追加）
- `useQuery(['notifications-unread-count'])` で 15 秒ごとに未読件数を polling
- ベル SVG アイコン + 絶対位置の赤バッジ
- バッジ: `unread_count` が 0 なら非表示、1〜99 はそのまま、100+ は `99+`
- クリックで dropdown 開閉、外クリックで閉じる（`useEffect` + `document.addEventListener('mousedown', ...)`）
- 開いた瞬間に `api.notifications.markAllRead()` を実行し、成功したら `invalidateQueries(['notifications-unread-count'])` + `invalidateQueries(['notifications'])`

#### `dashboard-ui/src/components/NotificationDropdown.tsx`

- NotificationBell の子
- open 時のみ `useQuery(['notifications', { limit: 20 }], { enabled: isOpen })` で最新 20 件取得
- 未読は背景色で強調（`bg-sky-950/40`）
- 各アイテムの表示は上記「ドロップダウン」セクションのフォーマット通り
- 相対時刻は `formatRelativeTime(iso)` ユーティリティをローカル定義
- 空状態: `通知はありません`
- 下部に `react-router-dom` の `Link` で `/notifications` へ

#### `dashboard-ui/src/pages/Notifications.tsx`

- 期間フィルタ + 種別フィルタを上部に配置
- `useQuery(['notifications', { page, from, to, kind }])` で取得
- 表形式（`dashboard-ui/src/pages/Positions.tsx` を参考にした純テーブル、TanStack Table は使わない）
- カラム: `日時 / 種別 / 戦略 / ペア / 方向 / 価格 / PnL / exit_reason`
- 日時は JST 絶対表示
- 前へ/次へボタン（`totalPages > 1` のときのみ表示）

### 既存ファイルの変更

- `dashboard-ui/src/App.tsx`
  - header の `<div className="max-w-7xl mx-auto flex flex-col sm:flex-row items-start sm:items-center gap-3">` に `<NotificationBell />` を追加（NavBar の右隣）
  - `<Routes>` に `/notifications` ルート追加（ナビには追加しない）
- `dashboard-ui/src/api/types.ts`
  - `Notification`, `NotificationsResponse` 型追加
- `dashboard-ui/src/api/client.ts`
  - `api.notifications.list({ ... })`, `api.notifications.unreadCount()`, `api.notifications.markAllRead()` を追加

## エッジケース

- **ドロップダウンを開いた瞬間に新しい通知が INSERT される**: `mark_all_read` は INSERT より後に走るため新着も既読化される可能性がある。許容（タイミング的に極めて稀、次のポーリングで新しい未読が出るなら結局見える）
- **外部要因で trades 行が物理削除される**: `ON DELETE CASCADE` で notifications 行も連鎖削除されるのでゴミは残らない。ただし「通知履歴としての監査性」は犠牲になる。現状のダッシュボードでは trades は物理削除しない運用なのでこれで十分
- **`pnl_amount` が NULL の close**: 本来発生しないが、万一発生したら表示は `-`
- **`exit_reason` が未知の文字列**: そのまま表示（rich formatting 無し）
- **ヘッダーの横幅が狭い**: ベルは flex 末尾に置くので折り返しても機能する。ドロップダウンは `position: absolute; right: 0` で固定
- **通知ページに直接 URL でアクセス**: `Notifications.tsx` は独立してマウント可。ベル経由でなくても動作
- **複数タブ間の整合**: タブ A でドロップダウン開いて既読化 → タブ B のベルバッジは最大 15 秒遅れて更新。許容

## テスト観点

### バックエンド

- `notifications::insert_trade_opened` が正しい kind/field で行を作ること
- `notifications::insert_trade_closed` が pnl_amount / exit_reason を埋めること
- open / close のトランザクション内で failure した場合、通知とトレードが両方ロールバックされること
- `list` がフィルタ（kind, from, to, unread_only）とページングで正しい結果と total を返すこと
- `mark_all_read` が未読のみを既読化し既読には触らないこと
- `purge_old_read` が既読 30 日超のみ削除すること

### フロントエンド

- ベルバッジが未読件数に追随すること（0 で非表示、100+ で `99+`）
- ドロップダウン open で `mark-all-read` が 1 回だけ呼ばれること
- ドロップダウン外クリックで閉じること
- ドロップダウン内の未読アイテムだけが強調色で表示されること
- 通知ページのフィルタとページングが動作すること
- 通知が 0 件のときの空状態表示

## マイグレーション

`migrations/20260408000001_notifications.sql` に上記 `CREATE TABLE` + インデックスを格納。ダウングレードは考慮しない方針（既存マイグレーションと同じポリシー）。

## 既存コードへの影響

- `crates/db/src/lib.rs`: `pub mod notifications;` 追加
- `crates/db/src/notifications.rs`: 新規
- `crates/executor/src/paper.rs`: `open_position` と `close_position` に通知 INSERT を追加
- `crates/app/src/api/mod.rs`: notifications ルート登録追加
- `crates/app/src/api/notifications.rs`: 新規
- `crates/app/src/main.rs`: daily batch に `purge_old_read` 呼び出し追加
- `dashboard-ui/src/App.tsx`: ヘッダーにベル追加、`/notifications` ルート追加
- `dashboard-ui/src/api/types.ts`: 型追加
- `dashboard-ui/src/api/client.ts`: API 関数追加
- `dashboard-ui/src/components/NotificationBell.tsx`: 新規
- `dashboard-ui/src/components/NotificationDropdown.tsx`: 新規
- `dashboard-ui/src/pages/Notifications.tsx`: 新規
- `migrations/20260408000001_notifications.sql`: 新規

## 将来の拡張余地

- 通知タイプの追加（`overnight_fee`, `balance_low`, `strategy_warning` 等）— `kind` の CHECK 制約を広げればスキーマ変更は不要
- 通知から対象トレード詳細へのディープリンク
- ユーザー設定による kind 別ミュート
- ナビタブへの通知ページ追加
- プッシュ通知・メール等の外部チャネル連携
