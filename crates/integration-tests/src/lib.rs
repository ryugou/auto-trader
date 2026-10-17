pub mod helpers;
pub mod mocks;

/// Proto-generated types for the Vegapunk GraphRAG gRPC service.
/// Server stubs are generated here (build.rs) for mock gRPC servers in tests;
/// client stubs live in the `auto-trader-vegapunk` crate.
#[allow(clippy::doc_overindented_list_items)]
pub mod proto {
    tonic::include_proto!("graphrag");
}
