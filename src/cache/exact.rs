//! Exact-match cache.
//!
//! FR-3.1 from the PRD: fast in-memory hash cache for identical prompts
//! (`SHA-256(model || prompt) + temperature = 0`). Caching non-zero-temperature
//! responses would cache non-deterministic outputs, so we explicitly reject them.

use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Wrapper around `moka::future::Cache<String, String>` that knows how to compute
/// the deterministic cache key for a chat completion request.
#[derive(Clone)]
pub struct ExactCache {
    inner: Arc<moka::future::Cache<String, String>>,
}

impl ExactCache {
    pub fn new(inner: Arc<moka::future::Cache<String, String>>) -> Self {
        Self { inner }
    }

    /// Compute the cache key for a chat request, or `None` if the request is not cacheable.
    /// A request is cacheable iff `temperature == 0` (deterministic decoding).
    ///
    /// `prompt_payload` is the canonical JSON representation of the request body
    /// (e.g., `serde_json::Value::to_string()`). Model + temperature are extracted
    /// from the JSON and merged into the hash so different models or temperatures
    /// produce different keys.
    pub fn compute_key(model: &str, prompt_payload: &str, temperature: f32) -> Option<String> {
        if temperature > 0.0 {
            return None;
        }
        let mut hasher = Sha256::new();
        hasher.update(model.as_bytes());
        hasher.update(b"\x00");
        hasher.update(prompt_payload.as_bytes());
        let hash = hasher.finalize();
        Some(format!("exact:{:x}", hash))
    }

    pub async fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key).await
    }

    pub async fn insert(&self, key: String, value: String) {
        self.inner.insert(key, value).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_cache() -> ExactCache {
        let inner = Arc::new(
            moka::future::Cache::builder()
                .max_capacity(10)
                .time_to_live(Duration::from_secs(60))
                .build(),
        );
        ExactCache::new(inner)
    }

    #[tokio::test]
    async fn basic_insert_get() {
        let cache = make_cache();
        let key = ExactCache::compute_key("gpt-4", "hello", 0.0).unwrap();
        cache.insert(key.clone(), "world".into()).await;
        assert_eq!(cache.get(&key).await.as_deref(), Some("world"));
    }

    #[test]
    fn rejects_nonzero_temperature() {
        assert!(ExactCache::compute_key("gpt-4", "hello", 0.1).is_none());
    }

    #[test]
    fn different_models_produce_different_keys() {
        let a = ExactCache::compute_key("gpt-4", "hello", 0.0).unwrap();
        let b = ExactCache::compute_key("claude-3", "hello", 0.0).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn different_prompts_produce_different_keys() {
        let a = ExactCache::compute_key("gpt-4", "hello", 0.0).unwrap();
        let b = ExactCache::compute_key("gpt-4", "world", 0.0).unwrap();
        assert_ne!(a, b);
    }
}
