//! MCP (Model Context Protocol) client and tool proxy.
//!
//! Supports two transports:
//!   - stdio: spawn a local process and communicate via stdin/stdout JSON-RPC
//!   - sse:   connect to an HTTP server via Server-Sent Events
//!
//! Each MCP server exposes a list of tools; each tool is registered as a
//! separate `McpProxyTool` in the tool registry.

use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tracing::{info, warn};

// ─── Config ──────────────────────────────────────────────────────────────────
//
// The serialized `McpServerConfig` lives in `pisci_kernel::store::settings`
// (kept with the rest of the `Settings` schema). It is re-exported here so
// existing `crate::tools::mcp::McpServerConfig` call sites keep working.

pub use crate::store::settings::McpServerConfig;

// ─── Tool info returned by tools/list ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Option<Value>,
}

// ─── JSON-RPC helpers ─────────────────────────────────────────────────────────

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

fn make_request(method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": next_id(),
        "method": method,
        "params": params,
    })
}

// ─── Stdio transport ──────────────────────────────────────────────────────────

struct StdioTransport {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    _child: Child,
}

impl StdioTransport {
    async fn spawn(config: &McpServerConfig) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(&config.command);
        cmd.args(&config.args)
            .envs(&config.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn MCP server '{}': {}", config.command, e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdin for MCP server"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdout for MCP server"))?;

        Ok(Self {
            stdin,
            stdout: BufReader::new(stdout),
            _child: child,
        })
    }

    async fn send_request(&mut self, request: &Value) -> Result<Value> {
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        let mut response_line = String::new();
        self.stdout.read_line(&mut response_line).await?;
        let response: Value = serde_json::from_str(response_line.trim()).map_err(|e| {
            anyhow!(
                "Invalid JSON-RPC response: {} (raw: {})",
                e,
                response_line.trim()
            )
        })?;
        Ok(response)
    }
}

// ─── SSE transport ────────────────────────────────────────────────────────────

struct SseTransport {
    base_url: String,
    client: reqwest::Client,
    endpoint: Option<String>,
}

impl SseTransport {
    fn new(url: &str) -> Self {
        Self {
            base_url: url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            endpoint: None,
        }
    }

    async fn connect(&mut self) -> Result<()> {
        // Connect to SSE stream to get the endpoint URL
        let sse_url = format!("{}/sse", self.base_url);
        let resp = self
            .client
            .get(&sse_url)
            .header("Accept", "text/event-stream")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| anyhow!("SSE connect failed: {}", e))?;

        let text = resp.text().await?;
        // Parse "data: /message?sessionId=..." from SSE stream
        for line in text.lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                let endpoint = if data.starts_with('/') {
                    format!("{}{}", self.base_url, data)
                } else {
                    data.to_string()
                };
                self.endpoint = Some(endpoint);
                break;
            }
        }
        if self.endpoint.is_none() {
            // Fallback: use /message endpoint directly
            self.endpoint = Some(format!("{}/message", self.base_url));
        }
        Ok(())
    }

    async fn send_request(&self, request: &Value) -> Result<Value> {
        let fallback = format!("{}/message", self.base_url);
        let endpoint = self.endpoint.as_deref().unwrap_or(&fallback);

        let resp = self
            .client
            .post(endpoint)
            .json(request)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| anyhow!("SSE request failed: {}", e))?;

        let response: Value = resp
            .json()
            .await
            .map_err(|e| anyhow!("Invalid JSON-RPC response: {}", e))?;
        Ok(response)
    }
}

// ─── MCP Client ───────────────────────────────────────────────────────────────

#[allow(clippy::large_enum_variant)]
enum Transport {
    Stdio(StdioTransport),
    Sse(SseTransport),
}

pub struct McpClient {
    config: McpServerConfig,
    transport: Mutex<Option<Transport>>,
}

