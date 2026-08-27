//! Bearer-token authentication middleware.
//!
//! Extracts the `Authorization: Bearer <key>` header from the incoming request,
//! validates it by hashing and looking up the credential in `state.credentials`,
//! and stashes the `ClientIdentity` into request extensions for downstream handlers.

use axum::async_trait;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::state::{AppState, ClientIdentity};

/// Extractor for the validated client identity. Available inside handlers via
/// `Extension<ClientIdentity>` (set by [`auth_middleware`]).
#[async_trait]
impl<S> FromRequestParts<S> for ClientIdentity
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<ClientIdentity>()
            .cloned()
            .ok_or(AppError::Unauthorized)
    }
}

/// Pull the bearer token out of `Authorization: Bearer <key>`.
/// Returns `Err(Unauthorized)` if the header is missing or malformed.
pub fn extract_api_key(headers: &HeaderMap) -> Result<String, AppError> {
    let auth_header = headers
        .get("authorization")
        .ok_or(AppError::Unauthorized)?
        .to_str()
        .map_err(|_| AppError::Unauthorized)?;

    if !auth_header.starts_with("Bearer ") {
        return Err(AppError::Unauthorized);
    }

    let key = auth_header["Bearer ".len()..].trim().to_string();
    if key.is_empty() {
        return Err(AppError::Unauthorized);
    }
    Ok(key)
}

/// Axum middleware function: validates the bearer token against the configured allowlist.
/// On success, inserts [`ClientIdentity`] into the request extensions for downstream handlers.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: Next,
) -> Result<Response, AppError> {
    let api_key = extract_api_key(request.headers())?;

    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    let hash = format!("{:x}", hasher.finalize());

    let identity = state.credentials.get(&hash).ok_or_else(|| {
        // P0 #3: Do NOT log API key prefixes.
        tracing::debug!("rejected API key (hash miss)");
        AppError::Unauthorized
    })?;

    request.extensions_mut().insert(identity.clone());
    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn build_headers(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", token)).unwrap(),
        );
        h
    }

    #[test]
    fn extracts_valid_bearer() {
        let key = extract_api_key(&build_headers("sk-abc-123")).unwrap();
        assert_eq!(key, "sk-abc-123");
    }

    #[test]
    fn rejects_missing_header() {
        let headers = HeaderMap::new();
        assert!(matches!(
            extract_api_key(&headers),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn rejects_non_bearer() {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Basic abc123"));
        assert!(matches!(extract_api_key(&h), Err(AppError::Unauthorized)));
    }

    #[test]
    fn rejects_empty_token() {
        assert!(matches!(
            extract_api_key(&build_headers("")),
            Err(AppError::Unauthorized)
        ));
    }
}
