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
{{- if .Values.oas2mcp.upstream.existingSecret -}}
{{- .Values.oas2mcp.upstream.existingSecret -}}
{{- else -}}
{{- printf "%s-upstream" (include "oas2mcp.fullname" .) -}}
{{- end -}}
{{- end -}}
