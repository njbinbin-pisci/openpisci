use super::openai::OpenAiClient;
use super::{LlmChunk, LlmClient, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

const DEEPSEEK_API_URL: &str = "https://api.deepseek.com/v1";

pub struct DeepSeekClient {
    inner: OpenAiClient,
}

impl DeepSeekClient {
    pub fn new(api_key: &str) -> Self {
        Self::with_timeout(api_key, 120)
    }
    pub fn with_timeout(api_key: &str, read_timeout_secs: u32) -> Self {
        Self {
            inner: OpenAiClient::with_timeout(api_key, DEEPSEEK_API_URL, read_timeout_secs),
        }
    }
}

#[async_trait]
impl LlmClient for DeepSeekClient {
    async fn stream(&self, req: LlmRequest, tx: Sender<LlmChunk>) -> Result<()> {
        self.inner.stream(req, tx).await
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse> {
        self.inner.complete(req).await
    }
}
