use crate::skills::loader::check_skill_compatibility;
use crate::store::{db::Skill, AppState};
use serde::Serialize;
use tauri::Manager;
use tauri::State;
use tracing::{info, warn};

// reqwest is available as a transitive dependency from other tools
use reqwest;

/// Perform a GET request with automatic retry on 429 (rate-limit) and 5xx errors.
///
/// Retry schedule: up to `max_retries` attempts with exponential back-off starting at
/// `base_delay_ms` ms (doubled each attempt, capped at 16 s).
/// Respects the `Retry-After` header when present.
async fn clawhub_get_with_retry(
    client: &reqwest::Client,
    url: &str,
    max_retries: u32,
) -> Result<reqwest::Response, String> {
    let base_delay_ms: u64 = 1000;
    let mut attempt = 0u32;

    loop {
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("网络请求失败：{}", e))?;

        let status = resp.status();

        // Success or a client error that won't be fixed by retrying (4xx except 429)
        if status.is_success() || (status.is_client_error() && status.as_u16() != 429) {
            return Ok(resp);
        }

        // 429 or 5xx — potentially retryable
        if attempt >= max_retries {
            return Ok(resp); // return the last response; caller handles the error status
        }

        // Honour Retry-After header if present (value in seconds)
        let retry_after_ms = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(|secs| secs * 1000)
            .unwrap_or(0);

        let backoff_ms = if retry_after_ms > 0 {
            retry_after_ms.min(30_000) // cap at 30 s
        } else {
            let exp = base_delay_ms * (1u64 << attempt.min(4)); // 1s, 2s, 4s, 8s, 16s
            exp.min(16_000)
        };

        warn!(
            "ClawHub {} for '{}', retrying in {}ms (attempt {}/{})",
            status, url, backoff_ms, attempt + 1, max_retries
        );
        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
        attempt += 1;
    }
}

#[derive(Debug, Serialize)]
pub struct SkillList {
    pub skills: Vec<Skill>,
    pub total: usize,
}

#[tauri::command]
pub async fn list_skills(state: State<'_, AppState>) -> Result<SkillList, String> {
    let db = state.db.lock().await;
    let skills = db.list_skills().map_err(|e| e.to_string())?;
    let total = skills.len();
    Ok(SkillList { skills, total })
}

#[tauri::command]
pub async fn toggle_skill(
    state: State<'_, AppState>,
    skill_id: String,
    enabled: bool,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.set_skill_enabled(&skill_id, enabled)
        .map_err(|e| e.to_string())
}

#[derive(Debug, Serialize)]
pub struct SkillCatalogItem {
    pub name: String,
    pub description: String,
    pub version: String,
    pub source: String,
    pub tools: Vec<String>,
    pub dependencies: Vec<String>,
    pub permissions: Vec<String>,
    pub platform: Vec<String>,
}

#[tauri::command]
pub async fn scan_skill_catalog(state: State<'_, AppState>) -> Result<Vec<SkillCatalogItem>, String> {
    let app_dir = state
        .app_handle
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
    let skills_dir = app_dir.join("skills");
    let mut loader = crate::skills::loader::SkillLoader::new(&skills_dir);
    loader.load_all().map_err(|e| e.to_string())?;

    let fs_skills = loader.list_skills();

    // Compute safe_name for each FS skill (same sanitisation as install_skill)
    let fs_safe_names: std::collections::HashSet<String> = fs_skills
        .iter()
        .map(|s| {
            s.name
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .collect::<String>()
                .to_lowercase()
        })
        .collect();

    // Sync FS → DB: upsert every skill found on disk so it appears in list_skills
    // Only sync non-builtin skills (builtin ones are seeded separately via seed_skills)
    {
        let db = state.db.lock().await;
        for skill in &fs_skills {
            if skill.source == "builtin" {
                continue;
            }
            let safe_name: String = skill
                .name
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .collect::<String>()
                .to_lowercase();
            if let Err(e) = db.upsert_skill(&safe_name, &skill.name, &skill.description, "📦") {
                warn!("scan_skill_catalog: failed to upsert '{}' in DB: {}", skill.name, e);
            }
        }

        // Sync DB → FS: remove DB entries whose files no longer exist on disk
        // (only for non-seeded skills — don't remove the built-in seed entries)
        if let Ok(db_skills) = db.list_skills() {
            let seeded_ids = [
                "web-search", "shell", "file-ops", "uia",
                "screen-vision", "scheduled-tasks", "docx", "xlsx",
            ];
            for db_skill in db_skills {
                if seeded_ids.contains(&db_skill.id.as_str()) {
                    continue;
                }
                if !fs_safe_names.contains(&db_skill.id) {
                    info!("scan_skill_catalog: removing orphan DB entry '{}'", db_skill.id);
                    let _ = db.delete_skill(&db_skill.id);
                }
            }
        }
    }

    let items = fs_skills
        .into_iter()
        .map(|s| SkillCatalogItem {
            name: s.name.clone(),
            description: s.description.clone(),
            version: s.version.clone(),
            source: s.source.clone(),
            tools: s.tools.clone(),
            dependencies: s.dependencies.clone(),
            permissions: s.permissions.clone(),
            platform: s.platform.clone(),
        })
        .collect::<Vec<_>>();
    Ok(items)
}

