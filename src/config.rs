//! Configuration loader for the Rust LLM Gateway.
//!
//! Reads `config/default.toml` and overlays environment overrides prefixed with
//! `APP__` (double underscore indicates nesting).
//!
//! Example: `APP__SERVER__PORT=9000` overrides `server.port`.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub rate_limit: RateLimitConfig,
    pub cache: CacheConfig,
    pub circuit_breaker: CircuitBreakerConfig,
    /// Ordered list of upstream LLM providers; `priority` (lower = first) drives failover order.
    pub providers: Vec<ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Maximum request body size in bytes (default 4 MiB).
    pub max_body_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    /// Client credentials accepted by the gateway.
    #[serde(default)]
    pub credentials: Vec<ClientCredential>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientCredential {
    pub key: String,
    pub client_id: String,
    pub tier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    /// Burst capacity (tokens) per API key. Refill rate is RPM/60 per second.
    pub default_rpm: u32,
    /// Token-per-minute budget per API key (used by the TPM limiter).
    pub default_tpm: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CacheConfig {
    pub exact_max_entries: u64,
    pub exact_ttl_seconds: u64,
    pub semantic_enabled: bool,
    /// Cosine similarity threshold for semantic cache hits (>= this is considered a hit).
    pub semantic_threshold: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CircuitBreakerConfig {
    /// Consecutive failures required to trip the breaker OPEN.
    pub failure_threshold: u32,
    /// Seconds before OPEN transitions to HALF-OPEN probe.
    pub cooldown_seconds: u64,
    /// Max concurrent probe requests allowed while HALF-OPEN.
    pub half_open_max_calls: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub endpoint: String,
    /// Bearer token forwarded to the upstream provider. May be empty for local vLLM/Ollama.
    pub api_key: Option<String>,
    /// Lower number = tried first. Failover proceeds in ascending order.
    pub priority: u32,
    /// Connection timeout (TCP + TLS) in seconds.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_seconds: u64,
    /// Request timeout (headers + full body for non-streaming) in seconds.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_seconds: u64,
    /// Max idle time between SSE chunks before killing stream.
    #[serde(default = "default_stream_idle")]
    pub stream_idle_timeout_seconds: u64,
}

fn default_connect_timeout() -> u64 {
    5
}
fn default_request_timeout() -> u64 {
    30
}
fn default_stream_idle() -> u64 {
    120
}

impl AppConfig {
    /// Load configuration from `config/default.toml` with `APP__`-prefixed env overrides.
    pub fn load() -> Result<Self, config::ConfigError> {
        let cfg: Self = config::Config::builder()
            .add_source(config::File::with_name("config/default").required(true))
            .add_source(
                config::Environment::with_prefix("APP")
                    .prefix_separator("__")
                    .separator("__")
                    .list_separator(",")
                    .try_parsing(true),
            )
            .build()?
            .try_deserialize()?;

        cfg.validate().map_err(config::ConfigError::Message)?;
        Ok(cfg)
    }

    /// Validate configuration at startup. Returns a descriptive error message
    /// for invalid configurations rather than panicking at request time.
    pub fn validate(&self) -> Result<(), String> {
        if self.providers.is_empty() {
            return Err("at least one provider must be configured".into());
        }
        for (i, p) in self.providers.iter().enumerate() {
            if p.name.is_empty() {
                return Err(format!("providers[{}].name must not be empty", i));
            }
            if p.endpoint.is_empty() {
                return Err(format!("providers[{}].endpoint must not be empty", i));
            }
            if !p.endpoint.starts_with("http://") && !p.endpoint.starts_with("https://") {
                return Err(format!(
                    "providers[{}].endpoint must start with http:// or https://, got: {}",
                    i, p.endpoint
                ));
            }
            if p.request_timeout_seconds == 0 {
                return Err(format!(
                    "providers[{}].request_timeout_seconds must be > 0",
                    i
                ));
            }
        }
        if self.auth.credentials.is_empty() {
            tracing::warn!("no client credentials configured — all requests will be rejected");
        }
        for (i, cred) in self.auth.credentials.iter().enumerate() {
            if cred.key.is_empty() {
                return Err(format!("auth.credentials[{}].key must not be empty", i));
            }
            if cred.client_id.is_empty() {
                return Err(format!(
                    "auth.credentials[{}].client_id must not be empty",
                    i
                ));
            }
        }
        if self.rate_limit.default_rpm == 0 {
            return Err("rate_limit.default_rpm must be > 0".into());
        }
        if self.rate_limit.default_tpm == 0 {
            return Err("rate_limit.default_tpm must be > 0".into());
        }
        Ok(())
    }

    /// Returns providers sorted by priority (ascending). Stable: original order wins ties.
    pub fn providers_sorted(&self) -> Vec<ProviderConfig> {
        let mut v = self.providers.clone();
        v.sort_by_key(|p| p.priority);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_toml() {
        let toml = r#"
[server]
host = "0.0.0.0"
port = 8080
max_body_bytes = 4194304

[auth]
credentials = [
    { key = "k1", client_id = "client1", tier = "default" },
    { key = "k2", client_id = "client2", tier = "premium" }
]

[rate_limit]
default_rpm = 60
default_tpm = 90000

[cache]
exact_max_entries = 50000
exact_ttl_seconds = 3600
semantic_enabled = false
semantic_threshold = 0.96

[circuit_breaker]
failure_threshold = 3
cooldown_seconds = 10
half_open_max_calls = 1

[[providers]]
name = "primary"
endpoint = "http://localhost:8000"
api_key = ""
priority = 1
connect_timeout_seconds = 5
request_timeout_seconds = 30
stream_idle_timeout_seconds = 120
"#;

        let parsed: AppConfig = toml::from_str(toml).unwrap();
        assert_eq!(parsed.server.port, 8080);
        assert_eq!(parsed.providers.len(), 1);
        assert_eq!(parsed.auth.credentials.len(), 2);
        assert_eq!(parsed.auth.credentials[0].client_id, "client1");
    }

    #[test]
    fn validates_empty_providers() {
        let config = AppConfig {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
                max_body_bytes: 4194304,
            },
            auth: AuthConfig {
                credentials: vec![ClientCredential {
                    key: "k".into(),
                    client_id: "c".into(),
                    tier: "default".into(),
                }],
            },
            rate_limit: RateLimitConfig {
                default_rpm: 60,
                default_tpm: 90000,
            },
            cache: CacheConfig {
                exact_max_entries: 100,
                exact_ttl_seconds: 60,
                semantic_enabled: false,
                semantic_threshold: 0.96,
            },
            circuit_breaker: CircuitBreakerConfig {
                failure_threshold: 3,
                cooldown_seconds: 10,
                half_open_max_calls: 1,
            },
            providers: vec![], // empty!
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validates_invalid_endpoint() {
        let config = AppConfig {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
                max_body_bytes: 4194304,
            },
            auth: AuthConfig {
                credentials: vec![ClientCredential {
                    key: "k".into(),
                    client_id: "c".into(),
                    tier: "default".into(),
                }],
            },
            rate_limit: RateLimitConfig {
                default_rpm: 60,
                default_tpm: 90000,
            },
            cache: CacheConfig {
                exact_max_entries: 100,
                exact_ttl_seconds: 60,
                semantic_enabled: false,
                semantic_threshold: 0.96,
            },
            circuit_breaker: CircuitBreakerConfig {
                failure_threshold: 3,
                cooldown_seconds: 10,
                half_open_max_calls: 1,
            },
            providers: vec![ProviderConfig {
                name: "bad".into(),
                endpoint: "ftp://not-http".into(), // invalid!
                api_key: None,
                priority: 1,
                connect_timeout_seconds: 5,
                request_timeout_seconds: 30,
                stream_idle_timeout_seconds: 120,
            }],
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.contains("http://"),
            "expected http scheme error, got: {}",
            err
        );
    }
}
