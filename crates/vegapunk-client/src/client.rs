use async_trait::async_trait;
use auto_trader_core::vegapunk_port::{SearchHit, SearchMode, SearchResults, VegapunkApi};
use tonic::metadata::MetadataValue;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};

use crate::proto::graph_rag_engine_client::GraphRagEngineClient;
use crate::proto::*;

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

/// Vegapunk gRPC client bound to one schema, with optional bearer-token authentication.
pub struct VegapunkClient {
    client: AuthClient,
    schema: String,
}

impl VegapunkClient {
    /// Connects to the gRPC endpoint and returns a client bound to the given schema.
    ///
    /// # Errors
    ///
    /// - Returns an error if the endpoint is not a valid URI.
    /// - Returns an error if the connection cannot be established.
    /// - Returns an error if the auth token is not a valid header value.
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

fn clamp_top_k_to_non_negative(top_k: i32) -> i32 {
    top_k.max(0)
}

fn search_hit_with_non_empty_text(item: SearchResultItem) -> Option<SearchHit> {
    let text = item.text?;
    if text.is_empty() {
        return None;
    }
    Some(SearchHit {
        text,
        score: item.score.unwrap_or(0.0),
    })
}

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
            top_k: Some(clamp_top_k_to_non_negative(top_k)),
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
            .filter_map(search_hit_with_non_empty_text)
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

#[cfg(test)]
mod tests;
