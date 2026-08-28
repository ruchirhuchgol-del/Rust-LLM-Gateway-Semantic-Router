# Rust LLM Gateway & Semantic Router

> A high-performance, OpenAI-compatible LLM gateway built in Rust for
> multi-provider routing, token-aware rate limiting, streaming inference,
> caching, failure recovery, and observability.

[![Rust](https://img.shields.io/badge/Rust-2021-orange?logo=rust)](https://www.rust-lang.org/)
[![Tokio](https://img.shields.io/badge/Runtime-Tokio-blue)](https://tokio.rs/)
[![Axum](https://img.shields.io/badge/Web-Axum-purple)](https://github.com/tokio-rs/axum)
[![Reqwest](https://img.shields.io/badge/HTTP-Reqwest-green)](https://github.com/seanmonstar/reqwest)
[![Tests](https://img.shields.io/badge/Tests-36%2F36%20Passing-brightgreen)](#testing)
[![Status](https://img.shields.io/badge/Status-v1.0.0--alpha-orange)](#project-status)

---

## Project Status

**Release Candidate — v1.0.0-alpha**

The core gateway implementation, resilience mechanisms, streaming pipeline,
caching engine, authentication, rate limiting, observability, integration
tests, and release validation are implemented and verified.

Current validation includes:

- 36/36 automated tests passing
- `cargo check --all-targets` passing
- `cargo clippy --all-targets --all-features -- -D warnings` passing
- `cargo fmt --check` passing
- 0.22 ms measured p95 net gateway overhead
- 5,375 RPS peak measured throughput
- 5.48 MB idle process working set
- 100% success at 1,000 concurrent requests

The 5K–10K localhost stress tests were also executed. Those runs were
constrained by Windows TCP ephemeral-port exhaustion rather than a gateway
panic or runtime crash. See [Concurrency & Limitations](#concurrency--limitations).

---

# Table of Contents

- [Why This Project](#why-this-project)
- [What It Does](#what-it-does)
- [Architecture](#architecture)
- [Request Lifecycle](#request-lifecycle)
- [System Design Principles](#system-design-principles)
- [Core Components](#core-components)
- [Provider Routing & Resilience](#provider-routing--resilience)
- [Streaming Architecture](#streaming-architecture)
- [Rate Limiting & Admission Control](#rate-limiting--admission-control)
- [Caching Architecture](#caching-architecture)
- [Observability](#observability)
- [Security](#security)
- [API](#api)
- [Configuration](#configuration)
- [Performance](#performance)
- [Concurrency & Limitations](#concurrency--limitations)
- [Testing](#testing)
- [Project Structure](#project-structure)
- [Running Locally](#running-locally)
- [Docker](#docker)
- [Design Decisions](#design-decisions)
- [Known Limitations](#known-limitations)
- [Roadmap](#roadmap)
- [Documentation](#documentation)
- [License](#license)

---

# Why This Project?

Modern applications increasingly depend on multiple LLM providers:

- OpenAI
- Anthropic
- Groq
- local vLLM deployments
- Ollama
- other OpenAI-compatible inference servers

Directly integrating every application with every provider creates
operational problems:

```text
                    Without a Gateway

Application A ───────► OpenAI
Application A ───────► Anthropic
Application A ───────► Ollama

Application B ───────► OpenAI
Application B ───────► Anthropic
Application B ───────► Ollama

                 duplicated logic
                         │
                         ├── authentication
                         ├── rate limiting
                         ├── retries
                         ├── failover
                         ├── caching
                         └── observability
````

The gateway centralizes those concerns:

```text
                         ┌──────────────────────┐
                         │    Applications      │
                         └──────────┬───────────┘
                                    │
                                    ▼
                         ┌──────────────────────┐
                         │ Rust LLM Gateway     │
                         │                      │
                         │ Auth                 │
                         │ Rate Limiting        │
                         │ Caching              │
                         │ Routing              │
                         │ Circuit Breaking     │
                         │ Streaming            │
                         │ Observability        │
                         └──────────┬───────────┘
                                    │
                 ┌──────────────────┼──────────────────┐
                 ▼                  ▼                  ▼
             OpenAI             Anthropic          Local LLM
                                                    vLLM/Ollama
```

The goal is to provide a single OpenAI-compatible interface while moving
provider complexity into an infrastructure layer.

---

# What It Does

The gateway provides:

| Capability            | Purpose                                   |
| --------------------- | ----------------------------------------- |
| OpenAI-compatible API | Unified client interface                  |
| Bearer authentication | Client authorization                      |
| RPM rate limiting     | Request admission control                 |
| TPM admission control | Token-aware budget enforcement            |
| Exact caching         | Avoid repeated deterministic inference    |
| Semantic cache engine | Similarity-based response lookup          |
| Provider routing      | Select eligible upstream providers        |
| Circuit breaker       | Isolate unhealthy providers               |
| Automatic failover    | Continue service during provider failures |
| SSE streaming         | Low-latency streaming responses           |
| TTFT tracking         | Measure first meaningful model output     |
| Usage accounting      | Track prompt/completion tokens            |
| Prometheus metrics    | Operational observability                 |
| Request IDs           | Distributed request correlation           |
| Graceful shutdown     | Controlled lifecycle management           |
| Request size limits   | Memory/resource protection                |

---

# Architecture

## High-Level System Architecture

```text
┌─────────────────────────────────────────────────────────────────────┐
│                         CLIENT APPLICATIONS                         │
│                                                                     │
│   Web App       Backend API       Agent       CLI       Internal AI  │
└────────────────────────────────┬────────────────────────────────────┘
                                 │
                                 │ HTTP/1.1 / HTTP/2
                                 ▼
┌─────────────────────────────────────────────────────────────────────┐
│                       RUST LLM GATEWAY                              │
│                                                                     │
│  ┌───────────────┐    ┌────────────────┐    ┌──────────────────┐   │
│  │ Trace /       │───►│ Authentication │───►│ RPM / TPM        │   │
│  │ Request ID    │    │                │    │ Admission        │   │
│  └───────────────┘    └────────────────┘    └────────┬─────────┘   │
│                                                       │             │
│                                                       ▼             │
│                                             ┌──────────────────┐    │
│                                             │ Cache Layer      │    │
│                                             │                  │    │
│                                             │ Exact            │    │
│                                             │ Semantic Engine  │    │
│                                             └────────┬─────────┘    │
│                                                      │ MISS          │
│                                                      ▼               │
│                                             ┌──────────────────┐    │
│                                             │ Provider Router  │    │
│                                             │ + Circuit Breaker│    │
│                                             └────────┬─────────┘    │
│                                                      │               │
│                                                      ▼               │
│                                             ┌──────────────────┐    │
│                                             │ Reqwest HTTP     │    │
│                                             │ Connection Pool   │    │
│                                             └────────┬─────────┘    │
│                                                      │               │
│                         ┌────────────────────────────┼────────────┐  │
│                         │                            │            │  │
│                         ▼                            ▼            ▼  │
│                    Non-streaming                 SSE stream     Retry│
│                         │                            │               │
│                         ▼                            ▼               │
│                   JSON response              Incremental parser    │
│                         │                            │               │
│                         └──────────────┬─────────────┘               │
│                                        ▼                             │
│                              Usage / Latency / TTFT                  │
│                              Reconciliation                          │
└────────────────────────────────────────┬────────────────────────────┘
                                         │
             ┌───────────────────────────┼────────────────────────┐
             ▼                           ▼                        ▼
      ┌─────────────┐             ┌─────────────┐          ┌─────────────┐
      │ OpenAI      │             │ Anthropic   │          │ Local LLM   │
      │             │             │             │          │ vLLM/Ollama │
      └─────────────┘             └─────────────┘          └─────────────┘

                         OBSERVABILITY
                              │
                              ▼
                    ┌────────────────────┐
                    │ Prometheus Metrics  │
                    │ Logs / Traces       │
                    └────────────────────┘
```

---

# Request Lifecycle

A request follows a deterministic pipeline:

```text
Client Request
      │
      ▼
┌───────────────────────┐
│ Request ID / Tracing  │
└───────────┬───────────┘
            ▼
┌───────────────────────┐
│ Bearer Authentication │
└───────────┬───────────┘
            │ invalid
            ├──────────────► 401
            │
            ▼
┌───────────────────────┐
│ RPM Admission         │
└───────────┬───────────┘
            │ exceeded
            ├──────────────► 429 + Retry-After
            │
            ▼
┌───────────────────────┐
│ TPM Reservation       │
└───────────┬───────────┘
            │ insufficient
            ├──────────────► 429
            │
            ▼
┌───────────────────────┐
│ Exact Cache Lookup    │
└───────────┬───────────┘
            │ HIT
            ├──────────────► Cached Response
            │
            ▼ MISS
┌───────────────────────┐
│ Semantic Cache        │
│ Lookup                │
└───────────┬───────────┘
            │ HIT
            ├──────────────► Cached Response
            │
            ▼ MISS
┌───────────────────────┐
│ Provider Selection    │
│ + Circuit Breaker     │
└───────────┬───────────┘
            ▼
┌───────────────────────┐
│ Reqwest Upstream      │
└───────────┬───────────┘
            │
      ┌─────┴─────┐
      │           │
      ▼           ▼
 Non-stream     Streaming
      │           │
      │           ▼
      │      Incremental SSE
      │      Parser
      │           │
      │           ▼
      │      TTFT + Usage
      │           │
      └─────┬─────┘
            ▼
┌───────────────────────┐
│ Token Reconciliation  │
└───────────┬───────────┘
            ▼
┌───────────────────────┐
│ Metrics / Telemetry   │
└───────────┬───────────┘
            ▼
        Client
```

---

# System Design Principles

The project is designed around several core distributed-systems and
high-performance backend principles.

## 1. Separation of Concerns

Each infrastructure responsibility is isolated:

```text
Authentication
      │
Rate Limiting
      │
Caching
      │
Routing
      │
Resilience
      │
Transport
      │
Telemetry
```

This makes individual components testable and replaceable.

---

## 2. Fail Fast

Invalid or unauthorized requests should not consume expensive downstream
resources.

Examples:

```text
Invalid API key
      ↓
401 immediately

RPM exhausted
      ↓
429 immediately

TPM budget unavailable
      ↓
429 immediately

Provider circuit OPEN
      ↓
Skip provider immediately
```

---

## 3. Admission Control / Backpressure

The gateway performs token admission before dispatching requests.

```text
Request
   │
   ▼
Estimate token requirement
   │
   ▼
Reserve budget
   │
   ├── insufficient ──► 429
   │
   ▼
Dispatch upstream
   │
   ▼
Actual usage
   │
   ▼
Reconcile reservation
```

This prevents requests from consuming downstream capacity when the
configured token budget is already exhausted.

---

## 4. Bounded Resources

The gateway avoids intentionally unbounded memory growth.

Bounded mechanisms include:

* request body limits
* bounded caches
* SSE parser buffer ceiling
* bounded metric labels
* provider state registries
* controlled background tasks

The incremental SSE parser also enforces a 1 MiB buffer ceiling.

---

## 5. Failure Isolation

Provider failures should not become gateway-wide failures.

```text
             Provider A
                 │
             failures
                 ▼
         ┌──────────────┐
         │ Circuit      │
         │ Breaker      │
         └──────┬───────┘
                │ OPEN
                ▼
          Skip Provider A
                │
                ▼
          Provider B
```

This isolates unhealthy dependencies.

---

## 6. Graceful Degradation

When possible:

```text
Primary Provider
      │
      ├── healthy ──► use primary
      │
      └── unhealthy
             │
             ▼
       fallback provider
```

Caching can also prevent repeated upstream calls.

---

## 7. Stateless Request Processing

Request processing is designed so that persistent business state is not
tied to individual worker threads.

Shared infrastructure state is held through application state and
concurrent structures.

This allows Tokio's async runtime to distribute work across worker
threads without thread affinity.

---

## 8. Observability by Design

Important request lifecycle events are measurable:

```text
request
   │
   ├── authentication
   ├── rate limiting
   ├── cache lookup
   ├── provider selection
   ├── upstream latency
   ├── TTFT
   ├── token usage
   └── final response
```

This allows performance problems to be attributed rather than guessed.

---

## 9. Measure Before Optimizing

Performance engineering follows:

```text
Correctness
     ↓
Observability
     ↓
Benchmark
     ↓
Identify bottleneck
     ↓
Optimize
     ↓
Benchmark again
```

Rather than:

```text
Premature optimization
        ↓
Complexity
        ↓
Unknown performance
```

---

# Core Components

## Authentication

The gateway accepts Bearer credentials.

Authentication is performed before provider dispatch.

Security properties include:

* API keys are not logged
* key material is not returned in errors
* SHA-256 based credential representation
* constant-time comparison
* invalid credentials fail fast

---

# Rate Limiting & Admission Control

The gateway implements two complementary controls.

## RPM

Requests-per-minute admission.

```text
Client
  │
  ▼
Token Bucket
  │
  ├── token available ──► request continues
  │
  └── exhausted ────────► 429
                              │
                              └── Retry-After
```

## TPM

Tokens-per-minute admission.

The gateway estimates token requirements before dispatch.

```text
Estimated Tokens
       │
       ▼
┌──────────────────┐
│ Reserve Capacity │
└────────┬─────────┘
         │
         ▼
     Upstream
         │
         ▼
 Actual Usage
         │
         ▼
 Reconcile Difference
```

Reservations are refunded/reconciled for:

* cache hits
* upstream failures
* lower-than-estimated usage
* request lifecycle termination

---

# Caching Architecture

## Exact Cache

Deterministic requests can be cached.

The cache is restricted to deterministic requests such as:

```text
temperature = 0
```

Conceptually:

```text
(model + canonical request)
              │
              ▼
          SHA-256
              │
              ▼
        Exact Cache
        ┌──────┴──────┐
        │             │
       HIT           MISS
        │             │
        ▼             ▼
    response       provider
```

The implementation uses an in-memory bounded cache.

---

## Semantic Cache Engine

The project also contains a semantic cache engine based on vector
similarity.

Conceptually:

```text
Prompt
  │
  ▼
Embedding Vector
  │
  ▼
Compatibility Signature
  │
  ▼
Cosine Similarity
  │
  ├── threshold met ──► cache hit
  │
  └── threshold missed ► cache miss
```

Compatibility includes factors such as:

* model
* system instructions
* tools
* response format

### Current Scope

The semantic cache engine and similarity infrastructure are implemented.

Integration with an actual local embedding inference model such as
MiniLM/ONNX remains an extension point rather than being represented as
a completed production embedding pipeline.

---

# Provider Routing & Resilience

Providers are maintained as prioritized candidates.

Conceptually:

```text
                Request
                   │
                   ▼
          Provider Selector
                   │
          ┌────────┼────────┐
          ▼        ▼        ▼
       Primary  Secondary  Tertiary
          │        │        │
          ▼        ▼        ▼
       Circuit   Circuit   Circuit
       Breaker  Breaker  Breaker
```

Providers that are not eligible are skipped.

Provider ordering is prepared in application state rather than repeatedly
sorting the provider list inside the request hot path.

---

# Circuit Breaker

Each provider has an explicit state machine:

```text
                 failures >= threshold
          ┌───────────────────────────────┐
          │                               ▼
     ┌──────────┐                    ┌──────────┐
     │  CLOSED  │                    │   OPEN   │
     └────┬─────┘                    └────┬─────┘
          ▲                               │
          │                               │ cooldown
          │                               ▼
          │                         ┌────────────┐
          └──── successful probe ─── │ HALF-OPEN │
                                    └─────┬──────┘
                                          │
                                          └── failure → OPEN
```

Typical behavior:

### CLOSED

Normal requests are dispatched.

### OPEN

Provider is considered unhealthy and requests fail over to another
eligible provider.

### HALF-OPEN

A limited probe determines whether the provider has recovered.

Client-side validation errors such as `400` and `422` do not falsely mark
a healthy provider as failed.

---

# Failure Classification

The gateway distinguishes client errors from provider failures.

| Outcome            | Typical Treatment              |
| ------------------ | ------------------------------ |
| 2xx                | Success                        |
| 400                | Client error                   |
| 401                | Provider/configuration error   |
| 403                | Provider/configuration error   |
| 408                | Retryable failure              |
| 422                | Client error                   |
| 429                | Provider capacity/rate failure |
| 5xx                | Provider failure               |
| Connection failure | Provider failure               |
| Timeout            | Provider failure               |
| Stream failure     | Provider failure               |

The exact retry/failover behavior is configuration/policy dependent.

---

# Streaming Architecture

Streaming requests use incremental SSE processing.

```text
LLM Provider
     │
     │ HTTP byte chunks
     ▼
Reqwest bytes_stream()
     │
     ▼
┌─────────────────────┐
│ Incremental SSE     │
│ Parser              │
└──────────┬──────────┘
           │
      ┌────┴───────────┐
      │                │
      ▼                ▼
 Client Stream      Inspector
      │                │
      │                ├── TTFT
      │                ├── Usage
      │                └── Metrics
      ▼
 Client
```

The parser handles:

* arbitrary TCP chunk boundaries
* multiple SSE events per chunk
* events split across chunks
* LF and CRLF delimiters
* `[DONE]`
* trailing usage information
* malformed JSON without panicking
* tool-call content
* bounded buffering

The gateway does not require network chunks to align with SSE events.

---

# TTFT

Time-to-First-Token is measured at the first meaningful model output
rather than simply measuring the first bytes received from the network.

Conceptually:

```text
Request dispatched
       │
       │
       │ provider processing
       │
       ▼
First meaningful content delta
       │
       ▼
       TTFT
```

This provides a more useful measure of model responsiveness.

---

# Timeout Model

The gateway separates different timeout concerns:

```text
Connection
   │
   ▼
Connect Timeout

HTTP request
   │
   ▼
Request Timeout

Streaming response
   │
   ▼
Stream Idle Timeout
```

A continuously active stream should not be treated as idle merely because
its total duration is long.

---

# Observability

The gateway exposes Prometheus-compatible metrics through:

```text
GET /metrics
```

Tracked telemetry includes concepts such as:

* request counts
* request latency
* gateway overhead
* TTFT
* token usage
* provider failures
* cache hits/misses
* rate-limit events
* circuit state

Metric labels are intentionally bounded.

Raw user-controlled values are not used as arbitrary metric labels.

---

# Security

Security principles include:

```text
                    Security Boundary

Client
  │
  ▼
Authentication
  │
  ▼
Validated Internal Identity
  │
  ▼
Gateway
  │
  ▼
Provider
```

Implemented protections include:

* Bearer authentication
* constant-time credential comparison
* hashed credential representation
* no API-key logging
* sanitized upstream errors
* request body limits
* bounded SSE buffering
* bounded metric cardinality
* configuration validation
* no unnecessary credential propagation

Internal upstream details such as URLs, addresses, and stack traces are
not returned directly to clients.

---

# API

The gateway exposes an OpenAI-compatible API surface.

## Chat Completions

```http
POST /v1/chat/completions
```

Supports:

* standard JSON responses
* streaming responses
* OpenAI-compatible message structures
* provider routing
* caching where eligible
* token accounting

Example:

```bash
curl http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "your-model",
    "messages": [
      {
        "role": "user",
        "content": "Explain Rust ownership in one paragraph."
      }
    ],
    "temperature": 0
  }'
```

---

## Streaming

```bash
curl http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "your-model",
    "messages": [
      {
        "role": "user",
        "content": "Explain async Rust."
      }
    ],
    "stream": true
  }'
```

---

## Embeddings

```http
POST /v1/embeddings
```

Example:

```bash
curl http://localhost:8080/v1/embeddings \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "embedding-model",
    "input": "Rust is a systems programming language."
  }'
```

---

## Health

```http
GET /healthz
```

Example:

```bash
curl http://localhost:8080/healthz
```

---

## Metrics

```http
GET /metrics
```

Example:

```bash
curl http://localhost:8080/metrics
```

See [API.md](API.md) for detailed endpoint behavior.

---

# Configuration

Configuration is provided through TOML.

Example structure:

```toml
[server]
host = "0.0.0.0"
port = 8080

[rate_limit]
rpm = 60
tpm = 100000

[cache]
enabled = true

[providers]
# Configure provider-specific settings here.
```

Provider configuration includes separate timeout controls such as:

```text
connect_timeout_seconds
request_timeout_seconds
stream_idle_timeout_seconds
```

See [CONFIGURATION.md](CONFIGURATION.md) for the complete configuration
reference.

---

# Performance

Performance measurements were performed against a release build using
optimization, LTO, a single codegen unit, panic abort, and symbol stripping.

## Release Artifact

| Metric         |                 Result |
| -------------- | ---------------------: |
| Target         | x86_64-pc-windows-msvc |
| Rust           |                  1.88+ |
| Binary size    |            **4.26 MB** |
| Optimization   |        `opt-level = 3` |
| LTO            |                Enabled |
| Codegen units  |                      1 |
| Panic strategy |                `abort` |
| Symbols        |               Stripped |

---

## Gateway Overhead

Gateway overhead is measured as the difference between direct mock-provider
latency and the corresponding proxied gateway latency.

| Requests | Net p50 |     Net p95 | Target | Result |
| -------: | ------: | ----------: | -----: | ------ |
|      100 | 0.13 ms | **0.19 ms** |  ≤5 ms | PASS   |
|    1,000 | 0.12 ms | **0.23 ms** |  ≤5 ms | PASS   |
|   10,000 | 0.10 ms | **0.22 ms** |  ≤5 ms | PASS   |

Measured p95 overhead is approximately **0.22 ms**, substantially below
the 5 ms target. 

---

# Throughput & Concurrency

Measured stress-test results:

| Concurrency | Requests |       RPS |       p50 |       p95 | Success |       RSS |
| ----------: | -------: | --------: | --------: | --------: | ------: | --------: |
|         100 |      200 | **4,039** |  18.93 ms |  33.13 ms |    100% |  19.52 MB |
|         500 |    1,000 | **5,375** |  72.85 ms | 117.54 ms |    100% |  46.24 MB |
|       1,000 |    2,000 | **4,231** | 198.52 ms | 290.69 ms |    100% | 115.80 MB |
|       5,000 |   10,000 |     1,126 |    3.57 s |    5.43 s |   78.0% | 408.07 MB |
|      10,000 |   20,000 |        55 |    9.98 s |   10.35 s |    2.8% | 488.26 MB |

The benchmark evidence shows 100% request success through 1,000 concurrent
requests. The 5K–10K Windows localhost tests experienced TCP ephemeral-port
exhaustion (`WSAEADDRINUSE` / connection reset) during the synchronous
barrier release. The gateway process remained stable without panics. 

Therefore:

> **10,000 concurrent connections is a PRD target, not a claim of 10K successful
> Windows localhost client connections.**

A production deployment should validate the target under an appropriate
Linux/networking environment and with a distributed load generator.

---

# Memory Footprint

Working-set/RSS measurements:

| State             |           RSS | Target | Status   |
| ----------------- | ------------: | -----: | -------- |
| Idle              |   **5.48 MB** | <50 MB | PASS     |
| 100 connections   |  **19.52 MB** | <50 MB | PASS     |
| 500 connections   |  **46.24 MB** | <50 MB | PASS     |
| 1,000 connections | **115.80 MB** |      — | Measured |

The idle footprint is approximately **5.48 MB**, well below the 50 MB
baseline target. 

The 50 MB figure should therefore be interpreted as a **base/idle footprint
target**, not a claim that the process remains below 50 MB at 1,000 active
connections.

---

# Streaming Performance

Incremental streaming tests verified that the gateway processes streams
without buffering the complete response.

Measured stream cases included:

|       Stream |     TTFT | Stream behavior |
| -----------: | -------: | --------------- |
|    10 tokens | <0.10 ms | Incremental     |
|   100 tokens | <0.10 ms | Incremental     |
| 1,000 tokens | <0.10 ms | Incremental     |

The benchmark suite reports incremental/unbuffered behavior across all
tested stream sizes. 

---

# Cache Performance

| Operation                  |         p50 |         p95 |
| -------------------------- | ----------: | ----------: |
| Exact cache hit            | **0.14 ms** | **0.42 ms** |
| Semantic vector similarity |    <0.10 µs |     0.10 µs |

The exact cache uses an in-memory Moka cache and the semantic engine uses
vector cosine similarity. 

---

# Rate-Limit Performance

| Operation        |     p50 |     p95 |
| ---------------- | ------: | ------: |
| RPM token bucket | 0.10 µs | 0.10 µs |
| TPM reservation  | 0.10 µs | 0.10 µs |

Rejected requests are prevented from reaching upstream providers and
return `429` with `Retry-After` behavior. 

---

# Testing

The project currently reports:

> **36 / 36 tests passing — 100% success rate**

Test coverage includes unit tests, integration tests, streaming behavior,
authentication, caching, rate limiting, failover, metrics, and resilience.

The test report records:

```text
Unit Tests              26 / 26 PASS
Integration Tests       10 / 10 PASS
cargo fmt --check             PASS
cargo check --all-targets     PASS
cargo clippy                   PASS
```

The complete verification report documents the test execution and
subsystem-level coverage. 

---

# Test Categories

## Authentication

* valid credentials
* invalid credentials
* unauthorized requests
* credential comparison

## Rate Limiting

* RPM exhaustion
* TPM admission
* Retry-After
* reservation lifecycle
* reconciliation

## Cache

* insert/get
* cache hit
* cache miss
* temperature restrictions
* model isolation
* prompt isolation
* semantic similarity

## Streaming

* SSE parsing
* chunk boundaries
* multiple events
* `[DONE]`
* usage extraction
* TTFT
* malformed events
* streaming lifecycle

## Resilience

* provider failure
* timeout
* failover
* circuit breaker
* half-open recovery

## API

* chat completions
* embeddings
* health endpoint
* metrics endpoint

---

# Project Structure

```text
rust-llm-gateway/
│
├── Cargo.toml
├── Cargo.lock
├── Dockerfile
├── .dockerignore
│
├── config/
│   └── default.toml
│
├── src/
│   ├── main.rs
│   ├── lib.rs
│   ├── config.rs
│   ├── state.rs
│   ├── error.rs
│   │
│   ├── handlers/
│   │   ├── chat.rs
│   │   └── embeddings.rs
│   │
│   ├── middleware/
│   │   ├── mod.rs
│   │   ├── auth.rs
│   │   ├── rate_limit.rs
│   │   └── trace.rs
│   │
│   ├── router/
│   │   ├── mod.rs
│   │   ├── selector.rs
│   │   └── circuit_breaker.rs
│   │
│   ├── cache/
│   │   ├── mod.rs
│   │   ├── exact.rs
│   │   └── semantic.rs
│   │
│   ├── proxy/
│   │   ├── mod.rs
│   │   ├── client.rs
│   │   ├── sse_parser.rs
│   │   └── stream.rs
│   │
│   └── telemetry/
│       ├── mod.rs
│       └── metrics.rs
│
├── tests/
│   ├── integration_tests.rs
│   └── load_test.js
│
├── ARCHITECTURE.md
├── API.md
├── CONFIGURATION.md
├── SECURITY.md
├── BENCHMARKS.md
├── DEVELOPMENT.md
├── TEST_REPORT.md
├── COMPLETION_REPORT.md
└── FINAL_RELEASE_REPORT.md
```

---

# Running Locally

## Prerequisites

* Rust 1.88+
* Cargo
* A configured upstream LLM provider

Verify Rust:

```bash
rustc --version
cargo --version
```

---

## Clone

```bash
git clone <your-repository-url>
cd rust-llm-gateway
```

---

## Configure

Review:

```text
config/default.toml
```

Configure provider endpoints, authentication, rate limits, caching,
timeouts, and server settings.

---

## Run

Development:

```bash
cargo run
```

Release:

```bash
cargo run --release
```

---

# Quality Checks

Format:

```bash
cargo fmt --check
```

Compile:

```bash
cargo check --all-targets
```

Test:

```bash
cargo test --all-targets
```

Lint:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Security/dependency audit:

```bash
cargo audit
```

Build release:

```bash
cargo build --release
```

---

# Docker

The project includes a multi-stage Docker build.

Build:

```bash
docker build -t rust-llm-gateway .
```

Run:

```bash
docker run \
  -p 8080:8080 \
  rust-llm-gateway
```

The image is designed around:

* multi-stage compilation
* minimal runtime image
* non-root execution
* health checking
* release optimization

---

# Design Decisions

## Why Rust?

The gateway is primarily an I/O-bound concurrent system where predictable
resource usage and efficient async execution are important.

Rust provides:

* memory safety
* zero-cost abstractions
* strong type guarantees
* predictable ownership
* efficient async execution
* no garbage collector
* excellent concurrency primitives

---

## Why Tokio?

Tokio provides the asynchronous runtime used for:

* concurrent HTTP requests
* timers
* streaming
* cancellation
* background tasks
* connection handling

---

## Why Axum?

Axum provides a lightweight typed HTTP layer built around Tokio and Tower.

This makes middleware composition natural:

```text
Trace
  ↓
Auth
  ↓
Rate Limit
  ↓
Handler
```

---

## Why Reqwest?

Reqwest provides:

* async HTTP
* connection pooling
* streaming bodies
* TLS
* configurable timeouts

It acts as the outbound transport layer.

---

## Why In-Memory Caching?

The first cache layer is intentionally local.

Advantages:

* extremely low latency
* no network hop
* simple operational model
* predictable performance

The architecture can later support distributed caches.

---

# Reliability Model

The gateway treats external LLM providers as unreliable dependencies.

Therefore:

```text
Provider
   │
   ├── latency
   ├── timeout
   ├── 429
   ├── 5xx
   ├── connection failure
   └── stream failure
          │
          ▼
    Failure Classifier
          │
          ▼
    Circuit Breaker
          │
          ▼
      Failover
```

This prevents one unhealthy provider from unnecessarily taking down the
entire request path.

---

# Concurrency Model

The architecture is based on asynchronous I/O rather than a thread-per-request
model.

Conceptually:

```text
                Tokio Runtime
                     │
        ┌────────────┼────────────┐
        ▼            ▼            ▼
     Worker 1     Worker 2     Worker N
        │            │            │
        └────────────┼────────────┘
                     │
              Shared AppState
                     │
       ┌─────────────┼─────────────┐
       ▼             ▼             ▼
   Rate Limits      Cache      Breakers
```

Shared state is designed around concurrent data structures and immutable
configuration where practical.

---

# Performance Engineering Philosophy

The project separates three different measurements:

```text
Gateway overhead
       ≠
Network latency
       ≠
LLM inference latency
```

This is important when evaluating an LLM gateway.

A slow model should not make the gateway itself appear slow.

Therefore benchmark infrastructure uses deterministic mock upstream
providers to isolate gateway behavior.

---

# Known Limitations

## 1. Semantic Embedding Inference

The semantic cache engine is implemented, but actual local embedding model
inference is an extension point.

A future implementation can integrate:

* ONNX Runtime
* MiniLM
* Candle
* another local embedding model

---

## 2. Distributed Rate Limiting

Current rate limiting is in-memory and node-local.

A multi-instance deployment would require shared coordination, such as:

```text
Gateway A ─┐
Gateway B ─┼──► Distributed Rate Limiter
Gateway C ─┘
```

Potential future implementations could use Redis or another shared state
system.

---

## 3. High-Concurrency Windows Benchmark

The gateway was stress-tested at 5K and 10K concurrent request levels.

Those localhost tests were constrained by Windows TCP ephemeral-port/socket
exhaustion during the load generator's synchronized connection burst.

The gateway remained stable without panics, but those runs should not be
interpreted as successful validation of 10K concurrent client connections.

Production-scale concurrency should be validated in an appropriate Linux
deployment environment with a dedicated load generator.

---

# Roadmap

## v1.0

Current release candidate:

* OpenAI-compatible API
* authentication
* RPM/TPM limiting
* exact cache
* semantic cache engine
* provider routing
* circuit breaker
* failover
* SSE
* TTFT
* usage accounting
* Prometheus metrics
* Docker
* integration testing

---

## v1.1

Potential improvements:

* local embedding inference
* distributed rate limiting
* distributed cache
* richer provider health checks
* adaptive routing
* cost-aware provider selection
* improved Linux high-concurrency benchmarking

---

## v2.0

Potential advanced infrastructure:

```text
                    Global Gateway
                         │
          ┌──────────────┼──────────────┐
          ▼              ▼              ▼
       Region A        Region B       Region C
          │              │              │
       Gateway         Gateway         Gateway
          │              │              │
          └──────────────┼──────────────┘
                         │
                 Shared Control Plane
```

Possible additions:

* distributed routing
* global rate limiting
* provider cost optimization
* dynamic health scoring
* adaptive load balancing
* multi-region deployment

---



---

# Project Status

```text
                    RUST LLM GATEWAY

              ┌──────────────────────┐
              │ Functional Core      │
              │       COMPLETE       │
              └──────────┬───────────┘
                         │
              ┌──────────▼───────────┐
              │ Automated Tests      │
              │      36 / 36         │
              └──────────┬───────────┘
                         │
              ┌──────────▼───────────┐
              │ Code Quality         │
              │ fmt + clippy PASS    │
              └──────────┬───────────┘
                         │
              ┌──────────▼───────────┐
              │ Performance          │
              │ Measured             │
              └──────────┬───────────┘
                         │
              ┌──────────▼───────────┐
              │ Release Candidate    │
              │    v1.0.0-alpha      │
              └──────────────────────┘
```

---

# Engineering Highlights

The project demonstrates practical engineering across several layers:

### Systems Programming

* Rust ownership and borrowing
* async execution
* concurrent state
* memory-bounded processing
* cancellation

### AI Infrastructure

* LLM provider abstraction
* token accounting
* model routing
* exact caching
* semantic cache engine
* streaming inference

### Distributed Systems

* circuit breakers
* retries/failover
* admission control
* failure classification
* graceful degradation

### Performance Engineering

* connection pooling
* incremental streaming
* bounded allocations
* hot-path optimization
* benchmark-driven optimization

### Production Engineering

* authentication
* sanitized errors
* structured telemetry
* Prometheus metrics
* Docker packaging
* automated testing

---

# Key Measured Results

| Metric                            |                        Result |
| --------------------------------- | ----------------------------: |
| Automated tests                   |                   **36 / 36** |
| Gateway overhead p95              |                   **0.22 ms** |
| Peak measured throughput          |                 **5,375 RPS** |
| Idle RSS                          |                   **5.48 MB** |
| Successful concurrency validation | **1,000 concurrent requests** |
| Exact cache hit p50               |                   **0.14 ms** |
| RPM limiter p95                   |                   **0.10 µs** |
| TPM reservation p95               |                   **0.10 µs** |
| Release binary                    |                   **4.26 MB** |

---

# License

Add your chosen license here.

Example:

```text
MIT License
```

---

# Author

**Ruchir Huchgol**

AI Engineer | Rust | LLM Infrastructure | Backend Systems

---

> Built as an exploration of production-oriented AI infrastructure:
> combining Rust systems engineering with LLM routing, streaming,
> resilience, caching, rate limiting, and observability.
