use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;

/// Context passed to every tool call
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub workspace_root: PathBuf,
    /// If true, skip permission checks (for scheduled tasks)
    pub bypass_permissions: bool,
}

/// Image data attached to a tool result (for Vision AI)
#[derive(Debug, Clone)]
pub struct ImageData {
    pub base64: String,
    pub media_type: String,
}

impl ImageData {
    pub fn png(base64: impl Into<String>) -> Self {
        Self { base64: base64.into(), media_type: "image/png".into() }
    }
    pub fn jpeg(base64: impl Into<String>) -> Self {
        Self { base64: base64.into(), media_type: "image/jpeg".into() }
    }
}

/// Result from a tool execution
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Content shown to the LLM
    pub content: String,
    /// Whether this is an error
    pub is_error: bool,
    /// Optional image data (screenshot etc.) passed to Vision AI
    pub image: Option<ImageData>,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false, image: None }
    }
    pub fn err(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true, image: None }
    }
    pub fn with_image(mut self, image: ImageData) -> Self {
        self.image = Some(image);
        self
    }
}

/// The Tool trait — all agent tools implement this
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (used in LLM tool definitions)
    fn name(&self) -> &str;

    /// Human-readable description
    fn description(&self) -> &str;

    /// JSON Schema for the tool's input parameters
    fn input_schema(&self) -> Value;

    /// Whether this tool is read-only (can run concurrently)
    fn is_read_only(&self) -> bool { false }

    /// Whether this tool requires user confirmation
    fn needs_confirmation(&self, _input: &Value) -> bool { false }

    /// Execute the tool
    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult>;
}

/// Registry of all available tools
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
    }

    pub fn all(&self) -> &[Box<dyn Tool>] {
        &self.tools
    }

    /// Build tool definitions for the LLM
    pub fn to_tool_defs(&self) -> Vec<crate::llm::ToolDef> {
        self.tools.iter().map(|t| crate::llm::ToolDef {
            name: t.name().to_string(),
            description: t.description().to_string(),
            input_schema: t.input_schema(),
        }).collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
