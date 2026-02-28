use std::collections::HashMap;

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for &val in embedding {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

pub fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[derive(Debug, Clone)]
pub struct ScoredResult {
    pub id: String,
    pub score: f32,
}

pub fn hybrid_merge(
    vector_results: &[(String, f32)],
    keyword_results: &[(String, f32)],
    vector_weight: f32,
    keyword_weight: f32,
    limit: usize,
) -> Vec<ScoredResult> {
    let mut scores: HashMap<String, f32> = HashMap::new();

    let v_max = vector_results.iter().map(|(_, s)| *s).fold(0.0f32, f32::max).max(1e-6);
    for (id, score) in vector_results {
        let normalized = score / v_max;
        *scores.entry(id.clone()).or_insert(0.0) += normalized * vector_weight;
    }

    let k_max = keyword_results.iter().map(|(_, s)| s.abs()).fold(0.0f32, f32::max).max(1e-6);
    for (id, score) in keyword_results {
        let normalized = score.abs() / k_max;
        *scores.entry(id.clone()).or_insert(0.0) += normalized * keyword_weight;
    }

    let mut results: Vec<ScoredResult> = scores
        .into_iter()
        .map(|(id, score)| ScoredResult { id, score })
        .collect();
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    results
}

pub fn content_hash(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── cosine_similarity ──────────────────────────────────────────────────
    #[test]
    fn identical_vectors_have_similarity_one() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5, "expected ~1.0, got {sim}");
    }

    #[test]
    fn orthogonal_vectors_have_similarity_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-5, "expected ~0.0, got {sim}");
    }

    #[test]
    fn opposite_vectors_have_similarity_minus_one() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-5, "expected ~-1.0, got {sim}");
    }

    #[test]
    fn zero_length_returns_zero() {
        let sim = cosine_similarity(&[], &[]);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn mismatched_length_returns_zero() {
        let sim = cosine_similarity(&[1.0, 2.0], &[1.0]);
        assert_eq!(sim, 0.0);
    }

    // ── embedding roundtrip ────────────────────────────────────────────────
    #[test]
    fn embedding_bytes_roundtrip() {
        let original = vec![0.1f32, 0.5, -0.3, 1.0];
        let bytes = embedding_to_bytes(&original);
        let restored = bytes_to_embedding(&bytes);
        for (a, b) in original.iter().zip(restored.iter()) {
            assert!((a - b).abs() < 1e-7, "mismatch: {a} vs {b}");
        }
    }

    // ── hybrid_merge ───────────────────────────────────────────────────────
    #[test]
    fn hybrid_merge_combines_scores() {
        let vec_results = vec![("id1".to_string(), 0.9), ("id2".to_string(), 0.4)];
        let kw_results  = vec![("id2".to_string(), 1.0), ("id3".to_string(), 0.6)];
        let results = hybrid_merge(&vec_results, &kw_results, 0.7, 0.3, 10);
        assert!(results.len() <= 3);
        // id2 appears in both — should rank highly
        let id2 = results.iter().find(|r| r.id == "id2").expect("id2 missing");
        assert!(id2.score > 0.0);
    }

    #[test]
    fn hybrid_merge_respects_limit() {
        let vecs: Vec<(String, f32)> = (0..20).map(|i| (format!("id{i}"), i as f32)).collect();
        let results = hybrid_merge(&vecs, &[], 1.0, 0.0, 5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn hybrid_merge_orders_by_descending_score() {
        let vec_results = vec![
            ("low".to_string(), 0.1),
            ("high".to_string(), 1.0),
            ("mid".to_string(), 0.5),
        ];
        let results = hybrid_merge(&vec_results, &[], 1.0, 0.0, 10);
        assert_eq!(results[0].id, "high");
    }

    // ── content_hash ───────────────────────────────────────────────────────
    #[test]
    fn same_content_gives_same_hash() {
        assert_eq!(content_hash("hello world"), content_hash("hello world"));
    }

    #[test]
    fn different_content_gives_different_hash() {
        assert_ne!(content_hash("foo"), content_hash("bar"));
    }

    #[test]
    fn hash_is_16_hex_chars() {
        let h = content_hash("test");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
