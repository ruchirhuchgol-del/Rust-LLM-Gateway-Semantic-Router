//! Semantic cache architecture & vector similarity engine.
//!
//! Provides approximate semantic caching across compatible prompts using
//! cosine similarity of embedding vectors.
//!
//! ## Safety & Partitioning (P2 #17)
//!
//! Semantic caching is NEVER applied blindly across different configurations.
//! Entries are partitioned by a `CompatibilitySignature` containing:
//!   - Model identifier
//!   - System prompt / instructions
//!   - Tool definitions & tool choice
//!   - Response format constraints
//!
//! Two prompts are only candidates for semantic match if their `CompatibilitySignature`
//! matches identically. This prevents serving responses generated under different
//! system instructions or tool configurations.
//!
//! ## Vector similarity
//!
//! Uses dot-product / cosine similarity over normalized embedding vectors:
//! ```text
//! similarity = dot(u, v) / (norm(u) * norm(v))
//! ```
//! Hits require `similarity >= threshold`, where threshold is configurable.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Configuration for the semantic cache.
#[derive(Clone, Debug)]
pub struct SemanticCacheConfig {
    pub enabled: bool,
    /// Minimum cosine similarity required to consider a cache entry a hit (e.g. 0.92 - 0.98).
    pub threshold: f64,
    /// Maximum number of embeddings stored before oldest entries are evicted.
    pub max_entries: usize,
    /// Time-to-live for cached semantic entries.
    pub ttl: Duration,
}

impl Default for SemanticCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 0.95,
            max_entries: 10_000,
            ttl: Duration::from_secs(3600),
        }
    }
}

/// A partition key ensuring responses are never shared across incompatible prompts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CompatibilitySignature {
    pub model: String,
    pub system_fingerprint: Option<String>,
    pub tools_hash: Option<String>,
    pub response_format: Option<String>,
}

/// An entry in the vector cache.
#[derive(Clone, Debug)]
pub struct SemanticEntry {
    pub prompt: String,
    pub signature: CompatibilitySignature,
    pub embedding: Vec<f32>,
    pub response: String,
    pub created_at: Instant,
}

/// Result of a semantic-cache lookup.
#[derive(Debug, PartialEq)]
pub enum SemanticLookupOutcome {
    Hit { response: String, similarity: f64 },
    Miss,
    Disabled,
}

/// Trait for embedding generation backends (e.g. local ONNX/Candle, fastembed, or mock).
#[axum::async_trait]
pub trait EmbeddingBackend: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, String>;
}

/// No-op backend used when semantic caching is disabled or unconfigured.
pub struct NoopEmbeddingBackend;

#[axum::async_trait]
impl EmbeddingBackend for NoopEmbeddingBackend {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, String> {
        Err("no embedding backend configured".into())
    }
}

/// In-memory semantic vector cache with bounded capacity and TTL.
pub struct SemanticCache {
    config: SemanticCacheConfig,
    backend: Arc<dyn EmbeddingBackend>,
    entries: RwLock<Vec<SemanticEntry>>,
}

impl SemanticCache {
    pub fn new(config: SemanticCacheConfig, backend: Arc<dyn EmbeddingBackend>) -> Self {
        Self {
            config,
            backend,
            entries: RwLock::new(Vec::new()),
        }
    }

