use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub use crate::agent::harness::ToolDefMode;

/// Snapshot of Settings fields that tools may need at runtime.
/// This avoids taking the full Settings lock inside async tool code.
#[derive(Debug, Clone, Default)]
pub struct ToolSettings {
    // Email
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub smtp_from_name: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub email_enabled: bool,
    /// Per-tool config map: tool_name → { field: value }
    /// Populated from Settings.user_tool_configs at agent launch.
    pub user_tool_configs: HashMap<String, Value>,
}

impl ToolSettings {
    pub fn from_settings(s: &crate::store::settings::Settings) -> Self {
        Self {
            smtp_host: s.smtp_host.clone(),
            smtp_port: s.smtp_port,
            smtp_username: s.smtp_username.clone(),
            smtp_password: s.smtp_password.clone(),
            smtp_from_name: s.smtp_from_name.clone(),
            imap_host: s.imap_host.clone(),
            imap_port: s.imap_port,
            email_enabled: s.email_enabled,
            user_tool_configs: s.user_tool_configs.clone(),
        }
    }
}

/// Context passed to every tool call
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub workspace_root: PathBuf,
    /// If true, skip permission checks (for scheduled tasks)
    #[allow(dead_code)]
    pub bypass_permissions: bool,
    /// Runtime-accessible settings snapshot (credentials etc.)
    pub settings: Arc<ToolSettings>,
    /// Maximum agent loop iterations (from Settings, default 50)
    pub max_iterations: Option<u32>,
    /// Memory owner: "pisci" for the main agent, or a koi_id for Koi agents.
    /// Used by memory_store and auto_extract_memories to scope reads/writes.
    pub memory_owner_id: String,
    /// Optional pool session ID for Chat Pool integration.
    pub pool_session_id: Option<String>,
    /// Cooperative cancellation flag for long-running tools.
    pub cancel: Arc<AtomicBool>,
}

impl ToolContext {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Image data attached to a tool result (for Vision AI)
#[derive(Debug, Clone)]
pub struct ImageData {
    pub base64: String,
    pub media_type: String,
}

impl ImageData {
    pub fn png(base64: impl Into<String>) -> Self {
        Self {
            base64: base64.into(),
            media_type: "image/png".into(),
        }
    }
    pub fn jpeg(base64: impl Into<String>) -> Self {
        Self {
            base64: base64.into(),
            media_type: "image/jpeg".into(),
        }
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
        Self {
            content: content.into(),
            is_error: false,
            image: None,
        }
    }
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            image: None,
        }
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

    /// Human-readable description (full version, used during schema
    /// correction or when the agent explicitly recalls a tool).
    fn description(&self) -> &str;

    /// JSON Schema for the tool's input parameters (full version,
    /// including long prose descriptions, examples, titles).
    fn input_schema(&self) -> Value;

    /// Minimal description for the default (`ToolDefMode::Minimal`)
    /// injection path. Override this for high-usage tools to hand-tune
    /// a terse one-line summary; the default simply forwards to
    /// [`Tool::description`] which retains full behaviour.
    ///
    /// p3 (`schema_correction`) surfaces the *full* description back to
    /// the model via a deterministic `[schema_correction]` envelope
    /// when a call fails with a structural schema error, so agents
    /// can still recover from terse minimal prompts.
    fn description_minimal(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.description())
    }

    /// Minimal JSON schema for default injection. The default
    /// implementation strips documentation-only keys (`description`,
    /// `examples`, `title`, `$comment`, `default`, `deprecated`,
    /// `readOnly`, `writeOnly`) while preserving every machine
    /// constraint: `type`, `required`, `properties`, `enum`, `const`,
    /// `oneOf` / `anyOf` / `allOf` / `not`, `additionalProperties`,
    /// `items`, `prefixItems`, `minimum`/`maximum` (+ exclusive),
    /// `minItems`/`maxItems`, `minLength`/`maxLength`, `pattern`,
    /// `format`, `multipleOf`, `uniqueItems`, `$ref` / `$defs` etc.
    ///
    /// Tools that want a hand-tuned minimal schema (e.g. to simplify
    /// union types the default stripper would otherwise keep verbatim)
    /// override this method directly.
    fn input_schema_minimal(&self) -> Value {
        strip_schema_to_minimal(&self.input_schema())
    }

    /// Whether this tool is read-only (can run concurrently)
    fn is_read_only(&self) -> bool {
        false
    }

    /// Whether this tool requires user confirmation
    fn needs_confirmation(&self, _input: &Value) -> bool {
        false
    }

    /// Execute the tool
    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult>;
}