impl McpClient {
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            transport: Mutex::new(None),
        }
    }

    async fn ensure_connected(&self) -> Result<()> {
        let mut guard = self.transport.lock().await;
        if guard.is_some() {
            return Ok(());
        }

        let transport = match self.config.transport.as_str() {
            "stdio" => {
                let mut t = StdioTransport::spawn(&self.config).await?;
                // Initialize handshake
                let init_req = make_request(
                    "initialize",
                    json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": { "name": "pisci-desktop", "version": "0.1.0" }
                    }),
                );
                let resp = t.send_request(&init_req).await?;
                if resp.get("error").is_some() {
                    return Err(anyhow!("MCP initialize error: {}", resp["error"]));
                }
                // Send initialized notification
                let notif = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized",
                    "params": {}
                });
                let mut line = serde_json::to_string(&notif)?;
                line.push('\n');
                t.stdin.write_all(line.as_bytes()).await?;
                t.stdin.flush().await?;

                Transport::Stdio(t)
            }
            "sse" => {
                let mut t = SseTransport::new(&self.config.url);
                t.connect().await?;
                // Initialize handshake
                let init_req = make_request(
                    "initialize",
                    json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": { "name": "pisci-desktop", "version": "0.1.0" }
                    }),
                );
                let resp = t.send_request(&init_req).await?;
                if resp.get("error").is_some() {
                    return Err(anyhow!("MCP initialize error: {}", resp["error"]));
                }
                Transport::Sse(t)
            }
            other => return Err(anyhow!("Unknown MCP transport: {}", other)),
        };

        *guard = Some(transport);
        info!("MCP server '{}' connected", self.config.name);
        Ok(())
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>> {
        self.ensure_connected().await?;
        let req = make_request("tools/list", json!({}));
        let resp = self.send_rpc(req).await?;
        let tools = resp["result"]["tools"]
            .as_array()
            .ok_or_else(|| anyhow!("tools/list: missing tools array"))?;
        let infos: Vec<McpToolInfo> = tools
            .iter()
            .filter_map(|t| serde_json::from_value(t.clone()).ok())
            .collect();
        Ok(infos)
    }

    #[allow(dead_code)]
    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<String> {
        self.ensure_connected().await?;
        let req = make_request(
            "tools/call",
            json!({
                "name": tool_name,
                "arguments": arguments,
            }),
        );
        let resp = self.send_rpc(req).await?;

        if let Some(err) = resp.get("error") {
            return Err(anyhow!("MCP tool error: {}", err));
        }

        let result = &resp["result"];
        // Extract text content from result
        if let Some(content) = result.get("content") {
            if let Some(arr) = content.as_array() {
                let texts: Vec<String> = arr
                    .iter()
                    .filter_map(|c| {
                        if c["type"].as_str() == Some("text") {
                            c["text"].as_str().map(|s| s.to_string())
                        } else {
                            Some(serde_json::to_string(c).unwrap_or_default())
                        }
                    })
                    .collect();
                return Ok(texts.join("\n"));
            }
        }
        Ok(serde_json::to_string(result)?)
    }

    async fn send_rpc(&self, req: Value) -> Result<Value> {
        let mut guard = self.transport.lock().await;
        match guard.as_mut() {
            Some(Transport::Stdio(t)) => t.send_request(&req).await,
            Some(Transport::Sse(t)) => t.send_request(&req).await,
            None => Err(anyhow!("MCP transport not connected")),
        }
    }

    /// Disconnect and reset the transport (will reconnect on next call)
    pub async fn disconnect(&self) {
        let mut guard = self.transport.lock().await;
        *guard = None;
    }
}

// ─── McpProxyTool ─────────────────────────────────────────────────────────────

/// A single tool exposed by an MCP server, registered as a Tool in the registry.
#[allow(dead_code)]
pub struct McpProxyTool {
    pub server_name: String,
    pub tool_name: String,
    pub tool_description: String,
    pub schema: Value,
    pub client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpProxyTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn input_schema(&self) -> Value {
        self.schema.clone()
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        info!(
            "MCP tool call: server={} tool={}",
            self.server_name, self.tool_name
        );
        match self.client.call_tool(&self.tool_name, input).await {
            Ok(output) => Ok(ToolResult::ok(output)),
            Err(e) => {
                warn!("MCP tool '{}' error: {}", self.tool_name, e);
                // Attempt reconnect on next call
                self.client.disconnect().await;
                Ok(ToolResult::err(format!("MCP tool error: {}", e)))
            }
        }
    }
}

// ─── Build proxy tools from config ───────────────────────────────────────────

/// Connect to an MCP server and return proxy tools for all its tools.
/// Returns an empty vec on connection failure (with a warning).
#[allow(dead_code)]
pub async fn build_mcp_tools(config: &McpServerConfig) -> Vec<McpProxyTool> {
    if !config.enabled || config.name.is_empty() {
        return vec![];
    }

    let client = Arc::new(McpClient::new(config.clone()));
    match client.list_tools().await {
        Ok(tools) => {
            info!(
                "MCP server '{}' provides {} tool(s)",
                config.name,
                tools.len()
            );
            tools
                .into_iter()
                .map(|t| McpProxyTool {
                    server_name: config.name.clone(),
                    tool_name: t.name.clone(),
                    tool_description: t.description.unwrap_or_default(),
                    schema: t.input_schema.unwrap_or_else(|| {
                        json!({
                            "type": "object",
                            "properties": {}
                        })
                    }),
                    client: client.clone(),
                })
                .collect()
        }
        Err(e) => {
            warn!("MCP server '{}' failed to connect: {}", config.name, e);
            vec![]
        }
    }
}
