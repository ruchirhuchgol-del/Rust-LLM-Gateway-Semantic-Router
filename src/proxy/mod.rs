//! Upstream HTTP dispatch & SSE streaming pipeline.
//!
//! - [`client`] - helper for building outbound reqwest requests per provider config.
//! - [`stream`] - zero-copy SSE passthrough that records TTFT without buffering.

pub mod client;
pub mod sse_parser;
pub mod stream;
