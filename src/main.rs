//! Rust LLM Gateway - binary entry point.
//!
//! Boots Tokio + tracing, loads config, and delegates to the library `run()` which
//! wires up the Axum router, middleware stack, and graceful shutdown.

#![forbid(unsafe_code)]

use rust_llm_gateway::config::AppConfig;
use rust_llm_gateway::run;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // ---- 1. Initialize tracing ----
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,rust_llm_gateway=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .json()
        .init();

    // ---- 2. Load configuration ----
    let config = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("FATAL: failed to load config: {}", e);
        std::process::exit(1);
    });

    // ---- 3. Run the gateway ----
    if let Err(e) = run(config).await {
        eprintln!("FATAL: gateway exited with error: {}", e);
        std::process::exit(1);
    }
}
