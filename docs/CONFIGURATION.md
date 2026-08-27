# Configuration Reference

Configuration is loaded from `config/default.toml` and can be overridden using environment variables prefixed with `APP__`.

---

## Example `config/default.toml`

```toml
[server]
host = "0.0.0.0"
port = 8080
max_body_bytes = 4194304  # 4 MiB

[auth]
credentials = [
    { key = "sk-prod-12345", client_id = "client-alpha", tier = "tier-1" },
    { key = "sk-dev-67890", client_id = "client-beta", tier = "default" }
]

[rate_limit]
default_rpm = 60          # 60 requests per minute
default_tpm = 90000       # 90,000 tokens per minute

[cache]
exact_max_entries = 50000
exact_ttl_seconds = 3600
semantic_enabled = false
semantic_threshold = 0.96

[circuit_breaker]
failure_threshold = 3
cooldown_seconds = 10
half_open_max_calls = 1

[[providers]]
name = "primary-vllm"
endpoint = "http://localhost:8000"
api_key = ""
priority = 1
connect_timeout_seconds = 5
request_timeout_seconds = 30
stream_idle_timeout_seconds = 120

[[providers]]
name = "secondary-groq"
endpoint = "https://api.groq.com/openai"
api_key = "gsk_..."
priority = 2
connect_timeout_seconds = 5
request_timeout_seconds = 20
stream_idle_timeout_seconds = 120
```

---

## Environment Variable Overrides

Any configuration field can be overridden via environment variables using double underscores (`__`) as delimiters:

| Variable | Target Config Field | Example Value |
|---|---|---|
| `APP__SERVER__PORT` | `server.port` | `9000` |
| `APP__SERVER__HOST` | `server.host` | `127.0.0.1` |
| `APP__RATE_LIMIT__DEFAULT_RPM` | `rate_limit.default_rpm` | `120` |
| `APP__RATE_LIMIT__DEFAULT_TPM` | `rate_limit.default_tpm` | `200000` |
| `APP__CACHE__SEMANTIC_ENABLED` | `cache.semantic_enabled` | `true` |