    /// Look up a prompt in the semantic cache.
    pub async fn lookup(
        &self,
        prompt: &str,
        signature: &CompatibilitySignature,
    ) -> SemanticLookupOutcome {
        if !self.config.enabled {
            return SemanticLookupOutcome::Disabled;
        }

        let query_embedding = match self.backend.embed(prompt).await {
            Ok(emb) => emb,
            Err(_) => return SemanticLookupOutcome::Miss,
        };

        let now = Instant::now();
        let entries = self.entries.read().await;

        let mut best_match: Option<(&SemanticEntry, f64)> = None;

        for entry in entries.iter() {
            // Must have matching compatibility signature (model, tools, format)
            if &entry.signature != signature {
                continue;
            }

            // Must not be expired
            if now.duration_since(entry.created_at) > self.config.ttl {
                continue;
            }

            let sim = cosine_similarity(&query_embedding, &entry.embedding);
            if sim >= self.config.threshold as f32 {
                if let Some((_, best_sim)) = best_match {
                    if (sim as f64) > best_sim {
                        best_match = Some((entry, sim as f64));
                    }
                } else {
                    best_match = Some((entry, sim as f64));
                }
            }
        }

        if let Some((entry, sim)) = best_match {
            SemanticLookupOutcome::Hit {
                response: entry.response.clone(),
                similarity: sim,
            }
        } else {
            SemanticLookupOutcome::Miss
        }
    }

    /// Store a prompt, embedding, and response in the semantic cache.
    pub async fn insert(
        &self,
        prompt: String,
        signature: CompatibilitySignature,
        response: String,
    ) {
        if !self.config.enabled {
            return;
        }

        let embedding = match self.backend.embed(&prompt).await {
            Ok(emb) => emb,
            Err(_) => return,
        };

        let mut entries = self.entries.write().await;

        // Bounded capacity eviction: remove oldest if at capacity
        if entries.len() >= self.config.max_entries {
            entries.remove(0);
        }

        entries.push(SemanticEntry {
            prompt,
            signature,
            embedding,
            response,
            created_at: Instant::now(),
        });
    }

    /// Explicitly clear all entries in the semantic cache.
    pub async fn clear(&self) {
        let mut entries = self.entries.write().await;
        entries.clear();
    }
}

/// Compute cosine similarity between two f32 slices: dot(a, b) / (|a| * |b|).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom > 0.0 {
        (dot / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockEmbeddingBackend;

    #[axum::async_trait]
    impl EmbeddingBackend for MockEmbeddingBackend {
        async fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
            // Generate simple deterministic normalized vector based on character sum
            let len = text.len() as f32;
            let vec = vec![len, (len * 2.0).sin(), (len * 3.0).cos()];
            let norm = (vec[0] * vec[0] + vec[1] * vec[1] + vec[2] * vec[2]).sqrt();
            Ok(vec.into_iter().map(|v| v / norm).collect())
        }
    }

    #[test]
    fn cosine_similarity_identical_vectors() {
        let v1 = vec![1.0, 0.0, 0.0];
        let v2 = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&v1, &v2) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let v1 = vec![1.0, 0.0, 0.0];
        let v2 = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&v1, &v2)).abs() < 1e-6);
    }

    #[tokio::test]
    async fn semantic_cache_hit_and_miss_with_signature() {
        let config = SemanticCacheConfig {
            enabled: true,
            threshold: 0.95,
            max_entries: 100,
            ttl: Duration::from_secs(60),
        };

        let cache = SemanticCache::new(config, Arc::new(MockEmbeddingBackend));

        let sig_a = CompatibilitySignature {
            model: "gpt-4".into(),
            system_fingerprint: None,
            tools_hash: None,
            response_format: None,
        };

        let sig_b = CompatibilitySignature {
            model: "claude-3".into(), // different model!
            system_fingerprint: None,
            tools_hash: None,
            response_format: None,
        };

        cache
            .insert("test prompt".into(), sig_a.clone(), "test response".into())
            .await;

        // Exact match with same signature = Hit
        let res1 = cache.lookup("test prompt", &sig_a).await;
        match res1 {
            SemanticLookupOutcome::Hit {
                response,
                similarity,
            } => {
                assert_eq!(response, "test response");
                assert!(similarity >= 0.95);
            }
            _ => panic!("expected hit"),
        }

        // Same prompt but different signature = Miss (Safety rule #17)
        let res2 = cache.lookup("test prompt", &sig_b).await;
        assert_eq!(res2, SemanticLookupOutcome::Miss);
    }
}
