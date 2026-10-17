use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Summarizer of news text for FX trading that uses the Gemini API.
pub struct GeminiSummarizer {
    client: Client,
    api_url: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
}

#[derive(Serialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiContentResponse,
}

#[derive(Deserialize)]
struct GeminiContentResponse {
    parts: Vec<GeminiPartResponse>,
}

#[derive(Deserialize)]
struct GeminiPartResponse {
    text: String,
}

impl GeminiSummarizer {
    /// Creates a summarizer for the Gemini endpoint, model and API key.
    ///
    /// # Errors
    ///
    /// - Returns an error if the HTTP client cannot be built.
    pub fn new(api_url: &str, api_key: &str, model: &str) -> anyhow::Result<Self> {
        let client = crate::http_client::build_http_client()?;
        Ok(Self {
            client,
            api_url: api_url.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        })
    }

    /// Asks Gemini for a short Japanese summary of the text from the viewpoint of FX trading.
    ///
    /// Returns an empty string when the response has no candidate text.
    ///
    /// # Errors
    ///
    /// - Returns an error if the request cannot be sent.
    /// - Returns an error if Gemini answers with a non-success status.
    /// - Returns an error if the response body cannot be decoded.
    pub async fn summarize_for_fx(&self, text: &str) -> anyhow::Result<String> {
        let prompt = format!(
            "以下のニュース/経済指標をFXトレードの観点で要約してください。\
             影響を受ける通貨ペアと方向性（強気/弱気）を含めてください。\
             日本語で3文以内で。\n\n{text}"
        );

        let request = GeminiRequest {
            contents: vec![GeminiContent {
                parts: vec![GeminiPart { text: prompt }],
            }],
        };

        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            self.api_url, self.model
        );

        let resp: GeminiResponse = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let summary = resp
            .candidates
            .first()
            .and_then(|c| c.content.parts.first())
            .map(|p| p.text.clone())
            .unwrap_or_default();

        Ok(summary)
    }
}
