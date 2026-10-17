#![doc = "Client for the Vegapunk GraphRAG gRPC service."]

/// Client that talks to the Vegapunk GraphRAG gRPC service.
pub mod client;

#[allow(clippy::doc_overindented_list_items)]
#[allow(clippy::result_large_err)]
#[expect(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "tonic-generated code for proto/graphrag.proto"
)]
pub mod proto {
    tonic::include_proto!("graphrag");
}
