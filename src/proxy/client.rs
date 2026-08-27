//! Reqwest client helpers.
//!
//! `AppState::http_client` is the shared `reqwest::Client` (with pooling configured in
//! `state.rs`). This module provides a small helper that builds a `RequestBuilder`
//! targeting a provider's `/v1/chat/completions` (or `/v1/embeddings`) endpoint with
//! the right timeout and bearer token.

use crate::config::ProviderConfig;
use bytes::Bytes;
use reqwest::Client;
use std::time::Duration;

/// Build an outbound POST request to the provider's chat-completions endpoint.
/// `body` is the canonical JSON bytes captured from the incoming client payload.
pub fn build_chat_request(
    client: &Client,
    provider: &ProviderConfig,
    body: Bytes,
) -> reqwest::RequestBuilder {
    let url = format!(
        "{}/v1/chat/completions",
        provider.endpoint.trim_end_matches('/')
    );
    build_request(client, provider, &url, body)
}

/// Build an outbound POST request to the provider's embeddings endpoint.
pub fn build_embeddings_request(
    client: &Client,
    provider: &ProviderConfig,
    body: Bytes,
) -> reqwest::RequestBuilder {
    let url = format!("{}/v1/embeddings", provider.endpoint.trim_end_matches('/'));
    build_request(client, provider, &url, body)
}

fn build_request(
    client: &Client,
    provider: &ProviderConfig,
    url: &str,
    body: Bytes,
) -> reqwest::RequestBuilder {
    let mut req = client
        .post(url)
        .timeout(Duration::from_secs(provider.request_timeout_seconds))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(body);

    // Only forward bearer token if a key is configured (non-empty).
    if let Some(key) = &provider.api_key {
        if !key.is_empty() {
            req = req.bearer_auth(key);
        }
    }
    req
}