/// Keys preserved unchanged (their semantic meaning is machine-readable).
/// Keys *not* in this set and not recognised as recursive containers
/// are dropped during minimisation.
const PRESERVED_SCHEMA_KEYS: &[&str] = &[
    "type",
    "required",
    "enum",
    "const",
    "additionalProperties",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "multipleOf",
    "minProperties",
    "maxProperties",
    "uniqueItems",
    "$ref",
    "nullable",
];

/// Keys whose values are themselves schemas (recurse into them) or a
/// map of schemas (recurse into each value).
const RECURSIVE_SCHEMA_KEYS_SINGLE: &[&str] = &[
    "items",
    "not",
    "contains",
    "propertyNames",
    "if",
    "then",
    "else",
];

/// Keys whose values are arrays of schemas (recurse into each element).
const RECURSIVE_SCHEMA_KEYS_ARRAY: &[&str] = &["oneOf", "anyOf", "allOf", "prefixItems"];

/// Keys whose values are objects whose values are schemas
/// (`properties`, `$defs`, `definitions`, `dependentSchemas`).
const RECURSIVE_SCHEMA_KEYS_MAP: &[&str] = &[
    "properties",
    "$defs",
    "definitions",
    "dependentSchemas",
    "patternProperties",
];

/// Keys explicitly dropped (pure documentation / authoring metadata).
const DROP_SCHEMA_KEYS: &[&str] = &[
    "description",
    "examples",
    "title",
    "$comment",
    "default",
    "deprecated",
    "readOnly",
    "writeOnly",
    "example",
];

/// Recursively strip a JSON schema down to its machine-enforceable
/// constraints. Structure is preserved (nested objects, arrays,
/// `oneOf`/`anyOf` branches) but documentation-only keys are removed.
pub fn strip_schema_to_minimal(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => Value::Object(strip_object_map(map)),
        Value::Array(arr) => Value::Array(arr.iter().map(strip_schema_to_minimal).collect()),
        other => other.clone(),
    }
}

