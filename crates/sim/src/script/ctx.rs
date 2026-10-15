use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use rhai::{Dynamic, Engine, EvalAltResult};

use crate::data::epoch_secs_to_datetime;
use crate::indicators::{IndicatorKey, IndicatorKind, IndicatorSeries};
use crate::series::{Dataset, Tf};
use crate::types::{M5_SECS, milli_to_pips};

const MAX_INDICATOR_KEYS_PER_RUN: usize = 64;

#[derive(Debug, Clone, Copy)]
enum OhlcField {
    Open,
    High,
    Low,
    Close,
}

#[derive(Clone)]
pub(super) struct RhaiCtx {
    dataset: Arc<Dataset>,
    t: usize,
    position: i8,
    entry_price_milli: i64,
    bars_held: u32,
    requested_keys: Arc<Mutex<HashSet<IndicatorKey>>>,
}

impl RhaiCtx {
    pub(super) fn new(
        dataset: Arc<Dataset>,
        t: usize,
        position: i8,
        entry_price_milli: i64,
        bars_held: u32,
        requested_keys: Arc<Mutex<HashSet<IndicatorKey>>>,
    ) -> RhaiCtx {
        RhaiCtx {
            dataset,
            t,
            position,
            entry_price_milli,
            bars_held,
            requested_keys,
        }
    }

    fn bar(&self) -> &crate::types::Bar {
        &self.dataset.bars()[self.t]
    }

    fn unrealized_pips(&self) -> f64 {
        if self.position == 0 {
            return 0.0;
        }
        let bar = self.bar();
        let diff_milli = if self.position == 1 {
            bar.bid_close - self.entry_price_milli
        } else {
            self.entry_price_milli - bar.ask_close
        };
        milli_to_pips(diff_milli)
    }

    fn end_time(&self) -> Result<i64, Box<EvalAltResult>> {
        bar_end_time(self.bar().open_time)
    }

    fn hour(&self) -> Result<i64, Box<EvalAltResult>> {
        use chrono::Timelike;
        Ok(end_time_to_utc(self.end_time()?)?.hour() as i64)
    }

    fn weekday(&self) -> Result<i64, Box<EvalAltResult>> {
        use chrono::Datelike;
        Ok(end_time_to_utc(self.end_time()?)?
            .weekday()
            .num_days_from_monday() as i64)
    }

    fn spread_pips(&self) -> f64 {
        let bar = self.bar();
        milli_to_pips(bar.ask_close - bar.bid_close)
    }

    fn parse_tf(tf: &str) -> Result<Tf, Box<EvalAltResult>> {
        Tf::parse(tf).ok_or_else(|| {
            format!("unsupported timeframe: \"{tf}\" (expected M5, M15, H1, or H4)").into()
        })
    }

    fn check_period(period: i64) -> Result<u32, Box<EvalAltResult>> {
        if !(1..=1000).contains(&period) {
            return Err("invalid period: must be between 1 and 1000".into());
        }
        Ok(period as u32)
    }

    fn check_shift(shift: i64) -> Result<usize, Box<EvalAltResult>> {
        if !(0..=1000).contains(&shift) {
            return Err("invalid shift: must be between 0 and 1000".into());
        }
        Ok(shift as usize)
    }

    fn check_mult(mult: f64) -> Result<u32, Box<EvalAltResult>> {
        let mult_x100 = (mult * 100.0).round();
        if !(10.0..=1000.0).contains(&mult_x100) {
            return Err("invalid mult: must round to between 0.1 and 10.0".into());
        }
        Ok(mult_x100 as u32)
    }

    fn resolve_index(&self, tf: Tf, shift: usize) -> Option<usize> {
        let completed = self.dataset.completed(tf, self.t);
        if shift >= completed {
            return None;
        }
        Some(completed - 1 - shift)
    }

    fn track_key(&self, key: IndicatorKey) -> Result<(), Box<EvalAltResult>> {
        let mut guard = self.requested_keys.lock().map_err(|_| {
            Box::<EvalAltResult>::from(
                "invalid indicator tracking state: requested_keys mutex poisoned",
            )
        })?;
        if !guard.contains(&key) && guard.len() >= MAX_INDICATOR_KEYS_PER_RUN {
            return Err(format!(
                "invalid indicator key count: already at the {MAX_INDICATOR_KEYS_PER_RUN}-key maximum; cannot request another"
            )
            .into());
        }
        guard.insert(key);
        Ok(())
    }

