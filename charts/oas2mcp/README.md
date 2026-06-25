# oas2mcp

![Version: 0.1.0](https://img.shields.io/badge/Version-0.1.0-informational?style=flat-square) ![Type: application](https://img.shields.io/badge/Type-application-informational?style=flat-square) ![AppVersion: 0.1.0](https://img.shields.io/badge/AppVersion-0.1.0-informational?style=flat-square)

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
| oas2mcp | object | `{"affinity":{},"baseUrl":null,"extraEnv":[],"extraVolumeMounts":[],"extraVolumes":[],"image":{"pullPolicy":"IfNotPresent","repository":"ghcr.io/therealm-tech/oas2mcp","tag":null},"imagePullSecrets":[],"ingress":{"annotations":{},"className":null,"enabled":false,"host":"oas2mcp.example.com","tlsSecret":null},"logFilter":"info","nodeSelector":{},"openapi":{"inline":null,"url":null},"podAnnotations":{},"podLabels":{},"podSecurityContext":{"fsGroup":1000,"runAsGroup":1000,"runAsNonRoot":true,"runAsUser":1000,"seccompProfile":{"type":"RuntimeDefault"}},"replicaCount":1,"resources":{"limits":{"ephemeral-storage":"256Mi","memory":"128Mi"},"requests":{"cpu":"50m","ephemeral-storage":"64Mi","memory":"64Mi"}},"securityContext":{"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"readOnlyRootFilesystem":true},"service":{"port":8000,"type":"ClusterIP"},"serviceAccount":{"annotations":{},"create":true,"name":null},"tolerations":[],"transport":"streamable-http","upstream":{"existingSecret":null,"forwardHeaders":[],"headers":[]}}` | oas2mcp server: deploys the OpenAPI→MCP proxy as an HTTP MCP endpoint. |
| oas2mcp.affinity | object | `{}` | Affinity rules for pod scheduling. |
| oas2mcp.baseUrl | string | `nil` | Upstream API base URL the tool calls are proxied to. Unset → taken from the document `servers`. |
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
| oas2mcp.nodeSelector | object | `{}` | Node selector for pod scheduling. |
| oas2mcp.openapi.inline | string | `nil` | Inline OpenAPI document (JSON or YAML). Stored in a ConfigMap and mounted; takes precedence over `url`. |
| oas2mcp.openapi.url | string | `nil` | URL of the OpenAPI document fetched at startup. Mutually exclusive with `inline`. |
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
| oas2mcp.securityContext | object | `{"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"readOnlyRootFilesystem":true}` | Container-level security context (restricted Pod Security Standard). |
| oas2mcp.securityContext.allowPrivilegeEscalation | bool | `false` | Disallow privilege escalation. |
| oas2mcp.securityContext.capabilities.drop | list | `["ALL"]` | Linux capabilities dropped from the container. |
| oas2mcp.securityContext.readOnlyRootFilesystem | bool | `true` | Mount the root filesystem read-only. |
| oas2mcp.service.port | int | `8000` | Service port (also the container listen port). |
| oas2mcp.service.type | string | `"ClusterIP"` | Service type. |
| oas2mcp.serviceAccount.annotations | object | `{}` | Annotations added to the ServiceAccount. |
| oas2mcp.serviceAccount.create | bool | `true` | Create a dedicated ServiceAccount for the server. |
| oas2mcp.serviceAccount.name | string | `nil` | ServiceAccount name. Unset → generated from the release name. |
| oas2mcp.tolerations | list | `[]` | Tolerations for pod scheduling. |
| oas2mcp.transport | string | `"streamable-http"` | MCP transport to expose. One of `streamable-http`, `sse`. (`stdio` makes no sense for a long-running Deployment.) |
| oas2mcp.upstream.existingSecret | string | `nil` | Name of a pre-existing Secret holding key `UPSTREAM_HEADERS`. Takes precedence over `headers`. |
| oas2mcp.upstream.forwardHeaders | list | `[]` | Names of incoming MCP-client request headers forwarded verbatim to the upstream API (e.g. `Authorization`). Per-call, not static; requires `transport: streamable-http`. |
| oas2mcp.upstream.headers | list | `[]` | Static `Name: Value` headers added to every upstream request (e.g. `Authorization: Bearer ...`). Stored in a Secret. |

----------------------------------------------
Autogenerated from chart metadata using [helm-docs v1.14.2](https://github.com/norwoodj/helm-docs/releases/v1.14.2)
