use super::openai::OpenAiClient;
use super::{LlmChunk, LlmClient, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

/// Zhipu AI (智谱) Z.AI OpenAI-compatible endpoint
/// Docs: https://docs.z.ai/guides/llm/glm-5
/// Also compatible with: https://open.bigmodel.cn/api/paas/v4 (China mainland)
const ZHIPU_API_URL: &str = "https://api.z.ai/api/paas/v4";

pub struct ZhipuClient {
    inner: OpenAiClient,
}

impl ZhipuClient {
    pub fn new(api_key: &str) -> Self {
        Self::with_timeout(api_key, 120)
    }
    pub fn with_timeout(api_key: &str, read_timeout_secs: u32) -> Self {
        Self {
            inner: OpenAiClient::with_timeout(api_key, ZHIPU_API_URL, read_timeout_secs),
        }
    }
}

#[async_trait]
impl LlmClient for ZhipuClient {
    async fn stream(&self, req: LlmRequest, tx: Sender<LlmChunk>) -> Result<()> {
        self.inner.stream(req, tx).await
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse> {
        self.inner.complete(req).await
    }
}
