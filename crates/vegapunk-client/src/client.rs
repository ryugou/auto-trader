use crate::proto::graph_rag_engine_client::GraphRagEngineClient;
use crate::proto::*;
use tonic::transport::{Channel, Endpoint};

pub struct VegapunkClient {
    client: GraphRagEngineClient<Channel>,
    schema: String,
}

impl VegapunkClient {
    pub async fn connect(endpoint: &str, schema: &str) -> anyhow::Result<Self> {
        let channel = Endpoint::from_shared(endpoint.to_string())?
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .connect()
            .await?;
        let client = GraphRagEngineClient::new(channel);
        Ok(Self {
            client,
            schema: schema.to_string(),
        })
    }

    pub async fn ingest_raw(
        &mut self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<IngestRawResponse> {
        let request = IngestRawRequest {
            text: text.to_string(),
            metadata: Some(IngestRawMetadata {
                source_type: source_type.to_string(),
                author: None,
                channel: Some(channel.to_string()),
                timestamp: Some(timestamp.to_string()),
            }),
            schema: Some(self.schema.clone()),
        };
        let response: tonic::Response<IngestRawResponse> = self.client.ingest_raw(request).await?;
        Ok(response.into_inner())
    }

    pub async fn search(
        &mut self,
        query: &str,
        mode: &str,
        top_k: i32,
    ) -> anyhow::Result<SearchResponse> {
        let request = SearchRequest {
            text: query.to_string(),
            filter: None,
            depth: None,
            top_k: Some(top_k),
            format: None,
            mode: Some(mode.to_string()),
            schema: Some(self.schema.clone()),
            offset: None,
            limit: None,
            structural_weight: None,
        };
        let response: tonic::Response<SearchResponse> = self.client.search(request).await?;
        Ok(response.into_inner())
    }

    pub async fn feedback(
        &mut self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()> {
        let request = FeedbackRequest {
            search_id: search_id.to_string(),
            rating,
            comment: comment.to_string(),
        };
        self.client.feedback(request).await?;
        Ok(())
    }

    pub async fn merge(&mut self) -> anyhow::Result<()> {
        let request = MergeRequest {
            schema: Some(self.schema.clone()),
        };
        self.client.merge(request).await?;
        Ok(())
    }
}
