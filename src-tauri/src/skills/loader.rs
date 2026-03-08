use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub triggers: Vec<String>,
    /// Supported platforms, e.g. ["windows"], ["linux", "macos"], or empty = all platforms.
    #[serde(default)]
    pub platform: Vec<String>,
    #[serde(default)]
    pub source: String,
    pub instructions: String,
    pub source_path: PathBuf,
}

/// Result of a pre-install compatibility check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityCheck {
    pub compatible: bool,
    /// Human-readable reasons why the skill is incompatible (empty = compatible).
    pub issues: Vec<String>,
    /// Warnings that don't block installation but are worth noting.
    pub warnings: Vec<String>,
}

impl CompatibilityCheck {
    pub fn ok() -> Self {
        Self { compatible: true, issues: vec![], warnings: vec![] }
    }
}

/// Check whether a skill can be installed on the current system.
///
/// Checks:
/// 1. `platform` field — must include "windows" (or be empty/absent).
/// 2. `dependencies` field — each entry is checked for availability via PATH lookup.
///    Recognised dependency names: python, python3, node, npm, npx, ruby, go, java, dotnet, git, ffmpeg.
pub async fn check_skill_compatibility(skill: &SkillDefinition) -> CompatibilityCheck {
    let mut issues = Vec::new();
    let mut warnings = Vec::new();

    // ── Platform check ────────────────────────────────────────────────────────
    if !skill.platform.is_empty() {
        let supported = skill.platform.iter().any(|p| {
            let p = p.to_lowercase();
            p == "windows" || p == "win" || p == "win32" || p == "win64"
        });
        if !supported {
            issues.push(format!(
                "此技能仅支持 {} 平台，当前系统为 Windows",
                skill.platform.join(" / ")
            ));
        }
    }

    // ── Dependency check ──────────────────────────────────────────────────────
    for dep in &skill.dependencies {
        let dep_lower = dep.to_lowercase();
        // Extract the executable name from entries like "python>=3.8" or "node@18"
        let exe = dep_lower
            .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
            .next()
            .unwrap_or(&dep_lower)
            .to_string();

        // Map common aliases
        let exe = match exe.as_str() {
            "python3" => "python".to_string(),
            "nodejs" => "node".to_string(),
            "dotnet-sdk" | "dotnet-runtime" => "dotnet".to_string(),
            other => other.to_string(),
        };

        // Only check well-known runtimes; ignore vague entries like "office" or "windows"
        let known_runtimes = [
            "python", "node", "npm", "npx", "ruby", "go", "java",
            "dotnet", "git", "ffmpeg", "cargo", "pip", "pip3",
        ];
        if !known_runtimes.contains(&exe.as_str()) {
            continue;
        }

        let found = tokio::process::Command::new("where")
            .arg(&exe)
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !found {
            issues.push(format!(
                "缺少依赖 `{}`：请先安装后再使用此技能",
                dep
            ));
        }
    }

    // ── Elevated-permission warning ───────────────────────────────────────────
    if !skill.permissions.is_empty() {
        warnings.push(format!(
            "此技能申请了高权限：{}",
            skill.permissions.join(", ")
        ));
    }

    let compatible = issues.is_empty();
    CompatibilityCheck { compatible, issues, warnings }
}

pub struct SkillLoader {
    skills_dir: PathBuf,
    skills: HashMap<String, SkillDefinition>,
}

