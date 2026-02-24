/// Agent Loop — the core recursive query-tool-result cycle.
use super::messages::AgentEvent;
use super::tool::{ToolContext, ToolRegistry};
use crate::llm::{ContentBlock, LlmClient, LlmMessage, LlmRequest, MessageContent};
use crate::policy::{PolicyDecision, PolicyGate};
use anyhow::Result;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

const MAX_ITERATIONS: usize = 20;

pub struct AgentLoop {
    pub client: Box<dyn LlmClient>,
    pub registry: Arc<ToolRegistry>,
    pub policy: Arc<PolicyGate>,
    pub system_prompt: String,
    pub model: String,
    pub max_tokens: u32,
}

impl AgentLoop {
    /// Run the agent loop for a single user turn.
    ///
    /// Sends `AgentEvent`s through `event_tx` for streaming to the frontend.
    /// Returns when the LLM produces a final response with no tool calls,
    /// or when `cancel` is set, or after MAX_ITERATIONS.
    pub async fn run(
        &self,
        mut messages: Vec<LlmMessage>,
        event_tx: mpsc::Sender<AgentEvent>,
        cancel: Arc<AtomicBool>,
        ctx: ToolContext,
    ) -> Result<Vec<LlmMessage>> {
        let mut total_input = 0u32;
        let mut total_output = 0u32;

        for _iteration in 0..MAX_ITERATIONS {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            // Build request
            let req = LlmRequest {
                messages: messages.clone(),
                system: Some(self.system_prompt.clone()),
                tools: self.registry.to_tool_defs(),
                model: self.model.clone(),
                max_tokens: self.max_tokens,
                stream: true,
            };

            // Call LLM (non-streaming in the loop; streaming is handled by chat_send command)
            let response = self.client.complete(req).await?;
            total_input += response.input_tokens;
            total_output += response.output_tokens;

            let text_buf = response.content.clone();
            let tool_calls: Vec<(String, String, serde_json::Value)> = response
                .tool_calls
                .iter()
                .map(|tc| (tc.id.clone(), tc.name.clone(), tc.input.clone()))
                .collect();

            // Emit text delta as a single event
            if !text_buf.is_empty() {
                let _ = event_tx.send(AgentEvent::TextDelta { delta: text_buf.clone() }).await;
            }

            // If no tool calls, we're done
            if tool_calls.is_empty() {
                // Add assistant message
                messages.push(LlmMessage {
                    role: "assistant".into(),
                    content: MessageContent::text(&text_buf),
                });
                break;
            }

            // Build assistant message with tool calls
            let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
            if !text_buf.is_empty() {
                assistant_blocks.push(ContentBlock::Text { text: text_buf.clone() });
            }
            for (id, name, input) in &tool_calls {
                assistant_blocks.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
            messages.push(LlmMessage {
                role: "assistant".into(),
                content: MessageContent::Blocks(assistant_blocks),
            });

            // Execute tools (with concurrency limit)
            let mut tool_result_blocks: Vec<ContentBlock> = Vec::new();

            // Check cancellation
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            // Run tools (serially for now; read-only tools could run concurrently)
            for (id, name, input) in &tool_calls {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }

                // Policy check
                let decision = self.policy.check_tool_call(name, input);
                match &decision {
                    PolicyDecision::Deny(reason) => {
                        warn!("Tool '{}' denied by policy: {}", name, reason);
                        let _ = event_tx.send(AgentEvent::ToolEnd {
                            id: id.clone(),
                            name: name.clone(),
                            result: format!("Denied by policy: {}", reason),
                            is_error: true,
                        }).await;
                        tool_result_blocks.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: format!("Error: {}", reason),
                            is_error: true,
                        });
                        continue;
                    }
                    PolicyDecision::Warn(msg) => {
                        warn!("Tool '{}' policy warning: {}", name, msg);
                    }
                    PolicyDecision::Allow => {}
                }

                // Emit tool start
                let _ = event_tx.send(AgentEvent::ToolStart {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }).await;

                // Execute tool
                let result = match self.registry.get(name) {
                    Some(tool) => {
                        debug!("Executing tool: {}", name);
                        match tool.call(input.clone(), &ctx).await {
                            Ok(r) => r,
                            Err(e) => {
                                warn!("Tool '{}' error: {}", name, e);
                                super::tool::ToolResult::err(format!("Tool error: {}", e))
                            }
                        }
                    }
                    None => {
                        super::tool::ToolResult::err(format!("Tool '{}' not found", name))
                    }
                };

                // Emit tool end
                let _ = event_tx.send(AgentEvent::ToolEnd {
                    id: id.clone(),
                    name: name.clone(),
                    result: result.content.clone(),
                    is_error: result.is_error,
                }).await;

                tool_result_blocks.push(ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: result.content,
                    is_error: result.is_error,
                });
            }

            // Add tool results as user message
            messages.push(LlmMessage {
                role: "user".into(),
                content: MessageContent::Blocks(tool_result_blocks),
            });
        }

        // Emit done
        let _ = event_tx.send(AgentEvent::Done {
            total_input_tokens: total_input,
            total_output_tokens: total_output,
        }).await;

        Ok(messages)
    }
}
