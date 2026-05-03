//! auto-trader 結合テスト基盤。
//!
//! Phase 1-3 のテストはモックのみで完結し、Phase 4 は `external-api`
//! feature flag を有効にした場合のみ実 API に接続する。

pub mod helpers;
pub mod mocks;
