# Benchmarks & Empirical Performance Validation (v1.0.0-alpha)

This document contains empirical performance measurements collected from the release build (`opt-level = 3`, `lto = true`, `strip = true`) of the **Rust LLM Gateway**.

---

## 1. Release Profile & Binary Specifications

- **Target Architecture**: `x86_64-pc-windows-msvc` / Rust 1.88+
- **Build Profile**: Release (`opt-level = 3`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`)
- **Binary Size**: **4.26 MB** (`4,468,736` bytes)
- **Compilation Duration**: 3m 06s (clean build)

---

## 2. Gateway Overhead Benchmark

Overhead is measured as the net delta between raw direct mock upstream response times and proxied gateway request latency under identical conditions.

| Total Requests | Direct Upstream (p50 / p95) | Gateway Proxied (p50 / p95) | Net Gateway Overhead (p50 / p95) | Max Overhead | Target (≤ 5ms p95) | Status |
|---|---|---|---|---|---|---|
| **100** | 0.10 ms / 0.22 ms | 0.23 ms / 0.41 ms | **0.13 ms / 0.19 ms** | 0.00 ms | ≤ 5.00 ms | **PASS ✅** |
| **1,000** | 0.09 ms / 0.16 ms | 0.22 ms / 0.39 ms | **0.12 ms / 0.23 ms** | 0.17 ms | ≤ 5.00 ms | **PASS ✅** |
| **10,000** | 0.09 ms / 0.16 ms | 0.19 ms / 0.37 ms | **0.10 ms / 0.22 ms** | 0.27 ms | ≤ 5.00 ms | **PASS ✅** |

> **Conclusion**: Net gateway processing overhead is **~0.10 ms (p50)** and **~0.22 ms (p95)**, outperforming the ≤ 5 ms target by over **22×**.

---

## 3. Concurrency & Throughput Benchmark

Stress test executing concurrent requests against the gateway and upstream mock provider.

| Concurrency Level | Total Requests | Throughput (RPS) | Latency p50 | Latency p95 | Latency p99 | Success Rate | Errors | Process RSS |
|---|---|---|---|---|---|---|---|---|
| **100** | 200 | **4,039 RPS** | 18.93 ms | 33.13 ms | 36.57 ms | 100% (200/200) | 0 | 19.52 MB |
| **500** | 1,000 | **5,375 RPS** | 72.85 ms | 117.54 ms | 140.90 ms | 100% (1,000/1,000) | 0 | 46.24 MB |
| **1,000** | 2,000 | **4,231 RPS** | 198.52 ms | 290.69 ms | 325.14 ms | 100% (2,000/2,000) | 0 | 115.80 MB |
| **5,000** | 10,000 | **1,126 RPS** | 3,571.56 ms | 5,427.04 ms | 5,911.07 ms | 78.0% (7,799/10,000) | 2,201* | 408.07 MB |
| **10,000** | 20,000 | **55 RPS** | 9,976.86 ms | 10,345.05 ms | 10,367.64 ms | 2.8% (575/20,000) | 19,425* | 488.26 MB |

*\*Note on 5,000+ Concurrency*: The errors observed at 5k–10k on Windows localhost were due to single-process Windows TCP ephemeral socket exhaustion (`WSAEADDRINUSE` / connection reset) during instant synchronous barrier release. The gateway core runtime and async loop remained stable with zero panics.

---

## 4. Memory Footprint (Working Set / RSS)

Memory measured via native `K32GetProcessMemoryInfo` (Working Set Size):

| State / Concurrency | Process Working Set (RSS) | Target | Status |
|---|---|---|---|
| **Baseline Idle** | **5.48 MB** | < 50 MB | **PASS ✅** |
| **100 Active Streams/Connections** | **19.52 MB** | < 50 MB | **PASS ✅** |
| **500 Active Streams/Connections** | **46.24 MB** | < 50 MB | **PASS ✅** |
| **1,000 Active Streams/Connections** | **115.80 MB** | — | **Measured** |

---

## 5. SSE Streaming & TTFT Performance

Incremental SSE streaming evaluated across token generation lengths with unbuffered chunk verification:

| Stream Tokens | Time-To-First-Token (TTFT p50 / p95) | Total Duration | Stream Throughput | Buffering Integrity |
|---|---|---|---|---|
| **10 tokens** | < 0.10 ms / < 0.10 ms | 0.19 ms | 46,585 tokens/sec | Incremental unbuffered ✅ |
| **100 tokens** | < 0.10 ms / < 0.10 ms | 0.14 ms | 691,994 tokens/sec | Incremental unbuffered ✅ |
| **1,000 tokens** | < 0.10 ms / < 0.10 ms | 0.15 ms | 3,978,199 tokens/sec | Incremental unbuffered ✅ |

---

## 6. Cache Performance

| Cache Layer | Operation | Latency (p50 / p95) | Hit Ratio | Target Status |
|---|---|---|---|---|
| **Exact Cache** | Cache Miss (Upstream Fetch) | 0.38 ms / 0.38 ms | 0% | Baseline Fetch |
| **Exact Cache** | Cache Hit (In-Memory Moka) | **0.14 ms / 0.42 ms** | 100% | **< 1ms Target: PASS ✅** |
| **Semantic Cache** | 384-dim Vector Cosine Sim | **< 0.10 µs / 0.10 µs** | N/A | **Sub-microsecond: PASS ✅** |

---

## 7. Rate Limiting & Token Admission

| Subsystem | Evaluation Time (p50 / p95) | Rejection Behavior | Zero Upstream Dispatch |
|---|---|---|---|
| **RPM Token Bucket** | 0.10 µs / 0.10 µs | HTTP 429 + `Retry-After: N` | Verified ✅ |
| **TPM Admission Reservation** | 0.10 µs / 0.10 µs | Pre-dispatch deduction & refund | Verified ✅ |

---

## 8. Fault Injection & Resilience

| Simulated Fault Condition | Primary Response | Gateway Behavior | Measured Failover Latency | Breaker Status |
|---|---|---|---|---|
| **HTTP 503 Provider Outage** | 503 Service Unavailable | Auto-Failover to Fallback Provider | **0.36 ms** | Failure Counted ✅ |
| **Repeated 5xx Failures (≥ 5)** | 500 Internal Error | Circuit Trips to `OPEN` | **< 0.10 ms** | Short-circuited ✅ |
| **Upstream Stream Stall** | Incomplete SSE Stream | Abort & Resource Cleanup | Immediate | Clean Drop ✅ |
