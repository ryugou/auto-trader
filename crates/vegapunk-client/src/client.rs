use crate::proto::graph_rag_engine_client::GraphRagEngineClient;
use crate::proto::*;
use tonic::metadata::MetadataValue;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};

type AuthClient = GraphRagEngineClient<InterceptedService<Channel, AuthInterceptor>>;

#[derive(Clone)]
struct AuthInterceptor {
    token: Option<MetadataValue<tonic::metadata::Ascii>>,
}

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if let Some(token) = &self.token {
            req.metadata_mut().insert("authorization", token.clone());
        }
        Ok(req)
    }
}

pub struct VegapunkClient {
    client: AuthClient,
    schema: String,
}

impl VegapunkClient {
    pub async fn connect(
        endpoint: &str,
        schema: &str,
        auth_token: Option<&str>,
    ) -> anyhow::Result<Self> {
        let channel = Endpoint::from_shared(endpoint.to_string())?
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .connect()
            .await?;

        let interceptor = AuthInterceptor {
            token: match auth_token {
                Some(t) => Some(
                    format!("Bearer {t}")
                        .parse::<MetadataValue<tonic::metadata::Ascii>>()
                        .map_err(|e| anyhow::anyhow!("invalid VEGAPUNK_AUTH_TOKEN: {e}"))?,
                ),
                None => None,
            },
        };

        let client = GraphRagEngineClient::with_interceptor(channel.clone(), interceptor.clone());
        Ok(Self {
            client,
            schema: schema.to_string(),
        })
    }
}

use async_trait::async_trait;
use auto_trader_core::vegapunk_port::{SearchHit, SearchMode, SearchResults, VegapunkApi};

#[async_trait]
impl VegapunkApi for VegapunkClient {
    async fn ingest_raw(
        &self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<()> {
        let mut client = self.client.clone();
        let request = IngestRawRequest {
            text: text.to_string(),
            metadata: Some(IngestRawMetadata {
                source_type: source_type.to_string(),
                author: None,
                channel: Some(channel.to_string()),
                timestamp: Some(timestamp.to_string()),
            }),
            schema: self.schema.clone(),
        };
        client.ingest_raw(request).await?;
        Ok(())
    }

    async fn search(
        &self,
        query: &str,
        mode: SearchMode,
        top_k: i32,
    ) -> anyhow::Result<SearchResults> {
        let mut client = self.client.clone();
        let request = SearchRequest {
            text: query.to_string(),
            filter: None,
            depth: None,
            // proto top_k は論理的に非負。consumer (MockVegapunkApi) と挙動を
            // 揃えるため負数を 0 に clamp してから forward する。
            top_k: Some(top_k.max(0)),
            format: None,
            mode: Some(mode.as_str().to_string()),
            schema: self.schema.clone(),
            offset: None,
            limit: None,
            structural_weight: None,
        };
        let response = client.search(request).await?.into_inner();
        let hits = response
            .results
            .into_iter()
            .map(|r| SearchHit {
                text: r.text.unwrap_or_default(),
                score: r.score.unwrap_or(0.0),
            })
            .collect();
        Ok(SearchResults {
            hits,
            search_id: response.search_id,
        })
    }

    async fn feedback(&self, search_id: &str, rating: i32, comment: &str) -> anyhow::Result<()> {
        let mut client = self.client.clone();
        let request = FeedbackRequest {
            search_id: search_id.to_string(),
            rating,
            comment: comment.to_string(),
        };
        client.feedback(request).await?;
        Ok(())
    }

    async fn merge(&self) -> anyhow::Result<()> {
        let mut client = self.client.clone();
        let request = MergeRequest {
            schema: self.schema.clone(),
        };
        client.merge(request).await?;
        Ok(())
    }
}
