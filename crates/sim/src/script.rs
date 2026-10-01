//! スクリプトのコンパイル、検証、隔離実行（spec 8 章）。
//!
//! エンジンの構成は spec 8.3 のとおり: `Engine::new_raw()` を基点に、算術・論理・基本の
//! 数学・反復・配列・マップ・文字列の 7 パッケージだけを登録する。`LanguageCorePackage`
//! （`eval`・`sleep` 等）、`BasicTimePackage`（`timestamp` 等）、`BasicFnPackage`、
//! `BasicBlobPackage`、`DebuggingPackage` は登録しない。`import` は `DummyModuleResolver` で
//! 失敗させ、`print`/`debug` ハンドラは設定しない。
//!
//! `eval` キーワードは `LanguageCorePackage` のような「パッケージ未登録なら使えない関数」では
//! なく、エンジンに組み込みの特殊構文として常に有効である（`rhai` の `func/call.rs` の
//! `KEYWORD_EVAL` 分岐）。そのため 8.3 の構成（パッケージ・リゾルバ・上限）だけでは `eval` は
//! 塞がれない。spec 8.4 は `eval("1")` を使うスクリプトが失敗することを明示的に要求しており、
//! それを満たすには `Engine::disable_symbol("eval")` が必要になる（rhai 公式の sandboxing ガイド
//! が推奨する標準的な手段でもある）。spec 8.3 の構成一覧には明記されていないが、8.4 の要求を
//! 満たすための必要な追加として実装する。
//!
//! 演算数の上限（spec 8.3）は `Engine::on_progress` で実装する。`on_progress` のクロージャは
//! `ScriptHost`（共有の `Engine`）に 1 度だけ登録するため、呼び出しごと・シミュレーションごとの
//! 累計という「呼び出し元ごとに違う状態」を直接キャプチャできない。spec 9.2 の
//! 「1 回のシミュレーションは 1 つのスレッド上で最初から最後まで実行する」という保証を使い、
//! スレッドローカルな状態を中継点にする: `ScriptRun` は呼び出しの直前にスレッドローカルへ
//! 自分の上限値と、ここまでの累計演算数をセットし、呼び出し直後にそのスレッドローカルから
//! 「この呼び出しで消費した演算数」を読み出して `ScriptRun` 自身のフィールド（真の累計）に
//! 加算する。累計の真値は常に `ScriptRun` 側が保持し、スレッドローカルは 1 呼び出し分の
//! 受け渡しにしか使わないため、別スレッドで動く複数の `ScriptRun` が混ざることはない。

use crate::config::SimConfig;
use crate::error::SimError;
use crate::indicators::{IndicatorKey, IndicatorKind, IndicatorSeries};
use crate::series::{Dataset, Tf};
use crate::types::{M5_SECS, milli_to_pips};
use rhai::module_resolvers::DummyModuleResolver;
use rhai::packages::{
    ArithmeticPackage, BasicArrayPackage, BasicIteratorPackage, BasicMapPackage, BasicMathPackage,
    BasicStringPackage, LogicPackage, Package,
};
use rhai::{AST, CallFnOptions, Dynamic, Engine, EvalAltResult, Scope};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

/// スクリプトのソースサイズの上限（spec 8.1）。
const MAX_SCRIPT_BYTES: usize = 32 * 1024;
/// `params()` が定義できるパラメータ数の上限（spec 8.1）。
const MAX_PARAM_COUNT: usize = 12;
/// 関数呼び出しの深さの上限（spec 8.3）。
const MAX_CALL_LEVELS: usize = 16;
/// 文字列長の上限（spec 8.3）。
const MAX_STRING_SIZE: usize = 4096;
/// 配列長の上限（spec 8.3）。
const MAX_ARRAY_SIZE: usize = 1024;
/// マップの要素数の上限（spec 8.3）。
const MAX_MAP_SIZE: usize = 256;
/// 1 回のシミュレーションが要求できる指標キーの種類の上限（spec 6.3）。
const MAX_INDICATOR_KEYS_PER_RUN: usize = 64;
/// 小数の刻み判定の許容誤差（spec 8.1、13 章）。
const FLOAT_TOLERANCE: f64 = 1e-9;

/// `value` が `min + k × step`（`k` は 0 以上の整数）と `FLOAT_TOLERANCE` 以内で一致するか。
/// 許容誤差は商 `k` ではなく値の差に適用する（spec 8.1 章）。
fn is_on_float_grid(value: f64, min: f64, step: f64) -> bool {
    let k = ((value - min) / step).round();
    k >= 0.0 && (value - (min + k * step)).abs() <= FLOAT_TOLERANCE
}

// ---------------------------------------------------------------------------
// パラメータ
// ---------------------------------------------------------------------------

/// パラメータの型（spec 8.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Int,
    Float,
}

/// `params()` が返す 1 パラメータの仕様（spec 8.1）。`min`/`max`/`step`/`default` は
/// `ParamKind` によらずすべて `f64` で保持する（整数パラメータの値も、呼び出し側が
/// `ParamKind::Int` を見て丸める）。
#[derive(Debug, Clone, PartialEq)]
pub struct ParamSpec {
    pub name: String,
    pub kind: ParamKind,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub default: f64,
}

/// 1 パラメータの実際の値（spec 13 章の `--params` 解決結果）。
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Int(i64),
    Float(f64),
}

pub type ParamSet = BTreeMap<String, ParamValue>;

// ---------------------------------------------------------------------------
// 演算数の追跡（スレッドローカル、設計意図は本ファイル冒頭のコメントを参照）
// ---------------------------------------------------------------------------

#[derive(Default)]
struct OpsRelay {
    /// この呼び出しが始まる前までの、シミュレーション全体の累計演算数。
    run_total_before_call: u64,
    /// シミュレーション全体の演算数の上限（`max_operations_per_run`）。
    run_max_total: u64,
    /// 1 回の呼び出しの演算数の上限（`max_operations_per_bar`）。
    bar_max: u64,
    /// 直近に `on_progress` から渡された、この呼び出し内の累計演算数。
    last_call_ops: u64,
}

thread_local! {
    static OPS_RELAY: RefCell<OpsRelay> = RefCell::new(OpsRelay::default());
}

/// 呼び出し前にスレッドローカルの状態を初期化する。
fn reset_ops_relay(run_total_before_call: u64, run_max_total: u64, bar_max: u64) {
    OPS_RELAY.with(|relay| {
        *relay.borrow_mut() = OpsRelay {
            run_total_before_call,
            run_max_total,
            bar_max,
            last_call_ops: 0,
        };
    });
}

/// 直前の呼び出しで消費された演算数を読み出す。
fn take_last_call_ops() -> u64 {
    OPS_RELAY.with(|relay| relay.borrow().last_call_ops)
}

/// `Engine::on_progress` に登録するクロージャ本体。
fn check_progress(ops: u64) -> Option<Dynamic> {
    OPS_RELAY.with(|relay| {
        let mut relay = relay.borrow_mut();
        relay.last_call_ops = ops;
        if ops > relay.bar_max {
            return Some(Dynamic::from(format!(
                "operation limit exceeded: {ops} operations in a single call (max_operations_per_bar={})",
                relay.bar_max
            )));
        }
        let run_total = relay.run_total_before_call.saturating_add(ops);
        if run_total > relay.run_max_total {
            return Some(Dynamic::from(format!(
                "operation limit exceeded: {run_total} total operations this simulation (max_operations_per_run={})",
                relay.run_max_total
            )));
        }
        None
    })
}