/// Install a skill from a URL (raw SKILL.md) or local file path.
/// The SKILL.md is downloaded, parsed, and written to the app skills directory.
#[tauri::command]
pub async fn install_skill(
    state: State<'_, AppState>,
    source: String,
) -> Result<SkillCatalogItem, String> {
    let app_dir = state
        .app_handle
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
    let skills_dir = app_dir.join("skills");

    let content = if source.starts_with("http://") || source.starts_with("https://") {
        // Basic URL validation — reject internal/private addresses
        let blocked = ["localhost", "127.0.0.1", "0.0.0.0", "192.168.", "10.", "172."];
        for pat in blocked {
            if source.contains(pat) {
                return Err(format!("Blocked URL: '{}' points to a private/local address", source));
            }
        }
        info!("Downloading skill from URL: {}", source);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(&source)
            .header("User-Agent", "Pisci-Desktop/1.0")
            .send()
            .await
            .map_err(|e| format!("Download failed: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} when downloading: {}", resp.status(), source));
        }
        resp.text().await.map_err(|e| format!("Failed to read response: {}", e))?
    } else {
        // Local file path
        tokio::fs::read_to_string(&source)
            .await
            .map_err(|e| format!("Failed to read local file '{}': {}", source, e))?
    };

    // Parse and validate frontmatter
    let loader = crate::skills::loader::SkillLoader::new(&skills_dir);
    let skill = loader
        .parse_skill_from_content(&content)
        .map_err(|e| format!("Failed to parse SKILL.md: {}", e))?;

    if skill.name.is_empty() || skill.name == "unnamed" {
        return Err("SKILL.md must declare a 'name' field in frontmatter".into());
    }

    // ── Compatibility check ───────────────────────────────────────────────────
    let compat = check_skill_compatibility(&skill).await;
    if !compat.compatible {
        return Err(format!(
            "技能 '{}' 与当前系统不兼容：\n{}",
            skill.name,
            compat.issues.join("\n")
        ));
    }
    for w in &compat.warnings {
        warn!("Skill '{}' compatibility warning: {}", skill.name, w);
    }

    // Warn if the skill declares elevated permissions
    if !skill.permissions.is_empty() {
        warn!(
            "Installing skill '{}' with permissions: {:?}",
            skill.name, skill.permissions
        );
    }

    // Sanitise name for use as directory name
    let safe_name: String = skill
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>()
        .to_lowercase();

    // Register in DB first — if this fails we abort before touching the filesystem
    {
        let db = state.db.lock().await;
        db.upsert_skill(&safe_name, &skill.name, &skill.description, "📦")
            .map_err(|e| format!("Failed to register skill in database: {}", e))?;
    }

    let skill_dir = skills_dir.join(&safe_name);
    if let Err(e) = tokio::fs::create_dir_all(&skill_dir).await {
        // Roll back DB entry on filesystem failure
        let db = state.db.lock().await;
        let _ = db.delete_skill(&safe_name);
        return Err(format!("Failed to create skill directory: {}", e));
    }

    let skill_file = skill_dir.join("SKILL.md");
    if let Err(e) = tokio::fs::write(&skill_file, &content).await {
        // Roll back DB entry and directory on write failure
        let db = state.db.lock().await;
        let _ = db.delete_skill(&safe_name);
        let _ = tokio::fs::remove_dir_all(&skill_dir).await;
        return Err(format!("Failed to write SKILL.md: {}", e));
    }

    info!("Installed skill '{}' to {:?}", skill.name, skill_dir);

    // Spawn background task: enrich triggers with LLM (bilingual, non-blocking)
    {
        let settings = state.settings.lock().await;
        let provider = settings.provider.clone();
        let api_key = match settings.provider.as_str() {
            "openai" | "custom" => settings.openai_api_key.clone(),
            "deepseek"          => settings.deepseek_api_key.clone(),
            "qwen" | "tongyi"   => settings.qwen_api_key.clone(),
            "minimax"           => settings.minimax_api_key.clone(),
            "zhipu"             => settings.zhipu_api_key.clone(),
            "kimi" | "moonshot" => settings.kimi_api_key.clone(),
            _                   => settings.anthropic_api_key.clone(),
        };
        let base_url = settings.custom_base_url.clone();
        let model = settings.model.clone();
        drop(settings);

        if !api_key.is_empty() {
            let enrich_skill = skill.clone();
            let enrich_file = skill_file.clone();
            tokio::spawn(async move {
                let client = crate::llm::build_client(
                    &provider,
                    &api_key,
                    if base_url.is_empty() { None } else { Some(&base_url) },
                );
                if let Err(e) = enrich_triggers_with_llm(&*client, &model, &enrich_skill, &enrich_file).await {
                    warn!("Trigger enrichment failed (non-fatal): {}", e);
                }
            });
        }
    }

    Ok(SkillCatalogItem {
        name: skill.name,
        description: skill.description,
        version: skill.version,
        source: "installed".to_string(),
        tools: skill.tools,
        dependencies: skill.dependencies,
        permissions: skill.permissions,
        platform: skill.platform,
    })
}

