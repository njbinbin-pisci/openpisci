//! Re-exports of coordination-metadata helpers from `pisci-core`.
//!
//! Callers inside the kernel should depend on
//! `pisci_kernel::pool::metadata::*` rather than reaching through
//! `pisci_core::project_state` directly. This indirection lets us
//! evolve the metadata layer (e.g. adding kernel-specific helpers that
//! need the DB) without rewriting call sites again.

pub use pisci_core::project_state::{
    assess_project_state, build_coordination_event_digest, contains_pisci_mention,
    coordination_event_type_for_content, detect_coordination_signal, enrich_pool_message_metadata,
    extract_project_status_signal, CoordinationEventDigest, CoordinationSignalKind,
    ProjectAssessment, ProjectDecision, STATUS_FOLLOW_UP, STATUS_READY, STATUS_WAITING,
};

use serde_json::Value;

/// Helper that encodes the metadata produced by [`enrich_pool_message_metadata`]
/// into the JSON string shape the DB expects. Services use this to go
/// directly from `(base: Value, content: &str)` to the string argument of
/// `Database::insert_pool_message_ext`.
pub fn enrich_as_json_string(base: Value, content: &str) -> String {
    enrich_pool_message_metadata(base, content).to_string()
}
