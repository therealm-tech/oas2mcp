---
title: Reading the API contract
description: Expose the OpenAPI document as an MCP resource, cut down per caller.
sidebar:
  order: 5
---

`--openapi-resource` exposes the OpenAPI document as an MCP resource, so a
client can read the contract behind the tools instead of guessing it from the
input schemas: the server advertises the `resources` capability, and
`resources/list` returns one resource, `openapi://document`, which
`resources/read` serves as `application/json` text. It follows every reload.

```bash
oas2mcp http --openapi-url https://api.example.com/openapi.json --openapi-resource
```

The whole document would describe every operation, including those
`--include-regex`/`--exclude-regex`/`--tag`/`--exclude-tag` drop and those a
caller's roles hide, so each caller reads a copy cut down to the operations it
lists in `tools/list`:

- `paths` keeps those operations alone; a path left with none is dropped, and
  `webhooks` always are.
- `components` keeps what the remaining document references, transitively:
  `$ref`s, discriminator mappings, and the security schemes a `security`
  requirement names. `tags` keeps the ones a remaining operation carries.
- Everything else — `info`, `servers`, the top-level `security`,
  `externalDocs`, extensions — is served as written.

What still shows is therefore the document's metadata and every schema a
visible operation shares with a hidden one, descriptions included. Leave the
flag off when those must stay private. An anonymous caller reads the document
cut down to the public tools (all of them with `--inbound-anonymous-discovery`),
and a URI other than `openapi://document` is answered with a resource-not-found
error (`-32002`, or `-32602` from protocol `2026-07-28` on).