/// Remove an installed skill by name. Only skills whose source is "installed" or "workspace"
/// can be removed this way; built-in skills are protected.
#[tauri::command]
pub async fn uninstall_skill(
    state: State<'_, AppState>,
    skill_name: String,
) -> Result<(), String> {
    let app_dir = state
        .app_handle
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".pisci"));
    let skills_dir = app_dir.join("skills");

    let safe_name: String = skill_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>()
        .to_lowercase();

    let skill_dir = skills_dir.join(&safe_name);

    // Safety: must be inside skills_dir (check before existence to avoid TOCTOU)
    if skill_dir.exists() {
        let canonical_dir = skill_dir.canonicalize().map_err(|e| e.to_string())?;
        let canonical_skills = skills_dir.canonicalize().map_err(|e| e.to_string())?;
        if !canonical_dir.starts_with(&canonical_skills) {
            return Err("Path traversal attempt blocked".into());
        }
    }

    // Remove from DB first — if the DB delete fails, abort before touching the filesystem
    {
        let db = state.db.lock().await;
        db.delete_skill(&safe_name)
            .map_err(|e| format!("Failed to remove skill from database: {}", e))?;
    }

    // Now remove the files; if this fails the DB entry is already gone, which is acceptable
    // (the skill won't appear in the list, and the orphan directory can be cleaned up manually)
    if skill_dir.exists() {
        tokio::fs::remove_dir_all(&skill_dir)
            .await
            .map_err(|e| format!("Skill removed from database but failed to delete files: {}", e))?;
    }

    info!("Uninstalled skill '{}'", skill_name);
    Ok(())
}

// ─── ClawHub Skill Registry ───────────────────────────────────────────────────

/// ClawHub public API base URL.
const CLAWHUB_API: &str = "https://clawhub.ai";