fn strip_object_map(map: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::with_capacity(map.len());
    for (k, v) in map {
        if DROP_SCHEMA_KEYS.contains(&k.as_str()) {
            continue;
        }
        if RECURSIVE_SCHEMA_KEYS_MAP.contains(&k.as_str()) {
            if let Value::Object(inner) = v {
                let mut sub = Map::with_capacity(inner.len());
                for (ik, iv) in inner {
                    sub.insert(ik.clone(), strip_schema_to_minimal(iv));
                }
                out.insert(k.clone(), Value::Object(sub));
            } else {
                out.insert(k.clone(), strip_schema_to_minimal(v));
            }
            continue;
        }
        if RECURSIVE_SCHEMA_KEYS_SINGLE.contains(&k.as_str()) {
            out.insert(k.clone(), strip_schema_to_minimal(v));
            continue;
        }
        if RECURSIVE_SCHEMA_KEYS_ARRAY.contains(&k.as_str()) {
            if let Value::Array(arr) = v {
                out.insert(
                    k.clone(),
                    Value::Array(arr.iter().map(strip_schema_to_minimal).collect()),
                );
            } else {
                out.insert(k.clone(), strip_schema_to_minimal(v));
            }
            continue;
        }
        if PRESERVED_SCHEMA_KEYS.contains(&k.as_str()) {
            // Constraint values (enum arrays etc.) are preserved verbatim;
            // they are leaf data, not schemas.
            out.insert(k.clone(), v.clone());
            continue;
        }
        // Unknown key — conservatively drop. This covers authoring
        // annotations like `x-*` and unrecognised future keywords.
    }
    out
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

    pub fn unregister(&mut self, name: &str) {
        self.tools.retain(|t| t.name() != name);
    }

    pub fn retain(&mut self, mut predicate: impl FnMut(&dyn Tool) -> bool) {
        self.tools.retain(|tool| predicate(tool.as_ref()));
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    pub fn all(&self) -> &[Box<dyn Tool>] {
        &self.tools
    }

    /// Build tool definitions for the LLM using the given injection
    /// [`ToolDefMode`]. `Minimal` is the default throughout the
    /// harness; `Full` is used by p3's schema-correction path and by
    /// the p11 `recall_tool_result` flow.
    pub fn to_tool_defs(&self, mode: ToolDefMode) -> Vec<crate::llm::ToolDef> {
        self.tools
            .iter()
            .map(|t| tool_def_for(t.as_ref(), mode))
            .collect()
    }

    /// Produce a single tool's definition in the requested mode.
    /// Returns `None` if the tool name is not registered.
    pub fn to_tool_defs_for(&self, name: &str, mode: ToolDefMode) -> Option<crate::llm::ToolDef> {
        self.get(name).map(|t| tool_def_for(t, mode))
    }
}

fn tool_def_for(tool: &dyn Tool, mode: ToolDefMode) -> crate::llm::ToolDef {
    match mode {
        ToolDefMode::Minimal => crate::llm::ToolDef {
            name: tool.name().to_string(),
            description: tool.description_minimal().into_owned(),
            input_schema: tool.input_schema_minimal(),
        },
        ToolDefMode::Full => crate::llm::ToolDef {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            input_schema: tool.input_schema(),
        },
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    // ── Fake tool for testing minimisation ───────────────────────────
    struct FakeTool {
        schema: Value,
        description: &'static str,
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            "fake"
        }
        fn description(&self) -> &str {
            self.description
        }
        fn input_schema(&self) -> Value {
            self.schema.clone()
        }
        async fn call(&self, _input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
            Ok(ToolResult::ok(""))
        }
    }

    #[test]
    fn strip_drops_description_examples_title_and_comments() {
        let schema = json!({
            "type": "object",
            "title": "Shell command",
            "description": "A really long prose description that bloats L4.",
            "$comment": "authoring note",
            "properties": {
                "cmd": {
                    "type": "string",
                    "description": "The shell command to run.",
                    "examples": ["ls -la", "pwd"],
                    "default": "ls"
                },
                "timeout_sec": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600
                }
            },
            "required": ["cmd"]
        });
        let minimised = strip_schema_to_minimal(&schema);
        assert_eq!(minimised.get("type"), Some(&json!("object")));
        assert!(minimised.get("description").is_none());
        assert!(minimised.get("title").is_none());
        assert!(minimised.get("$comment").is_none());

        let props = minimised.get("properties").unwrap();
        let cmd = props.get("cmd").unwrap();
        assert_eq!(cmd.get("type"), Some(&json!("string")));
        assert!(cmd.get("description").is_none());
        assert!(cmd.get("examples").is_none());
        assert!(cmd.get("default").is_none());
        let timeout = props.get("timeout_sec").unwrap();
        assert_eq!(timeout.get("minimum"), Some(&json!(1)));
        assert_eq!(timeout.get("maximum"), Some(&json!(600)));
        assert_eq!(minimised.get("required"), Some(&json!(["cmd"])));
    }

    #[test]
    fn strip_preserves_enum_const_and_union_branches() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": {
                    "description": "one of these values",
                    "enum": ["read", "write", "append"]
                },
                "verb": {
                    "description": "constant",
                    "const": "get"
                },
                "payload": {
                    "description": "union",
                    "oneOf": [
                        { "type": "string", "pattern": "^[a-z]+$", "description": "lowercase" },
                        { "type": "integer", "minimum": 0 }
                    ]
                }
            }
        });
        let minimised = strip_schema_to_minimal(&schema);
        let props = minimised.get("properties").unwrap();
        assert_eq!(
            props.get("mode").unwrap().get("enum"),
            Some(&json!(["read", "write", "append"]))
        );
        assert_eq!(props.get("verb").unwrap().get("const"), Some(&json!("get")));
        assert!(props.get("payload").unwrap().get("description").is_none());
        let one_of = props.get("payload").unwrap().get("oneOf").unwrap();
        let branches = one_of.as_array().unwrap();
        assert_eq!(branches.len(), 2);
        assert!(branches[0].get("description").is_none());
        assert_eq!(branches[0].get("pattern"), Some(&json!("^[a-z]+$")));
        assert_eq!(branches[1].get("minimum"), Some(&json!(0)));
    }

    #[test]
    fn strip_recurses_into_nested_objects_and_arrays() {
        let schema = json!({
            "type": "array",
            "description": "drop me",
            "items": {
                "type": "object",
                "description": "drop me too",
                "properties": {
                    "nested": {
                        "type": "object",
                        "title": "drop",
                        "properties": {
                            "leaf": { "type": "string", "description": "drop" }
                        },
                        "required": ["leaf"]
                    }
                },
                "required": ["nested"]
            },
            "minItems": 1,
            "maxItems": 10
        });
        let minimised = strip_schema_to_minimal(&schema);
        assert!(minimised.get("description").is_none());
        assert_eq!(minimised.get("minItems"), Some(&json!(1)));
        assert_eq!(minimised.get("maxItems"), Some(&json!(10)));
        let items = minimised.get("items").unwrap();
        assert!(items.get("description").is_none());
        let nested = items.get("properties").unwrap().get("nested").unwrap();
        assert!(nested.get("title").is_none());
        assert_eq!(nested.get("required"), Some(&json!(["leaf"])));
        let leaf = nested.get("properties").unwrap().get("leaf").unwrap();
        assert!(leaf.get("description").is_none());
        assert_eq!(leaf.get("type"), Some(&json!("string")));
    }

    #[test]
    fn strip_drops_unknown_authoring_keys_like_x_prefix() {
        let schema = json!({
            "type": "object",
            "x-frontend-hint": "red button",
            "properties": { "k": { "type": "string" } }
        });
        let minimised = strip_schema_to_minimal(&schema);
        assert!(minimised.get("x-frontend-hint").is_none());
        assert!(minimised.get("properties").is_some());
    }

    #[tokio::test]
    async fn tool_registry_returns_minimal_and_full_defs() {
        let full_schema = json!({
            "type": "object",
            "description": "big prose",
            "properties": { "x": { "type": "string", "description": "drop" } },
            "required": ["x"]
        });
        let tool = FakeTool {
            schema: full_schema.clone(),
            description: "A really verbose description that bloats L4 tokens.",
        };
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(tool));

        let minimal = registry.to_tool_defs(ToolDefMode::Minimal);
        assert_eq!(minimal.len(), 1);
        // Default description_minimal forwards full description; that's
        // fine — minimisation wins come from the schema path and from
        // hand-tuned minimal descriptions (applied per high-usage tool).
        assert!(!minimal[0].description.is_empty());
        assert!(minimal[0]
            .input_schema
            .get("description")
            .is_none());
        let inner_x = minimal[0]
            .input_schema
            .get("properties")
            .unwrap()
            .get("x")
            .unwrap();
        assert!(inner_x.get("description").is_none());
        assert_eq!(inner_x.get("type"), Some(&json!("string")));

        let full = registry.to_tool_defs(ToolDefMode::Full);
        assert_eq!(full.len(), 1);
        assert!(full[0].input_schema.get("description").is_some());

        let single = registry.to_tool_defs_for("fake", ToolDefMode::Full);
        assert!(single.is_some());
        let missing = registry.to_tool_defs_for("nope", ToolDefMode::Full);
        assert!(missing.is_none());
    }

    #[test]
    fn tool_def_mode_defaults_to_minimal_via_trait_default() {
        let mode = ToolDefMode::default();
        assert_eq!(mode, ToolDefMode::Minimal);
    }
}
