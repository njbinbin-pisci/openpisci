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
    #[serde(default)]
    pub source: String,
    pub instructions: String,
    pub source_path: PathBuf,
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
            "builtin".to_string()
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
            dependencies: frontmatter
                .get("dependencies")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            permissions: frontmatter
                .get("permissions")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            triggers: frontmatter
                .get("triggers")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            source,
            instructions,
            source_path: path.parent().unwrap_or(path).to_path_buf(),
        })
    }

    fn create_builtin_skills(&self) -> Result<()> {
        let builtins = vec![
            (
                "office-automation",
                "Office Automation",
                "Automate Microsoft Office tasks (Word, Excel, Outlook)",
                vec!["office"],
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
                "Use UIA and screen capture to control desktop applications.\n\n\
                 ## Capabilities\n\
                 - Find and interact with UI elements\n\
                 - Click buttons, type text, send hotkeys\n\
                 - Take screenshots for visual verification\n\
                 - Automate multi-step desktop workflows",
            ),
        ];

        for (id, name, desc, tools, instructions) in builtins {
            let skill_dir = self.skills_dir.join(id);
            std::fs::create_dir_all(&skill_dir)?;
            let tools_yaml: Vec<String> = tools.iter().map(|t| format!("  - {}", t)).collect();
            let content = format!(
                "---\nname: {}\ndescription: {}\nversion: \"1.0\"\ntools:\n{}\n---\n\n# {}\n\n{}\n",
                name,
                desc,
                tools_yaml.join("\n"),
                name,
                instructions
            );
            std::fs::write(skill_dir.join("SKILL.md"), content)?;
        }
        Ok(())
    }

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
            dependencies: frontmatter
                .get("dependencies")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            permissions: frontmatter
                .get("permissions")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            triggers: frontmatter
                .get("triggers")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
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
    if content.starts_with("---") {
        if let Some(end) = content[3..].find("---") {
            let yaml_str = &content[3..end + 3];
            let instructions = content[end + 6..].trim().to_string();
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
