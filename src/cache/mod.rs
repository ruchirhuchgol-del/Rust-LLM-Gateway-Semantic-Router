//! Cache layers for the LLM gateway.
//!
//! Two-tier cache:
//!   * [`exact::ExactCache`] - O(1) SHA-256 hash lookup for deterministic requests
//!     (temperature = 0). Backed by `moka::future::Cache` for TTL + capacity eviction.
//!   * [`semantic::SemanticCache`] - (Phase 2) cosine-similarity lookup over a local
//!     embedding model. Stubbed for now; wiring `ort` (ONNX runtime) is the documented
//!     extension point in `PRD §7 Next Steps`.

pub mod exact;
pub mod semantic;
