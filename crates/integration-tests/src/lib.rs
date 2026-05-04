//! auto-trader 結合テスト基盤。
//!
//! Phase 1-3 のテストはモックのみで完結し、Phase 4 は `external-api`
//! feature flag を有効にした場合のみ実 API に接続する。

pub mod helpers;
pub mod mocks;

/// Proto-generated types for the Vegapunk GraphRAG gRPC service.
/// Server stubs are generated here (build.rs) for mock gRPC servers in tests;
/// client stubs live in the `auto-trader-vegapunk` crate.
#[allow(clippy::doc_overindented_list_items)]
pub mod proto {
    tonic::include_proto!("graphrag");
}
