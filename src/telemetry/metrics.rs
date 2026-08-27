//! Prometheus metrics registration & helpers.
//!
//! All metric operations go through the global `metrics::Recorder` installed once at
//! startup via [`init`]. The recorder is backed by `metrics-exporter-prometheus`
//! and exposed through the `/metrics` HTTP endpoint (see `handlers/metrics.rs`).

use std::time::Duration;

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Metric name constants - kept here so typos are caught at compile time.
pub mod names {
    pub const REQUESTS_TOTAL: &str = "gateway_requests_total";
    pub const REQUEST_ERRORS_TOTAL: &str = "gateway_request_errors_total";
    pub const CACHE_HITS_TOTAL: &str = "gateway_cache_hits_total";
    pub const CACHE_MISSES_TOTAL: &str = "gateway_cache_misses_total";
    pub const TOKENS_TOTAL: &str = "gateway_tokens_total";
    pub const ACTIVE_CONNECTIONS: &str = "gateway_active_connections";
    pub const TTFT_SECONDS: &str = "gateway_ttft_seconds";
    pub const REQUEST_LATENCY_SECONDS: &str = "gateway_request_latency_seconds";
    pub const UPSTREAM_LATENCY_SECONDS: &str = "gateway_upstream_latency_seconds";
    pub const GATEWAY_OVERHEAD_SECONDS: &str = "gateway_overhead_seconds";
    pub const RATE_LIMIT_REJECTIONS_TOTAL: &str = "gateway_rate_limit_rejections_total";
    pub const PROVIDER_REQUESTS_TOTAL: &str = "gateway_provider_requests_total";
    pub const PROVIDER_FAILURES_TOTAL: &str = "gateway_provider_failures_total";
    pub const PROVIDER_CIRCUIT_STATE: &str = "gateway_provider_circuit_state";
    pub const UPSTREAM_FIRST_BYTE_SECONDS: &str = "gateway_upstream_first_byte_seconds";
}

/// Install the Prometheus recorder and return a handle for `/metrics` rendering.
///
/// Safe to call multiple times: the recorder is installed exactly once via a
/// `OnceLock`, and subsequent calls return a clone of the cached handle. This
/// makes `init()` test-friendly when many test cases need a fresh `AppState`.
pub fn init() -> PrometheusHandle {
    static HANDLE: std::sync::OnceLock<PrometheusHandle> = std::sync::OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let handle = PrometheusBuilder::new()
                .set_buckets(&[
                    0.0001, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0,
                    2.5, 5.0, 10.0, 30.0,
                ])
                .expect("invalid Prometheus bucket configuration")
                .install_recorder()
                .expect("failed to install Prometheus recorder");
            describe_metrics();
            handle
        })
        .clone()
}

fn describe_metrics() {
    describe_counter!(
        names::REQUESTS_TOTAL,
        "Total requests processed by the gateway"
    );
    describe_counter!(names::REQUEST_ERRORS_TOTAL, "Total request errors");
    describe_counter!(
        names::CACHE_HITS_TOTAL,
        "Total cache hits (exact + semantic)"
    );
    describe_counter!(names::CACHE_MISSES_TOTAL, "Total cache misses");
    describe_counter!(names::TOKENS_TOTAL, "Total tokens (prompt + completion)");
    describe_gauge!(names::ACTIVE_CONNECTIONS, "Active in-flight connections");
    describe_histogram!(names::TTFT_SECONDS, "Time-to-first-token (streaming)");
    describe_histogram!(names::REQUEST_LATENCY_SECONDS, "End-to-end request latency");
    describe_histogram!(names::UPSTREAM_LATENCY_SECONDS, "Upstream provider latency");
    describe_histogram!(
        names::GATEWAY_OVERHEAD_SECONDS,
        "Gateway processing overhead"
    );
    describe_counter!(names::RATE_LIMIT_REJECTIONS_TOTAL, "Rate limit rejections");
    describe_counter!(names::PROVIDER_REQUESTS_TOTAL, "Requests sent to providers");
    describe_counter!(names::PROVIDER_FAILURES_TOTAL, "Provider failures");
    describe_gauge!(
        names::PROVIDER_CIRCUIT_STATE,
        "Circuit breaker state (0=closed, 1=open, 2=half_open)"
    );
    describe_histogram!(
        names::UPSTREAM_FIRST_BYTE_SECONDS,
        "Time to first byte from upstream"
    );
}

