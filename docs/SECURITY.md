# Security & Threat Model

## Security Principles

1. **Zero Secret Leakage in Logs**:
   - API keys and tokens are never logged.
   - Authentication hashes credentials via SHA-256 before lookup in constant-time maps.
   - Debug logs omit key prefixes and credential values.

2. **Sanitized Error Responses**:
   - Upstream connection errors, DNS details, and internal stack traces are stripped before responses reach clients.
   - Clients receive standard error codes (`upstream_error`, `internal_error`, `rate_limited`) without exposing backend topology.

3. **Memory & Payload Bounds**:
   - `RequestBodyLimitLayer` rejects oversized bodies at the socket layer before allocation.
   - `SseParser` imposes a hard 1 MiB buffer ceiling to protect against un-delimited data floods.
   - Moka caches enforce both TTL and strict capacity ceilings.

4. **Constant-Time Operations**:
   - API keys are looked up by cryptographic hash.
   - Rate limit and circuit breaker state locks are sub-microsecond non-blocking mutexes that never cross async `.await` boundaries.

5. **Container Security**:
   - The Docker runtime image runs as non-root user `rustgw` (`UID 10001`).
   - Minimal Debian slim footprint containing only root certificates and the compiled binary.
