---
title: Limitations
description: What oas2mcp does not support.
sidebar:
  order: 3
---

- OpenAPI **3.0.x** and **3.1.x** are supported. A newer 3.x revision is read
  on a best-effort basis (as 3.1) with a warning; Swagger 2.0 is rejected —
  convert it first, e.g. with `swagger2openapi`.
- Only **local** `$ref`s are resolved. A reference into another file or a URL
  is not fetched; it degrades to a bare `object` in the tool's input schema.
- Request bodies are always sent as JSON. An operation whose `content` offers
  no `application/json` (or `…+json`) media type still gets a `body` argument,
  built from the first media type it does declare.
- OpenAPI 3.1 `webhooks` are not exposed as tools: a webhook is a callback the
  upstream API sends *to* the server, not an operation the server can call.
- Cookie parameters are ignored.
- An upstream response body is read whole into memory, with no size limit; a
  binary body reaches the client base64-encoded in full.
- Templated `servers` URLs (`https://{region}.example.com`) are not expanded;
  pass `--base-url` for those.
- The legacy `sse` transport is kept for compatibility but is deprecated by the
  MCP specification; prefer `http` for new remote deployments.
