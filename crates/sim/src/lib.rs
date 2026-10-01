//! `auto-trader-sim`: 過去の USD/JPY 5 分足に Rhai スクリプトのアルゴリズムを
//! 流して評価するシミュレーション基盤。
//!
//! 設計の正本: `docs/superpowers/specs/2026-10-01-simulation-foundation-design.md`。
//! 実装計画: `docs/superpowers/plans/2026-10-01-simulation-foundation.md`。
//!
//! このファイルはモジュール宣言だけを行う。各モジュールの責務は実装計画の
//! File Structure 表のとおり。

pub mod benchmark;
pub mod config;
pub mod data;
pub mod error;
pub mod fetch;
pub mod indicators;
pub mod script;
pub mod series;
pub mod types;
