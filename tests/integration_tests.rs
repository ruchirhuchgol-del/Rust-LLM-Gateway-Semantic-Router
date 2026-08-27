//! Integration tests for the Rust LLM Gateway.
//!
//! These tests use `axum::Router` directly without binding a TCP port - they exercise
//! the full middleware stack (auth -> rate_limit -> handler) via `Router::oneshot`.
//!
//! We DON'T hit real upstream providers - instead we set `provider.endpoint` to
//! `http://127.0.0.1:1` (immediate connection refused) so the failover path is tested
//! without external dependencies.

#![cfg(test)]

use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use serde_json::json;
use tower::ServiceExt;

use rust_llm_gateway::config::{
    AppConfig, AuthConfig, CacheConfig, CircuitBreakerConfig, ClientCredential, ProviderConfig,
    RateLimitConfig, ServerConfig,
};
use rust_llm_gateway::state::AppState;
use rust_llm_gateway::telemetry::metrics;

/// Build a test ProviderConfig pointing to an unreachable endpoint.
fn unreachable_provider(name: &str, priority: u32) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        endpoint: "http://127.0.0.1:1".into(), // port 1 = immediate refused
        api_key: None,
        priority,
        connect_timeout_seconds: 1,
        request_timeout_seconds: 1,
        stream_idle_timeout_seconds: 5,
    }
}

/// Build a test ProviderConfig pointing to a given URL.
fn mock_provider(name: &str, url: &str, priority: u32) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        endpoint: url.into(),
        api_key: None,
        priority,
        connect_timeout_seconds: 5,
        request_timeout_seconds: 30,
        stream_idle_timeout_seconds: 120,
    }
}

/// Helper: build a default test AppConfig with the given providers.
fn test_config(providers: Vec<ProviderConfig>) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            max_body_bytes: 1024 * 1024,
        },
        auth: AuthConfig {
            credentials: vec![ClientCredential {
                key: "test-key".into(),
                client_id: "test-client".into(),
                tier: "default".into(),
            }],
        },
        rate_limit: RateLimitConfig {
            default_rpm: 1000,
            default_tpm: 100_000,
        },
        cache: CacheConfig {
            exact_max_entries: 100,
            exact_ttl_seconds: 60,
            semantic_enabled: false,
            semantic_threshold: 0.96,
        },
        circuit_breaker: CircuitBreakerConfig {
            failure_threshold: 5,
            cooldown_seconds: 1,
            half_open_max_calls: 1,
        },
        providers,
    }
}

/// Helper: build an AppState suitable for testing.
async fn make_test_state() -> AppState {
    let config = test_config(vec![unreachable_provider("test-unreachable", 1)]);
    let handle = metrics::init();
    AppState::new(config, handle).await
}

// ---------------------------------------------------------------------------
// Health & Metrics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn healthz_returns_200() {
    let state = make_test_state().await;
    let app = rust_llm_gateway::build_test_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_format() {
    let state = make_test_state().await;
    let app = rust_llm_gateway::build_test_router(state);

    rust_llm_gateway::telemetry::metrics::record_request("test_provider", "success");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 16384).await.unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("gateway_requests_total")
            || text.contains("# HELP")
            || text.contains("# EOF"),
        "expected prometheus metrics, got: {}",
        &text[..text.len().min(200)]
    );
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_completions_without_api_key_returns_401() {
    let state = make_test_state().await;
    let app = rust_llm_gateway::build_test_router(state);

    let body = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hi"}],
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn chat_completions_with_invalid_api_key_returns_401() {
    let state = make_test_state().await;
    let app = rust_llm_gateway::build_test_router(state);

    let body = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hi"}],
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer wrong-key")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Provider failover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_completions_fails_over_when_upstream_unreachable() {
    let state = make_test_state().await;
    let app = rust_llm_gateway::build_test_router(state);

    let body = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hi"}],
        "temperature": 0.0,
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer test-key")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_completions_returns_429_when_rate_limit_exhausted() {
    let config = AppConfig {
        rate_limit: RateLimitConfig {
            default_rpm: 1,
            default_tpm: 1000,
        },
        ..test_config(vec![unreachable_provider("test-unreachable", 1)])
    };
    let handle = metrics::init();
    let state = AppState::new(config, handle).await;
    let app = rust_llm_gateway::build_test_router(state);

    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.0,
        }))
        .unwrap(),
    );

    // First request — consumes the only RPM token (will still 502 due to upstream).
    let resp1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer test-key")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::BAD_GATEWAY);

    // Second request — should hit rate limit (429) before reaching upstream.
    let resp2 = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer test-key")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(resp2.headers().contains_key("retry-after"));
}

// ---------------------------------------------------------------------------
// Error envelope
// ---------------------------------------------------------------------------

/// P1 #5: Malformed JSON should return 400 via AppError envelope.
#[tokio::test]
async fn malformed_json_returns_400_with_error_envelope() {
    let state = make_test_state().await;
    let app = rust_llm_gateway::build_test_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer test-key")
                .body(Body::from(b"this is not json".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        v.get("error").is_some(),
        "expected error envelope, got: {}",
        v
    );
    assert_eq!(v["error"]["code"], "bad_request");
}

// ---------------------------------------------------------------------------
// Happy-path with mock upstream
// ---------------------------------------------------------------------------

/// Helper to start a local mock upstream server that always succeeds.
async fn start_mock_upstream() -> (String, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new()
        .route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "id": "chatcmpl-mock",
                    "object": "chat.completion",
                    "created": 1677652288,
                    "model": "mock-model",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "Hello from mock upstream!"
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 10,
                        "total_tokens": 20
                    }
                }))
            }),
        )
        .route(
            "/v1/embeddings",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "object": "list",
                    "data": [{"object": "embedding", "embedding": [0.1, 0.2], "index": 0}],
                    "model": "text-embedding-3-small",
                    "usage": {"prompt_tokens": 5, "total_tokens": 5}
                }))
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (url, handle)
}

#[tokio::test]
async fn chat_completions_happy_path_with_mock_upstream() {
    let (upstream_url, _handle) = start_mock_upstream().await;

    let config = test_config(vec![mock_provider("test-mock", &upstream_url, 1)]);
    let handle = metrics::init();
    let state = AppState::new(config, handle).await;
    let app = rust_llm_gateway::build_test_router(state);

    let body = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hello"}],
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer test-key")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let resp_body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    assert_eq!(
        v["choices"][0]["message"]["content"],
        "Hello from mock upstream!"
    );
}

#[tokio::test]
async fn embeddings_happy_path_with_mock_upstream() {
    let (upstream_url, _handle) = start_mock_upstream().await;

    let config = test_config(vec![mock_provider("test-mock", &upstream_url, 1)]);
    let handle = metrics::init();
    let state = AppState::new(config, handle).await;
    let app = rust_llm_gateway::build_test_router(state);

    let body = json!({
        "model": "text-embedding-3-small",
        "input": "hello world",
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer test-key")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

/// Verify that the error envelope sanitizes internal details.
#[tokio::test]
async fn upstream_error_does_not_leak_internal_details() {
    let state = make_test_state().await;
    let app = rust_llm_gateway::build_test_router(state);

    let body = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hi"}],
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer test-key")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let resp_body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    let msg = v["error"]["message"].as_str().unwrap_or("");
    // Must NOT contain raw upstream error details, IP addresses, or provider names
    assert!(
        !msg.contains("127.0.0.1"),
        "error message leaked internal IP: {}",
        msg
    );
    assert!(
        !msg.contains("connect"),
        "error message leaked connection details: {}",
        msg
    );
}
