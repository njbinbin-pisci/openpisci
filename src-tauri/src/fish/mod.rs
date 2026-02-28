/// Fish (小鱼) — User-defined sub-Agent system for OpenPisci.
///
/// Each Fish is a specialized Agent with its own persona, tool permissions,
/// system prompt, and optional user configuration. Fish are defined via
/// FISH.toml files and can be activated to create dedicated chat sessions.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// FISH.toml definition structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FishDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub agent: FishAgentConfig,
    #[serde(default)]
    pub settings: Vec<FishSettingDef>,
    /// Whether this is a built-in fish (not user-installed)
    #[serde(default)]
    pub builtin: bool,
}

fn default_icon() -> String {
    "🐠".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FishAgentConfig {
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default)]
    pub model: String, // empty = use global default
}

fn default_system_prompt() -> String {
    "You are a helpful specialized assistant.".to_string()
}

fn default_max_iterations() -> u32 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FishSettingDef {
    pub key: String,
    pub label: String,
    #[serde(default = "default_setting_type")]
    pub setting_type: String, // "text", "password", "select", "toggle"
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub placeholder: String,
    #[serde(default)]
    pub options: Vec<FishSettingOption>,
}

fn default_setting_type() -> String {
    "text".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FishSettingOption {
    pub value: String,
    pub label: String,
}

// ---------------------------------------------------------------------------
// Fish instance (runtime state)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FishInstance {
    pub fish_id: String,
    pub session_id: String,
    pub status: String, // "active", "paused", "error"
    pub user_config: HashMap<String, String>,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Built-in Fish definitions
// ---------------------------------------------------------------------------

/// Returns all built-in Fish definitions (compiled into the binary).
pub fn builtin_fish() -> Vec<FishDefinition> {
    vec![
        FishDefinition {
            id: "file-assistant".to_string(),
            name: "文件助手".to_string(),
            description: "专注于文件管理、整理和批量操作的小鱼。擅长文件重命名、目录整理、内容搜索等任务。".to_string(),
            icon: "🐠".to_string(),
            tools: vec![
                "file_read".to_string(),
                "file_write".to_string(),
                "shell".to_string(),
                "memory_store".to_string(),
            ],
            agent: FishAgentConfig {
                system_prompt: "你是一条专注于文件管理的小鱼（OpenPisci 子 Agent）。\n\
                    你的专长是：\n\
                    - 文件和目录的整理、重命名、移动\n\
                    - 批量文件操作（批量重命名、格式转换等）\n\
                    - 文件内容搜索和分析\n\
                    - 目录结构可视化和报告\n\n\
                    安全原则：\n\
                    - 删除操作前必须确认\n\
                    - 优先在用户指定的工作目录内操作\n\
                    - 遇到系统文件时谨慎处理\n\n\
                    当你了解到用户的文件管理偏好时，使用 memory_store 保存。".to_string(),
                max_iterations: 20,
                model: String::new(),
            },
            settings: vec![
                FishSettingDef {
                    key: "workspace".to_string(),
                    label: "默认工作目录".to_string(),
                    setting_type: "text".to_string(),
                    default: String::new(),
                    placeholder: "例如：C:\\Users\\你的用户名\\Documents".to_string(),
                    options: vec![],
                },
            ],
            builtin: true,
        },
    ]
}

// ---------------------------------------------------------------------------
// Fish Registry
// ---------------------------------------------------------------------------

pub struct FishRegistry {
    fish: Vec<FishDefinition>,
}

impl FishRegistry {
    /// Load Fish from built-ins + user directory.
    pub fn load(user_fish_dir: Option<&Path>) -> Self {
        let mut fish = builtin_fish();

        if let Some(dir) = user_fish_dir {
            if dir.exists() {
                match load_user_fish(dir) {
                    Ok(user_fish) => {
                        tracing::info!("Loaded {} user fish from {}", user_fish.len(), dir.display());
                        fish.extend(user_fish);
                    }
                    Err(e) => tracing::warn!("Failed to load user fish: {}", e),
                }
            }
        }

        Self { fish }
    }

    pub fn list(&self) -> &[FishDefinition] {
        &self.fish
    }

    pub fn get(&self, id: &str) -> Option<&FishDefinition> {
        self.fish.iter().find(|f| f.id == id)
    }
}

/// Scan a directory for FISH.toml files and parse them.
fn load_user_fish(dir: &Path) -> Result<Vec<FishDefinition>> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(dir).context("reading fish dir")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let toml_path = path.join("FISH.toml");
            if toml_path.exists() {
                match load_fish_toml(&toml_path) {
                    Ok(mut def) => {
                        def.builtin = false;
                        result.push(def);
                    }
                    Err(e) => tracing::warn!("Failed to parse {}: {}", toml_path.display(), e),
                }
            }
        }
    }
    Ok(result)
}

fn load_fish_toml(path: &PathBuf) -> Result<FishDefinition> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("parsing {}", path.display()))
}

/// Build a system prompt for a Fish, injecting user config values.
pub fn build_fish_system_prompt(def: &FishDefinition, user_config: &HashMap<String, String>) -> String {
    let mut prompt = def.agent.system_prompt.clone();

    // Inject user config as context
    if !user_config.is_empty() {
        let config_block: String = user_config.iter()
            .filter_map(|(k, v)| {
                if v.is_empty() { return None; }
                // Find the label for this key
                let label = def.settings.iter()
                    .find(|s| s.key == *k)
                    .map(|s| s.label.as_str())
                    .unwrap_or(k.as_str());
                Some(format!("- {}: {}", label, v))
            })
            .collect::<Vec<_>>()
            .join("\n");

        if !config_block.is_empty() {
            prompt.push_str(&format!("\n\n## 用户配置\n{}", config_block));
        }
    }

    prompt
}
