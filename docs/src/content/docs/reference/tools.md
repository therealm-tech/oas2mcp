---
title: How operations map to tools
description: Tool names, annotations, results and output schemas derived from each operation.
sidebar:
  order: 2
---

Given this operation:

```yaml
paths:
  /pet/{petId}:
    get:
      operationId: getPetById
      parameters:
        - { name: petId, in: path, required: true, schema: { type: integer } }
```

`oas2mcp` advertises a `getPetById` tool whose input schema requires a `petId`
property. Calling it with `{ "petId": 1 }` issues `GET <base-url>/pet/1` and
returns the upstream response. A non-2xx upstream status is surfaced as an MCP
tool error.

The name goes through, in this order: the raw name (`operationId`, or a
`<method>_<path>` fallback) → the operation filters, which match that raw name →
the `--rename` rules → sanitisation to `[A-Za-z0-9_-]` → the `--max-name-len`
cap → deduplication against the tools already registered. See
[Renaming the exposed tools](/oas2mcp/guides/operations/#renaming-the-exposed-tools).

The operation's `summary`, folded onto one line, is the tool's `title`, the
display name MCP clients show to people. The tool's `description` is the
`summary` followed by the operation's `description`, so a client that does not
render the title, and the model, still read the summary.

## Tool annotations

Each tool is advertised with the MCP behaviour hints its HTTP method implies,
following the method semantics of RFC 9110, so a client can tell a read from a
write — to skip confirming a `GET`, say, or to warn before a `DELETE`:

| Method                    | `readOnlyHint` | `destructiveHint` | `idempotentHint` |
| ------------------------- | -------------- | ----------------- | ---------------- |
| `GET`, `HEAD`, `OPTIONS`, `TRACE` | `true` | `false`           | `true`           |
| `POST`                    | `false`        | unset             | `false`          |
| `PUT`                     | `false`        | `true`            | `true`           |
| `PATCH`                   | `false`        | `true`            | `false`          |
| `DELETE`                  | `false`        | `true`            | `true`           |

`openWorldHint` is `true` on every tool, since each one calls an external API. A
`POST` may create a resource or trigger anything at all, so its
`destructiveHint` is left out and clients apply the specification's default,
`true`. The hints are only as good as the document's use of HTTP: an API that
deletes on a `GET` gets a tool marked read-only. For such an API, pass
`--auto-tool-annotations=false` to advertise no annotations at all.

## The shape of a tool result

A result carries a text upstream response twice, in two fields with two
audiences (a binary one is covered [below](#binary-responses)):

| Field               | Content                                       | Read it if you are |
| ------------------- | --------------------------------------------- | ------------------ |
| `content`           | One text block, `HTTP <status>\n\n<body>`      | a human or a model |
| `structuredContent` | The response body parsed as JSON              | a program          |
| `isError`           | `true` when the upstream status is 4xx or 5xx | either             |

**A machine should read `structuredContent`.** It is the parsed body and nothing
else — no status line to strip, no string to split. It is absent when the body
is not JSON (an empty `204`, a `text/plain` payload, a gateway's HTML error
page), so treat it as optional and fall back to the text block. It is always a
JSON object: an object body is passed through verbatim, while an array or a
scalar body is wrapped as `{"result": <body>}`, because MCP protocol versions up
to `2025-11-25` type `structuredContent` as an object. The shape is the same
whichever version the client negotiates.

The text block always exists and always keeps its status prefix: that is what
lets a model tell a `404` from a `200`, where `isError` only says yes or no. A
failing call still gets a `structuredContent` when the upstream error body is
itself JSON.

Without `--tool-output-schema`, no tool declares an `outputSchema`, and
`structuredContent` is informative rather than contractual.

## Output schemas

With `--tool-output-schema`, a tool declares an `outputSchema` taken from its
operation's success response, with local `$ref`s inlined as in the input
schema. It is declared only when every success response the operation
documents (each `2xx` code and the `2XX` range) carries the same
`application/json` or `…+json` body schema, and that schema is `type: object` —
the only root MCP accepts. An operation answering an array, a scalar, no body,
or a `204` beside a `200`, declares none. `default` is not read: it
conventionally describes errors.

The schema is a contract, which is why it is off by default. MCP requires
`structuredContent` to conform to the declared schema, and clients may validate
it: an upstream that strays from its own OpenAPI document — an undocumented
field under `additionalProperties: false`, a missing required one, a `null`
where the document says `string` — then fails the call on the client side
instead of returning what the upstream sent. The schema is advertised in the
dialect the document is written in, so a 3.0 schema (`nullable`) read as JSON
Schema 2020-12 can reject values the upstream considers valid. Turn it on for
an upstream you trust to honour its document.

A tool with an output schema returns an upstream error without
`structuredContent`: the error body would not conform to the success schema. It
stays in the text block.

### Binary responses

The response `Content-Type` decides how the body is read. Text — `text/*`, JSON
and `+json`, XML and `+xml`, YAML, `application/x-www-form-urlencoded` — gets
the shape above. Any other body is passed on base64-encoded, after a text block
such as `HTTP 200 OK, image/png, 12345 bytes` that a client ignoring binary
content still shows:

| `Content-Type`                                 | Second content block                                      |
| ---------------------------------------------- | --------------------------------------------------------- |
| `image/*`                                      | an `image` block                                          |
| `audio/*`                                      | an `audio` block                                          |
| anything else (`application/pdf`, `…/zip`, …)  | an embedded resource, its `blob` named by the request URL |

A body without a `Content-Type` is text when it is valid UTF-8, and an
`application/octet-stream` resource otherwise. A binary result never carries
`structuredContent`; `isError` follows the status as for text.
