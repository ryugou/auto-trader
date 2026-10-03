//! `auto-trader-sim` 全体で使うエラー型。
//!
//! 設計の正本: `docs/superpowers/specs/2026-10-01-simulation-foundation-design.md`。
//! 計画の「共通の型」（`docs/superpowers/plans/2026-10-01-simulation-foundation.md` Task 1）に
//! 記載された型をそのまま実装する。バリアントの追加・削除は以降のタスクでも行わない
//! （計画上、全タスクがこの型をそのまま消費する契約のため）。

/// `auto-trader-sim` のすべての失敗経路が収束するエラー型。
///
/// 各バリアントは、CLI の終了コード・ログの出し方を決める「事象の種類」に対応する
/// (spec 13 章の終了コード表、8.4 章のエラー扱い)。握りつぶしは行わず、原因を
/// 呼び出し元まで伝播させる。
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    /// 設定ファイルの読み込み・検証に関する失敗（spec 4 章）。
    #[error("config: {0}")]
    Config(String),
    /// CLI 引数・期間指定の誤り（spec 13 章）。
    #[error("argument: {0}")]
    Args(String),
    /// 起動時のテーブル存在チェックで、必要なテーブルが見つからない場合（spec 12 章）。
    #[error(
        "missing tables: {0:?}. run the new auto-trader image once (it applies migrations), or `auto-trader-sim migrate` on a non-production database"
    )]
    MissingTables(Vec<String>),
    /// スクリプトの登録時検証の違反（spec 8.1、8.4 章）。
    #[error("invalid_script: {0}")]
    InvalidScript(String),
    /// `backfill` で 1 件以上の取得に失敗した場合（spec 5.2 章）。
    #[error("fetch incomplete: {0:?}")]
    FetchIncomplete(Vec<String>),
    /// バッチ行の作成後、保存以外の理由（スレッド panic を含む）で中断した場合（spec 12 章）。
    #[error("batch failed: {0}")]
    BatchFailed(String),
    /// DB アクセスの失敗をそのまま伝播する。
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// 上記のいずれにも分類されない失敗（外部 crate のエラーをラップする場合など）。
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
