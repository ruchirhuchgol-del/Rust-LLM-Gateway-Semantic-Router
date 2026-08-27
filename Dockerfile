# ---- Builder stage ----
FROM rust:1.88-bookworm AS builder

WORKDIR /app

# Cache crates.io index & dependencies separately for faster rebuilds
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/dummy.rs \
    && sed -i 's#path = "src/main.rs"#path = "src/dummy.rs"#' Cargo.toml \
    && cargo build --release \
    && sed -i 's#path = "src/dummy.rs"#path = "src/main.rs"#' Cargo.toml \
    && rm src/dummy.rs

# Copy actual source tree and build the real binary
COPY . .
RUN cargo build --release && strip target/release/rust-llm-gateway

# ---- Runtime stage (minimal, ~5MB) ----
FROM debian:bookworm-slim AS runtime

# CA certs for HTTPS calls + curl for health probes
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Non-root user for safety
RUN useradd --uid 10001 --gid 0 --home-dir /app --no-create-home rustgw
WORKDIR /app

COPY --from=builder /app/target/release/rust-llm-gateway /usr/local/bin/rust-llm-gateway
COPY --from=builder /app/config ./config

USER 10001:0
EXPOSE 8080

# P1 #6: Use curl for health probes instead of a non-existent --health-check CLI flag.
# The previous HEALTHCHECK launched a second full gateway instance that failed to bind.
HEALTHCHECK --interval=10s --timeout=2s --start-period=5s --retries=3 \
    CMD ["curl", "-sf", "http://127.0.0.1:8080/healthz"]

ENTRYPOINT ["/usr/local/bin/rust-llm-gateway"]
