pub mod vector;

use crate::store::db::{Database, Memory};
use anyhow::Result;

/// Hybrid search: combines FTS5 keyword results with vector similarity results.
/// If `query_embedding` is None, only FTS5 is used.
/// Returns up to `top_k` Memory entries sorted by hybrid score (descending).
#[allow(dead_code)]
pub fn search_hybrid(
    db: &Database,
    query: &str,
    query_embedding: Option<&[f32]>,
    top_k: usize,
) -> Result<Vec<Memory>> {
    // FTS keyword results
    let keyword_results = db.fts_search(query, top_k * 2).unwrap_or_default();

    // Vector results (only if embedding provided)
    let vector_results = if let Some(emb) = query_embedding {
        db.search_by_embedding(emb, top_k * 2)?
            .into_iter()
            .map(|(m, score)| (m.id, score))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    // Hybrid merge
    let merged = vector::hybrid_merge(&vector_results, &keyword_results, 0.6, 0.4, top_k);

    // Fetch full Memory objects by id
    let all_memories = db.list_memories()?;
    let memory_map: std::collections::HashMap<String, Memory> = all_memories
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();

    let results = merged
        .into_iter()
        .filter_map(|r| memory_map.get(&r.id).cloned())
        .collect();
    Ok(results)
}
