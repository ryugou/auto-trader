//! `sim_candles` の読み書きと、`auto-trader-sim` が必要とするテーブルの存在確認
//! (spec 5.1 章、12 章)。
//!
//! ここで作る接続はマイグレーションを実行しない(`PgPoolOptions` を直接使う)。
//! `migrate` サブコマンドだけが `sqlx::migrate!` を呼ぶ(spec 12 章: 本番 DB では
//! 売買プロセスの起動時にだけマイグレーションを適用し、`auto-trader-sim migrate`
//! を本番へ向けて実行してはならない)。

use crate::error::SimError;
use crate::types::{
    Bar, DB_ACQUIRE_TIMEOUT, DB_MAX_CONNECTIONS, REQUIRED_TABLES, SIM_EXCHANGE, SIM_PAIR,
    SIM_TIMEFRAME,
};
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// マイグレーションを実行しない接続を作る(`migrate` 以外のサブコマンド用。spec 12 章)。
pub async fn connect(database_url: &str) -> Result<PgPool, SimError> {
    PgPoolOptions::new()
        .max_connections(DB_MAX_CONNECTIONS)
        .acquire_timeout(DB_ACQUIRE_TIMEOUT)
        .connect(database_url)
        .await
        .map_err(SimError::Db)
}

/// 起動時のテーブル存在チェック(spec 12 章)。欠けているテーブル名を
/// `SimError::MissingTables` で返す(運用者が次のアクションを判断できるよう、
/// `SimError::MissingTables` 自体のメッセージに対処法を含めてある)。
pub async fn ensure_tables(pool: &PgPool) -> Result<(), SimError> {
    let existing: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname = current_schema() AND tablename = ANY($1)",
    )
    .bind(&REQUIRED_TABLES[..])
    .fetch_all(pool)
    .await?;

    let missing: Vec<String> = REQUIRED_TABLES
        .iter()
        .filter(|t| !existing.iter().any(|e| e == *t))
        .map(|t| t.to_string())
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(SimError::MissingTables(missing))
    }
}

/// ミリ円 (i64) を `NUMERIC(10,3)` と 1 対 1 に対応する `Decimal` へ変換する
/// (spec 3 章)。`Decimal::new(v, 3)` は `v * 10^-3` を厳密に表す。
fn milli_to_decimal(v: i64) -> Decimal {
    Decimal::new(v, 3)
}

/// `Decimal` (`NUMERIC(10,3)`) をミリ円 (i64) へ変換する。
///
/// `Decimal::to_i64()` は内部で `trunc()` してから整数化するため、端数が
/// あっても黙って切り捨てられてしまう。ここでは端数の有無を明示的に確認し、
/// 端数がある(= 保存時の規約が破れている)場合は値を握りつぶさず
/// `SimError::Other` として報告する。
fn decimal_to_milli(d: Decimal, field: &'static str, open_time: i64) -> Result<i64, SimError> {
    let scaled = d * Decimal::from(1000);
    if !scaled.fract().is_zero() {
        return Err(SimError::Other(anyhow::anyhow!(
            "sim_candles row open_time={open_time}: {field}={d} does not convert to an integer milli-yen value (scaled={scaled})"
        )));
    }
    scaled.to_i64().ok_or_else(|| {
        SimError::Other(anyhow::anyhow!(
            "sim_candles row open_time={open_time}: {field}={d} overflows i64 after scaling to milli-yen"
        ))
    })
}

fn epoch_secs_to_datetime(secs: i64, context: &str) -> Result<DateTime<Utc>, SimError> {
    Utc.timestamp_opt(secs, 0).single().ok_or_else(|| {
        SimError::Other(anyhow::anyhow!(
            "{context}: epoch seconds {secs} is not a representable UTC instant"
        ))
    })
}

/// `sim_candles` の 1 行をそのまま受け取る行表現。`Bar` への変換は
/// `TryFrom<CandleRow>` で行う(`crates/db/src/candles.rs` の既存パターンに合わせる)。
#[derive(sqlx::FromRow)]
struct CandleRow {
    open_time: DateTime<Utc>,
    bid_open: Decimal,
    bid_high: Decimal,
    bid_low: Decimal,
    bid_close: Decimal,
    ask_open: Decimal,
    ask_high: Decimal,
    ask_low: Decimal,
    ask_close: Decimal,
}

impl TryFrom<CandleRow> for Bar {
    type Error = SimError;

