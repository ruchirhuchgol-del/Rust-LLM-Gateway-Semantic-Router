//! Typed API Request/Response layer for OpenAI-compatible endpoints.
//!
//! These types are the contract between the gateway and client requests.
//! They implement Deserialize (for parsing incoming requests) and Serialize
//! (for cache key computation and re-serialization to upstream providers).
//!
//! ## Cache key correctness
//!
//! `ChatCompletionRequest` derives `Serialize` so that `serde_json::to_string()`
//! produces a canonical JSON representation. The exact cache key is computed as
//! `SHA-256(model || "\0" || canonical_json)` — this means every output-affecting
//! field (model, messages, temperature, top_p, seed, tools, tool_choice,
//! response_format, max_tokens, stop) is included in the cache key by construction.
//! The `#[serde(flatten)] extra` field captures provider-specific fields too.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// OpenAI-compatible chat completion request.
///
/// All output-affecting fields are explicitly modeled. Provider-specific fields
/// are captured by the `extra` map and included in serialization (thus in the
/// cache key hash).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_top_p")]
    pub top_p: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<StopTokens>,

    /// Any other provider-specific or unsupported fields are captured here
    /// and forwarded to the upstream provider unchanged.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

fn default_temperature() -> f32 {
    1.0
}
fn default_top_p() -> f32 {
    1.0
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Message content can be a plain string or a structured content array
/// (for multi-modal requests with images/audio).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<Value>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String, // usually "function"
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tool {
    pub r#type: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResponseFormat {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StopTokens {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

// ----- Response Structs -----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: Message,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

// ----- Embeddings Structs -----

/// OpenAI-compatible embedding request.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmbeddingRequest {
    pub model: String,
    /// The input text(s) to embed. Required field — not Option.
    pub input: EmbeddingInput,
    #[serde(default)]
    pub encoding_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Embedding input can be a single string, array of strings,
/// array of token IDs, or array of token ID arrays.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    String(String),
    StringArray(Vec<String>),
    TokenArray(Vec<u32>),
    TokenMatrix(Vec<Vec<u32>>),
}
