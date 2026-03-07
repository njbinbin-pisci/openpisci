use super::openai::OpenAiClient;
use super::{LlmChunk, LlmClient, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

/// Kimi (Moonshot AI) OpenAI-compatible endpoint
/// Docs: https://platform.moonshot.cn/docs
/// China: https://api.moonshot.cn/v1
/// Global: https://api.moonshot.ai/v1
const KIMI_API_URL: &str = "https://api.moonshot.cn/v1";

pub struct KimiClient {
    inner: OpenAiClient,
}

impl KimiClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            inner: OpenAiClient::new(api_key, KIMI_API_URL),
        }
    }
}

#[async_trait]
impl LlmClient for KimiClient {
    async fn stream(&self, req: LlmRequest, tx: Sender<LlmChunk>) -> Result<()> {
        self.inner.stream(req, tx).await
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse> {
        self.inner.complete(req).await
    }
}
