---
title: Deploying on Kubernetes
description: Deploy oas2mcp with its Helm chart.
sidebar:
  order: 8
---

A Helm chart is provided under [charts/oas2mcp](https://github.com/therealm-tech/oas2mcp/tree/main/charts/oas2mcp). It deploys the
server with the `http` transport, a restricted security context, and
resource requests/limits. The upstream auth headers are stored in a `Secret`.

```bash
helm install petstore charts/oas2mcp \
  --set oas2mcp.openapi.url=https://petstore3.swagger.io/api/v3/openapi.json \
  --set-string 'oas2mcp.upstream.headers[0]=Authorization: Bearer <token>'
```

The OpenAPI document can come from a URL (`oas2mcp.openapi.url`) or be supplied
inline (`oas2mcp.openapi.inline`), in which case it is mounted from a
`ConfigMap`. To reuse an existing `Secret` for the upstream headers, set
`oas2mcp.upstream.existingSecret` (key `UPSTREAM_HEADERS`). See the chart's
[README](https://github.com/therealm-tech/oas2mcp/blob/main/charts/oas2mcp/README.md) for every value.

To trust a private/corporate CA for outbound TLS, either drop the PEM bundle
into `oas2mcp.caCerts.inline` (stored in a `Secret`, mounted, and wired to
`CA_CERT_FILE` automatically), or mount it from a resource you already manage
via `oas2mcp.caCerts.existing` (`kind: ConfigMap` or `Secret` — a `ConfigMap`
is the natural home for public CA certs):

```bash
# inline PEM → generated Secret
helm install petstore charts/oas2mcp \
  --set oas2mcp.openapi.url=https://internal.example.com/openapi.json \
  --set-file oas2mcp.caCerts.inline=./corp-ca.pem

# or reference an existing ConfigMap
helm install petstore charts/oas2mcp \
  --set oas2mcp.openapi.url=https://internal.example.com/openapi.json \
  --set oas2mcp.caCerts.existing.kind=ConfigMap \
  --set oas2mcp.caCerts.existing.name=corp-ca
```
