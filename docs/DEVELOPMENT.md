# Development & Contribution Guide

## Prerequisites

- Rust 1.75+ (or 1.88+ / latest stable)
- (Windows) Visual Studio C++ Build Tools with MSVC compiler & linker
- (Linux/macOS) standard `build-essential` or Xcode command-line tools

---

## Local Workflow

### 1. Build and Run
```bash
cargo build
cargo run
```

### 2. Run Test Suite
```bash
# Run unit & integration tests
cargo test

# Run tests with backtrace
RUST_BACKTRACE=1 cargo test -- --nocapture
```

### 3. Linting & Formatting
```bash
cargo fmt --check
cargo clippy --all-targets --all-features
```

---

## Project Structure

- `src/handlers/`: API endpoint implementations (`chat.rs`, `embeddings.rs`, `health.rs`, `metrics.rs`).
- `src/middleware/`: Axum middleware layers (`auth.rs`, `rate_limit.rs`, `trace.rs`).
- `src/proxy/`: Outbound proxy client, SSE parser (`sse_parser.rs`), and zero-copy streaming pipeline (`stream.rs`).
- `src/router/`: Circuit breaker state machine (`circuit_breaker.rs`) and provider selector (`selector.rs`).
- `src/cache/`: Exact match cache (`exact.rs`) and semantic vector cache (`semantic.rs`).
- `src/telemetry/`: Prometheus metrics registration and helpers (`metrics.rs`).
- `tests/`: Integration tests with deterministic mock servers (`integration_tests.rs`).