    fn try_from(r: CandleRow) -> Result<Self, SimError> {
        let open_time = r.open_time.timestamp();
        Ok(Bar {
            open_time,
            bid_open: decimal_to_milli(r.bid_open, "bid_open", open_time)?,
            bid_high: decimal_to_milli(r.bid_high, "bid_high", open_time)?,
            bid_low: decimal_to_milli(r.bid_low, "bid_low", open_time)?,
            bid_close: decimal_to_milli(r.bid_close, "bid_close", open_time)?,
            ask_open: decimal_to_milli(r.ask_open, "ask_open", open_time)?,
            ask_high: decimal_to_milli(r.ask_high, "ask_high", open_time)?,
            ask_low: decimal_to_milli(r.ask_low, "ask_low", open_time)?,
            ask_close: decimal_to_milli(r.ask_close, "ask_close", open_time)?,
        })
    }
}

/// `sim_candles` へ主キー upsert する。同じ `open_time` を再投入すると値が
/// 更新される(spec 5.2 章: 同じ日付を再取得しても結果は変わらない)。
/// バッチ全体を 1 トランザクションにまとめ、途中で失敗した場合に半端な
/// 行だけが保存された状態を残さない。
pub async fn upsert_bars(pool: &PgPool, bars: &[Bar]) -> Result<u64, SimError> {
    if bars.is_empty() {
        return Ok(0);
    }
    let mut tx = pool.begin().await?;
    let mut affected = 0u64;
    for bar in bars {
        let open_time = epoch_secs_to_datetime(bar.open_time, "upsert_bars")?;
        let result = sqlx::query(
            r#"
            INSERT INTO sim_candles
                (exchange, pair, timeframe, open_time,
                 bid_open, bid_high, bid_low, bid_close,
                 ask_open, ask_high, ask_low, ask_close)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (exchange, pair, timeframe, open_time) DO UPDATE SET
                bid_open = EXCLUDED.bid_open,
                bid_high = EXCLUDED.bid_high,
                bid_low = EXCLUDED.bid_low,
                bid_close = EXCLUDED.bid_close,
                ask_open = EXCLUDED.ask_open,
                ask_high = EXCLUDED.ask_high,
                ask_low = EXCLUDED.ask_low,
                ask_close = EXCLUDED.ask_close
            "#,
        )
        .bind(SIM_EXCHANGE)
        .bind(SIM_PAIR)
        .bind(SIM_TIMEFRAME)
        .bind(open_time)
        .bind(milli_to_decimal(bar.bid_open))
        .bind(milli_to_decimal(bar.bid_high))
        .bind(milli_to_decimal(bar.bid_low))
        .bind(milli_to_decimal(bar.bid_close))
        .bind(milli_to_decimal(bar.ask_open))
        .bind(milli_to_decimal(bar.ask_high))
        .bind(milli_to_decimal(bar.ask_low))
        .bind(milli_to_decimal(bar.ask_close))
        .execute(&mut *tx)
        .await?;
        affected += result.rows_affected();
    }
    tx.commit().await?;
    Ok(affected)
}

/// `open_time` 昇順で足を読み込む。`to_exclusive` を指定すると
/// `open_time < to_exclusive` の範囲だけを返す。
pub async fn load_bars(pool: &PgPool, to_exclusive: Option<i64>) -> Result<Vec<Bar>, SimError> {
    let rows: Vec<CandleRow> = match to_exclusive {
        Some(t) => {
            let to_dt = epoch_secs_to_datetime(t, "load_bars to_exclusive")?;
            sqlx::query_as::<_, CandleRow>(
                r#"
                SELECT open_time, bid_open, bid_high, bid_low, bid_close,
                       ask_open, ask_high, ask_low, ask_close
                FROM sim_candles
                WHERE exchange = $1 AND pair = $2 AND timeframe = $3 AND open_time < $4
                ORDER BY open_time ASC
                "#,
            )
            .bind(SIM_EXCHANGE)
            .bind(SIM_PAIR)
            .bind(SIM_TIMEFRAME)
            .bind(to_dt)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, CandleRow>(
                r#"
                SELECT open_time, bid_open, bid_high, bid_low, bid_close,
                       ask_open, ask_high, ask_low, ask_close
                FROM sim_candles
                WHERE exchange = $1 AND pair = $2 AND timeframe = $3
                ORDER BY open_time ASC
                "#,
            )
            .bind(SIM_EXCHANGE)
            .bind(SIM_PAIR)
            .bind(SIM_TIMEFRAME)
            .fetch_all(pool)
            .await?
        }
    };

    rows.into_iter().map(Bar::try_from).collect()
}
