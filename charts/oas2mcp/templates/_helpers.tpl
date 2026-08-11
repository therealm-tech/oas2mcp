{{/* Chart name, overridable by the release name. */}}
{{- define "oas2mcp.name" -}}
{{- default .Chart.Name .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* Fully qualified app name. */}}
{{- define "oas2mcp.fullname" -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* Chart label (name-version). */}}
{{- define "oas2mcp.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* Selector labels. */}}
{{- define "oas2mcp.selectorLabels" -}}
app.kubernetes.io/name: {{ include "oas2mcp.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/* Common labels. */}}
{{- define "oas2mcp.labels" -}}
helm.sh/chart: {{ include "oas2mcp.chart" . }}
{{ include "oas2mcp.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/* Fully qualified container image reference. */}}
{{- define "oas2mcp.image" -}}
{{- $tag := .Values.oas2mcp.image.tag | default .Chart.AppVersion -}}
{{- printf "%s:%s" .Values.oas2mcp.image.repository $tag -}}
{{- end -}}

{{/* ServiceAccount name to use. */}}
{{- define "oas2mcp.serviceAccountName" -}}
{{- if .Values.oas2mcp.serviceAccount.create -}}
{{- default (include "oas2mcp.fullname" .) .Values.oas2mcp.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.oas2mcp.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/* Name of the Secret holding the upstream headers (existing or generated). */}}
{{- define "oas2mcp.upstreamSecretName" -}}
{{- if .Values.oas2mcp.upstream.existingSecret.name -}}
{{- .Values.oas2mcp.upstream.existingSecret.name -}}
{{- else -}}
{{- printf "%s-upstream" (include "oas2mcp.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/* Key in the upstream-headers Secret: the existing Secret's key, else the generated default. */}}
{{- define "oas2mcp.upstreamSecretKey" -}}
{{- if .Values.oas2mcp.upstream.existingSecret.name -}}
{{- .Values.oas2mcp.upstream.existingSecret.key -}}
{{- else -}}
UPSTREAM_HEADERS
{{- end -}}
{{- end -}}

{{/* Name of the Secret holding the OpenAPI document-fetch headers (existing or generated). */}}
{{- define "oas2mcp.openapiSecretName" -}}
{{- if .Values.oas2mcp.openapi.existingSecret.name -}}
{{- .Values.oas2mcp.openapi.existingSecret.name -}}
{{- else -}}
{{- printf "%s-openapi-headers" (include "oas2mcp.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/* Key in the document-fetch-headers Secret: the existing Secret's key, else the generated default. */}}
{{- define "oas2mcp.openapiSecretKey" -}}
{{- if .Values.oas2mcp.openapi.existingSecret.name -}}
{{- .Values.oas2mcp.openapi.existingSecret.key -}}
{{- else -}}
OPENAPI_HEADERS
{{- end -}}
{{- end -}}

{{/* Name of the Secret holding the OAuth client secret (existing or generated). */}}
{{- define "oas2mcp.oauthSecretName" -}}
{{- if .Values.oas2mcp.openapi.oauth.existingSecret.name -}}
{{- .Values.oas2mcp.openapi.oauth.existingSecret.name -}}
{{- else -}}
{{- printf "%s-openapi-oauth" (include "oas2mcp.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/* Key in the OAuth client-secret Secret: the existing Secret's key, else the generated default. */}}
{{- define "oas2mcp.oauthSecretKey" -}}
{{- if .Values.oas2mcp.openapi.oauth.existingSecret.name -}}
{{- .Values.oas2mcp.openapi.oauth.existingSecret.key -}}
{{- else -}}
OPENAPI_OAUTH_CLIENT_SECRET
{{- end -}}
{{- end -}}

{{/*
  The upstream grant repeats the document grant's shape below. Kept as two
  explicit copies rather than one dict-parameterised helper: there are exactly
  two grants and never a third, and passing maps through `include` reads worse
  than the duplication saves.
*/}}

{{/* Name of the Secret holding the upstream OAuth client secret (existing or generated). */}}
{{- define "oas2mcp.upstreamOauthSecretName" -}}
{{- if .Values.oas2mcp.upstream.oauth.existingSecret.name -}}
{{- .Values.oas2mcp.upstream.oauth.existingSecret.name -}}
{{- else -}}
{{- printf "%s-upstream-oauth" (include "oas2mcp.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/* Key in the upstream OAuth client-secret Secret. */}}
{{- define "oas2mcp.upstreamOauthSecretKey" -}}
{{- if .Values.oas2mcp.upstream.oauth.existingSecret.name -}}
{{- .Values.oas2mcp.upstream.oauth.existingSecret.key -}}
{{- else -}}
UPSTREAM_OAUTH_CLIENT_SECRET
{{- end -}}
{{- end -}}

{{/* Whether the upstream client authenticates with a signed assertion. */}}
{{- define "oas2mcp.upstreamOauthPrivateKeyEnabled" -}}
{{- if or .Values.oas2mcp.upstream.oauth.privateKey.inline .Values.oas2mcp.upstream.oauth.privateKey.existingSecret.name -}}
true
{{- end -}}
{{- end -}}

{{/* Refuse two upstream client credentials at once, matching the binary. */}}
{{- define "oas2mcp.upstreamOauthClientAuthValidate" -}}
{{- if include "oas2mcp.upstreamOauthPrivateKeyEnabled" . -}}
{{- if or .Values.oas2mcp.upstream.oauth.clientSecret .Values.oas2mcp.upstream.oauth.existingSecret.name -}}
{{- fail "oas2mcp.upstream.oauth.privateKey and oas2mcp.upstream.oauth.clientSecret/existingSecret are mutually exclusive; pick one way to authenticate the client" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* Whether a Secret must be generated to hold the inline upstream signing key. */}}
{{- define "oas2mcp.upstreamOauthPrivateKeyGenerateSecret" -}}
{{- if and .Values.oas2mcp.upstream.oauth.privateKey.inline (not .Values.oas2mcp.upstream.oauth.privateKey.existingSecret.name) -}}
true
{{- end -}}
{{- end -}}

{{/* Name of the Secret holding the upstream OAuth signing key (existing or generated). */}}
{{- define "oas2mcp.upstreamOauthPrivateKeySecretName" -}}
{{- if .Values.oas2mcp.upstream.oauth.privateKey.existingSecret.name -}}
{{- .Values.oas2mcp.upstream.oauth.privateKey.existingSecret.name -}}
{{- else -}}
{{- printf "%s-upstream-oauth-key" (include "oas2mcp.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/* Key in the upstream signing-key Secret (also the mounted file name). */}}
{{- define "oas2mcp.upstreamOauthPrivateKeyKey" -}}
{{- .Values.oas2mcp.upstream.oauth.privateKey.existingSecret.key -}}
{{- end -}}

{{/* Whether the client authenticates with a signed assertion (inline key or an existing Secret). */}}
{{- define "oas2mcp.oauthPrivateKeyEnabled" -}}
{{- if or .Values.oas2mcp.openapi.oauth.privateKey.inline .Values.oas2mcp.openapi.oauth.privateKey.existingSecret.name -}}
true
{{- end -}}
{{- end -}}

{{/* Refuse two client credentials at once: the binary rejects the pair, so catch it at render time rather than at pod start. */}}
{{- define "oas2mcp.oauthClientAuthValidate" -}}
{{- if include "oas2mcp.oauthPrivateKeyEnabled" . -}}
{{- if or .Values.oas2mcp.openapi.oauth.clientSecret .Values.oas2mcp.openapi.oauth.existingSecret.name -}}
{{- fail "oas2mcp.openapi.oauth.privateKey and oas2mcp.openapi.oauth.clientSecret/existingSecret are mutually exclusive; pick one way to authenticate the client" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* Whether a Secret must be generated to hold the inline signing key (no existing Secret chosen). */}}
{{- define "oas2mcp.oauthPrivateKeyGenerateSecret" -}}
{{- if and .Values.oas2mcp.openapi.oauth.privateKey.inline (not .Values.oas2mcp.openapi.oauth.privateKey.existingSecret.name) -}}
true
{{- end -}}
{{- end -}}

{{/* Name of the Secret holding the OAuth client signing key (existing or generated). */}}
{{- define "oas2mcp.oauthPrivateKeySecretName" -}}
{{- if .Values.oas2mcp.openapi.oauth.privateKey.existingSecret.name -}}
{{- .Values.oas2mcp.openapi.oauth.privateKey.existingSecret.name -}}
{{- else -}}
{{- printf "%s-openapi-oauth-key" (include "oas2mcp.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/* Key in the signing-key Secret (also the mounted file name): the existing Secret's key, else the default. */}}
{{- define "oas2mcp.oauthPrivateKeyKey" -}}
{{- .Values.oas2mcp.openapi.oauth.privateKey.existingSecret.key -}}
{{- end -}}

{{/* Fail fast on an existing CA source with an invalid kind. */}}
{{- define "oas2mcp.caCertsValidate" -}}
{{- with .Values.oas2mcp.caCerts.existing.name -}}
{{- if not (has $.Values.oas2mcp.caCerts.existing.kind (list "ConfigMap" "Secret")) -}}
{{- fail "oas2mcp.caCerts.existing.kind must be \"ConfigMap\" or \"Secret\" when existing.name is set" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* Whether any extra CA certificates are configured (inline or an existing resource). */}}
{{- define "oas2mcp.caCertsEnabled" -}}
{{- if or .Values.oas2mcp.caCerts.inline .Values.oas2mcp.caCerts.existing.name -}}
true
{{- end -}}
{{- end -}}

{{/* Whether the CA certificates are mounted from an existing ConfigMap. */}}
{{- define "oas2mcp.caCertsFromConfigMap" -}}
{{- if and .Values.oas2mcp.caCerts.existing.name (eq .Values.oas2mcp.caCerts.existing.kind "ConfigMap") -}}
true
{{- end -}}
{{- end -}}

{{/* Whether a Secret must be generated to hold the inline CA bundle (no existing resource chosen). */}}
{{- define "oas2mcp.caCertsGenerateSecret" -}}
{{- if and .Values.oas2mcp.caCerts.inline (not .Values.oas2mcp.caCerts.existing.name) -}}
true
{{- end -}}
{{- end -}}

{{/* Name of the Secret holding the extra CA certificates (an existing Secret, else the generated one). Only meaningful for the Secret-backed cases. */}}
{{- define "oas2mcp.caCertsSecretName" -}}
{{- if and .Values.oas2mcp.caCerts.existing.name (eq .Values.oas2mcp.caCerts.existing.kind "Secret") -}}
{{- .Values.oas2mcp.caCerts.existing.name -}}
{{- else -}}
{{- printf "%s-ca-certs" (include "oas2mcp.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/* Key in the CA-certificates source (also the mounted file name): the existing resource's key, else the generated default. */}}
{{- define "oas2mcp.caCertsKey" -}}
{{- if .Values.oas2mcp.caCerts.existing.name -}}
{{- .Values.oas2mcp.caCerts.existing.key -}}
{{- else -}}
ca-certificates.crt
{{- end -}}
{{- end -}}
