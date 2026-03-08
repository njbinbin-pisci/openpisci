/// Fish (小鱼) — Stateless sub-Agent system for OpenPisci.
///
/// Each Fish is a specialized Agent with its own persona, tool permissions,
/// and system prompt. Fish are defined via FISH.toml files and invoked
/// ephemerally by the main Agent through `call_fish` — no persistent session.
///
/// Fish can also be auto-generated from installed Skills (via SkillLoader),
/// giving each skill its own isolated execution environment.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// FISH.toml definition structures
// ---------------------------------------------------------------------------

/// Where a Fish definition comes from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FishSource {
    /// Compiled into the binary (hardcoded in builtin_fish())
    #[default]
    Builtin,
    /// Auto-generated from an installed Skill (SKILL.md)
    Skill,
    /// User-created FISH.toml in the fish directory
    User,
}

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
    /// Where this fish definition comes from
    #[serde(default)]
    pub source: FishSource,
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
            source: FishSource::Builtin,
        },
    ]
}

/// Skill → Fish icon mapping based on skill name keywords.
fn skill_icon(skill_name: &str) -> &'static str {
    let lower = skill_name.to_lowercase();
    if lower.contains("office") || lower.contains("word") || lower.contains("excel") { return "📊"; }
    if lower.contains("file") || lower.contains("文件") { return "📁"; }
    if lower.contains("web") || lower.contains("browser") || lower.contains("网页") { return "🌐"; }
    if lower.contains("system") || lower.contains("admin") || lower.contains("系统") { return "⚙️"; }
    if lower.contains("desktop") || lower.contains("桌面") || lower.contains("uia") { return "🖥️"; }
    if lower.contains("email") || lower.contains("邮件") { return "📧"; }
    if lower.contains("code") || lower.contains("代码") { return "💻"; }
    "🐡"
}

/// Build a Fish ID from a skill name (slugified).
fn skill_fish_id(skill_name: &str) -> String {
    let slug = skill_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>();
    // Collapse multiple dashes
    let mut result = String::new();
    let mut last_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !last_dash { result.push(c); }
            last_dash = true;
        } else {
            result.push(c);
            last_dash = false;
        }
    }
    format!("skill-{}", result.trim_matches('-'))
}

/// Convert a SkillDefinition into a FishDefinition.
/// The skill's instructions become the fish's system_prompt.
/// The skill's tools list becomes the fish's allowed tools.
pub fn fish_from_skill(skill: &crate::skills::loader::SkillDefinition) -> FishDefinition {
    FishDefinition {
        id: skill_fish_id(&skill.name),
        name: format!("{} 助手", skill.name),
        description: skill.description.clone(),
        icon: skill_icon(&skill.name).to_string(),
        tools: skill.tools.clone(),
        agent: FishAgentConfig {
            system_prompt: format!(
                "你是一条专注于「{}」任务的小鱼（OpenPisci 子 Agent）。\n\n{}\n\n\
                请专注于你的专长领域，高效完成用户交给你的任务。\
                遇到超出你工具权限范围的任务时，请明确告知用户。",
                skill.name,
                skill.instructions
            ),
            max_iterations: 25,
            model: String::new(),
        },
        settings: vec![],
        builtin: true,
        source: FishSource::Skill,
    }
}

// ---------------------------------------------------------------------------
// Fish Registry
// ---------------------------------------------------------------------------

pub struct FishRegistry {
    fish: Vec<FishDefinition>,
}

impl FishRegistry {
    /// Load Fish from built-ins + skills (auto-generated) + user directory.
    ///
    /// `app_data_dir` is used to locate both the user fish directory
    /// (`<app_data>/fish/`) and the skills directory (`<app_data>/skills/`).
    pub fn load(app_data_dir: Option<&Path>) -> Self {
        let mut fish = builtin_fish();

        // Auto-generate Fish from installed Skills
        if let Some(dir) = app_data_dir {
            let skills_dir = dir.join("skills");
            if skills_dir.exists() {
                let mut loader = crate::skills::loader::SkillLoader::new(&skills_dir);
                match loader.load_all() {
                    Ok(()) => {
                        let skill_fish: Vec<FishDefinition> = loader
                            .list_skills()
                            .into_iter()
                            .map(fish_from_skill)
                            .collect();
                        tracing::info!(
                            "Generated {} skill-based fish from {}",
                            skill_fish.len(),
                            skills_dir.display()
                        );
                        fish.extend(skill_fish);
                    }
                    Err(e) => tracing::warn!("Failed to load skills for fish generation: {}", e),
                }
            }
        }

        // Load user-created FISH.toml files
        if let Some(dir) = app_data_dir {
            let user_fish_dir = dir.join("fish");
            if user_fish_dir.exists() {
                match load_user_fish(&user_fish_dir) {
                    Ok(mut user_fish) => {
                        tracing::info!(
                            "Loaded {} user fish from {}",
                            user_fish.len(),
                            user_fish_dir.display()
                        );
                        for f in &mut user_fish {
                            f.source = FishSource::User;
                        }
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

/// Build a system prompt for a Fish.
pub fn build_fish_system_prompt(def: &FishDefinition) -> String {
    def.agent.system_prompt.clone()
}
