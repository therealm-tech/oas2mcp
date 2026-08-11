# oas2mcp

![Version: 0.6.0](https://img.shields.io/badge/Version-0.6.0-informational?style=flat-square) ![Type: application](https://img.shields.io/badge/Type-application-informational?style=flat-square) ![AppVersion: 0.4.0](https://img.shields.io/badge/AppVersion-0.4.0-informational?style=flat-square)

Expose an OpenAPI document as a Model Context Protocol (MCP) server over the Streamable HTTP or legacy SSE transport.

**Homepage:** <https://github.com/therealm-tech/oas2mcp>

## Maintainers

| Name | Email | Url |
| ---- | ------ | --- |
| Guillaume Leroy | <gleroy@therealm.tech> |  |

## Source Code

* <https://github.com/therealm-tech/oas2mcp>

## Values

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| oas2mcp | object | `{"affinity":{},"auth":{"jwks":{"file":null,"url":null},"roleClaim":null,"roleMapper":[],"traceClaims":["sub"]},"baseUrl":null,"caCerts":{"existing":{"key":"ca-certificates.crt","kind":"ConfigMap","name":null},"inline":null},"extraEnv":[],"extraVolumeMounts":[],"extraVolumes":[],"image":{"pullPolicy":"IfNotPresent","repository":"ghcr.io/therealm-tech/oas2mcp","tag":null},"imagePullSecrets":[],"ingress":{"annotations":{},"className":null,"enabled":false,"host":"oas2mcp.example.com","tlsSecret":null},"logFilter":"info","metrics":{"otlp":{"endpoint":null},"prometheus":{"enabled":false,"port":9090,"serviceMonitor":{"enabled":false,"interval":null,"labels":{},"metricRelabelings":[],"relabelings":[],"scrapeTimeout":null}},"serviceName":null},"nodeSelector":{},"openapi":{"existingSecret":{"key":"OPENAPI_HEADERS","name":null},"headers":[],"inline":null,"oauth":{"audience":null,"clientId":null,"clientSecret":null,"existingSecret":{"key":"OPENAPI_OAUTH_CLIENT_SECRET","name":null},"privateKey":{"assertionAudience":null,"assertionLifetime":null,"existingSecret":{"key":"client-key.pem","name":null},"inline":null,"keyId":null,"signingAlg":null},"scopes":[],"tokenUrl":null},"reloadEvery":null,"url":null},"operations":{"exclude":[],"excludeRegex":[],"excludeTags":[],"include":[],"includeRegex":[],"includeTags":[]},"podAnnotations":{},"podLabels":{},"podSecurityContext":{"fsGroup":1000,"runAsGroup":1000,"runAsNonRoot":true,"runAsUser":1000,"seccompProfile":{"type":"RuntimeDefault"}},"replicaCount":1,"resources":{"limits":{"ephemeral-storage":"256Mi","memory":"128Mi"},"requests":{"cpu":"50m","ephemeral-storage":"64Mi","memory":"64Mi"}},"revisionHistoryLimit":3,"securityContext":{"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"readOnlyRootFilesystem":true},"service":{"port":8000,"type":"ClusterIP"},"serviceAccount":{"annotations":{},"create":true,"name":null},"streamResponses":true,"tolerations":[],"transport":"streamable-http","upstream":{"existingSecret":{"key":"UPSTREAM_HEADERS","name":null},"forwardHeaders":[],"headers":[],"oauth":{"assertion":null,"audience":null,"clientId":null,"clientSecret":null,"existingSecret":{"key":"UPSTREAM_OAUTH_CLIENT_SECRET","name":null},"grant":null,"issuer":null,"privateKey":{"assertionAudience":null,"assertionLifetime":null,"existingSecret":{"key":"upstream-client-key.pem","name":null},"inline":null,"keyId":null,"signingAlg":null},"scopes":[],"subject":null,"subjectClaim":null,"tokenUrl":null}}}` | oas2mcp server: deploys the OpenAPI→MCP proxy as an HTTP MCP endpoint. |
| oas2mcp.affinity | object | `{}` | Affinity rules for pod scheduling. |
| oas2mcp.auth | object | `{"jwks":{"file":null,"url":null},"roleClaim":null,"roleMapper":[],"traceClaims":["sub"]}` | Restrict the tool set per caller by matching the roles in their verified JWT against tool-name regexes. Requires `transport: streamable-http` (the only transport exposing the client's JWT). A caller with no valid token, or whose roles match nothing, sees and can call no tools. |
| oas2mcp.auth.jwks.file | string | `nil` | Path to a JWKS document on disk (mount it via `extraVolumes`/`extraVolumeMounts`). Mutually exclusive with `url`. Maps to `OAUTH_JWKS_FILE`. |
| oas2mcp.auth.jwks.url | string | `nil` | URL of a JWKS document, fetched at startup, whose keys verify incoming JWT signatures. Mutually exclusive with `file`. One of the two is required when `roleMapper` is set. Maps to `OAUTH_JWKS_URL`. |
| oas2mcp.auth.roleClaim | string | `nil` | JWT claim listing the caller's roles (an array or a whitespace-separated string). Unset → the binary default (`roles`). Maps to `OAUTH_ROLE_CLAIM`. |
| oas2mcp.auth.roleMapper | list | `[]` | Role→tool mappings as `role:tool_name_regex` (e.g. `admin:.*`, `reader:^get`). A tool is visible if any of the caller's roles maps to a regex matching the tool name. Set → enables JWT verification, which requires `jwks.url` or `jwks.file`. Maps to `OAUTH_ROLE_MAPPER`. |
| oas2mcp.auth.traceClaims | list | `["sub"]` | JWT claim names copied into the per-call tracing log as a `jwt.claims` field (e.g. `sub`, `email`, `tenant_id`), for observability. Logged only, never a metric label. Needs `roleMapper` (claims come from the verified JWT). Defaults to `[sub]`; set `[]` to disable. Maps to `TRACE_CLAIMS`. |
| oas2mcp.baseUrl | string | `nil` | Upstream API base URL the tool calls are proxied to. Unset → taken from the document `servers`. |
| oas2mcp.caCerts | object | `{"existing":{"key":"ca-certificates.crt","kind":"ConfigMap","name":null},"inline":null}` | Extra CA certificates to trust for every outbound TLS connection (upstream API, document fetch, OAuth token endpoint, JWKS). Added on top of the built-in public roots — supply only your private/corporate CA. Mounted as a file and pointed at via `CA_CERT_FILE`. |
| oas2mcp.caCerts.existing | object | `{"key":"ca-certificates.crt","kind":"ConfigMap","name":null}` | Mount the PEM bundle from a resource you already manage, instead of `inline`. Takes precedence over `inline` when `name` is set. |
| oas2mcp.caCerts.existing.key | string | `"ca-certificates.crt"` | Key in the resource holding the PEM bundle (also the mounted file name). |
| oas2mcp.caCerts.existing.kind | string | `"ConfigMap"` | Kind of the resource holding the bundle: `ConfigMap` (the natural home for public CA certs) or `Secret`. |
| oas2mcp.caCerts.existing.name | string | `nil` | Name of the resource. Set → mounts the bundle from it instead of generating a Secret from `inline`. |
| oas2mcp.caCerts.inline | string | `nil` | Inline PEM bundle of one or more CA certificates. Stored in a Secret and mounted. Ignored when `existing.name` is set. |
| oas2mcp.extraEnv | list | `[]` | Extra environment variables injected into the container. |
| oas2mcp.extraVolumeMounts | list | `[]` | Extra volume mounts added to the container. |
| oas2mcp.extraVolumes | list | `[]` | Extra volumes added to the pod. |
| oas2mcp.image.pullPolicy | string | `"IfNotPresent"` | Image pull policy. |
| oas2mcp.image.repository | string | `"ghcr.io/therealm-tech/oas2mcp"` | Container image repository. |
| oas2mcp.image.tag | string | `nil` | Image tag. Unset → the chart `appVersion`. |
| oas2mcp.imagePullSecrets | list | `[]` | Image pull secrets for private registries. |
| oas2mcp.ingress.annotations | object | `{}` | Ingress annotations. |
| oas2mcp.ingress.className | string | `nil` | Ingress class name. |
| oas2mcp.ingress.enabled | bool | `false` | Create an Ingress for the HTTP transport. |
| oas2mcp.ingress.host | string | `"oas2mcp.example.com"` | Host the Ingress routes from. |
| oas2mcp.ingress.tlsSecret | string | `nil` | Name of a TLS Secret for the host. Unset → no TLS block. |
| oas2mcp.logFilter | string | `"info"` | `tracing` filter directive, mapped to `RUST_LOG`. |
| oas2mcp.metrics | object | `{"otlp":{"endpoint":null},"prometheus":{"enabled":false,"port":9090,"serviceMonitor":{"enabled":false,"interval":null,"labels":{},"metricRelabelings":[],"relabelings":[],"scrapeTimeout":null}},"serviceName":null}` | Tool-call metrics (OpenTelemetry). Export over OTLP and/or a Prometheus `/metrics` endpoint; both are independent. |
| oas2mcp.metrics.otlp.endpoint | string | `nil` | Base OTLP endpoint to push metrics to over HTTP (e.g. `http://otel-collector:4318`); `/v1/metrics` is appended. Unset → no OTLP export. Maps to `OTEL_EXPORTER_OTLP_ENDPOINT`. |
| oas2mcp.metrics.prometheus.enabled | bool | `false` | Serve a Prometheus `/metrics` endpoint on a dedicated port and Service port. Required by the ServiceMonitor below. |
| oas2mcp.metrics.prometheus.port | int | `9090` | Port the Prometheus `/metrics` endpoint listens on (container and Service). |
| oas2mcp.metrics.prometheus.serviceMonitor.enabled | bool | `false` | Create a Prometheus Operator ServiceMonitor scraping `/metrics`. Requires `prometheus.enabled` and the `monitoring.coreos.com` CRD. |
| oas2mcp.metrics.prometheus.serviceMonitor.interval | string | `nil` | Scrape interval (e.g. `30s`). Unset → the Prometheus default. |
| oas2mcp.metrics.prometheus.serviceMonitor.labels | object | `{}` | Extra labels on the ServiceMonitor so your Prometheus instance selects it (e.g. `release: kube-prometheus-stack`). |
| oas2mcp.metrics.prometheus.serviceMonitor.metricRelabelings | list | `[]` | Prometheus `metricRelabelings` applied to scraped samples before ingestion. |
| oas2mcp.metrics.prometheus.serviceMonitor.relabelings | list | `[]` | Prometheus `relabelings` applied to scraped targets before ingestion. |
| oas2mcp.metrics.prometheus.serviceMonitor.scrapeTimeout | string | `nil` | Per-scrape timeout (e.g. `10s`). Unset → the Prometheus default. |
| oas2mcp.metrics.serviceName | string | `nil` | `service.name` reported on exported metrics. Unset → the binary default (`oas2mcp`). Maps to `OTEL_SERVICE_NAME`. |
| oas2mcp.nodeSelector | object | `{}` | Node selector for pod scheduling. |
| oas2mcp.openapi.existingSecret.key | string | `"OPENAPI_HEADERS"` | Key in that Secret holding the newline-separated `Name: Value` headers. |
| oas2mcp.openapi.existingSecret.name | string | `nil` | Name of a pre-existing Secret holding the document-fetch headers. Set → takes precedence over `headers`. |
| oas2mcp.openapi.headers | list | `[]` | `Name: Value` headers sent when fetching `url` (e.g. for a private document). Stored in a Secret. Ignored with `inline`. |
| oas2mcp.openapi.inline | string | `nil` | Inline OpenAPI document (JSON or YAML). Stored in a ConfigMap and mounted; takes precedence over `url`. |
| oas2mcp.openapi.oauth.audience | string | `nil` | OAuth2 `audience` parameter, when the provider requires it (e.g. Auth0). |
| oas2mcp.openapi.oauth.clientId | string | `nil` | OAuth2 client ID. |
| oas2mcp.openapi.oauth.clientSecret | string | `nil` | OAuth2 client secret, sent over HTTP Basic. Stored in a Secret. Ignored when `existingSecret.name` is set. Mutually exclusive with `privateKey`. |
| oas2mcp.openapi.oauth.existingSecret.key | string | `"OPENAPI_OAUTH_CLIENT_SECRET"` | Key in that Secret holding the client secret. |
| oas2mcp.openapi.oauth.existingSecret.name | string | `nil` | Name of a pre-existing Secret holding the client secret. Set → takes precedence over `clientSecret`. |
| oas2mcp.openapi.oauth.privateKey | object | `{"assertionAudience":null,"assertionLifetime":null,"existingSecret":{"key":"client-key.pem","name":null},"inline":null,"keyId":null,"signingAlg":null}` | Authenticate the client with a JWT assertion it signs per request (`private_key_jwt`, RFC 7523 §2.2) instead of a shared secret — for providers that will not issue one. Register the **public** half of the key with the provider. Mutually exclusive with `clientSecret`/`existingSecret`; setting both fails the render. |
| oas2mcp.openapi.oauth.privateKey.assertionAudience | string | `nil` | `aud` claim of the client assertion. Unset → the token endpoint URL, which is what most providers expect; set it when yours wants its issuer identifier instead. Maps to `OPENAPI_OAUTH_ASSERTION_AUDIENCE`. |
| oas2mcp.openapi.oauth.privateKey.assertionLifetime | string | `nil` | How long a client assertion stays valid (e.g. `30s`, `2m`). Unset → the binary default (`60s`). Maps to `OPENAPI_OAUTH_ASSERTION_LIFETIME`. |
| oas2mcp.openapi.oauth.privateKey.existingSecret.key | string | `"client-key.pem"` | Key in that Secret holding the PEM private key. Doubles as the mounted file name. |
| oas2mcp.openapi.oauth.privateKey.existingSecret.name | string | `nil` | Name of a pre-existing Secret holding the PEM private key. Set → takes precedence over `inline`. |
| oas2mcp.openapi.oauth.privateKey.inline | string | `nil` | Inline PKCS#8 PEM private key. Stored in a Secret and mounted read-only at mode `0440` (root-owned, readable via `fsGroup` — `0400` would be unreadable by the non-root container). Ignored when `existingSecret.name` is set. |
| oas2mcp.openapi.oauth.privateKey.keyId | string | `nil` | `kid` header put on the client assertion, when the provider has several keys registered for this client. Maps to `OPENAPI_OAUTH_KEY_ID`. |
| oas2mcp.openapi.oauth.privateKey.signingAlg | string | `nil` | Assertion signature algorithm — `rs256`/`rs384`/`rs512`, `ps256`/`ps384`/`ps512`, `es256`/`es384`, `eddsa`. Must match the key type. Unset → the binary default (`rs256`). Maps to `OPENAPI_OAUTH_SIGNING_ALG`. |
| oas2mcp.openapi.oauth.scopes | list | `[]` | OAuth2 scopes requested for the token. |
| oas2mcp.openapi.oauth.tokenUrl | string | `nil` | OAuth2 `client_credentials` token endpoint. Set → the document fetch authenticates with an auto-refreshed bearer token (best paired with `reloadEvery`). Ignored with `inline`. |
| oas2mcp.openapi.reloadEvery | string | `nil` | Re-fetch `url` on this interval and rebuild the tool set (e.g. `30s`, `5m`, `1h`). Unset → load once at startup. Ignored with `inline`. |
| oas2mcp.openapi.url | string | `nil` | URL of the OpenAPI document fetched at startup. Mutually exclusive with `inline`. |
| oas2mcp.operations | object | `{"exclude":[],"excludeRegex":[],"excludeTags":[],"include":[],"includeRegex":[],"includeTags":[]}` | Filter which OpenAPI operations are exposed as tools. An operation is kept if it matches any `include`/`includeRegex`/`includeTags` allowlist (or all allowlists are empty), then dropped if it matches any `exclude`/`excludeRegex`/`excludeTags`. Use it to cut a large API down to a usable tool set. |
| oas2mcp.operations.exclude | list | `[]` | Glob denylist on the operation name; takes precedence over the allowlist. Maps to `EXCLUDE_OPERATIONS`. |
| oas2mcp.operations.excludeRegex | list | `[]` | Regex denylist on the operation name; takes precedence over the allowlist. Maps to `EXCLUDE_OPERATIONS_REGEX`. |
| oas2mcp.operations.excludeTags | list | `[]` | Drop operations carrying one of these OpenAPI tags (case-insensitive); takes precedence over the allowlist. Maps to `EXCLUDE_TAGS`. |
| oas2mcp.operations.include | list | `[]` | Glob allowlist on the operation name (operationId, or `<method>_<path>`); `*` and `?` supported. Maps to `INCLUDE_OPERATIONS`. |
| oas2mcp.operations.includeRegex | list | `[]` | Regex allowlist on the operation name (e.g. `^(get|post)ApiV4Projects`). Maps to `INCLUDE_OPERATIONS_REGEX`. |
| oas2mcp.operations.includeTags | list | `[]` | Only expose operations carrying one of these OpenAPI tags (case-insensitive). Maps to `INCLUDE_TAGS`. |
| oas2mcp.podAnnotations | object | `{}` | Annotations added to the pod. |
| oas2mcp.podLabels | object | `{}` | Extra labels added to the pod. |
| oas2mcp.podSecurityContext | object | `{"fsGroup":1000,"runAsGroup":1000,"runAsNonRoot":true,"runAsUser":1000,"seccompProfile":{"type":"RuntimeDefault"}}` | Pod-level security context (restricted Pod Security Standard). |
| oas2mcp.podSecurityContext.fsGroup | int | `1000` | Owning group for mounted volumes. |
| oas2mcp.podSecurityContext.runAsGroup | int | `1000` | GID to run every container as. |
| oas2mcp.podSecurityContext.runAsNonRoot | bool | `true` | Forbid running as root. |
| oas2mcp.podSecurityContext.runAsUser | int | `1000` | UID to run every container as. |
| oas2mcp.podSecurityContext.seccompProfile.type | string | `"RuntimeDefault"` | Seccomp profile applied to the pod. |
| oas2mcp.replicaCount | int | `1` | Number of server replicas. |
| oas2mcp.resources | object | `{"limits":{"ephemeral-storage":"256Mi","memory":"128Mi"},"requests":{"cpu":"50m","ephemeral-storage":"64Mi","memory":"64Mi"}}` | Compute resources. CPU has a request but no limit (compressible); memory and ephemeral storage are capped. |
| oas2mcp.resources.limits.ephemeral-storage | string | `"256Mi"` | Ephemeral storage limit. |
| oas2mcp.resources.limits.memory | string | `"128Mi"` | Memory limit. |
| oas2mcp.resources.requests.cpu | string | `"50m"` | CPU request. |
| oas2mcp.resources.requests.ephemeral-storage | string | `"64Mi"` | Ephemeral storage request. |
| oas2mcp.resources.requests.memory | string | `"64Mi"` | Memory request. |
| oas2mcp.revisionHistoryLimit | int | `3` | Number of old ReplicaSets the Deployment keeps for rollback. |
| oas2mcp.securityContext | object | `{"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"readOnlyRootFilesystem":true}` | Container-level security context (restricted Pod Security Standard). |
| oas2mcp.securityContext.allowPrivilegeEscalation | bool | `false` | Disallow privilege escalation. |
| oas2mcp.securityContext.capabilities.drop | list | `["ALL"]` | Linux capabilities dropped from the container. |
| oas2mcp.securityContext.readOnlyRootFilesystem | bool | `true` | Mount the root filesystem read-only. |
| oas2mcp.service.port | int | `8000` | Service port (also the container listen port). |
| oas2mcp.service.type | string | `"ClusterIP"` | Service type. |
| oas2mcp.serviceAccount.annotations | object | `{}` | Annotations added to the ServiceAccount. |
| oas2mcp.serviceAccount.create | bool | `true` | Create a dedicated ServiceAccount for the server. |
| oas2mcp.serviceAccount.name | string | `nil` | ServiceAccount name. Unset → generated from the release name. |
| oas2mcp.streamResponses | bool | `true` | Reply on `streamable-http` with an SSE flow and stateful sessions. Set to `false` when the MCP server sits behind a proxy (e.g. Envoy AI Gateway) that can't parse the SSE priming event — that switches it to single `application/json` replies (stateless). Only affects `transport: streamable-http`. |
| oas2mcp.tolerations | list | `[]` | Tolerations for pod scheduling. |
| oas2mcp.transport | string | `"streamable-http"` | MCP transport to expose. One of `streamable-http`, `sse`. (`stdio` makes no sense for a long-running Deployment.) |
| oas2mcp.upstream.existingSecret.key | string | `"UPSTREAM_HEADERS"` | Key in that Secret holding the newline-separated `Name: Value` headers. |
| oas2mcp.upstream.existingSecret.name | string | `nil` | Name of a pre-existing Secret holding the upstream headers. Set → takes precedence over `headers`. |
| oas2mcp.upstream.forwardHeaders | list | `[]` | Names of incoming MCP-client request headers forwarded verbatim to the upstream API (e.g. `Authorization`). Per-call, not static; requires `transport: streamable-http`. |
| oas2mcp.upstream.headers | list | `[]` | Static `Name: Value` headers added to every upstream request (e.g. `Authorization: Bearer ...`). Stored in a Secret. |
| oas2mcp.upstream.oauth | object | `{"assertion":null,"audience":null,"clientId":null,"clientSecret":null,"existingSecret":{"key":"UPSTREAM_OAUTH_CLIENT_SECRET","name":null},"grant":null,"issuer":null,"privateKey":{"assertionAudience":null,"assertionLifetime":null,"existingSecret":{"key":"upstream-client-key.pem","name":null},"inline":null,"keyId":null,"signingAlg":null},"scopes":[],"subject":null,"subjectClaim":null,"tokenUrl":null}` | Obtain the upstream `Authorization: Bearer` from an OAuth2 `client_credentials` grant, auto-refreshed before expiry, instead of a static token in `headers`. Configured independently of `openapi.oauth` — the document and the API may sit behind different providers. When a token is configured it outranks a forwarded `Authorization` but not a static one in `headers`. |
| oas2mcp.upstream.oauth.assertion | string | `nil` | Who signs the `jwt-bearer` assertion: `self-signed` (the default, needs `privateKey`) or `caller` (relays the caller's own verified JWT, needs no key but requires their token's `aud` to be the authorization server). Maps to `UPSTREAM_OAUTH_ASSERTION`. |
| oas2mcp.upstream.oauth.audience | string | `nil` | OAuth2 `audience` parameter, when the provider requires it (e.g. Auth0). |
| oas2mcp.upstream.oauth.clientId | string | `nil` | OAuth2 client ID. |
| oas2mcp.upstream.oauth.clientSecret | string | `nil` | OAuth2 client secret, sent over HTTP Basic. Stored in a Secret. Ignored when `existingSecret.name` is set. Mutually exclusive with `privateKey`. |
| oas2mcp.upstream.oauth.existingSecret.key | string | `"UPSTREAM_OAUTH_CLIENT_SECRET"` | Key in that Secret holding the client secret. |
| oas2mcp.upstream.oauth.existingSecret.name | string | `nil` | Name of a pre-existing Secret holding the client secret. Set → takes precedence over `clientSecret`. |
| oas2mcp.upstream.oauth.grant | string | `nil` | Which grant obtains the token: `client-credentials` (the default) gets one token for the server itself, shared by every caller; `jwt-bearer` (RFC 7523 §2.1) obtains one **per caller**, so the upstream API sees who is really acting. Unset → the binary default. Maps to `UPSTREAM_OAUTH_GRANT`. |
| oas2mcp.upstream.oauth.issuer | string | `nil` | `iss` of the `jwt-bearer` assertion, identifying oas2mcp to the provider. Unset → `clientId`. Maps to `UPSTREAM_OAUTH_ISSUER`. |
| oas2mcp.upstream.oauth.privateKey | object | `{"assertionAudience":null,"assertionLifetime":null,"existingSecret":{"key":"upstream-client-key.pem","name":null},"inline":null,"keyId":null,"signingAlg":null}` | Authenticate with a JWT assertion signed per request (`private_key_jwt`, RFC 7523 §2.2) instead of a shared secret. Also signs the `jwt-bearer` grant assertion when `grant: jwt-bearer` with a `self-signed` assertion — one key, loaded once. Mutually exclusive with `clientSecret`/`existingSecret`; setting both fails the render. |
| oas2mcp.upstream.oauth.privateKey.assertionAudience | string | `nil` | `aud` claim of the client assertion. Unset → the token endpoint URL. Maps to `UPSTREAM_OAUTH_ASSERTION_AUDIENCE`. |
| oas2mcp.upstream.oauth.privateKey.assertionLifetime | string | `nil` | How long a client assertion stays valid (e.g. `30s`). Unset → the binary default (`60s`). Maps to `UPSTREAM_OAUTH_ASSERTION_LIFETIME`. |
| oas2mcp.upstream.oauth.privateKey.existingSecret.key | string | `"upstream-client-key.pem"` | Key in that Secret holding the PEM private key. Doubles as the mounted file name. |
| oas2mcp.upstream.oauth.privateKey.existingSecret.name | string | `nil` | Name of a pre-existing Secret holding the PEM private key. Set → takes precedence over `inline`. |
| oas2mcp.upstream.oauth.privateKey.inline | string | `nil` | Inline PKCS#8 PEM private key. Stored in a Secret and mounted read-only at mode `0440`. Ignored when `existingSecret.name` is set. |
| oas2mcp.upstream.oauth.privateKey.keyId | string | `nil` | `kid` header put on the client assertion. Maps to `UPSTREAM_OAUTH_KEY_ID`. |
| oas2mcp.upstream.oauth.privateKey.signingAlg | string | `nil` | Assertion signature algorithm — `rs256`/`rs384`/`rs512`, `ps256`/`ps384`/`ps512`, `es256`/`es384`, `eddsa`. Must match the key type. Unset → the binary default (`rs256`). Maps to `UPSTREAM_OAUTH_SIGNING_ALG`. |
| oas2mcp.upstream.oauth.scopes | list | `[]` | OAuth2 scopes requested for the upstream token. |
| oas2mcp.upstream.oauth.subject | string | `nil` | Fixed `sub` for the `jwt-bearer` assertion — a named service account, the same for every caller. Mutually exclusive with `subjectClaim`. Maps to `UPSTREAM_OAUTH_SUBJECT`. |
| oas2mcp.upstream.oauth.subjectClaim | string | `nil` | Claim of the **caller's** verified JWT whose value becomes the assertion's `sub` — i.e. per-caller delegation. Unset → the binary default (`sub`); many providers need `email` instead. Requires `auth.roleMapper` with a JWKS and `transport: streamable-http`; the server refuses to start otherwise. Maps to `UPSTREAM_OAUTH_SUBJECT_CLAIM`. |
| oas2mcp.upstream.oauth.tokenUrl | string | `nil` | OAuth2 `client_credentials` token endpoint for upstream calls. Set → enables the whole block; requires `clientId` plus one credential. |

----------------------------------------------
Autogenerated from chart metadata using [helm-docs v1.14.2](https://github.com/norwoodj/helm-docs/releases/v1.14.2)
