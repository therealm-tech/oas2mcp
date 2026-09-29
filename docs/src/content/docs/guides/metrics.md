---
title: Metrics
description: OpenTelemetry metrics for every tool call, over OTLP or a Prometheus endpoint.
sidebar:
  order: 6
---

Every tool call is counted and timed and exposed as OpenTelemetry metrics:

| Instrument | Type | Description |
|------------|------|-------------|
| `mcp.tool.calls` | counter | Number of tool calls. |
| `mcp.tool.call.duration` | histogram (seconds) | Duration of the proxied upstream request. |

Both carry the attributes `tool` (the tool/operation name) and `outcome` — and
nothing else, so metric cardinality stays bounded. `outcome` is one of:

| Value | Meaning |
| --- | --- |
| `success` | The upstream answered with a non-error status. |
| `error` | The upstream answered with a 4xx/5xx, or the request could not be built or sent. |
| `auth_error` | The upstream OAuth token could not be obtained, so **no** request was made. Points at the provider or the credential, not at the API. |
| `cancelled` | The client cancelled the call or disconnected before it completed; the upstream request in flight was aborted and no result was sent. |

To break activity down by caller, log the relevant JWT claims with
`--trace-claim` (see [Tracing the caller's JWT claims](/oas2mcp/guides/caller-auth/#tracing-the-callers-jwt-claims)) and aggregate them in your logging backend, rather
than turning a per-user identifier into a metric label.

Enable either exporter, both, or neither — they are independent:

```bash
# OTLP push to a collector + a Prometheus scrape endpoint, at once.
oas2mcp http \
  --openapi-file ./examples/petstore.yaml \
  --bind-addr 0.0.0.0:8000 \
  --otlp-endpoint http://otel-collector:4318 \
  --metrics-addr 0.0.0.0:9090
# Push: POST http://otel-collector:4318/v1/metrics  (HTTP/protobuf, every 30s)
# Pull: GET  http://0.0.0.0:9090/metrics            (Prometheus text format)
```

The Prometheus endpoint runs on its own HTTP server (the `--metrics-addr`
address), separate from the MCP transport, so it works under `stdio` too. OTLP
honours the standard `OTEL_EXPORTER_OTLP_*` environment variables.