    fn ohlc(&self, tf: &str, shift: i64, field: OhlcField) -> Result<Dynamic, Box<EvalAltResult>> {
        let tf = Self::parse_tf(tf)?;
        let shift = Self::check_shift(shift)?;
        let Some(idx) = self.resolve_index(tf, shift) else {
            return Ok(Dynamic::UNIT);
        };
        let series = self.dataset.series(tf);
        let value = match field {
            OhlcField::Open => series.open[idx],
            OhlcField::High => series.high[idx],
            OhlcField::Low => series.low[idx],
            OhlcField::Close => series.close[idx],
        };
        Ok(Dynamic::from(value))
    }

    fn single_indicator(
        &self,
        tf: &str,
        kind: IndicatorKind,
        period: i64,
        shift: i64,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let tf_v = Self::parse_tf(tf)?;
        let period_v = Self::check_period(period)?;
        let shift_v = Self::check_shift(shift)?;
        let key = IndicatorKey {
            tf: tf_v,
            kind,
            period: period_v,
            mult_x100: 0,
        };
        self.track_key(key)?;
        let Some(idx) = self.resolve_index(tf_v, shift_v) else {
            return Ok(Dynamic::UNIT);
        };
        let series = self.dataset.indicator(key);
        let IndicatorSeries::Single(values) = &*series else {
            unreachable!("compute() always returns Single for this IndicatorKind")
        };
        let v = values[idx];
        Ok(if v.is_nan() {
            Dynamic::UNIT
        } else {
            Dynamic::from(v)
        })
    }

    fn channel_indicator(
        &self,
        tf: &str,
        period: i64,
        shift: i64,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let tf_v = Self::parse_tf(tf)?;
        let period_v = Self::check_period(period)?;
        let shift_v = Self::check_shift(shift)?;
        let key = IndicatorKey {
            tf: tf_v,
            kind: IndicatorKind::Donchian,
            period: period_v,
            mult_x100: 0,
        };
        self.track_key(key)?;
        let Some(idx) = self.resolve_index(tf_v, shift_v) else {
            return Ok(Dynamic::UNIT);
        };
        let series = self.dataset.indicator(key);
        let IndicatorSeries::Channel { lower, upper } = &*series else {
            unreachable!("compute() always returns Channel for IndicatorKind::Donchian")
        };
        if lower[idx].is_nan() || upper[idx].is_nan() {
            return Ok(Dynamic::UNIT);
        }
        let mut map = rhai::Map::new();
        map.insert("upper".into(), Dynamic::from(upper[idx]));
        map.insert("lower".into(), Dynamic::from(lower[idx]));
        Ok(Dynamic::from_map(map))
    }

    fn band_indicator(
        &self,
        tf: &str,
        kind: IndicatorKind,
        period: i64,
        mult: f64,
        shift: i64,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let tf_v = Self::parse_tf(tf)?;
        let period_v = Self::check_period(period)?;
        let shift_v = Self::check_shift(shift)?;
        let mult_x100 = Self::check_mult(mult)?;
        let key = IndicatorKey {
            tf: tf_v,
            kind,
            period: period_v,
            mult_x100,
        };
        self.track_key(key)?;
        let Some(idx) = self.resolve_index(tf_v, shift_v) else {
            return Ok(Dynamic::UNIT);
        };
        let series = self.dataset.indicator(key);
        let IndicatorSeries::Band {
            lower,
            middle,
            upper,
        } = &*series
        else {
            unreachable!("compute() always returns Band for Bb/Keltner")
        };
        if lower[idx].is_nan() {
            return Ok(Dynamic::UNIT);
        }
        let mut map = rhai::Map::new();
        map.insert("upper".into(), Dynamic::from(upper[idx]));
        map.insert("middle".into(), Dynamic::from(middle[idx]));
        map.insert("lower".into(), Dynamic::from(lower[idx]));
        Ok(Dynamic::from_map(map))
    }
}

pub(super) fn bar_end_time(open_time: i64) -> Result<i64, Box<EvalAltResult>> {
    open_time
        .checked_add(M5_SECS)
        .ok_or_else(|| "invalid ctx.time: open_time plus the bar length overflows i64".into())
}

