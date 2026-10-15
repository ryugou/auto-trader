use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::error::SimError;
use crate::types::{
    Bar, DB_ACQUIRE_TIMEOUT, DB_MAX_CONNECTIONS, REQUIRED_TABLES, SIM_EXCHANGE, SIM_PAIR,
    SIM_TIMEFRAME,
};

pub async fn connect(database_url: &str) -> Result<PgPool, SimError> {
    PgPoolOptions::new()
        .max_connections(DB_MAX_CONNECTIONS)
        .acquire_timeout(DB_ACQUIRE_TIMEOUT)
        .connect(database_url)
        .await
        .map_err(SimError::Db)
}

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

fn milli_to_decimal(v: i64) -> Decimal {
    Decimal::new(v, 3)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MilliScaleRejection {
    Fractional { scaled: Decimal },
    Overflow,
}

pub(crate) fn decimal_to_milli(d: Decimal) -> Result<i64, MilliScaleRejection> {
    let scaled = d * Decimal::from(1000);
    if !scaled.fract().is_zero() {
        return Err(MilliScaleRejection::Fractional { scaled });
    }
    scaled.to_i64().ok_or(MilliScaleRejection::Overflow)
}

fn decimal_to_milli_rejecting_fraction(
    d: Decimal,
    field: &'static str,
    open_time: i64,
) -> Result<i64, SimError> {
    decimal_to_milli(d).map_err(|rejection| match rejection {
        MilliScaleRejection::Fractional { scaled } => SimError::Other(anyhow::anyhow!(
            "invalid sim_candles field \"{field}\": value {d} at open_time {open_time} does not scale to an integer milli-yen amount (scaled={scaled})"
        )),
        MilliScaleRejection::Overflow => SimError::Other(anyhow::anyhow!(
            "invalid sim_candles field \"{field}\": value {d} at open_time {open_time} overflows i64 at milli-yen scale"
        )),
    })
}

pub(crate) fn format_epoch_secs_rfc3339(secs: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp(secs, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

pub(crate) fn epoch_secs_to_datetime(secs: i64, context: &str) -> Result<DateTime<Utc>, SimError> {
    Utc.timestamp_opt(secs, 0).single().ok_or_else(|| {
        SimError::Other(anyhow::anyhow!(
            "invalid epoch seconds: {secs} is not a representable UTC instant (context={context})"
        ))
    })
}

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
            bid_open: decimal_to_milli_rejecting_fraction(r.bid_open, "bid_open", open_time)?,
            bid_high: decimal_to_milli_rejecting_fraction(r.bid_high, "bid_high", open_time)?,
            bid_low: decimal_to_milli_rejecting_fraction(r.bid_low, "bid_low", open_time)?,
            bid_close: decimal_to_milli_rejecting_fraction(r.bid_close, "bid_close", open_time)?,
            ask_open: decimal_to_milli_rejecting_fraction(r.ask_open, "ask_open", open_time)?,
            ask_high: decimal_to_milli_rejecting_fraction(r.ask_high, "ask_high", open_time)?,
            ask_low: decimal_to_milli_rejecting_fraction(r.ask_low, "ask_low", open_time)?,
            ask_close: decimal_to_milli_rejecting_fraction(r.ask_close, "ask_close", open_time)?,
        })
    }
}

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

#[cfg(test)]
mod tests;
