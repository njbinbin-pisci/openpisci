use super::openai::OpenAiClient;
use super::{LlmChunk, LlmClient, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

const QWEN_API_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";

pub struct QwenClient {
    inner: OpenAiClient,
}

impl QwenClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            inner: OpenAiClient::new(api_key, QWEN_API_URL),
        }
    }
}

#[async_trait]
impl LlmClient for QwenClient {
    async fn stream(&self, req: LlmRequest, tx: Sender<LlmChunk>) -> Result<()> {
        self.inner.stream(req, tx).await
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse> {
        self.inner.complete(req).await
    }
}