impl SkillLoader {
    pub fn new(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            skills_dir: skills_dir.into(),
            skills: HashMap::new(),
        }
    }

    pub fn load_all(&mut self) -> Result<()> {
        if !self.skills_dir.exists() {
            std::fs::create_dir_all(&self.skills_dir)?;
            self.create_builtin_skills()?;
        }

        let entries = std::fs::read_dir(&self.skills_dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let skill_file = path.join("SKILL.md");
                if skill_file.exists() {
                    match self.load_skill(&skill_file) {
                        Ok(skill) => {
                            info!("Loaded skill: {}", skill.name);
                            self.skills.insert(skill.name.clone(), skill);
                        }
                        Err(e) => {
                            warn!("Failed to load skill from {:?}: {}", skill_file, e);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn load_skill(&self, path: &Path) -> Result<SkillDefinition> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("Failed to read {:?}", path))?;

        let (frontmatter, instructions) = parse_frontmatter(&content)?;

        let source = if path.to_string_lossy().contains("\\registry\\")
            || path.to_string_lossy().contains("/registry/")
        {
            "registry".to_string()
        } else if path.to_string_lossy().contains("\\workspace\\")
            || path.to_string_lossy().contains("/workspace/")
        {
            "workspace".to_string()
        } else {
            // Distinguish builtin skills (created by create_builtin_skills) from
            // user-installed skills that also live in the top-level skills dir.
            const BUILTIN_IDS: &[&str] = &[
                "office-automation", "file-management", "web-automation",
                "system-admin", "desktop-control",
            ];
            let dir_name = path.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if BUILTIN_IDS.contains(&dir_name) {
                "builtin".to_string()
            } else {
                "installed".to_string()
            }
        };

        let tools = frontmatter
            .get("tools")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let unknown_tools: Vec<String> = tools
            .iter()
            .filter(|t| !Self::is_known_tool(t))
            .cloned()
            .collect();
        if !unknown_tools.is_empty() {
            warn!(
                "Skill {:?} declares unknown tools: {:?}",
                path, unknown_tools
            );
        }

        let parse_str_list = |key: &str| -> Vec<String> {
            frontmatter
                .get(key)
                .and_then(|v| v.as_sequence())
                .map(|seq| seq.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };

        Ok(SkillDefinition {
            name: frontmatter
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed")
                .to_string(),
            description: frontmatter
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            version: frontmatter
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("1.0")
                .to_string(),
            author: frontmatter
                .get("author")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            tools,
            dependencies: parse_str_list("dependencies"),
            permissions: parse_str_list("permissions"),
            triggers: parse_str_list("triggers"),
            platform: parse_str_list("platform"),
            source,
            instructions,
            source_path: path.parent().unwrap_or(path).to_path_buf(),
        })
    }

    fn create_builtin_skills(&self) -> Result<()> {
        let builtins: Vec<(&str, &str, &str, Vec<&str>, Vec<&str>, &str)> = vec![
            (
                "office-automation",
                "Office Automation",
                "Automate Microsoft Office tasks (Word, Excel, Outlook)",
                vec!["office"],
                vec![
                    "office", "word", "excel", "outlook", "pptx", "PPT", "spreadsheet", "document",
                    "Office自动化", "办公", "表格", "文档", "Word文档", "Excel表格", "幻灯片",
                    "演示文稿", "邮件", "报表", "数据表", "写报告", "制作PPT",
                ],
                "Use the `office` tool to create, edit, and manage Office documents.\n\n\
                 ## Capabilities\n\
                 - Create Word documents with formatted text\n\
                 - Create Excel spreadsheets with data and formulas\n\
                 - Send emails via Outlook\n\
                 - Read and modify existing documents",
            ),
            (
                "file-management",
                "File Management",
                "Organize, search, and manage files on the system",
                vec!["file_read", "file_write", "shell"],
                vec![
                    "file", "files", "folder", "directory", "rename", "move", "copy", "delete",
                    "文件", "文件夹", "目录", "整理", "搜索文件", "批量重命名", "移动文件",
                    "复制文件", "删除文件", "文件管理", "查找文件",
                ],
                "Use file tools to manage the user's files.\n\n\
                 ## Capabilities\n\
                 - Read and write text files\n\
                 - Search for files using shell commands\n\
                 - Organize files into directories\n\
                 - Batch rename and move files",
            ),
            (
                "web-automation",
                "Web Automation",
                "Automate web browsing tasks using Chrome",
                vec!["browser", "web_search"],
                vec![
                    "browser", "web", "chrome", "crawl", "scrape", "search", "navigate", "url",
                    "网页", "浏览器", "爬虫", "抓取", "网络", "搜索", "自动填表", "网页自动化",
                    "打开网页", "浏览网页", "数据抓取",
                ],
                "Use the browser tool to automate web tasks.\n\n\
                 ## Capabilities\n\
                 - Navigate to URLs\n\
                 - Fill forms and click buttons\n\
                 - Extract data from web pages\n\
                 - Take screenshots of web content",
            ),
            (
                "system-admin",
                "System Administration",
                "Manage Windows system settings and processes",
                vec!["powershell", "wmi_tool", "shell"],
                vec![
                    "system", "windows", "powershell", "process", "service", "registry", "admin",
                    "系统", "系统管理", "进程", "服务", "注册表", "系统设置", "系统信息",
                    "系统监控", "系统维护", "Windows管理", "系统优化",
                ],
                "Use PowerShell and WMI to manage the Windows system.\n\n\
                 ## Capabilities\n\
                 - Query system information\n\
                 - Manage services and processes\n\
                 - Configure system settings\n\
                 - Monitor system health",
            ),
            (
                "desktop-control",
                "Desktop Control",
                "Control Windows desktop applications via UI Automation",
                vec!["uia", "screen_capture"],
                vec![
                    "desktop", "uia", "automation", "click", "type", "screenshot", "window",
                    "桌面", "桌面控制", "UI自动化", "点击", "输入", "截图", "窗口", "自动化操作",
                    "界面操作", "桌面自动化", "应用控制",
                ],
                "Use UIA and screen capture to control desktop applications.\n\n\
                 ## Capabilities\n\
                 - Find and interact with UI elements\n\
                 - Click buttons, type text, send hotkeys\n\
                 - Take screenshots for visual verification\n\
                 - Automate multi-step desktop workflows",
            ),
        ];

        for (id, name, desc, tools, triggers, instructions) in builtins {
            let skill_dir = self.skills_dir.join(id);
            std::fs::create_dir_all(&skill_dir)?;
            let tools_yaml: Vec<String> = tools.iter().map(|t| format!("  - {}", t)).collect();
            let triggers_yaml: Vec<String> = triggers.iter().map(|t| format!("  - \"{}\"", t)).collect();
            let content = format!(
                "---\nname: {}\ndescription: {}\nversion: \"1.0\"\ntools:\n{}\ntriggers:\n{}\n---\n\n# {}\n\n{}\n",
                name,
                desc,
                tools_yaml.join("\n"),
                triggers_yaml.join("\n"),
                name,
                instructions
            );
            std::fs::write(skill_dir.join("SKILL.md"), content)?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_skill(&self, name: &str) -> Option<&SkillDefinition> {
        self.skills.get(name)
    }

    pub fn list_skills(&self) -> Vec<&SkillDefinition> {
        self.skills.values().collect()
    }

    /// Parse a SkillDefinition from raw SKILL.md content without writing to disk.
    /// Used by install_skill to validate before writing.
    pub fn parse_skill_from_content(&self, content: &str) -> Result<SkillDefinition> {
        let tmp = std::path::Path::new("skill.md");
        let (frontmatter, instructions) = parse_frontmatter(content)?;

        let tools = frontmatter
            .get("tools")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let parse_str_list = |key: &str| -> Vec<String> {
            frontmatter
                .get(key)
                .and_then(|v| v.as_sequence())
                .map(|seq| seq.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };

        Ok(SkillDefinition {
            name: frontmatter
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed")
                .to_string(),
            description: frontmatter
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            version: frontmatter
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("1.0")
                .to_string(),
            author: frontmatter
                .get("author")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            tools,
            dependencies: parse_str_list("dependencies"),
            permissions: parse_str_list("permissions"),
            triggers: parse_str_list("triggers"),
            platform: parse_str_list("platform"),
            source: "installed".to_string(),
            instructions,
            source_path: tmp.to_path_buf(),
        })
    }

    pub fn generate_skill_prompt(&self, enabled_skills: &[String]) -> String {
        let mut prompt = String::new();
        for name in enabled_skills {
            if let Some(skill) = self.skills.get(name) {
                prompt.push_str(&format!(
                    "\n## Skill: {}\nSource: {}\nPermissions: {}\n{}\n\n{}\n",
                    skill.name,
                    skill.source,
                    if skill.permissions.is_empty() { "none".to_string() } else { skill.permissions.join(", ") },
                    skill.description,
                    skill.instructions
                ));
            }
        }
        prompt
    }

    /// Generate a lightweight skill directory (name + one-line description only).
    /// Used in system prompt instead of full instructions to avoid context overflow.
    /// Each entry is ~10 tokens; 100 skills ≈ 1000 tokens total.
    pub fn generate_skill_directory(&self, enabled_skills: &[String]) -> String {
        let mut lines = Vec::new();
        for name in enabled_skills {
            if let Some(skill) = self.skills.get(name) {
                lines.push(format!("- **{}**: {}", skill.name, skill.description));
            }
        }
        lines.join("\n")
    }

    /// Search skills by keyword using in-memory substring matching.
    /// Supports mixed Chinese/English queries without any external tokenizer.
    ///
    /// Matching strategy:
    /// 1. Split query into tokens on whitespace and punctuation
    /// 2. For each skill, concatenate name + description + triggers
    /// 3. A skill matches if any query token (>= 2 chars) appears in the concatenated string
    /// 4. Results sorted by number of matching tokens descending, top 5 returned
    pub fn search_skills(&self, query: &str) -> Vec<&SkillDefinition> {
        // Split query into tokens: split on ASCII whitespace/punctuation, keep CJK chars together
        let tokens: Vec<String> = query
            .split(|c: char| c.is_ascii_punctuation() || c.is_ascii_whitespace())
            .filter(|t| t.chars().count() >= 2)
            .map(|t| t.to_lowercase())
            .collect();

        if tokens.is_empty() {
            return vec![];
        }

        let mut scored: Vec<(usize, &SkillDefinition)> = self
            .skills
            .values()
            .filter_map(|skill| {
                let haystack = format!(
                    "{} {} {}",
                    skill.name.to_lowercase(),
                    skill.description.to_lowercase(),
                    skill.triggers.join(" ").to_lowercase()
                );
                let hits = tokens.iter().filter(|t| haystack.contains(t.as_str())).count();
                if hits > 0 { Some((hits, skill)) } else { None }
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().take(5).map(|(_, s)| s).collect()
    }

    /// Get the full instructions for a skill by name.
    pub fn get_skill_instructions(&self, name: &str) -> Option<String> {
        self.skills.get(name).map(|s| {
            format!(
                "## Skill: {}\n{}\n\nPermissions: {}\n\n{}",
                s.name,
                s.description,
                if s.permissions.is_empty() { "none".to_string() } else { s.permissions.join(", ") },
                s.instructions
            )
        })
    }

    fn is_known_tool(name: &str) -> bool {
        matches!(
            name,
            "file_read"
                | "file_write"
                | "shell"
                | "web_search"
                | "powershell_query"
                | "wmi_tool"
                | "office"
                | "browser"
                | "uia"
                | "screen_capture"
                | "com"
                | "email"
        )
    }
}

fn parse_frontmatter(content: &str) -> Result<(serde_yaml::Value, String)> {
    let content = content.trim();
    if let Some(stripped) = content.strip_prefix("---") {
        if let Some(end) = stripped.find("---") {
            let yaml_str = &stripped[..end];
            let instructions = stripped[end + 3..].trim().to_string();
            let frontmatter: serde_yaml::Value =
                serde_yaml::from_str(yaml_str).with_context(|| "Failed to parse YAML frontmatter")?;
            return Ok((frontmatter, instructions));
        }
    }
    Ok((
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        content.to_string(),
    ))
}
