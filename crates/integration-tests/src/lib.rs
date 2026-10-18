#![doc = "Test support for the auto-trader workspace: helpers that build the application, database and trade flows, and mocks of the external services it calls."]

/// Helpers that spawn the application, seed the database and drive trade flows in tests.
pub mod helpers;
/// Mocks of the external services that the application calls.
pub mod mocks;

#[expect(
    missing_docs,
    clippy::too_many_lines,
    reason = "tonic-generated code for proto/graphrag.proto"
)]
#[allow(clippy::doc_overindented_list_items)]
pub mod proto {
    tonic::include_proto!("graphrag");
}