// ---- Recording helpers ------------------------------------------------------

pub fn record_request(provider: &str, status: &str) {
    counter!(
        names::REQUESTS_TOTAL,
        &[
            ("provider", provider.to_string()),
            ("status", status.to_string())
        ]
    )
    .increment(1);
}

pub fn record_error(provider: &str, error: &str) {
    counter!(
        names::REQUEST_ERRORS_TOTAL,
        &[
            ("provider", provider.to_string()),
            ("error", error.to_string())
        ]
    )
    .increment(1);
}

pub fn record_cache_hit(cache_type: &str) {
    counter!(names::CACHE_HITS_TOTAL, &[("type", cache_type.to_string())]).increment(1);
}

pub fn record_cache_miss(cache_type: &str) {
    counter!(
        names::CACHE_MISSES_TOTAL,
        &[("type", cache_type.to_string())]
    )
    .increment(1);
}

/// `kind` is `"prompt"` or `"completion"`.
pub fn record_tokens(provider: &str, kind: &str, count: u64) {
    counter!(
        names::TOKENS_TOTAL,
        &[
            ("provider", provider.to_string()),
            ("kind", kind.to_string())
        ]
    )
    .increment(count);
}

pub fn set_active_connections(count: f64) {
    gauge!(names::ACTIVE_CONNECTIONS).set(count);
}

pub fn record_ttft(provider: &str, duration: Duration) {
    histogram!(names::TTFT_SECONDS, &[("provider", provider.to_string())])
        .record(duration.as_secs_f64());
}

pub fn record_request_latency(duration: Duration) {
    histogram!(names::REQUEST_LATENCY_SECONDS).record(duration.as_secs_f64());
}

pub fn record_upstream_latency(provider: &str, duration: Duration) {
    histogram!(
        names::UPSTREAM_LATENCY_SECONDS,
        &[("provider", provider.to_string())]
    )
    .record(duration.as_secs_f64());
}

pub fn record_gateway_overhead(duration: Duration) {
    histogram!(names::GATEWAY_OVERHEAD_SECONDS).record(duration.as_secs_f64());
}

pub fn record_rate_limit_rejection(kind: &str) {
    counter!(
        names::RATE_LIMIT_REJECTIONS_TOTAL,
        &[("kind", kind.to_string())]
    )
    .increment(1);
}

pub fn record_provider_request(provider: &str) {
    counter!(
        names::PROVIDER_REQUESTS_TOTAL,
        &[("provider", provider.to_string())]
    )
    .increment(1);
}

pub fn record_provider_failure(provider: &str, reason: &str) {
    counter!(
        names::PROVIDER_FAILURES_TOTAL,
        &[
            ("provider", provider.to_string()),
            ("reason", reason.to_string())
        ]
    )
    .increment(1);
}

pub fn set_provider_circuit_state(provider: &str, state: f64) {
    gauge!(
        names::PROVIDER_CIRCUIT_STATE,
        &[("provider", provider.to_string())]
    )
    .set(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent_and_renders_metrics() {
        let h1 = init();
        let h2 = init();

        record_request("test-provider", "success");

        let snapshot = h1.render();
        assert!(
            snapshot.contains("gateway_requests_total"),
            "expected gateway_requests_total in snapshot: {}",
            snapshot
        );
        assert_eq!(h1.render(), h2.render());
    }
}
