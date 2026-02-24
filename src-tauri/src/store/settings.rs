use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Anthropic API key
    #[serde(default)]
    pub anthropic_api_key: String,
    /// OpenAI API key
    #[serde(default)]
    pub openai_api_key: String,
    /// Active LLM provider: "anthropic" | "openai" | "custom"
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Model name (e.g. "claude-sonnet-4-5" or "gpt-4o")
    #[serde(default = "default_model")]
    pub model: String,
    /// Custom base URL for OpenAI-compatible endpoints
    #[serde(default)]
    pub custom_base_url: String,
    /// Workspace root directory (files are restricted to this path)
    #[serde(default = "default_workspace")]
    pub workspace_root: String,
    /// UI language: "zh" | "en"
    #[serde(default = "default_language")]
    pub language: String,
    /// Maximum tokens per LLM response
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Whether to show permission dialogs for shell commands
    #[serde(default = "default_true")]
    pub confirm_shell_commands: bool,
    /// Whether to show permission dialogs for file writes
    #[serde(default = "default_true")]
    pub confirm_file_writes: bool,

    /// Internal: path to the config file (not serialized)
    #[serde(skip)]
    pub config_path: PathBuf,
}

fn default_provider() -> String { "anthropic".into() }
fn default_model() -> String { "claude-sonnet-4-5".into() }
fn default_workspace() -> String {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Pisci")
        .to_string_lossy()
        .into_owned()
}
fn default_language() -> String { "zh".into() }
fn default_max_tokens() -> u32 { 4096 }
fn default_true() -> bool { true }

impl Default for Settings {
    fn default() -> Self {
        Self {
            anthropic_api_key: String::new(),
            openai_api_key: String::new(),
            provider: default_provider(),
            model: default_model(),
            custom_base_url: String::new(),
            workspace_root: default_workspace(),
            language: default_language(),
            max_tokens: default_max_tokens(),
            confirm_shell_commands: true,
            confirm_file_writes: true,
            config_path: PathBuf::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        let mut settings = if path.exists() {
            let content = std::fs::read_to_string(path)?;
            serde_json::from_str::<Settings>(&content).unwrap_or_default()
        } else {
            Settings::default()
        };
        settings.config_path = path.to_path_buf();
        Ok(settings)
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&self.config_path, json)?;
        Ok(())
    }

    /// Returns true if at least one API key is configured
    pub fn is_configured(&self) -> bool {
        !self.anthropic_api_key.trim().is_empty() || !self.openai_api_key.trim().is_empty()
    }

    /// Returns the active API key for the configured provider
    pub fn active_api_key(&self) -> &str {
        match self.provider.as_str() {
            "openai" | "custom" => &self.openai_api_key,
            _ => &self.anthropic_api_key,
        }
    }
}
