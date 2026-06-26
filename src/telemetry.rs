//! OpenTelemetry metrics for tool calls.
//!
//! Every MCP tool call is counted and timed, attributed by the tool name and
//! the call outcome, plus the caller's JWT `sub` when role-based authorization
//! is enabled and the token carried one. The metrics are exported two ways,
//! enabled independently from the CLI:
//!
//! - **OTLP** (`--otlp-endpoint`): pushed to a collector over HTTP/protobuf.
//! - **Prometheus** (`--metrics-addr`): scraped from a `/metrics` endpoint.
//!
//! With neither configured the instruments still exist but record to nowhere,
//! so the call path never has to branch on whether telemetry is on.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context as _;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter, MeterProvider as _};
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use prometheus::{Encoder as _, Registry, TextEncoder};

use crate::cli::Cli;

/// How often the OTLP push exporter flushes accumulated metrics to the collector.
const OTLP_EXPORT_INTERVAL: Duration = Duration::from_secs(30);

/// Instrumentation scope name reported on every metric.
const METER_NAME: &str = "oas2mcp";

/// Explicit histogram buckets for the call-duration metric, in seconds. The
/// SDK default boundaries top out at 10000, tuned for milliseconds; an upstream
/// HTTP request is better served by sub-second-to-tens-of-seconds buckets.
const DURATION_BUCKETS_SECONDS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// The result of a tool call, recorded as a low-cardinality `outcome` attribute.
#[derive(Clone, Copy)]
pub enum Outcome {
    Success,
    Error,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// The instruments recorded on every tool call. Cheap to clone — each handle is
/// reference-counted inside the SDK — so it travels with every per-session
/// [`OpenApiServer`](crate::server::OpenApiServer) clone.
#[derive(Clone)]
pub struct Metrics {
    /// Count of tool calls, attributed by `tool`, `outcome`, and — when JWT
    /// authorization is enabled and the token carried one — `sub`.
    calls: Counter<u64>,
    /// Wall-clock duration of the proxied upstream request, in seconds.
    duration: Histogram<f64>,
}

impl Metrics {
    fn new(meter: &Meter) -> Self {
        Self {
            calls: meter
                .u64_counter("mcp.tool.calls")
                .with_description("Number of MCP tool calls, by tool, outcome and subject.")
                .build(),
            duration: meter
                .f64_histogram("mcp.tool.call.duration")
                .with_unit("s")
                .with_description("Duration of the proxied upstream request for an MCP tool call.")
                .with_boundaries(DURATION_BUCKETS_SECONDS.to_vec())
                .build(),
        }
    }

    /// Instruments backed by a provider with no exporters: recording is a
    /// no-op. Used by tests that build a server without a telemetry pipeline.
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self::new(&SdkMeterProvider::builder().build().meter(METER_NAME))
    }

    /// Record one completed tool call. `sub` is the caller's JWT subject when
    /// role-based authorization is enabled and the token carried a `sub` claim;
    /// `None` leaves the attribute off entirely.
    pub fn record_call(&self, tool: &str, outcome: Outcome, sub: Option<&str>, elapsed: Duration) {
        let mut attrs = vec![
            KeyValue::new("tool", tool.to_string()),
            KeyValue::new("outcome", outcome.as_str()),
        ];
        if let Some(sub) = sub {
            attrs.push(KeyValue::new("sub", sub.to_string()));
        }
        self.calls.add(1, &attrs);
        self.duration.record(elapsed.as_secs_f64(), &attrs);
    }
}

/// A configured metrics pipeline: the instruments to record into, the live
/// meter provider (kept so it can be flushed and shut down), and — when the
/// Prometheus exporter is enabled — the registry and address its `/metrics`
/// endpoint is served from.
pub struct Telemetry {
    pub metrics: Metrics,
    provider: SdkMeterProvider,
    prometheus: Option<(Registry, SocketAddr)>,
}

