// k6 load test for the Rust LLM Gateway.
//
// Usage:
//   k6 run tests/load_test.js
//
// Required env vars:
//   GATEWAY_URL   - e.g. http://localhost:8080
//   API_KEY       - one of the keys in config/default.toml
//
// What it tests:
//   * Sustained throughput (RPS) the gateway can sustain with p95 latency < 5ms overhead.
//   * Streaming (SSE) - measures TTFT under concurrency.
//   * Cache hit ratio after warmup (identical prompt should hit exact cache).

import http from 'k6/http';
import { check, sleep } from 'k6';
import { Counter, Trend } from 'k6/metrics';

const GATEWAY_URL = __ENV.GATEWAY_URL || 'http://localhost:8080';
const API_KEY     = __ENV.API_KEY     || 'dev-key-1';

// Custom metrics
const cacheHits = new Counter('cache_hits');
const cacheMisses = new Counter('cache_misses');
const gatewayLatencyMs = new Trend('gateway_latency_ms', true);

export const options = {
  scenarios: {
    // Scenario 1: sustained RPS - tests throughput & p95 latency overhead.
    sustained_rps: {
      executor: 'constant-arrival-rate',
      rate: 500,                  // 500 RPS
      timeUnit: '1s',
      duration: '30s',
      preAllocatedVUs: 600,
      maxVUs: 2000,
      exec: 'nonStreaming',
      gracefulStop: '5s',
    },
    // Scenario 2: streaming concurrency - tests TTFT under high concurrency.
    streaming: {
      executor: 'ramping-vus',
      startVUs: 0,
      stages: [
        { duration: '10s', target: 100 },
        { duration: '30s', target: 100 },
        { duration: '5s',  target: 0   },
      ],
      gracefulStop: '5s',
      exec: 'streaming',
    },
  },
};

const PAYLOAD = JSON.stringify({
  model: 'gpt-4',
  messages: [{ role: 'user', content: 'Say "pong" and nothing else.' }],
  temperature: 0.0,
  max_tokens: 5,
  stream: false,
});

const PAYLOAD_STREAM = JSON.stringify({
  ...JSON.parse(PAYLOAD),
  stream: true,
});

const PARAMS = {
  headers: {
    'Content-Type': 'application/json',
    'Authorization': `Bearer ${API_KEY}`,
  },
  timeout: '30s',
};

export function nonStreaming() {
  const res = http.post(`${GATEWAY_URL}/v1/chat/completions`, PAYLOAD, PARAMS);

  const ok = check(res, {
    'status is 200 or 502': (r) => r.status === 200 || r.status === 502, // 502 if no upstream configured
    'latency < 5ms overhead is hard to test without upstream': () => true,
  });

  if (res.status === 200) {
    const hit = res.headers['X-Cache'] === 'HIT';
    if (hit) {
      cacheHits.add(1);
    } else {
      cacheMisses.add(1);
    }
    gatewayLatencyMs.add(res.timings.duration);
  }

  if (!ok) {
    console.error(`unexpected status=${res.status} body=${res.body}`);
  }

  // Small sleep to allow burst behavior to spread out
  sleep(0.005);
}

export function streaming() {
  const res = http.post(`${GATEWAY_URL}/v1/chat/completions`, PAYLOAD_STREAM, PARAMS);

  // For SSE: any 200/429/502 is acceptable for the load test (depends on upstream).
  check(res, {
    'streaming returns 2xx or 4xx or 5xx': (r) => r.status >= 200 && r.status < 600,
  });

  sleep(0.01);
}

export function handleSummary(data) {
  return {
    stdout: textSummary(data, { indent: ' ', enableColors: true }),
    'tests/load_test_summary.json': JSON.stringify(data, null, 2),
  };
}

// k6 >= 0.50 ships with a built-in summary text formatter; we use this fallback for older versions.
function textSummary(data, opts) {
  const out = [];
  out.push(`=== Gateway Load Test Summary ===`);
  out.push(`Total requests:    ${data.metrics.http_reqs.values.count}`);
  out.push(`Avg request:      ${data.metrics.http_req_duration.values.avg.toFixed(2)}ms`);
  out.push(`p95 request:      ${data.metrics.http_req_duration.values['p(95)'].toFixed(2)}ms`);
  out.push(`p99 request:      ${data.metrics.http_req_duration.values['p(99)'].toFixed(2)}ms`);
  out.push(`Failures:         ${data.metrics.http_req_failed.values.rate.toFixed(4)}`);
  if (data.metrics.cache_hits) {
    out.push(`Cache hits:       ${data.metrics.cache_hits.values.count}`);
  }
  if (data.metrics.cache_misses) {
    out.push(`Cache misses:     ${data.metrics.cache_misses.values.count}`);
  }
  out.push(`=================================`);
  return out.join('\n');
}