pub(super) fn end_time_to_utc(
    end_time: i64,
) -> Result<chrono::DateTime<chrono::Utc>, Box<EvalAltResult>> {
    epoch_secs_to_datetime(end_time, "ctx.time").map_err(|e| e.to_string().into())
}

pub(super) fn register_ctx(engine: &mut Engine) {
    engine.register_type_with_name::<RhaiCtx>("Ctx");
    register_scalar_getters(engine);
    register_ohlc_fns(engine);
    register_single_value_indicator_fns(engine);
    register_donchian_fn(engine);
    register_band_indicator_fns(engine);
}

fn register_scalar_getters(engine: &mut Engine) {
    engine.register_get("position", |ctx: &mut RhaiCtx| ctx.position as i64);
    engine.register_get("entry_price", |ctx: &mut RhaiCtx| {
        if ctx.position == 0 {
            0.0
        } else {
            ctx.entry_price_milli as f64 / 1000.0
        }
    });
    engine.register_get("bars_held", |ctx: &mut RhaiCtx| ctx.bars_held as i64);
    engine.register_get("unrealized_pips", |ctx: &mut RhaiCtx| ctx.unrealized_pips());
    engine.register_get("time", |ctx: &mut RhaiCtx| ctx.end_time());
    engine.register_get("hour", |ctx: &mut RhaiCtx| ctx.hour());
    engine.register_get("weekday", |ctx: &mut RhaiCtx| ctx.weekday());
    engine.register_get("spread", |ctx: &mut RhaiCtx| ctx.spread_pips());
}

fn register_ohlc_fns(engine: &mut Engine) {
    engine.register_fn("open", |ctx: &mut RhaiCtx, tf: &str, shift: i64| {
        ctx.ohlc(tf, shift, OhlcField::Open)
    });
    engine.register_fn("high", |ctx: &mut RhaiCtx, tf: &str, shift: i64| {
        ctx.ohlc(tf, shift, OhlcField::High)
    });
    engine.register_fn("low", |ctx: &mut RhaiCtx, tf: &str, shift: i64| {
        ctx.ohlc(tf, shift, OhlcField::Low)
    });
    engine.register_fn("close", |ctx: &mut RhaiCtx, tf: &str, shift: i64| {
        ctx.ohlc(tf, shift, OhlcField::Close)
    });
}

fn register_single_value_indicator_fns(engine: &mut Engine) {
    engine.register_fn(
        "sma",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, shift: i64| {
            ctx.single_indicator(tf, IndicatorKind::Sma, period, shift)
        },
    );
    engine.register_fn(
        "ema",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, shift: i64| {
            ctx.single_indicator(tf, IndicatorKind::Ema, period, shift)
        },
    );
    engine.register_fn(
        "rsi",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, shift: i64| {
            ctx.single_indicator(tf, IndicatorKind::Rsi, period, shift)
        },
    );
    engine.register_fn(
        "atr",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, shift: i64| {
            ctx.single_indicator(tf, IndicatorKind::Atr, period, shift)
        },
    );
    engine.register_fn(
        "adx",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, shift: i64| {
            ctx.single_indicator(tf, IndicatorKind::Adx, period, shift)
        },
    );
}

fn register_donchian_fn(engine: &mut Engine) {
    engine.register_fn(
        "donchian",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, shift: i64| {
            ctx.channel_indicator(tf, period, shift)
        },
    );
}

fn register_band_indicator_fns(engine: &mut Engine) {
    engine.register_fn(
        "bb",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, mult: i64, shift: i64| {
            ctx.band_indicator(tf, IndicatorKind::Bb, period, mult as f64, shift)
        },
    );
    engine.register_fn(
        "bb",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, mult: f64, shift: i64| {
            ctx.band_indicator(tf, IndicatorKind::Bb, period, mult, shift)
        },
    );
    engine.register_fn(
        "keltner",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, mult: i64, shift: i64| {
            ctx.band_indicator(tf, IndicatorKind::Keltner, period, mult as f64, shift)
        },
    );
    engine.register_fn(
        "keltner",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, mult: f64, shift: i64| {
            ctx.band_indicator(tf, IndicatorKind::Keltner, period, mult, shift)
        },
    );
}
