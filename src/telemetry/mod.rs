//! Telemetry & observability module.
//!
//! Exposes a `metrics` submodule that initializes the Prometheus recorder and provides
//! thin helper functions for the rest of the codebase to record counters / histograms
//! without sprinkling raw `metrics!` macro calls everywhere.

pub mod metrics;
