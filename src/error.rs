//! Error type shared by all Axum handlers. Implements `IntoResponse` so handlers
//! can return `Result<T, AppError>` and Axum will serialize errors automatically.

use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::fmt;

#[derive(Debug)]
pub enum AppError {
    /// 401 - missing or invalid bearer token.
    Unauthorized,
    /// 429 - rate limit exceeded. `retry_after` is the number of seconds the client should wait.
    RateLimited { retry_after: u64 },
    /// 502 - upstream provider returned an error or all providers failed.
    Upstream(String),
    /// 400 - malformed request body or missing required fields.
    BadRequest(String),
    /// 500 - unexpected internal failure.
    Internal(String),
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after: Option<u64>,
}

impl AppError {
    pub fn internal(msg: impl Into<String>) -> Self {
        AppError::Internal(msg.into())
    }

    pub fn upstream(msg: impl Into<String>) -> Self {
        AppError::Upstream(msg.into())
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        AppError::BadRequest(msg.into())
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Unauthorized => write!(f, "unauthorized"),
            AppError::RateLimited { retry_after } => {
                write!(f, "rate_limited (retry after {}s)", retry_after)
            }
            AppError::Upstream(msg) => write!(f, "upstream: {}", msg),
            AppError::BadRequest(msg) => write!(f, "bad request: {}", msg),
            AppError::Internal(msg) => write!(f, "internal: {}", msg),
        }
    }
}

impl std::error::Error for AppError {}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message, retry_after): (StatusCode, &'static str, String, Option<u64>) =
            match &self {
                AppError::Unauthorized => (
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "Invalid or missing API key".into(),
                    None,
                ),
                AppError::RateLimited { retry_after } => (
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limited",
                    "Rate limit exceeded".into(),
                    Some(*retry_after),
                ),
                AppError::Upstream(_msg) => (
                    StatusCode::BAD_GATEWAY,
                    "upstream_error",
                    "Upstream provider error or timeout".into(),
                    None,
                ),
                AppError::BadRequest(msg) => {
                    (StatusCode::BAD_REQUEST, "bad_request", msg.clone(), None)
                }
                AppError::Internal(_msg) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "Internal server error".into(),
                    None,
                ),
            };

        let body = ErrorBody {
            error: ErrorDetail {
                code,
                message,
                retry_after,
            },
        };

        let mut response = (status, axum::Json(body)).into_response();
        if let AppError::RateLimited { retry_after } = &self {
            // https://datatracker.ietf.org/doc/html/rfc6585#section-4
            if let Ok(val) = HeaderValue::from_str(&retry_after.to_string()) {
                response.headers_mut().insert("retry-after", val);
            }
        }
        response
    }
}

/// Convenience converter so handlers can use `?` on any error type that has a Display.
impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::bad_request(format!("invalid JSON: {}", e))
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::upstream(format!("reqwest: {}", e))
    }
}

/// Convert Axum's `Json` extractor rejection into `AppError` so handlers using
/// `Json<T>` can return `Result<R, AppError>`.
impl From<axum::extract::rejection::JsonRejection> for AppError {
    fn from(e: axum::extract::rejection::JsonRejection) -> Self {
        AppError::bad_request(format!("invalid JSON body: {}", e))
    }
}

/// Convert Axum's body extraction error (e.g., payload too large).
impl From<axum::extract::rejection::BytesRejection> for AppError {
    fn from(e: axum::extract::rejection::BytesRejection) -> Self {
        AppError::bad_request(format!("body extraction failed: {}", e))
    }
}
