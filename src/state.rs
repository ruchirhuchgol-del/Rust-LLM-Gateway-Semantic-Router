//! Shared application state shared across Axum handlers.
//!
//! `AppState` is wrapped in `Arc` internally by `axum::extract::State` cloning; the inner
//! fields use `Arc` for the heavy handles (config, http client, caches) and `DashMap`
//! for the per-key mutable state (rate limiters, circuit breakers) to avoid global locks.

use crate::cache::exact::ExactCache;
use crate::cache::semantic::{NoopEmbeddingBackend, SemanticCache, SemanticCacheConfig};
use crate::config::AppConfig;
use crate::middleware::rate_limit::RateLimitBuckets;
use crate::router::circuit_breaker::CircuitBreaker;
use dashmap::DashMap;
use metrics_exporter_prometheus::PrometheusHandle;
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct ClientIdentity {
    pub client_id: String,
    pub tier: String,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    /// Pre-sorted providers to avoid O(N log N) allocation on every request
    pub sorted_providers: Arc<[crate::config::ProviderConfig]>,
    /// Shared HTTP client with built-in connection pooling, HTTP/2, and keep-alive.
    pub http_client: Client,
    /// Per-API-key rate-limit buckets (RPM + TPM).
    pub rate_limiter: Arc<DashMap<String, RateLimitBuckets>>,
    /// Exact-match cache routed through the `ExactCache` wrapper.
    pub exact_cache: ExactCache,
    /// Semantic vector cache for approximate prompt matching.
    pub semantic_cache: Arc<SemanticCache>,
    /// Per-provider-name circuit breakers.
    pub circuit_breakers: Arc<DashMap<String, Arc<CircuitBreaker>>>,
    /// Pre-computed hash map of credentials for constant-time-ish auth lookup.
    /// Key is SHA-256(api_key).
    pub credentials: Arc<HashMap<String, ClientIdentity>>,
    /// Prometheus handle used to render `/metrics`.
    pub metrics_handle: PrometheusHandle,
}

impl AppState {
    /// Construct shared state from the loaded config. Idempotent & side-effect-free
    /// apart from initializing the reqwest client and Moka cache.
    pub async fn new(config: AppConfig, metrics_handle: PrometheusHandle) -> Self {
        let http_client = Client::builder()
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(32)
            .tcp_keepalive(Duration::from_secs(60))
            .tcp_nodelay(true)
            .build()
            .expect("failed to build reqwest client");

        let inner_cache = Arc::new(
            moka::future::Cache::builder()
                .max_capacity(config.cache.exact_max_entries)
                .time_to_live(Duration::from_secs(config.cache.exact_ttl_seconds))
                .build(),
        );
        let exact_cache = ExactCache::new(inner_cache);

        let semantic_cache_cfg = SemanticCacheConfig {
            enabled: config.cache.semantic_enabled,
            threshold: config.cache.semantic_threshold,
            max_entries: config.cache.exact_max_entries as usize,
            ttl: Duration::from_secs(config.cache.exact_ttl_seconds),
        };
        let semantic_cache = Arc::new(SemanticCache::new(
            semantic_cache_cfg,
            Arc::new(NoopEmbeddingBackend),
        ));

        let mut credentials = HashMap::new();
        for cred in &config.auth.credentials {
            let mut hasher = Sha256::new();
            hasher.update(cred.key.as_bytes());
            let hash = format!("{:x}", hasher.finalize());
            credentials.insert(
                hash,
                ClientIdentity {
                    client_id: cred.client_id.clone(),
                    tier: cred.tier.clone(),
                },
            );
        }

        let sorted_providers: Arc<[_]> = config.providers_sorted().into();

        Self {
            config: Arc::new(config),
            sorted_providers,
            http_client,
            rate_limiter: Arc::new(DashMap::new()),
            exact_cache,
            semantic_cache,
            circuit_breakers: Arc::new(DashMap::new()),
            credentials: Arc::new(credentials),
            metrics_handle,
        }
    }

    /// Look up (or lazily create) the circuit breaker for `provider_name`.
    pub fn breaker_for(&self, provider_name: &str) -> Arc<CircuitBreaker> {
        let cfg = &self.config.circuit_breaker;
        self.circuit_breakers
            .entry(provider_name.to_string())
            .or_insert_with(|| {
                Arc::new(CircuitBreaker::new(
                    cfg.failure_threshold,
                    Duration::from_secs(cfg.cooldown_seconds),
                    cfg.half_open_max_calls,
                ))
            })
            .clone()
    }
}