/// A skill entry from the ClawHub registry.
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct ClawHubSkill {
    /// Unique skill slug on ClawHub (e.g. "my-skill").
    pub slug: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub downloads: u64,
    pub stars: u64,
    pub tags: Vec<String>,
    /// URL to fetch SKILL.md via `/api/v1/skills/<slug>/file?path=SKILL.md`
    pub skill_url: Option<String>,
    /// URL to download the zip bundle via `/api/v1/download?slug=<slug>`
    pub zip_url: Option<String>,
    /// OS/platform requirements from ClawHub metadata (e.g. ["windows"], ["linux"])
    pub platform: Vec<String>,
    /// Dependency requirements extracted from SKILL.md frontmatter (if pre-fetched)
    pub dependencies: Vec<String>,
    /// Whether this skill is compatible with the current system (None = not yet checked)
    pub compatible: Option<bool>,
    /// Compatibility issues (populated when compatible = false)
    pub compat_issues: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ClawHubSearchResult {
    pub items: Vec<ClawHubSkill>,
    pub total: usize,
    pub query: String,
}

/// Search ClawHub for skills.
///
/// Uses vector search (`/api/v1/search?q=`) when a query is provided,
/// or the list endpoint (`/api/v1/skills?sort=stars`) when the query is empty.
#[tauri::command]
pub async fn clawhub_search(query: String, limit: Option<u32>) -> Result<ClawHubSearchResult, String> {
    let limit = limit.unwrap_or(20).min(50);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("Pisci-Desktop/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    let q = query.trim().to_string();

    // Choose endpoint: vector search when query is non-empty, list by stars otherwise
    let (url, use_search_endpoint) = if q.is_empty() {
        (
            format!("{}/api/v1/skills?sort=stars&limit={}", CLAWHUB_API, limit),
            false,
        )
    } else {
        (
            format!(
                "{}/api/v1/search?q={}&limit={}",
                CLAWHUB_API,
                urlencoding::encode(&q),
                limit
            ),
            true,
        )
    };
    info!("ClawHub search: {}", url);

    let resp = clawhub_get_with_retry(&client, &url, 3)
        .await
        .map_err(|e| format!("无法连接到 ClawHub（{}）：{}。请检查网络连接。", CLAWHUB_API, e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let hint = if status.as_u16() == 429 {
            "（请求过于频繁，请稍后再试）".to_string()
        } else {
            String::new()
        };
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "ClawHub 返回错误 HTTP {}{}：{}",
            status,
            hint,
            if body.len() > 300 { &body[..300] } else { &body }
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("ClawHub 响应格式异常：{}", e))?;

    // Parse items from either endpoint format:
    // - /api/v1/search  → { results: [{ slug, displayName, summary, version, score }] }
    // - /api/v1/skills  → { items:   [{ slug, displayName, summary, tags, stats, latestVersion, metadata }] }
    let items: Vec<ClawHubSkill> = if use_search_endpoint {
        let results = body["results"].as_array().cloned().unwrap_or_default();
        results.iter().filter_map(|r| {
            let slug = r["slug"].as_str().unwrap_or("").to_string();
            if slug.is_empty() { return None; }
            let name = r["displayName"].as_str().unwrap_or(&slug).to_string();
            let description = r["summary"].as_str().unwrap_or("").to_string();
            let version = r["version"].as_str().unwrap_or("latest").to_string();
            let skill_url = Some(format!("{}/api/v1/skills/{}/file?path=SKILL.md", CLAWHUB_API, slug));
            let zip_url = Some(format!("{}/api/v1/download?slug={}", CLAWHUB_API, slug));
            Some(ClawHubSkill {
                slug, name, description, version,
                author: String::new(),
                downloads: 0, stars: 0,
                tags: vec![],
                skill_url, zip_url,
                platform: vec![], dependencies: vec![],
                compatible: None, compat_issues: vec![],
            })
        }).collect()
    } else {
        let raw_items = body["items"].as_array().cloned().unwrap_or_default();
        raw_items.iter().filter_map(|item| {
            let slug = item["slug"].as_str().unwrap_or("").to_string();
            if slug.is_empty() { return None; }
            let name = item["displayName"].as_str().unwrap_or(&slug).to_string();
            let description = item["summary"].as_str().unwrap_or("").to_string();
            let version = item["latestVersion"]["version"].as_str().unwrap_or("latest").to_string();

            // tags is an object { tag_name: versionId } in the list endpoint
            let tags: Vec<String> = item["tags"]
                .as_object()
                .map(|obj| obj.keys().cloned().collect())
                .unwrap_or_default();

            let stats = &item["stats"];
            let downloads = stats["installsAllTime"].as_u64()
                .or_else(|| stats["downloads"].as_u64())
                .unwrap_or(0);
            let stars = stats["stars"].as_u64().unwrap_or(0);

            // OS platform from metadata (clawdis.os field)
            let platform: Vec<String> = item["metadata"]["os"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();

            let skill_url = Some(format!("{}/api/v1/skills/{}/file?path=SKILL.md", CLAWHUB_API, slug));
            let zip_url = Some(format!("{}/api/v1/download?slug={}", CLAWHUB_API, slug));

            Some(ClawHubSkill {
                slug, name, description, version,
                author: String::new(),
                downloads, stars, tags,
                skill_url, zip_url,
                platform, dependencies: vec![],
                compatible: None, compat_issues: vec![],
            })
        }).collect()
    };

    let total = items.len();
    Ok(ClawHubSearchResult { items, total, query })
}

/// Pre-check whether a skill (from URL or local path) is compatible with the current system.
/// Returns compatibility info without actually installing the skill.
#[tauri::command]
pub async fn check_skill_compat(source: String) -> Result<crate::skills::loader::CompatibilityCheck, String> {
    let content = if source.starts_with("http://") || source.starts_with("https://") {
        let blocked = ["localhost", "127.0.0.1", "0.0.0.0", "192.168.", "10.", "172."];
        for pat in blocked {
            if source.contains(pat) {
                return Err(format!("Blocked URL: '{}'", source));
            }
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.get(&source).header("User-Agent", "Pisci-Desktop/1.0")
            .send().await.map_err(|e| format!("Download failed: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {} when fetching: {}", resp.status(), source));
        }
        resp.text().await.map_err(|e| e.to_string())?
    } else {
        tokio::fs::read_to_string(&source).await
            .map_err(|e| format!("Failed to read '{}': {}", source, e))?
    };

    let loader = crate::skills::loader::SkillLoader::new(std::path::Path::new("."));
    let skill = loader.parse_skill_from_content(&content)
        .map_err(|e| format!("Failed to parse SKILL.md: {}", e))?;

    Ok(check_skill_compatibility(&skill).await)
}

/// Install a skill from ClawHub by slug.
/// Fetches SKILL.md via `/api/v1/skills/<slug>/file?path=SKILL.md`,
/// falls back to the zip download if the file endpoint fails.
#[tauri::command]
pub async fn clawhub_install(
    state: State<'_, AppState>,
    slug: String,
    version: Option<String>,
) -> Result<SkillCatalogItem, String> {
    // Validate slug — only allow alphanumeric, hyphens, underscores, dots
    if !slug.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err(format!("无效的技能 slug：'{}'", slug));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Pisci-Desktop/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    // Build the file URL, optionally pinning a version
    let file_url = if let Some(ref ver) = version {
        format!("{}/api/v1/skills/{}/file?path=SKILL.md&version={}", CLAWHUB_API, slug, ver)
    } else {
        format!("{}/api/v1/skills/{}/file?path=SKILL.md", CLAWHUB_API, slug)
    };
    info!("ClawHub install: fetching SKILL.md for '{}' from {}", slug, file_url);

    let resp = clawhub_get_with_retry(&client, &file_url, 3)
        .await
        .map_err(|e| format!("下载失败：{}", e))?;

    let content = if resp.status().is_success() {
        resp.text().await.map_err(|e| format!("读取 SKILL.md 失败：{}", e))?
    } else {
        let file_status = resp.status();
        // Fallback: download the zip bundle and extract SKILL.md
        let zip_url = if let Some(ref ver) = version {
            format!("{}/api/v1/download?slug={}&version={}", CLAWHUB_API, slug, ver)
        } else {
            format!("{}/api/v1/download?slug={}", CLAWHUB_API, slug)
        };
        info!("ClawHub: file endpoint returned {}, trying zip: {}", file_status, zip_url);
        let zip_resp = clawhub_get_with_retry(&client, &zip_url, 3)
            .await
            .map_err(|e| format!("Zip 下载失败：{}", e))?;
        if !zip_resp.status().is_success() {
            let hint = if zip_resp.status().as_u16() == 429 {
                "请求过于频繁，请稍后再试".to_string()
            } else {
                format!("HTTP {}", zip_resp.status())
            };
            return Err(format!("ClawHub：技能 '{}' 安装失败（{}）", slug, hint));
        }
        let zip_bytes = zip_resp.bytes().await.map_err(|e| e.to_string())?;
        extract_skill_md_from_zip(&zip_bytes)
            .map_err(|e| format!("从 zip 中提取 SKILL.md 失败：{}", e))?
    };

    // Delegate to existing install_skill logic (includes compat check)
    install_skill(state, content).await
}

/// Extract SKILL.md text from a zip archive bytes.
fn extract_skill_md_from_zip(zip_bytes: &[u8]) -> anyhow::Result<String> {
    use std::io::Read;
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_lowercase();
        if name == "skill.md" || name.ends_with("/skill.md") {
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            return Ok(content);
        }
    }
    anyhow::bail!("SKILL.md not found in zip archive")
}

/// Call the LLM to generate bilingual (Chinese + English) trigger keywords for a skill,
/// then merge them into the SKILL.md frontmatter and write the file back.
///
/// This runs as a background task after installation — failures are non-fatal.
async fn enrich_triggers_with_llm(
    client: &dyn crate::llm::LlmClient,
    model: &str,
    skill: &crate::skills::loader::SkillDefinition,
    skill_file: &std::path::Path,
) -> anyhow::Result<()> {
    use crate::llm::{LlmMessage, LlmRequest, MessageContent};
    use tokio::time::{timeout, Duration};

    let existing_triggers = if skill.triggers.is_empty() {
        String::new()
    } else {
        format!("\nExisting triggers: {}", skill.triggers.join(", "))
    };

    let prompt = format!(
        "You are a multilingual keyword expert. Given this skill:\n\
         Name: {name}\n\
         Description: {desc}{existing}\n\n\
         Generate 10-20 trigger keywords in both Chinese and English that a user might say \
         when they need this skill. Include synonyms, abbreviations, and common phrases. \
         Return ONLY a JSON array of strings, no explanation, no markdown fences.\n\
         Example: [\"pptx\",\"PPT\",\"幻灯片\",\"演示文稿\",\"presentation\",\"slideshow\"]",
        name = skill.name,
        desc = skill.description,
        existing = existing_triggers,
    );

    let req = LlmRequest {
        messages: vec![LlmMessage {
            role: "user".into(),
            content: MessageContent::Text(prompt),
        }],
        system: None,
        tools: vec![],
        model: model.to_string(),
        max_tokens: 512,
        stream: false,
        vision_override: None,
    };

    let response = timeout(Duration::from_secs(20), client.complete(req))
        .await
        .map_err(|_| anyhow::anyhow!("LLM trigger enrichment timed out"))?
        .map_err(|e| anyhow::anyhow!("LLM error: {}", e))?;

    let text = response.content;

    // Extract JSON array from response (may be wrapped in prose)
    let json_start = text.find('[').ok_or_else(|| anyhow::anyhow!("No JSON array in response"))?;
    let json_end = text.rfind(']').ok_or_else(|| anyhow::anyhow!("No closing ] in response"))?;
    let json_str = &text[json_start..=json_end];

    let new_triggers: Vec<String> = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse trigger JSON: {}", e))?;

    if new_triggers.is_empty() {
        return Ok(());
    }

    // Merge with existing triggers, deduplicating case-insensitively
    let mut merged: Vec<String> = skill.triggers.clone();
    let existing_lower: std::collections::HashSet<String> =
        merged.iter().map(|t| t.to_lowercase()).collect();
    for t in new_triggers {
        let t = t.trim().to_string();
        if !t.is_empty() && !existing_lower.contains(&t.to_lowercase()) {
            merged.push(t);
        }
    }

    // Read current SKILL.md content
    let current = tokio::fs::read_to_string(skill_file).await
        .map_err(|e| anyhow::anyhow!("Failed to read SKILL.md: {}", e))?;

    // Build the new triggers YAML block
    let triggers_yaml = merged.iter()
        .map(|t| format!("  - \"{}\"", t.replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join("\n");
    let triggers_block = format!("triggers:\n{}", triggers_yaml);

    // Replace or insert triggers block in frontmatter
    let updated = if current.contains("triggers:") {
        // Replace existing triggers block (handles multi-line list)
        let re = regex::Regex::new(r"(?m)^triggers:(\n  - [^\n]*)*")
            .map_err(|e| anyhow::anyhow!("Regex error: {}", e))?;
        re.replace(&current, triggers_block.as_str()).into_owned()
    } else {
        // Insert before closing --- of frontmatter
        if let Some(pos) = current.find("\n---\n") {
            let (front, rest) = current.split_at(pos);
            format!("{}\n{}{}", front, triggers_block, rest)
        } else {
            // No frontmatter end found, append to end of frontmatter section
            current.replacen("---", &format!("---\n{}", triggers_block), 2)
        }
    };

    tokio::fs::write(skill_file, updated).await
        .map_err(|e| anyhow::anyhow!("Failed to write enriched SKILL.md: {}", e))?;

    info!("Enriched triggers for skill '{}': {} keywords", skill.name, merged.len());
    Ok(())
}
