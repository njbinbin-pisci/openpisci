use crate::agent::tool::ImageData;
use crate::llm::{ContentBlock, LlmMessage, MessageContent};
use anyhow::{anyhow, Result};
use base64::Engine;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_ARTIFACTS_PER_SESSION: usize = 16;
const MAX_SELECTED_PER_SESSION: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionArtifactSummary {
    pub id: String,
    pub label: String,
    pub source_tool: String,
    pub media_type: String,
    pub selected: bool,
}

#[derive(Debug, Clone)]
struct VisionArtifact {
    id: String,
    label: String,
    source_tool: String,
    image: ImageData,
}

#[derive(Debug, Default)]
struct SessionVisionState {
    artifacts: Vec<VisionArtifact>,
    selected_ids: Vec<String>,
}

#[derive(Debug, Default)]
struct VisionStore {
    sessions: HashMap<String, SessionVisionState>,
}

static VISION_STORE: Lazy<Mutex<VisionStore>> = Lazy::new(|| Mutex::new(VisionStore::default()));

fn image_media_type_from_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn prune_session(session: &mut SessionVisionState) {
    if session.artifacts.len() <= MAX_ARTIFACTS_PER_SESSION {
        return;
    }
    let remove_count = session
        .artifacts
        .len()
        .saturating_sub(MAX_ARTIFACTS_PER_SESSION);
    let removed_ids: HashSet<String> = session
        .artifacts
        .iter()
        .take(remove_count)
        .map(|a| a.id.clone())
        .collect();
    session.artifacts.drain(0..remove_count);
    session.selected_ids.retain(|id| !removed_ids.contains(id));
}

fn to_summaries(session: &SessionVisionState) -> Vec<VisionArtifactSummary> {
    let selected: HashSet<&str> = session.selected_ids.iter().map(String::as_str).collect();
    session
        .artifacts
        .iter()
        .map(|artifact| VisionArtifactSummary {
            id: artifact.id.clone(),
            label: artifact.label.clone(),
            source_tool: artifact.source_tool.clone(),
            media_type: artifact.image.media_type.clone(),
            selected: selected.contains(artifact.id.as_str()),
        })
        .collect()
}

pub async fn store_tool_image(
    session_id: &str,
    source_tool: &str,
    label: Option<String>,
    image: &ImageData,
) -> VisionArtifactSummary {
    let mut store = VISION_STORE.lock().await;
    let session = store.sessions.entry(session_id.to_string()).or_default();
    let artifact = VisionArtifact {
        id: format!("img_{}", &Uuid::new_v4().simple().to_string()[..8]),
        label: label.unwrap_or_else(|| format!("{} capture", source_tool)),
        source_tool: source_tool.to_string(),
        image: image.clone(),
    };
    let summary = VisionArtifactSummary {
        id: artifact.id.clone(),
        label: artifact.label.clone(),
        source_tool: artifact.source_tool.clone(),
        media_type: artifact.image.media_type.clone(),
        selected: false,
    };
    session.artifacts.push(artifact);
    prune_session(session);
    summary
}

pub async fn store_image_path(
    session_id: &str,
    source_tool: &str,
    path: &Path,
    label: Option<String>,
) -> Result<VisionArtifactSummary> {
    let media_type = image_media_type_from_path(path)
        .ok_or_else(|| anyhow!("Unsupported image file type: {}", path.display()))?;
    let bytes = std::fs::read(path)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let image = ImageData {
        base64: b64,
        media_type: media_type.to_string(),
    };
    Ok(store_tool_image(session_id, source_tool, label, &image).await)
}

pub async fn list_artifacts(session_id: &str) -> Vec<VisionArtifactSummary> {
    let store = VISION_STORE.lock().await;
    store
        .sessions
        .get(session_id)
        .map(to_summaries)
        .unwrap_or_default()
}

pub async fn select_artifacts(
    session_id: &str,
    artifact_ids: &[String],
    merge: bool,
) -> Result<Vec<VisionArtifactSummary>> {
    let mut store = VISION_STORE.lock().await;
    let session = store.sessions.entry(session_id.to_string()).or_default();
    let available: HashSet<&str> = session.artifacts.iter().map(|a| a.id.as_str()).collect();
    for id in artifact_ids {
        if !available.contains(id.as_str()) {
            return Err(anyhow!("Vision artifact '{}' not found", id));
        }
    }

    let mut next = if merge {
        session.selected_ids.clone()
    } else {
        Vec::new()
    };
    for id in artifact_ids {
        if !next.contains(id) {
            next.push(id.clone());
        }
    }
    if next.len() > MAX_SELECTED_PER_SESSION {
        return Err(anyhow!(
            "At most {} vision artifacts can be selected at once",
            MAX_SELECTED_PER_SESSION
        ));
    }
    session.selected_ids = next;
    Ok(to_summaries(session)
        .into_iter()
        .filter(|item| item.selected)
        .collect())
}

pub async fn clear_selection(session_id: &str) {
    let mut store = VISION_STORE.lock().await;
    if let Some(session) = store.sessions.get_mut(session_id) {
        session.selected_ids.clear();
    }
}

pub async fn clear_session(session_id: &str) {
    let mut store = VISION_STORE.lock().await;
    store.sessions.remove(session_id);
}

pub async fn inject_selected_context(messages: &[LlmMessage], session_id: &str) -> Vec<LlmMessage> {
    let store = VISION_STORE.lock().await;
    let Some(session) = store.sessions.get(session_id) else {
        return messages.to_vec();
    };
    if session.selected_ids.is_empty() {
        return messages.to_vec();
    }

    let selected_lookup: HashSet<&str> = session.selected_ids.iter().map(String::as_str).collect();
    let selected: Vec<&VisionArtifact> = session
        .artifacts
        .iter()
        .filter(|artifact| selected_lookup.contains(artifact.id.as_str()))
        .collect();
    if selected.is_empty() {
        return messages.to_vec();
    }

    let mut req_messages = messages.to_vec();
    let mut blocks = Vec::new();
    let text = format!(
        "Agent-selected visual context for the next step. Analyze only these selected images unless you decide to change the selection via vision_context.\n{}",
        selected
            .iter()
            .map(|a| format!("- {} ({}, id={})", a.label, a.source_tool, a.id))
            .collect::<Vec<_>>()
            .join("\n")
    );
    blocks.push(ContentBlock::Text { text });
    for artifact in selected {
        blocks.push(ContentBlock::Image {
            source: crate::llm::ImageSource {
                source_type: "base64".to_string(),
                media_type: artifact.image.media_type.clone(),
                data: artifact.image.base64.clone(),
            },
        });
    }
    req_messages.push(LlmMessage {
        role: "user".into(),
        content: MessageContent::Blocks(blocks),
    });
    req_messages
}

#[cfg(test)]
mod tests {
    use super::{clear_session, list_artifacts, select_artifacts, store_tool_image};
    use crate::agent::tool::ImageData;

    #[tokio::test]
    async fn stores_and_selects_artifacts() {
        clear_session("test_vision").await;
        let saved = store_tool_image(
            "test_vision",
            "screen_capture",
            Some("page 1".into()),
            &ImageData::png("abc"),
        )
        .await;
        let selected = select_artifacts("test_vision", &[saved.id.clone()], false)
            .await
            .unwrap();
        assert_eq!(selected.len(), 1);
        let listed = list_artifacts("test_vision").await;
        assert_eq!(listed.len(), 1);
        assert!(listed[0].selected);
    }
}
