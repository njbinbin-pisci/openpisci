use super::openai::OpenAiClient;
use super::{LlmChunk, LlmClient, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

/// MiniMax OpenAI-compatible endpoint
/// Docs: https://platform.minimax.io/docs/api-reference/text-intro
const MINIMAX_API_URL: &str = "https://api.minimax.io/v1";

pub struct MiniMaxClient {
    inner: OpenAiClient,
}

impl MiniMaxClient {
    #[allow(dead_code)]
    pub fn new(api_key: &str) -> Self {
        Self::with_timeout(api_key, 120)
    }
    pub fn with_timeout(api_key: &str, read_timeout_secs: u32) -> Self {
        Self {
            inner: OpenAiClient::with_timeout(api_key, MINIMAX_API_URL, read_timeout_secs),
        }
    }
}

#[async_trait]
impl LlmClient for MiniMaxClient {
    async fn stream(&self, req: LlmRequest, tx: Sender<LlmChunk>) -> Result<()> {
        self.inner.stream(req, tx).await
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse> {
        self.inner.complete(req).await
    }
}
