---
title: Serving over HTTP
description: Host header validation and access logs of the HTTP transports.
sidebar:
  order: 7
---

## Host header validation

The `http` transport checks the `Host` header of every request and
answers `403 Forbidden: Host header is not allowed` when it is not on the
allowlist. This stops DNS rebinding: a page the victim visits re-resolves its
own domain to a loopback address and then talks to the MCP server listening
there, from inside the browser. The rebound request still carries the
attacker's hostname, so checking `Host` breaks the attack.

The default follows `--bind-addr`, because that is what decides whether the
attack applies at all:

| Bind address | Default allowlist |
| --- | --- |
| loopback (`127.0.0.1`, `::1` — the default) | `localhost`, `127.0.0.1`, `::1` |
| anything routable (`0.0.0.0`, a pod IP, …) | any `Host` |

A server bound to a routable address is deliberately reachable under a name it
cannot guess — a Kubernetes `Service`, an Ingress host, a load balancer — so
rejecting those by default would only mean rejecting every real request.

Name the hosts explicitly to check them there too:

```bash
oas2mcp http --openapi-file petstore.yaml \
  --bind-addr 0.0.0.0:8000 \
  --allowed-host mcp.example.com \
  --allowed-host petstore-oas2mcp.default.svc.cluster.local
```

An entry without a port matches any port; `--allowed-host '*'` accepts anything
and turns the check off. Setting the flag replaces the default rather than
adding to it, so include the loopback names yourself if you still want them.

## Access logs

Both HTTP transports log every response. A rejection is logged at `warn` with
the reason the server sent back, plus the headers that decide whether a request
is accepted:

```text
WARN oas2mcp::transport::access_log: rejected HTTP request method=POST path="/mcp"
  status=400 elapsed_ms=0 reason="Bad Request: Unsupported MCP-Protocol-Version: 1999-01-01"
  host=Some("localhost:8000") protocol_version=Some("1999-01-01")
  accept=Some("application/json, text/event-stream") content_type=Some("application/json")
  session=false
```

This matters because the MCP layer answers most malformed requests itself, and
states the reason only in the response body — a missing `Mcp-Session-Id`, an
unsupported `MCP-Protocol-Version`, an `Accept` header without both
`application/json` and `text/event-stream`. If a client is stuck on a `400`, that
log line is where the answer is. `5xx` responses are logged at `error` the same
way.

Successful responses are logged at `debug`, so a full access log needs
`--log-filter oas2mcp=debug` (which also turns on the rest of the debug output).

Two fields are deliberately withheld: the `Authorization` header is never
logged, and `Mcp-Session-Id` is reported as `session=true`/`false` rather than by
value — the session id is a capability, and whoever holds it can speak into that
session.
