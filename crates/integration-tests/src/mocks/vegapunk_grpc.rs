//! Mock gRPC server for Vegapunk GraphRAG, used by SwingLLM integration tests.
//!
//! Starts a real tonic gRPC server on a random port so that
//! `VegapunkClient::connect` can establish a real connection.
//! Only the `search` RPC returns canned results; all other RPCs
//! return `Unimplemented`.

use crate::proto::graph_rag_engine_server::{GraphRagEngine, GraphRagEngineServer};
use crate::proto::*;
use std::net::SocketAddr;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};

/// Mock Vegapunk gRPC server. Call [`start`] to launch on a random port.
pub struct MockVegapunkGrpc {
    addr: SocketAddr,
}

impl MockVegapunkGrpc {
    /// Start a mock gRPC server on a random port.
    /// `search_texts`: texts returned by the Search RPC.
    pub async fn start(search_texts: Vec<String>) -> Self {
        let svc = MockService {
            search_texts: search_texts.clone(),
        };

        // Bind to port 0 to get a random available port.
        // Use tokio TcpListener and serve_with_incoming to avoid
        // the TOCTOU race of bind-drop-rebind.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock vegapunk grpc");
        let addr = listener.local_addr().unwrap();
        let incoming = TcpListenerStream::new(listener);

        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(GraphRagEngineServer::new(svc))
                .serve_with_incoming(incoming)
                .await
                .ok();
        });

        // No sleep needed — listener is already bound and accepting.

        Self { addr }
    }

    /// gRPC endpoint URL for VegapunkClient::connect.
    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }
}

// ─── Service impl ────────────────────────────────────────────────────────

struct MockService {
    search_texts: Vec<String>,
}

#[tonic::async_trait]
impl GraphRagEngine for MockService {
    async fn search(
        &self,
        _req: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        let results = self
            .search_texts
            .iter()
            .enumerate()
            .map(|(i, text)| SearchResultItem {
                r#type: "mock".to_string(),
                id: Some(format!("mock-{i}")),
                text: Some(text.clone()),
                score: Some(0.9 - i as f32 * 0.1),
                person: None,
                timestamp: None,
                summary: None,
                channel: None,
                decided_at: None,
                rationales: vec![],
            })
            .collect();

        Ok(Response::new(SearchResponse {
            results,
            search_id: "mock-search-id".to_string(),
            total_count: self.search_texts.len() as i32,
            similar_patterns: vec![],
        }))
    }

    // ─── Stubs for all other RPCs ─────────────────────────────────────────

    async fn ingest(&self, _: Request<IngestRequest>) -> Result<Response<IngestResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn upsert_nodes(&self, _: Request<UpsertNodesRequest>) -> Result<Response<UpsertNodesResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn upsert_edges(&self, _: Request<UpsertEdgesRequest>) -> Result<Response<UpsertEdgesResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn upsert_vectors(&self, _: Request<UpsertVectorsRequest>) -> Result<Response<UpsertVectorsResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn merge(&self, _: Request<MergeRequest>) -> Result<Response<MergeResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn rebuild(&self, _: Request<RebuildRequest>) -> Result<Response<RebuildResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn backup(&self, _: Request<BackupRequest>) -> Result<Response<BackupResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn migrate(&self, _: Request<MigrateRequest>) -> Result<Response<MigrateResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn feedback(&self, _: Request<FeedbackRequest>) -> Result<Response<FeedbackResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn get_needs_review(&self, _: Request<GetNeedsReviewRequest>) -> Result<Response<GetNeedsReviewResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn resolve_match(&self, _: Request<ResolveMatchRequest>) -> Result<Response<ResolveMatchResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn get_job_status(&self, _: Request<GetJobStatusRequest>) -> Result<Response<GetJobStatusResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn ingest_raw(&self, _: Request<IngestRawRequest>) -> Result<Response<IngestRawResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn ingest_file(&self, _: Request<IngestFileRequest>) -> Result<Response<IngestFileResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn delete_schema(&self, _: Request<DeleteSchemaRequest>) -> Result<Response<DeleteSchemaResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn reingest(&self, _: Request<ReingestRequest>) -> Result<Response<ReingestResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn improve_prompts(&self, _: Request<ImprovePromptsRequest>) -> Result<Response<ImprovePromptsResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn create_schema(&self, _: Request<CreateSchemaRequest>) -> Result<Response<CreateSchemaResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn get_schema(&self, _: Request<GetSchemaRequest>) -> Result<Response<GetSchemaResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn list_schemas(&self, _: Request<ListSchemasRequest>) -> Result<Response<ListSchemasResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn update_schema(&self, _: Request<UpdateSchemaRequest>) -> Result<Response<UpdateSchemaResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn list_schema_templates(&self, _: Request<ListSchemaTemplatesRequest>) -> Result<Response<ListSchemaTemplatesResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn get_schema_migration_status(&self, _: Request<GetSchemaMigrationStatusRequest>) -> Result<Response<GetSchemaMigrationStatusResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn purge_raw_messages(&self, _: Request<PurgeRawMessagesRequest>) -> Result<Response<PurgeRawMessagesResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn set_maintenance_mode(&self, _: Request<SetMaintenanceModeRequest>) -> Result<Response<MaintenanceModeResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
    async fn get_maintenance_mode(&self, _: Request<GetMaintenanceModeRequest>) -> Result<Response<MaintenanceModeResponse>, Status> {
        Err(Status::unimplemented("mock"))
    }
}