impl Telemetry {
    /// Build the metrics pipeline from the CLI. Always produces usable
    /// [`Metrics`]; an exporter that is configured but fails to build is an
    /// error — fail loud rather than silently drop telemetry.
    pub fn from_cli(cli: &Cli) -> anyhow::Result<Self> {
        let resource = Resource::builder()
            .with_service_name(cli.otel_service_name.clone())
            .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
            .build();

        let mut builder = SdkMeterProvider::builder().with_resource(resource);

        // OTLP push exporter (HTTP/protobuf). Enabled when an endpoint is set.
        // The `/v1/metrics` signal path is appended to the base, matching the
        // OTLP spec's behaviour for `OTEL_EXPORTER_OTLP_ENDPOINT`.
        if let Some(endpoint) = cli.otlp_endpoint.as_ref() {
            let exporter = opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_endpoint(metrics_endpoint(endpoint.as_str()))
                .build()
                .context("building the OTLP metric exporter")?;
            let reader = PeriodicReader::builder(exporter)
                .with_interval(OTLP_EXPORT_INTERVAL)
                .build();
            builder = builder.with_reader(reader);
            tracing::info!(%endpoint, "exporting tool-call metrics over OTLP");
        }

        // Prometheus pull exporter. Enabled when a metrics address is set.
        let prometheus = if let Some(addr) = cli.metrics_addr {
            let registry = Registry::new();
            let exporter = opentelemetry_prometheus::exporter()
                .with_registry(registry.clone())
                .build()
                .context("building the Prometheus metric exporter")?;
            builder = builder.with_reader(exporter);
            tracing::info!(%addr, "serving Prometheus tool-call metrics at GET /metrics");
            Some((registry, addr))
        } else {
            None
        };

        let provider = builder.build();
        let metrics = Metrics::new(&provider.meter(METER_NAME));

        Ok(Self {
            metrics,
            provider,
            prometheus,
        })
    }

    /// Spawn the Prometheus `/metrics` HTTP server when configured; a no-op
    /// otherwise. The listener is bound eagerly so a bad address fails at
    /// startup, then served in the background for the process lifetime.
    pub async fn serve_metrics(&self) -> anyhow::Result<()> {
        let Some((registry, addr)) = self.prometheus.clone() else {
            return Ok(());
        };
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding the metrics endpoint {addr}"))?;
        let app = axum::Router::new().route(
            "/metrics",
            axum::routing::get(move || {
                let registry = registry.clone();
                async move { encode_metrics(&registry) }
            }),
        );
        tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                tracing::error!(error = %err, "the Prometheus metrics server stopped");
            }
        });
        Ok(())
    }

    /// Flush and shut the meter provider down, pushing any buffered metrics a
    /// final time. Called on graceful shutdown.
    pub fn shutdown(&self) {
        if let Err(err) = self.provider.shutdown() {
            tracing::warn!(error = %err, "failed to shut the meter provider down cleanly");
        }
    }
}

/// Append the OTLP metrics signal path to a base endpoint. Setting the endpoint
/// explicitly bypasses the SDK's own env-based join, so we replicate it here so
/// the `--otlp-endpoint` flag and `OTEL_EXPORTER_OTLP_ENDPOINT` behave alike.
fn metrics_endpoint(base: &str) -> String {
    format!("{}/v1/metrics", base.trim_end_matches('/'))
}

/// Gather and encode the registry into the Prometheus text exposition format.
fn encode_metrics(registry: &Registry) -> axum::response::Response {
    use axum::response::IntoResponse as _;

    let encoder = TextEncoder::new();
    let mut buf = Vec::new();
    match encoder.encode(&registry.gather(), &mut buf) {
        Ok(()) => (
            [(axum::http::header::CONTENT_TYPE, encoder.format_type())],
            buf,
        )
            .into_response(),
        Err(err) => {
            tracing::error!(error = %err, "encoding Prometheus metrics failed");
            axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_endpoint_appends_signal_path_without_doubling_slashes() {
        assert_eq!(
            metrics_endpoint("http://localhost:4318"),
            "http://localhost:4318/v1/metrics"
        );
        assert_eq!(
            metrics_endpoint("http://localhost:4318/"),
            "http://localhost:4318/v1/metrics"
        );
    }

    #[test]
    fn disabled_metrics_record_without_panicking() {
        // The no-op pipeline must accept recordings (with and without a sub).
        let metrics = Metrics::disabled();
        metrics.record_call("getPet", Outcome::Success, None, Duration::from_millis(5));
        metrics.record_call(
            "deletePet",
            Outcome::Error,
            Some("user-123"),
            Duration::from_millis(9),
        );
    }
}