/// `rhai` のエラーを人間が読めるメッセージに変換する。`ErrorTerminated` は
/// `check_progress` が積んだ独自メッセージを取り出し、それ以外は `rhai` の標準の
/// `Display`（位置情報を含む）をそのまま使う。
fn describe_rhai_error(err: &EvalAltResult) -> String {
    match err {
        EvalAltResult::ErrorTerminated(token, pos) => {
            let detail = token
                .clone()
                .into_immutable_string()
                .map(|s| s.to_string())
                .unwrap_or_else(|_| "script terminated (operation limit exceeded)".to_string());
            if pos.is_none() {
                detail
            } else {
                format!("{detail} ({pos})")
            }
        }
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// CompiledScript / ScriptHost
// ---------------------------------------------------------------------------

/// 登録済みのスクリプト（spec 8.1 の検証を通過したもの）。
pub struct CompiledScript {
    pub source: String,
    /// ソースの UTF-8 バイト列の SHA-256（小文字の 16 進）。
    pub sha256: String,
    /// `params()` から抽出したパラメータ仕様。名前の辞書順。
    pub params: Vec<ParamSpec>,
    ast: AST,
}

impl CompiledScript {
    /// すべてのパラメータを既定値にした `ParamSet`。
    pub fn default_params(&self) -> ParamSet {
        self.params
            .iter()
            .map(|p| {
                let value = match p.kind {
                    ParamKind::Int => ParamValue::Int(p.default.round() as i64),
                    ParamKind::Float => ParamValue::Float(p.default),
                };
                (p.name.clone(), value)
            })
            .collect()
    }

    /// spec 13 章の `--params` の規則で `given` を検証し、指定のないパラメータを既定値で
    /// 埋めた `ParamSet` を返す。未知の名前、範囲外、刻み外、型の不一致は `SimError::Args`。
    pub fn resolve_params(
        &self,
        given: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ParamSet, SimError> {
        let known: HashSet<&str> = self.params.iter().map(|p| p.name.as_str()).collect();
        for key in given.keys() {
            if !known.contains(key.as_str()) {
                return Err(SimError::Args(format!("unknown parameter '{key}'")));
            }
        }

        let mut result = ParamSet::new();
        for spec in &self.params {
            let value = match given.get(&spec.name) {
                None => match spec.kind {
                    ParamKind::Int => ParamValue::Int(spec.default.round() as i64),
                    ParamKind::Float => ParamValue::Float(spec.default),
                },
                Some(json_value) => resolve_one_param(spec, json_value)?,
            };
            result.insert(spec.name.clone(), value);
        }
        Ok(result)
    }
}

/// `given` の 1 つの値を `spec` に照らして検証する。
fn resolve_one_param(
    spec: &ParamSpec,
    json_value: &serde_json::Value,
) -> Result<ParamValue, SimError> {
    match spec.kind {
        ParamKind::Int => {
            // JSON の数値に小数点がある場合 (`30.0` 等)、serde_json はそれを f64 表現として
            // 保持し `as_i64()` は None を返す。これにより「整数パラメータへの小数指定」が
            // 自然に型の不一致として弾かれる(spec 13 章)。
            let n = json_value.as_i64().ok_or_else(|| {
                SimError::Args(format!(
                    "parameter '{}' must be an integer, got {json_value}",
                    spec.name
                ))
            })?;
            let (min, max, step) = (spec.min as i64, spec.max as i64, spec.step as i64);
            if n < min || n > max {
                return Err(SimError::Args(format!(
                    "parameter '{}' = {n} is out of range [{min}, {max}]",
                    spec.name
                )));
            }
            // n - min は i64 でオーバーフローしうるため i128 で計算する。
            if step != 0 && (n as i128 - min as i128) % step as i128 != 0 {
                return Err(SimError::Args(format!(
                    "parameter '{}' = {n} is not on the step grid (min={min}, step={step})",
                    spec.name
                )));
            }
            Ok(ParamValue::Int(n))
        }
        ParamKind::Float => {
            // JSON の整数値は f64 として受け付ける(spec 13 章: 小数パラメータへの JSON の
            // 整数は小数として受け付ける)。
            let f = json_value.as_f64().ok_or_else(|| {
                SimError::Args(format!(
                    "parameter '{}' must be a number, got {json_value}",
                    spec.name
                ))
            })?;
            if f < spec.min - FLOAT_TOLERANCE || f > spec.max + FLOAT_TOLERANCE {
                return Err(SimError::Args(format!(
                    "parameter '{}' = {f} is out of range [{}, {}]",
                    spec.name, spec.min, spec.max
                )));
            }
            if !is_on_float_grid(f, spec.min, spec.step) {
                return Err(SimError::Args(format!(
                    "parameter '{}' = {f} is not on the step grid (min={}, step={})",
                    spec.name, spec.min, spec.step
                )));
            }
            Ok(ParamValue::Float(f))
        }
    }
}

/// `ParamSet` を `on_bar(ctx, p)` の `p` に渡す `rhai::Map` に変換する。
fn param_set_to_rhai_map(params: &ParamSet) -> rhai::Map {
    params
        .iter()
        .map(|(name, value)| {
            let dynamic = match value {
                ParamValue::Int(n) => Dynamic::from(*n),
                ParamValue::Float(f) => Dynamic::from(*f),
            };
            (name.as_str().into(), dynamic)
        })
        .collect()
}

/// サンドボックス化された `rhai::Engine` を保持する（spec 8.3）。`Engine` は `sync`
/// feature により `Send + Sync` であり、複数の `ScriptRun` から共有して使える。
pub struct ScriptHost {
    engine: Engine,
    max_operations_per_bar: u64,
}

impl ScriptHost {
    /// spec 8.3 のとおりにエンジンを構成する。
    pub fn new(cfg: &SimConfig) -> ScriptHost {
        let mut engine = Engine::new_raw();

        ArithmeticPackage::new().register_into_engine(&mut engine);
        LogicPackage::new().register_into_engine(&mut engine);
        BasicMathPackage::new().register_into_engine(&mut engine);
        BasicIteratorPackage::new().register_into_engine(&mut engine);
        BasicArrayPackage::new().register_into_engine(&mut engine);
        BasicMapPackage::new().register_into_engine(&mut engine);
        BasicStringPackage::new().register_into_engine(&mut engine);

        engine.set_module_resolver(DummyModuleResolver::new());
        // eval は組み込みキーワードでパッケージに依存しないため、明示的に無効化する
        // (本ファイル冒頭のコメントを参照)。
        engine.disable_symbol("eval");

        engine.set_max_call_levels(MAX_CALL_LEVELS);
        engine.set_max_string_size(MAX_STRING_SIZE);
        engine.set_max_array_size(MAX_ARRAY_SIZE);
        engine.set_max_map_size(MAX_MAP_SIZE);

        engine.on_progress(check_progress);

        register_ctx(&mut engine);

        ScriptHost {
            engine,
            max_operations_per_bar: cfg.max_operations_per_bar,
        }
    }

    /// spec 8.1 の検証を行い、`CompiledScript` を返す。違反は `SimError::InvalidScript`。
    pub fn compile(&self, source: &str) -> Result<CompiledScript, SimError> {
        if source.len() > MAX_SCRIPT_BYTES {
            return Err(SimError::InvalidScript(format!(
                "script source is {} bytes, exceeding the {MAX_SCRIPT_BYTES}-byte limit",
                source.len()
            )));
        }

        let ast = self
            .engine
            .compile(source)
            .map_err(|e| SimError::InvalidScript(format!("parse error: {e}")))?;

        let mut has_params_fn = false;
        let mut has_on_bar_fn = false;
        for f in ast.iter_functions() {
            if f.name == "params" && f.params.is_empty() {
                has_params_fn = true;
            }
            if f.name == "on_bar" && f.params.len() == 2 {
                has_on_bar_fn = true;
            }
        }
        if !has_params_fn {
            return Err(SimError::InvalidScript(
                "script must define `fn params()` with 0 arguments".to_string(),
            ));
        }
        if !has_on_bar_fn {
            return Err(SimError::InvalidScript(
                "script must define `fn on_bar(ctx, p)` with 2 arguments".to_string(),
            ));
        }

        // params() は max_operations_per_bar の上限のもとで呼ぶ(spec 8.1)。このコンパイル時
        // 呼び出しは特定のシミュレーションに属さないため、シミュレーション全体の上限
        // (max_operations_per_run) は適用しない(run_max_total を事実上無制限にする)。
        reset_ops_relay(0, u64::MAX, self.max_operations_per_bar);
        let mut scope = Scope::new();
        let options = CallFnOptions::new().eval_ast(false);
        let map: rhai::Map = self
            .engine
            .call_fn_with_options(options, &mut scope, &ast, "params", ())
            .map_err(|e| {
                SimError::InvalidScript(format!("params() failed: {}", describe_rhai_error(&e)))
            })?;

        let params = parse_param_specs(&map)?;

        let sha256 = {
            let digest = Sha256::digest(source.as_bytes());
            hex::encode(digest)
        };

        Ok(CompiledScript {
            source: source.to_string(),
            sha256,
            params,
            ast,
        })
    }
}

/// `params()` の戻り値（`rhai::Map`）を検証し、`Vec<ParamSpec>`（名前の辞書順）にする
/// (spec 8.1 の登録時の検証)。
fn parse_param_specs(map: &rhai::Map) -> Result<Vec<ParamSpec>, SimError> {
    if map.len() > MAX_PARAM_COUNT {
        return Err(SimError::InvalidScript(format!(
            "params() defines {} parameters, exceeding the {MAX_PARAM_COUNT}-parameter limit",
            map.len()
        )));
    }

    let mut specs = Vec::with_capacity(map.len());
    for (name, value) in map.iter() {
        let name = name.to_string();
        let spec_map = value.clone().try_cast::<rhai::Map>().ok_or_else(|| {
            SimError::InvalidScript(format!(
                "params().{name} must be a map with min/max/step/default"
            ))
        })?;

        let field = |key: &str| -> Result<&Dynamic, SimError> {
            spec_map.get(key).ok_or_else(|| {
                SimError::InvalidScript(format!("params().{name} is missing '{key}'"))
            })
        };
        let min_d = field("min")?;
        let max_d = field("max")?;
        let step_d = field("step")?;
        let default_d = field("default")?;

        let all_int = min_d.is_int() && max_d.is_int() && step_d.is_int() && default_d.is_int();
        let all_float =
            min_d.is_float() && max_d.is_float() && step_d.is_float() && default_d.is_float();
        if !all_int && !all_float {
            return Err(SimError::InvalidScript(format!(
                "params().{name}: min/max/step/default must all be integers or all be floats"
            )));
        }

        let (kind, min, max, step, default) = if all_int {
            let min_i = min_d.as_int().expect("checked is_int above");
            let max_i = max_d.as_int().expect("checked is_int above");
            let step_i = step_d.as_int().expect("checked is_int above");
            let default_i = default_d.as_int().expect("checked is_int above");
            if !(min_i <= default_i && default_i <= max_i) {
                return Err(SimError::InvalidScript(format!(
                    "params().{name}: must satisfy min <= default <= max (min={min_i}, default={default_i}, max={max_i})"
                )));
            }
            if step_i <= 0 {
                return Err(SimError::InvalidScript(format!(
                    "params().{name}: step must be > 0, got {step_i}"
                )));
            }
            // i64 同士の差は min=i64::MIN, default=i64::MAX 等でオーバーフローするため i128 で計算する。
            if (default_i as i128 - min_i as i128) % step_i as i128 != 0 {
                return Err(SimError::InvalidScript(format!(
                    "params().{name}: default must equal min + k*step for a non-negative integer k (min={min_i}, step={step_i}, default={default_i})"
                )));
            }
            (
                ParamKind::Int,
                min_i as f64,
                max_i as f64,
                step_i as f64,
                default_i as f64,
            )
        } else {
            let min_f = min_d.as_float().expect("checked is_float above");
            let max_f = max_d.as_float().expect("checked is_float above");
            let step_f = step_d.as_float().expect("checked is_float above");
            let default_f = default_d.as_float().expect("checked is_float above");
            // rhai の f64 除算は未検査で NaN / inf を作れる。比較が常に false になり検証を
            // すり抜けるため、最初に有限性を要求する。
            if !(min_f.is_finite()
                && max_f.is_finite()
                && step_f.is_finite()
                && default_f.is_finite())
            {
                return Err(SimError::InvalidScript(format!(
                    "params().{name}: min/max/step/default must be finite (min={min_f}, max={max_f}, step={step_f}, default={default_f})"
                )));
            }
            if !(min_f <= default_f && default_f <= max_f) {
                return Err(SimError::InvalidScript(format!(
                    "params().{name}: must satisfy min <= default <= max (min={min_f}, default={default_f}, max={max_f})"
                )));
            }
            // 有限性は上で保証済みなので NaN は到達しない(clippy の neg_cmp_op_on_partial_ord 回避のため <=)。
            if step_f <= 0.0 {
                return Err(SimError::InvalidScript(format!(
                    "params().{name}: step must be > 0, got {step_f}"
                )));
            }
            if !is_on_float_grid(default_f, min_f, step_f) {
                return Err(SimError::InvalidScript(format!(
                    "params().{name}: default must equal min + k*step for a non-negative integer k (value difference within 1e-9) (min={min_f}, step={step_f}, default={default_f})"
                )));
            }
            (ParamKind::Float, min_f, max_f, step_f, default_f)
        };

        specs.push(ParamSpec {
            name,
            kind,
            min,
            max,
            step,
            default,
        });
    }

    // rhai::Map は BTreeMap<_, _> なので反復はすでに名前の辞書順だが、この不変条件に
    // implicit に頼らず明示する(spec 8.1: 名前の辞書順)。
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(specs)
}

// ---------------------------------------------------------------------------
// 実行
// ---------------------------------------------------------------------------

/// `on_bar` 呼び出し時点でのポジション状態（spec 8.2 の `ctx.position` 等の元データ）。
/// `t` は `Dataset::bars()` の添字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarState {
    pub t: usize,
    pub position: i8,
    pub entry_price_milli: i64,
    pub bars_held: u32,
}

/// 1 本の足に対する売買判断（`ScriptRun` の抽象。テストでは手製の実装に差し替えられる）。
pub trait Decider {
    /// 1（買い持ち）、-1（売り持ち）、0（持たない）を返す。`Err` はエラーメッセージ
    /// （呼び出し元が `open_time` 等の文脈を付与して記録する）。
    fn on_bar(&mut self, state: BarState) -> Result<i8, String>;
}

/// 足の OHLC のうちどれを返すか。
#[derive(Debug, Clone, Copy)]
enum OhlcField {
    Open,
    High,
    Low,
    Close,
}

/// `ctx`(spec 8.2) として script に渡す値。`Clone` は `Arc`/`Copy` フィールドだけなので
/// 安価。`sync` feature 下で `rhai` のカスタム型として使うには `Send + Sync + Clone +
/// 'static` が必要で、`Arc<Dataset>` と `Arc<Mutex<_>>` がそれを満たす。
#[derive(Clone)]
struct RhaiCtx {
    dataset: Arc<Dataset>,
    t: usize,
    position: i8,
    entry_price_milli: i64,
    bars_held: u32,
    /// このシミュレーションで要求済みの指標キー(spec 6.3: 1 回のシミュレーションにつき
    /// 64 種類まで)。`ScriptRun` が所有する 1 つの `Arc<Mutex<_>>` を全呼び出しの `RhaiCtx`
    /// が共有するため、シミュレーション全体を通した累計になる。
    requested_keys: Arc<Mutex<HashSet<IndicatorKey>>>,
}

impl RhaiCtx {
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

    /// 足 `t` の終了時刻(UTC エポック秒)。
    fn end_time(&self) -> i64 {
        self.bar().open_time + M5_SECS
    }

    fn hour(&self) -> i64 {
        use chrono::Timelike;
        end_time_to_utc(self.end_time()).hour() as i64
    }

    fn weekday(&self) -> i64 {
        use chrono::Datelike;
        end_time_to_utc(self.end_time())
            .weekday()
            .num_days_from_monday() as i64
    }

    fn spread_pips(&self) -> f64 {
        let bar = self.bar();
        milli_to_pips(bar.ask_close - bar.bid_close)
    }

    fn parse_tf(tf: &str) -> Result<Tf, Box<EvalAltResult>> {
        Tf::parse(tf)
            .ok_or_else(|| format!("unknown timeframe '{tf}' (expected M5, M15, H1, or H4)").into())
    }

    fn check_period(period: i64) -> Result<u32, Box<EvalAltResult>> {
        if !(1..=1000).contains(&period) {
            return Err(format!("period must be between 1 and 1000, got {period}").into());
        }
        Ok(period as u32)
    }

    fn check_shift(shift: i64) -> Result<usize, Box<EvalAltResult>> {
        if !(0..=1000).contains(&shift) {
            return Err(format!("shift must be between 0 and 1000, got {shift}").into());
        }
        Ok(shift as usize)
    }

    /// `mult` を `mult_x100`(10〜1000)に丸める(spec 6.3)。
    fn check_mult(mult: f64) -> Result<u32, Box<EvalAltResult>> {
        let mult_x100 = (mult * 100.0).round();
        if !(10.0..=1000.0).contains(&mult_x100) {
            return Err(format!("mult must round to between 0.1 and 10.0, got {mult}").into());
        }
        Ok(mult_x100 as u32)
    }

    /// 足 `self.t` の時点で完成している `tf` の本数から、`shift` 本前の系列添字を求める。
    /// 本数不足なら `None`(呼び出し元が `()` を返す)。
    fn resolve_index(&self, tf: Tf, shift: usize) -> Option<usize> {
        let completed = self.dataset.completed(tf, self.t);
        if shift >= completed {
            return None;
        }
        Some(completed - 1 - shift)
    }

    /// `key` を要求済みキーに記録する。新規かつ既に 64 種類に達している場合は `Err`
    /// (spec 6.3: この判定はキャッシュの有無に依存しない)。
    fn track_key(&self, key: IndicatorKey) -> Result<(), Box<EvalAltResult>> {
        let mut guard = self
            .requested_keys
            .lock()
            .expect("requested_keys mutex poisoned (a prior panic corrupted run state)");
        if !guard.contains(&key) && guard.len() >= MAX_INDICATOR_KEYS_PER_RUN {
            return Err(format!(
                "this simulation already requested {MAX_INDICATOR_KEYS_PER_RUN} distinct indicator keys (the maximum); cannot request another"
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
            // bb/keltner は lower/middle/upper が同じ添字で揃って NaN になる(indicators.rs の
            // 実装: bb は同じ本数条件、keltner は ema/atr のどちらかが NaN なら全体が NaN)。
            return Ok(Dynamic::UNIT);
        }
        let mut map = rhai::Map::new();
        map.insert("upper".into(), Dynamic::from(upper[idx]));
        map.insert("middle".into(), Dynamic::from(middle[idx]));
        map.insert("lower".into(), Dynamic::from(lower[idx]));
        Ok(Dynamic::from_map(map))
    }
}

/// `end_time`(UTC エポック秒)を `DateTime<Utc>` に変換する。`Bar::open_time` は DB の
/// `TIMESTAMPTZ` 由来の妥当な値であることが `Dataset` 構築時点で保証されているため、
/// 変換の失敗は呼び出し元のデータ不変条件違反であり、ここでは回復しない。
fn end_time_to_utc(end_time: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(end_time, 0)
        .expect("bar end_time must be a valid UTC timestamp (Dataset invariant)")
}

/// `ctx` のプロパティ・メソッドを `engine` に登録する(spec 8.2)。
fn register_ctx(engine: &mut Engine) {
    engine.register_type_with_name::<RhaiCtx>("Ctx");

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

    engine.register_fn(
        "donchian",
        |ctx: &mut RhaiCtx, tf: &str, period: i64, shift: i64| {
            ctx.channel_indicator(tf, period, shift)
        },
    );

    // mult は整数・小数のどちらでも受け付ける(spec 6.3)ため、同名で 2 つのオーバーロードを
    // 登録する(rhai は引数の型で多重定義を解決する)。
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

/// 1 回のシミュレーションにわたる、1 本のスクリプトの実行状態（`this`、演算数の累計、
/// 要求済み指標キー）。`'a` は `ScriptHost`/`CompiledScript` を間借りする期間。
pub struct ScriptRun<'a> {
    host: &'a ScriptHost,
    script: &'a CompiledScript,
    dataset: Arc<Dataset>,
    params_dynamic: Dynamic,
    /// spec 8.1: 状態を保持するマップ。初期値は `#{}`、1 回のシミュレーションの間保持される。
    this: Dynamic,
    requested_keys: Arc<Mutex<HashSet<IndicatorKey>>>,
    max_operations_per_bar: u64,
    max_operations_per_run: u64,
    /// このシミュレーションで `on_bar` の呼び出しが消費した演算数の真の累計
    /// (スレッドローカルはこの値をクロージャへ中継するためだけに使う)。
    run_total_ops: u64,
}

impl<'a> ScriptRun<'a> {
    pub fn new(
        host: &'a ScriptHost,
        script: &'a CompiledScript,
        params: &ParamSet,
        dataset: Arc<Dataset>,
        cfg: &SimConfig,
    ) -> ScriptRun<'a> {
        ScriptRun {
            host,
            script,
            dataset,
            params_dynamic: Dynamic::from_map(param_set_to_rhai_map(params)),
            this: Dynamic::from_map(rhai::Map::new()),
            requested_keys: Arc::new(Mutex::new(HashSet::new())),
            max_operations_per_bar: cfg.max_operations_per_bar,
            max_operations_per_run: cfg.max_operations_per_run,
            run_total_ops: 0,
        }
    }
}

impl Decider for ScriptRun<'_> {
    fn on_bar(&mut self, state: BarState) -> Result<i8, String> {
        let ctx = RhaiCtx {
            dataset: self.dataset.clone(),
            t: state.t,
            position: state.position,
            entry_price_milli: state.entry_price_milli,
            bars_held: state.bars_held,
            requested_keys: self.requested_keys.clone(),
        };
        let args = (Dynamic::from(ctx), self.params_dynamic.clone());

        reset_ops_relay(
            self.run_total_ops,
            self.max_operations_per_run,
            self.max_operations_per_bar,
        );
        let mut scope = Scope::new();
        let options = CallFnOptions::new()
            .eval_ast(false)
            .bind_this_ptr(&mut self.this);
        let result = self.host.engine.call_fn_with_options::<Dynamic>(
            options,
            &mut scope,
            &self.script.ast,
            "on_bar",
            args,
        );

        self.run_total_ops = self.run_total_ops.saturating_add(take_last_call_ops());

        match result {
            Ok(value) => {
                if value.is_int() {
                    let n = value.as_int().expect("checked is_int above");
                    match n {
                        -1..=1 => Ok(n as i8),
                        other => Err(format!(
                            "on_bar returned out-of-range integer {other} (expected 1, -1, or 0)"
                        )),
                    }
                } else {
                    Err(format!(
                        "on_bar returned a value of type '{}' (expected an integer 1, -1, or 0)",
                        value.type_name()
                    ))
                }
            }
            Err(err) => Err(describe_rhai_error(&err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Bar;

    // ---- test helpers ------------------------------------------------------

    /// 有効な `params()` を固定し、`on_bar` の本体だけを差し替えた最小スクリプトを作る。
    fn wrap_on_bar(body: &str) -> String {
        format!("fn params() {{ #{{}} }}\nfn on_bar(ctx, p) {{\n{body}\n}}\n")
    }

    fn empty_params() -> ParamSet {
        ParamSet::new()
    }

    fn state(t: usize) -> BarState {
        BarState {
            t,
            position: 0,
            entry_price_milli: 0,
            bars_held: 0,
        }
    }

    /// `source` を登録し、`on_bar` を 1 回だけ `state` で呼んだ結果を返す。
    fn run_single(
        dataset: &Arc<Dataset>,
        cfg: &SimConfig,
        source: &str,
        params: &ParamSet,
        bar_state: BarState,
    ) -> Result<i8, String> {
        let host = ScriptHost::new(cfg);
        let script = host.compile(source).expect("test script must compile");
        let mut run = ScriptRun::new(&host, &script, params, dataset.clone(), cfg);
        run.on_bar(bar_state)
    }

    /// 決定的な中値の系列(計画 Task 3 Step 3 の式)から M5 の `Bar` を `n` 本作る。
    /// スプレッドは 10 ミリ円(1 pip)固定、始値は直前の足の終値と同じにする。
    fn deterministic_bars(n: usize) -> Vec<Bar> {
        const SPREAD_MILLI: i64 = 10;
        let mid_closes: Vec<f64> = (0..n)
            .map(|i| 150.0 + 0.3 * (i as f64 * 0.07).sin() + 0.001 * i as f64)
            .collect();
        let close_milli: Vec<i64> = mid_closes
            .iter()
            .map(|&v| (v * 1000.0).round() as i64)
            .collect();
        (0..n)
            .map(|i| {
                let close = close_milli[i];
                let open = if i == 0 {
                    close_milli[0]
                } else {
                    close_milli[i - 1]
                };
                let half = SPREAD_MILLI / 2;
                let hi = open.max(close);
                let lo = open.min(close);
                Bar {
                    open_time: i as i64 * M5_SECS,
                    bid_open: open - half,
                    bid_high: hi - half,
                    bid_low: lo - half,
                    bid_close: close - half,
                    ask_open: open + half,
                    ask_high: hi + half,
                    ask_low: lo + half,
                    ask_close: close + half,
                }
            })
            .collect()
    }

    /// `n` 本すべてを warmup なしの読み込み範囲にした `Dataset`(`bars()` の添字 == 配列添字)。
    fn deterministic_dataset(n: usize) -> Arc<Dataset> {
        let bars = deterministic_bars(n);
        let from = bars[0].open_time;
        let to = bars[n - 1].open_time + M5_SECS;
        Arc::new(Dataset::new(bars, from, to, 0, 64).expect("deterministic dataset must build"))
    }

    // ---- Step 2: 登録時の検証の失敗 ----------------------------------------

    #[test]
    fn registration_rejects_each_8_1_violation() {
        let cfg = SimConfig::default();
        let host = ScriptHost::new(&cfg);

        let oversized_padding = "x".repeat(40_000);
        let thirteen_params = (0..13)
            .map(|i| format!("p{i}: #{{ min: 0, max: 10, step: 1, \"default\": 0 }}"))
            .collect::<Vec<_>>()
            .join(", ");

        let cases: Vec<(&str, String)> = vec![
            (
                "syntax error",
                "fn params() { #{} }\nfn on_bar(ctx, p) { ctx.position".to_string(),
            ),
            (
                "source exceeds 32 KiB",
                format!(
                    "// {oversized_padding}\nfn params() {{ #{{}} }}\nfn on_bar(ctx, p) {{ 0 }}\n"
                ),
            ),
            (
                "params() undefined",
                "fn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "on_bar undefined",
                "fn params() { #{} }".to_string(),
            ),
            (
                "params() has 1 argument",
                "fn params(x) { #{} }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "on_bar has 1 argument",
                "fn params() { #{} }\nfn on_bar(ctx) { 0 }".to_string(),
            ),
            (
                "params() returns a non-map",
                "fn params() { 42 }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "a param spec is missing 'min'",
                "fn params() { #{ entry: #{ max: 60, step: 2, \"default\": 20 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "a param spec mixes int and float",
                "fn params() { #{ entry: #{ min: 10, max: 60.0, step: 2, \"default\": 20 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "min > default",
                "fn params() { #{ entry: #{ min: 30, max: 60, step: 2, \"default\": 20 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "default > max",
                "fn params() { #{ entry: #{ min: 10, max: 15, step: 2, \"default\": 20 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "step == 0",
                "fn params() { #{ entry: #{ min: 10, max: 60, step: 0, \"default\": 20 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "13 parameters",
                format!("fn params() {{ #{{ {thirteen_params} }} }}\nfn on_bar(ctx, p) {{ 0 }}\n"),
            ),
            (
                "default not on the step grid",
                "fn params() { #{ entry: #{ min: 10, max: 60, step: 2, \"default\": 15 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "float step is NaN",
                "fn params() { #{ x: #{ min: 0.0, max: 1.0, step: 0.0/0.0, \"default\": 0.0 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "float max is infinite",
                "fn params() { #{ x: #{ min: 0.0, max: 1.0/0.0, step: 0.5, \"default\": 0.0 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "float min is -infinite",
                "fn params() { #{ x: #{ min: -1.0/0.0, max: 1.0, step: 0.5, \"default\": 0.0 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "float default is NaN",
                "fn params() { #{ x: #{ min: 0.0, max: 1.0, step: 0.5, \"default\": 0.0/0.0 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "int default off the step grid even with the full i64 range",
                "fn params() { #{ x: #{ min: -9223372036854775807 - 1, max: 9223372036854775807, step: 2, \"default\": 9223372036854775807 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
            (
                "params() infinite loop",
                "fn params() { loop {} }\nfn on_bar(ctx, p) { 0 }".to_string(),
            ),
        ];

        for (name, source) in cases {
            // CompiledScript (rhai::AST を保持) は Debug を実装しないため、Result の中身を
            // そのまま {:?} に渡さず、バリアントで分岐して文脈を組み立てる。
            match host.compile(&source) {
                Err(SimError::InvalidScript(_)) => {}
                Err(other) => panic!(
                    "case '{name}' should be InvalidScript, got a different SimError: {other}"
                ),
                Ok(_) => panic!(
                    "case '{name}' should be InvalidScript, but the script compiled successfully"
                ),
            }
        }
    }

    #[test]
    fn full_i64_range_int_param_does_not_overflow_when_checking_the_step_grid() {
        let host = ScriptHost::new(&SimConfig::default());
        let source = "fn params() { #{ a: #{ min: -9223372036854775807 - 1, max: 9223372036854775807, step: 1, \"default\": 9223372036854775807 } } }\nfn on_bar(ctx, p) { 0 }";
        // step: 1 なので刻みに乗る。i64 の差分計算がオーバーフローして panic しないこと。
        assert!(host.compile(source).is_ok());
    }

    // ---- Step 3: 正常な登録 ------------------------------------------------

    #[test]
    fn donchian_sar_script_registers_with_a_single_entry_param() {
        let host = ScriptHost::new(&SimConfig::default());
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/donchian_sar.rhai"
        ))
        .expect("donchian_sar.rhai must exist");

        let script = host
            .compile(&source)
            .expect("spec 8.1 script must register");

        assert_eq!(
            script.params,
            vec![ParamSpec {
                name: "entry".to_string(),
                kind: ParamKind::Int,
                min: 10.0,
                max: 60.0,
                step: 2.0,
                default: 20.0,
            }]
        );
    }

    #[test]
    fn sha256_matches_a_direct_sha2_computation() {
        let host = ScriptHost::new(&SimConfig::default());
        let source = wrap_on_bar("0");
        let script = host.compile(&source).expect("valid script");

        let expected = {
            let digest = Sha256::digest(source.as_bytes());
            hex::encode(digest)
        };
        assert_eq!(script.sha256, expected);
    }

    #[test]
    fn a_script_with_no_parameters_registers_with_an_empty_params_list() {
        let host = ScriptHost::new(&SimConfig::default());
        let script = host
            .compile(&wrap_on_bar("0"))
            .expect("fn params() { #{} } must register");
        assert!(script.params.is_empty());
    }

    #[test]
    fn a_top_level_let_statement_registers_but_is_never_evaluated() {
        let host = ScriptHost::new(&SimConfig::default());
        let source = "let x = 1;\nfn params() { #{} }\nfn on_bar(ctx, p) { x }\n";
        let script = host
            .compile(source)
            .expect("a top-level statement must not block registration");

        let dataset = deterministic_dataset(10);
        let mut run = ScriptRun::new(
            &host,
            &script,
            &empty_params(),
            dataset,
            &SimConfig::default(),
        );
        let result = run.on_bar(state(5));
        assert!(
            result.is_err(),
            "on_bar referencing a top-level `let` must fail at runtime (the statement was never run), got {result:?}"
        );
    }

    // ---- Step 4: resolve_params --------------------------------------------

    fn donchian_sar_script(host: &ScriptHost) -> CompiledScript {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/donchian_sar.rhai"
        ))
        .expect("donchian_sar.rhai must exist");
        host.compile(&source)
            .expect("spec 8.1 script must register")
    }

    #[test]
    fn resolve_params_uses_default_when_nothing_is_given() {
        let host = ScriptHost::new(&SimConfig::default());
        let script = donchian_sar_script(&host);
        let given = serde_json::Map::new();
        let resolved = script
            .resolve_params(&given)
            .expect("empty params must resolve");
        assert_eq!(resolved.get("entry"), Some(&ParamValue::Int(20)));
    }

    #[test]
    fn resolve_params_fills_unspecified_params_with_default() {
        let host = ScriptHost::new(&SimConfig::default());
        // 2 パラメータ: 1 つだけ指定し、残りが default で埋まることを確認する。
        let script = host
            .compile(
                "fn params() { #{ a: #{ min: 0, max: 10, step: 1, \"default\": 3 }, b: #{ min: 0, max: 10, step: 1, \"default\": 7 } } }\nfn on_bar(ctx, p) { 0 }\n",
            )
            .expect("valid script");
        let mut given = serde_json::Map::new();
        given.insert("a".to_string(), serde_json::json!(5));
        let resolved = script
            .resolve_params(&given)
            .expect("partial params must resolve");
        assert_eq!(resolved.get("a"), Some(&ParamValue::Int(5)));
        assert_eq!(resolved.get("b"), Some(&ParamValue::Int(7)));
    }

    #[test]
    fn resolve_params_rejects_unknown_name() {
        let host = ScriptHost::new(&SimConfig::default());
        let script = donchian_sar_script(&host);
        let mut given = serde_json::Map::new();
        given.insert("bogus".to_string(), serde_json::json!(1));
        assert!(matches!(
            script.resolve_params(&given),
            Err(SimError::Args(_))
        ));
    }

    #[test]
    fn resolve_params_rejects_out_of_range_value() {
        let host = ScriptHost::new(&SimConfig::default());
        let script = donchian_sar_script(&host);
        let mut given = serde_json::Map::new();
        given.insert("entry".to_string(), serde_json::json!(1000));
        assert!(matches!(
            script.resolve_params(&given),
            Err(SimError::Args(_))
        ));
    }

    #[test]
    fn resolve_params_rejects_value_off_the_step_grid() {
        let host = ScriptHost::new(&SimConfig::default());
        let script = donchian_sar_script(&host);
        let mut given = serde_json::Map::new();
        given.insert("entry".to_string(), serde_json::json!(31)); // min=10, step=2 -> odd offsets rejected
        assert!(matches!(
            script.resolve_params(&given),
            Err(SimError::Args(_))
        ));
    }

    #[test]
    fn resolve_params_rejects_a_float_given_to_an_int_param() {
        let host = ScriptHost::new(&SimConfig::default());
        let script = donchian_sar_script(&host);
        let mut given = serde_json::Map::new();
        given.insert("entry".to_string(), serde_json::json!(30.0));
        assert!(matches!(
            script.resolve_params(&given),
            Err(SimError::Args(_))
        ));
    }

    #[test]
    fn resolve_params_accepts_a_json_integer_for_a_float_param() {
        let host = ScriptHost::new(&SimConfig::default());
        let script = host
            .compile(
                "fn params() { #{ mult: #{ min: 0.5, max: 5.0, step: 0.5, \"default\": 2.0 } } }\nfn on_bar(ctx, p) { 0 }\n",
            )
            .expect("valid script");
        let mut given = serde_json::Map::new();
        given.insert("mult".to_string(), serde_json::json!(3)); // JSON integer, not 3.0
        let resolved = script
            .resolve_params(&given)
            .expect("int-as-float must be accepted");
        assert_eq!(resolved.get("mult"), Some(&ParamValue::Float(3.0)));
    }

    #[test]
    fn compile_rejects_float_default_whose_value_gap_to_grid_exceeds_tolerance() {
        let host = ScriptHost::new(&SimConfig::default());
        // 値の差は 1e-8 > 1e-9。商 k の差(1e-10)で判定すると誤って受理される。
        let result = host.compile(
            "fn params() { #{ x: #{ min: 0.0, max: 1000.0, step: 100.0, \"default\": 100.00000001 } } }\nfn on_bar(ctx, p) { 0 }\n",
        );
        assert!(matches!(result, Err(SimError::InvalidScript(_))));
    }

    #[test]
    fn compile_accepts_float_default_with_only_rounding_error() {
        let host = ScriptHost::new(&SimConfig::default());
        host.compile(
            "fn params() { #{ x: #{ min: 0.1, max: 1.0, step: 0.1, \"default\": 0.3 } } }\nfn on_bar(ctx, p) { 0 }\n",
        )
        .expect("0.3 differs from 0.1 + 2*0.1 only by float rounding");
    }

    #[test]
    fn resolve_params_rejects_float_whose_value_gap_to_grid_exceeds_tolerance() {
        let host = ScriptHost::new(&SimConfig::default());
        let script = host
            .compile(
                "fn params() { #{ x: #{ min: 0.0, max: 1000.0, step: 100.0, \"default\": 100.0 } } }\nfn on_bar(ctx, p) { 0 }\n",
            )
            .expect("valid script");
        let mut given = serde_json::Map::new();
        given.insert("x".to_string(), serde_json::json!(100.00000001));
        assert!(matches!(
            script.resolve_params(&given),
            Err(SimError::Args(_))
        ));
    }

    #[test]
    fn resolve_params_accepts_float_on_the_grid() {
        let host = ScriptHost::new(&SimConfig::default());
        let script = host
            .compile(
                "fn params() { #{ x: #{ min: 0.0, max: 1000.0, step: 100.0, \"default\": 100.0 } } }\nfn on_bar(ctx, p) { 0 }\n",
            )
            .expect("valid script");
        let mut given = serde_json::Map::new();
        given.insert("x".to_string(), serde_json::json!(300.0));
        let resolved = script.resolve_params(&given).expect("300.0 is on the grid");
        assert_eq!(resolved.get("x"), Some(&ParamValue::Float(300.0)));
    }

    // ---- Step 5: 実行 --------------------------------------------------------

    const T_MID: usize = 300;

    #[test]
    fn ctx_scalar_properties_match_bar_state_and_bar_values() {
        let dataset = deterministic_dataset(400);
        let cfg = SimConfig::default();
        let bar = dataset.bars()[T_MID];

        // position / entry_price / bars_held / unrealized_pips(買い)
        let entry_price_milli = bar.bid_close - 500; // 50 pips 含み益
        let bar_state = BarState {
            t: T_MID,
            position: 1,
            entry_price_milli,
            bars_held: 7,
        };
        let expected_entry_price = entry_price_milli as f64 / 1000.0;
        let mut long_params = ParamSet::new();
        long_params.insert(
            "entry_price_x1e6".to_string(),
            ParamValue::Int((expected_entry_price * 1_000_000.0).round() as i64),
        );
        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar(concat!(
                "if ctx.position != 1 { return -1; }\n",
                "if ((ctx.entry_price * 1000000.0).round() - p.entry_price_x1e6).abs() > 1 { return -1; }\n",
                "if ctx.bars_held != 7 { return -1; }\n",
                "if (ctx.unrealized_pips - 50.0).abs() > 1e-6 { return -1; }\n",
                "1"
            )),
            &long_params,
            bar_state,
        );
        assert_eq!(
            result,
            Ok(1),
            "position/entry_price/bars_held/unrealized_pips (long)"
        );

        // 売り、含み損のケースと、position 0 の初期値のケース
        let short_state = BarState {
            t: T_MID,
            position: -1,
            entry_price_milli: bar.ask_close - 300, // 建値が ask より低い -> 含み損30pips
            bars_held: 2,
        };
        let short_result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar("if (ctx.unrealized_pips - (-30.0)).abs() > 1e-6 { -1 } else { 1 }"),
            &empty_params(),
            short_state,
        );
        assert_eq!(short_result, Ok(1), "unrealized_pips (short)");

        let flat_result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar(
                "if ctx.entry_price == 0.0 && ctx.unrealized_pips == 0.0 && ctx.bars_held == 0 { 1 } else { -1 }",
            ),
            &empty_params(),
            state(T_MID),
        );
        assert_eq!(flat_result, Ok(1), "flat position defaults");
    }

    #[test]
    fn ctx_time_hour_weekday_and_spread_match_the_bar() {
        let dataset = deterministic_dataset(400);
        let cfg = SimConfig::default();
        let bar = dataset.bars()[T_MID];
        let end_time = bar.open_time + M5_SECS;
        let expected = end_time_to_utc(end_time);
        let expected_hour = {
            use chrono::Timelike;
            expected.hour() as i64
        };
        let expected_weekday = {
            use chrono::Datelike;
            expected.weekday().num_days_from_monday() as i64
        };
        let expected_spread_pips = milli_to_pips(bar.ask_close - bar.bid_close);

        let mut params = ParamSet::new();
        params.insert("time".to_string(), ParamValue::Int(end_time));
        params.insert("hour".to_string(), ParamValue::Int(expected_hour));
        params.insert("weekday".to_string(), ParamValue::Int(expected_weekday));
        params.insert(
            "spread_x1000".to_string(),
            ParamValue::Int((expected_spread_pips * 1000.0).round() as i64),
        );

        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar(concat!(
                "if ctx.time != p.time { return -1; }\n",
                "if ctx.hour != p.hour { return -1; }\n",
                "if ctx.weekday != p.weekday { return -1; }\n",
                "if ((ctx.spread * 1000.0).round() - p.spread_x1000).abs() > 1 { return -1; }\n",
                "1"
            )),
            &params,
            state(T_MID),
        );
        assert_eq!(result, Ok(1));
    }

    #[test]
    fn ctx_close_returns_the_latest_completed_m15_and_shift_1_returns_the_previous_one() {
        let dataset = deterministic_dataset(400);
        let cfg = SimConfig::default();
        let completed = dataset.completed(Tf::M15, T_MID);
        assert!(completed >= 2, "test needs at least 2 completed M15 bars");
        let series = dataset.series(Tf::M15);
        let latest = series.close[completed - 1];
        let previous = series.close[completed - 2];

        let mut params = ParamSet::new();
        params.insert(
            "latest_x1e6".to_string(),
            ParamValue::Int((latest * 1_000_000.0).round() as i64),
        );
        params.insert(
            "previous_x1e6".to_string(),
            ParamValue::Int((previous * 1_000_000.0).round() as i64),
        );

        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar(concat!(
                "let c0 = ctx.close(\"M15\", 0);\n",
                "let c1 = ctx.close(\"M15\", 1);\n",
                "if ((c0 * 1000000.0).round() - p.latest_x1e6).abs() > 1 { return -1; }\n",
                "if ((c1 * 1000000.0).round() - p.previous_x1e6).abs() > 1 { return -1; }\n",
                "1"
            )),
            &params,
            state(T_MID),
        );
        assert_eq!(result, Ok(1));
    }

    /// 8 種の指標が `Dataset::indicator` の対応する添字の値を返すことを、各指標ごとに
    /// 確認する。期待値は `Dataset::completed`/`Dataset::indicator`(Task 3 で検証済み)から
    /// 独立に計算し、`p` 経由でスクリプトへ渡して比較させる。
    fn assert_ctx_single_indicator_matches_dataset(
        dataset: &Arc<Dataset>,
        kind: IndicatorKind,
        fn_name: &str,
    ) {
        let cfg = SimConfig::default();
        let tf = Tf::M15;
        let period = 5u32;
        let shift = 0usize;
        let completed = dataset.completed(tf, T_MID);
        let idx = completed - 1 - shift;
        let key = IndicatorKey {
            tf,
            kind,
            period,
            mult_x100: 0,
        };
        let series = dataset.indicator(key);
        let IndicatorSeries::Single(values) = &*series else {
            panic!("expected Single series for {fn_name}")
        };
        let expected = values[idx];
        assert!(
            !expected.is_nan(),
            "{fn_name}: test setup must have enough bars"
        );

        let mut params = ParamSet::new();
        params.insert(
            "expected_x1e6".to_string(),
            ParamValue::Int((expected * 1_000_000.0).round() as i64),
        );
        let body = format!(
            "let v = ctx.{fn_name}(\"M15\", {period}, {shift});\nif ((v * 1000000.0).round() - p.expected_x1e6).abs() > 1 {{ -1 }} else {{ 1 }}"
        );
        let result = run_single(dataset, &cfg, &wrap_on_bar(&body), &params, state(T_MID));
        assert_eq!(result, Ok(1), "{fn_name} did not match Dataset::indicator");
    }

    #[test]
    fn ctx_sma_matches_dataset_indicator() {
        let dataset = deterministic_dataset(400);
        assert_ctx_single_indicator_matches_dataset(&dataset, IndicatorKind::Sma, "sma");
    }

    #[test]
    fn ctx_ema_matches_dataset_indicator() {
        let dataset = deterministic_dataset(400);
        assert_ctx_single_indicator_matches_dataset(&dataset, IndicatorKind::Ema, "ema");
    }

    #[test]
    fn ctx_rsi_matches_dataset_indicator() {
        let dataset = deterministic_dataset(400);
        assert_ctx_single_indicator_matches_dataset(&dataset, IndicatorKind::Rsi, "rsi");
    }

    #[test]
    fn ctx_atr_matches_dataset_indicator() {
        let dataset = deterministic_dataset(400);
        assert_ctx_single_indicator_matches_dataset(&dataset, IndicatorKind::Atr, "atr");
    }

    #[test]
    fn ctx_adx_matches_dataset_indicator() {
        let dataset = deterministic_dataset(400);
        assert_ctx_single_indicator_matches_dataset(&dataset, IndicatorKind::Adx, "adx");
    }

    #[test]
    fn ctx_donchian_has_upper_and_lower_keys_matching_dataset_indicator() {
        let dataset = deterministic_dataset(400);
        let cfg = SimConfig::default();
        let tf = Tf::M15;
        let period = 5u32;
        let completed = dataset.completed(tf, T_MID);
        let idx = completed - 1;
        let key = IndicatorKey {
            tf,
            kind: IndicatorKind::Donchian,
            period,
            mult_x100: 0,
        };
        let series = dataset.indicator(key);
        let IndicatorSeries::Channel { lower, upper } = &*series else {
            panic!("expected Channel series for donchian")
        };
        let (expected_lower, expected_upper) = (lower[idx], upper[idx]);

        let mut params = ParamSet::new();
        params.insert(
            "lower_x1e6".to_string(),
            ParamValue::Int((expected_lower * 1_000_000.0).round() as i64),
        );
        params.insert(
            "upper_x1e6".to_string(),
            ParamValue::Int((expected_upper * 1_000_000.0).round() as i64),
        );
        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar(concat!(
                "let ch = ctx.donchian(\"M15\", 5, 0);\n",
                "if ((ch.upper * 1000000.0).round() - p.upper_x1e6).abs() > 1 { return -1; }\n",
                "if ((ch.lower * 1000000.0).round() - p.lower_x1e6).abs() > 1 { return -1; }\n",
                "1"
            )),
            &params,
            state(T_MID),
        );
        assert_eq!(result, Ok(1));
    }

    fn assert_ctx_band_indicator_matches_dataset(
        dataset: &Arc<Dataset>,
        kind: IndicatorKind,
        fn_name: &str,
    ) {
        let cfg = SimConfig::default();
        let tf = Tf::M15;
        let period = 5u32;
        let completed = dataset.completed(tf, T_MID);
        let idx = completed - 1;
        let key = IndicatorKey {
            tf,
            kind,
            period,
            mult_x100: 200,
        };
        let series = dataset.indicator(key);
        let IndicatorSeries::Band {
            lower,
            middle,
            upper,
        } = &*series
        else {
            panic!("expected Band series for {fn_name}")
        };
        let (expected_lower, expected_middle, expected_upper) =
            (lower[idx], middle[idx], upper[idx]);

        let mut params = ParamSet::new();
        params.insert(
            "lower_x1e6".to_string(),
            ParamValue::Int((expected_lower * 1_000_000.0).round() as i64),
        );
        params.insert(
            "middle_x1e6".to_string(),
            ParamValue::Int((expected_middle * 1_000_000.0).round() as i64),
        );
        params.insert(
            "upper_x1e6".to_string(),
            ParamValue::Int((expected_upper * 1_000_000.0).round() as i64),
        );
        let body = format!(
            concat!(
                "let b = ctx.{fn_name}(\"M15\", 5, 2, 0);\n",
                "if ((b.upper * 1000000.0).round() - p.upper_x1e6).abs() > 1 {{ return -1; }}\n",
                "if ((b.middle * 1000000.0).round() - p.middle_x1e6).abs() > 1 {{ return -1; }}\n",
                "if ((b.lower * 1000000.0).round() - p.lower_x1e6).abs() > 1 {{ return -1; }}\n",
                "1"
            ),
            fn_name = fn_name
        );
        let result = run_single(dataset, &cfg, &wrap_on_bar(&body), &params, state(T_MID));
        assert_eq!(result, Ok(1), "{fn_name} did not match Dataset::indicator");
    }

    #[test]
    fn ctx_bb_has_upper_middle_lower_keys_matching_dataset_indicator() {
        let dataset = deterministic_dataset(400);
        assert_ctx_band_indicator_matches_dataset(&dataset, IndicatorKind::Bb, "bb");
    }

    #[test]
    fn ctx_keltner_has_upper_middle_lower_keys_matching_dataset_indicator() {
        let dataset = deterministic_dataset(400);
        assert_ctx_band_indicator_matches_dataset(&dataset, IndicatorKind::Keltner, "keltner");
    }

    #[test]
    fn insufficient_bars_return_unit_and_using_unit_arithmetically_is_an_error() {
        let dataset = deterministic_dataset(10); // わずか10本: 大きい period は未定義になる
        let cfg = SimConfig::default();

        let returns_unit = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar("let v = ctx.sma(\"H4\", 1000, 0); if v == () { 1 } else { -1 }"),
            &empty_params(),
            state(5),
        );
        assert_eq!(returns_unit, Ok(1), "insufficient data must yield ()");

        let arithmetic_on_unit = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar("let v = ctx.sma(\"H4\", 1000, 0); v + 1"),
            &empty_params(),
            state(5),
        );
        assert!(
            arithmetic_on_unit.is_err(),
            "using () in arithmetic must be a runtime error, got {arithmetic_on_unit:?}"
        );
    }

    #[test]
    fn mult_accepts_both_int_and_float_and_rounds_nearby_values_to_the_same_key() {
        let dataset = deterministic_dataset(400);
        let cfg = SimConfig::default();

        let int_vs_float = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar(concat!(
                "let a = ctx.bb(\"M15\", 5, 2, 0);\n",
                "let b = ctx.bb(\"M15\", 5, 2.0, 0);\n",
                "if a.upper == b.upper && a.middle == b.middle && a.lower == b.lower { 1 } else { -1 }"
            )),
            &empty_params(),
            state(T_MID),
        );
        assert_eq!(
            int_vs_float,
            Ok(1),
            "mult=2 (int) and mult=2.0 (float) must agree"
        );

        let rounded_same_key = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar(concat!(
                "let a = ctx.bb(\"M15\", 5, 2.004, 0);\n",
                "let b = ctx.bb(\"M15\", 5, 2.0, 0);\n",
                "if a.upper == b.upper && a.middle == b.middle && a.lower == b.lower { 1 } else { -1 }"
            )),
            &empty_params(),
            state(T_MID),
        );
        assert_eq!(
            rounded_same_key,
            Ok(1),
            "mult=2.004 and mult=2.0 must round to the same key"
        );
    }

    #[test]
    fn invalid_ctx_arguments_are_runtime_errors() {
        let dataset = deterministic_dataset(400);
        let cfg = SimConfig::default();

        let cases: &[(&str, &str)] = &[
            ("unknown tf", "ctx.close(\"D1\", 0)"),
            ("period = 0", "ctx.sma(\"M15\", 0, 0)"),
            ("period = 1001", "ctx.sma(\"M15\", 1001, 0)"),
            ("shift = -1", "ctx.close(\"M15\", -1)"),
            ("shift = 1001", "ctx.close(\"M15\", 1001)"),
            ("mult = 0.01", "ctx.bb(\"M15\", 5, 0.01, 0)"),
            ("mult = 11", "ctx.bb(\"M15\", 5, 11, 0)"),
            ("period as float", "ctx.sma(\"M15\", 5.0, 0)"),
        ];

        for (name, expr) in cases {
            let body = format!("let v = {expr};\n0");
            let result = run_single(
                &dataset,
                &cfg,
                &wrap_on_bar(&body),
                &empty_params(),
                state(T_MID),
            );
            assert!(
                result.is_err(),
                "case '{name}' ({expr}) should be a runtime error, got {result:?}"
            );
        }
    }

    #[test]
    fn requesting_more_than_64_distinct_indicator_keys_fails_on_the_65th_across_calls() {
        // ScriptRun::new の p は 1 回のシミュレーションを通して固定なので(spec 9.1)、
        // どの範囲を要求するかは ctx.bars_held(= 呼び出しごとに Rust 側から渡す BarState)で
        // 分岐させる。1 回目で period 1..=60 (60 種)、2 回目で 61..=64 (4 種、計 64 種)、
        // 3 回目で 65 番目の新規キーを要求する。すべて同一の ScriptRun(= 同一シミュレーション)
        // に対して行うことで、要求済みキーの上限がシミュレーション全体を通して累積することを
        // 確認する。
        let dataset = deterministic_dataset(400);
        let cfg = SimConfig::default();
        let host = ScriptHost::new(&cfg);
        let script = host
            .compile(&wrap_on_bar(concat!(
                "if ctx.bars_held == 0 { for i in 1..61 { ctx.sma(\"M5\", i, 0); } return 0; }\n",
                "if ctx.bars_held == 1 { for i in 61..65 { ctx.sma(\"M5\", i, 0); } return 0; }\n",
                "ctx.sma(\"M5\", 65, 0);\n",
                "0"
            )))
            .expect("valid script");
        let mut run = ScriptRun::new(&host, &script, &empty_params(), dataset, &cfg);

        let call1 = run.on_bar(BarState {
            t: 0,
            position: 0,
            entry_price_milli: 0,
            bars_held: 0,
        });
        assert_eq!(
            call1,
            Ok(0),
            "first 60 distinct keys must succeed: {call1:?}"
        );

        let call2 = run.on_bar(BarState {
            t: 1,
            position: 0,
            entry_price_milli: 0,
            bars_held: 1,
        });
        assert_eq!(
            call2,
            Ok(0),
            "keys 61..=64 (total 64) must succeed: {call2:?}"
        );

        let call3 = run.on_bar(BarState {
            t: 2,
            position: 0,
            entry_price_milli: 0,
            bars_held: 2,
        });
        assert!(call3.is_err(), "the 65th distinct key must fail: {call3:?}");
    }

    #[test]
    fn this_persists_across_on_bar_calls() {
        let dataset = deterministic_dataset(10);
        let cfg = SimConfig::default();
        let host = ScriptHost::new(&cfg);
        let script = host
            .compile(&wrap_on_bar(
                "if this.seen == 1 { return 1; } this.seen = 1; 0",
            ))
            .expect("valid script");
        let mut run = ScriptRun::new(&host, &script, &empty_params(), dataset, &cfg);

        let first = run.on_bar(state(0));
        assert_eq!(first, Ok(0), "first call: `this.seen` is not set yet");
        let second = run.on_bar(state(1));
        assert_eq!(
            second,
            Ok(1),
            "second call must observe `this.seen` written by the first call"
        );
    }

    #[test]
    fn on_bar_returning_a_value_other_than_1_minus_1_or_0_is_an_error() {
        let dataset = deterministic_dataset(10);
        let cfg = SimConfig::default();

        for (name, body) in [
            ("int 2", "2"),
            ("float 1.0", "1.0"),
            ("string", "\"x\""),
            ("unit", "()"),
        ] {
            let result = run_single(
                &dataset,
                &cfg,
                &wrap_on_bar(body),
                &empty_params(),
                state(0),
            );
            assert!(
                result.is_err(),
                "on_bar returning {name} must be an error, got {result:?}"
            );
        }
    }

    #[test]
    fn an_infinite_loop_in_on_bar_is_an_error_under_a_small_max_operations_per_bar() {
        let dataset = deterministic_dataset(10);
        let cfg = SimConfig {
            max_operations_per_bar: 10_000,
            ..SimConfig::default()
        };

        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar("loop {}"),
            &empty_params(),
            state(0),
        );
        assert!(result.is_err());
    }

    #[test]
    fn repeated_calls_eventually_fail_once_max_operations_per_run_is_exhausted() {
        let dataset = deterministic_dataset(10);
        let cfg = SimConfig {
            max_operations_per_run: 50, // 小さい予算: 数回の呼び出しで使い切る
            ..SimConfig::default()
        };
        let host = ScriptHost::new(&cfg);
        let script = host
            .compile(&wrap_on_bar("let i = 0; while i < 5 { i += 1; } 0"))
            .expect("valid script");
        let mut run = ScriptRun::new(&host, &script, &empty_params(), dataset, &cfg);

        let results: Vec<Result<i8, String>> = (0..20).map(|t| run.on_bar(state(t))).collect();
        assert!(
            results[0].is_ok(),
            "first call must succeed: {:?}",
            results[0]
        );
        assert!(
            results.iter().any(|r| r.is_err()),
            "the run must eventually fail once max_operations_per_run is exhausted: {results:?}"
        );
        let first_err = results.iter().position(|r| r.is_err()).unwrap();
        assert!(
            results[first_err..].iter().all(|r| r.is_err()),
            "once the run budget is exhausted, it must not recover: {results:?}"
        );
    }

    #[test]
    fn import_statement_fails_at_execution_via_the_dummy_module_resolver() {
        let dataset = deterministic_dataset(10);
        let cfg = SimConfig::default();
        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar("import \"x\" as y;\n0"),
            &empty_params(),
            state(0),
        );
        assert!(
            result.is_err(),
            "import must fail at execution, got {result:?}"
        );
    }

    #[test]
    fn sleep_fails_at_execution_because_lang_core_is_not_registered() {
        let dataset = deterministic_dataset(10);
        let cfg = SimConfig::default();
        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar("sleep(1); 0"),
            &empty_params(),
            state(0),
        );
        assert!(
            result.is_err(),
            "sleep must fail at execution, got {result:?}"
        );
    }

    #[test]
    fn timestamp_fails_at_execution_because_basic_time_is_not_registered() {
        let dataset = deterministic_dataset(10);
        let cfg = SimConfig::default();
        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar("let t = timestamp(); 0"),
            &empty_params(),
            state(0),
        );
        assert!(
            result.is_err(),
            "timestamp must fail at execution, got {result:?}"
        );
    }

    #[test]
    fn rand_fails_at_execution_because_no_package_registers_it() {
        let dataset = deterministic_dataset(10);
        let cfg = SimConfig::default();
        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar("let r = rand(); 0"),
            &empty_params(),
            state(0),
        );
        assert!(
            result.is_err(),
            "rand must fail at execution, got {result:?}"
        );
    }

    #[test]
    fn eval_fails_registration_because_the_symbol_is_disabled() {
        let host = ScriptHost::new(&SimConfig::default());
        match host.compile(&wrap_on_bar("eval(\"1\")")) {
            Err(SimError::InvalidScript(_)) => {}
            Err(other) => panic!(
                "eval should be rejected as InvalidScript, got a different SimError: {other}"
            ),
            Ok(_) => panic!(
                "eval should be rejected at registration (disabled symbol), but the script compiled successfully"
            ),
        }
    }

    #[test]
    fn concurrent_script_runs_on_different_threads_do_not_mix_run_totals_or_this() {
        let host = ScriptHost::new(&SimConfig::default());
        let script = host
            .compile(&wrap_on_bar(concat!(
                "let i = 0; while i < 30 { i += 1; }\n",
                "if ctx.bars_held == 0 { this.tag = p.tag; return 0; }\n",
                "if this.tag == p.tag { 1 } else { -1 }"
            )))
            .expect("valid script");
        let dataset = deterministic_dataset(60);

        let outcome_a: Mutex<Option<Vec<Result<i8, String>>>> = Mutex::new(None);
        let outcome_b: Mutex<Option<Vec<Result<i8, String>>>> = Mutex::new(None);

        std::thread::scope(|scope| {
            scope.spawn(|| {
                let cfg = SimConfig {
                    max_operations_per_run: 50, // 小さい予算: 途中で失敗するはず
                    ..SimConfig::default()
                };
                let mut params = ParamSet::new();
                params.insert("tag".to_string(), ParamValue::Int(1));
                let mut run = ScriptRun::new(&host, &script, &params, dataset.clone(), &cfg);
                let results: Vec<_> = (0..10)
                    .map(|i| {
                        run.on_bar(BarState {
                            t: i,
                            position: 0,
                            entry_price_milli: 0,
                            bars_held: i as u32,
                        })
                    })
                    .collect();
                *outcome_a.lock().unwrap() = Some(results);
            });
            scope.spawn(|| {
                let cfg = SimConfig::default(); // 既定値 (1e9) の大きい予算
                let mut params = ParamSet::new();
                params.insert("tag".to_string(), ParamValue::Int(2));
                let mut run = ScriptRun::new(&host, &script, &params, dataset.clone(), &cfg);
                let results: Vec<_> = (0..10)
                    .map(|i| {
                        run.on_bar(BarState {
                            t: i,
                            position: 0,
                            entry_price_milli: 0,
                            bars_held: i as u32,
                        })
                    })
                    .collect();
                *outcome_b.lock().unwrap() = Some(results);
            });
        });

        let results_a = outcome_a.into_inner().unwrap().unwrap();
        let results_b = outcome_b.into_inner().unwrap().unwrap();

        assert!(
            !results_a.iter().any(|r| r == &Ok(-1)),
            "thread A observed a `this` value that does not match its own tag: {results_a:?}"
        );
        assert!(
            !results_b.iter().any(|r| r == &Ok(-1)),
            "thread B observed a `this` value that does not match its own tag: {results_b:?}"
        );
        assert!(
            results_a.iter().any(|r| r.is_err()),
            "thread A (small run budget) should eventually fail: {results_a:?}"
        );
        assert!(
            results_b.iter().all(|r| r.is_ok()),
            "thread B (large run budget) must not be affected by thread A's small budget: {results_b:?}"
        );
    }
}
