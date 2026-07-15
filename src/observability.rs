use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

pub use codex_otel::{
    OtelExporter, OtelHttpProtocol, OtelProvider, OtelSettings, OtelTlsConfig,
    StatsigMetricsSettings, current_span_trace_id, current_span_w3c_trace_context,
    inject_span_w3c_trace_headers, set_parent_from_w3c_trace_context,
    span_w3c_trace_context, traceparent_context_from_env,
};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use crate::error::{Error, Result};

/// Entry point for configuring tracing and OpenTelemetry.
pub struct Observability;

impl Observability {
    /// Create an observability builder with SDK defaults.
    pub fn builder() -> ObservabilityBuilder {
        ObservabilityBuilder::default()
    }

    /// Create a builder after applying supported environment variables.
    pub fn from_env() -> Result<ObservabilityBuilder> {
        ObservabilityBuilder::default().with_env()
    }

    /// Apply environment configuration and install the tracing subscriber.
    pub fn init_from_env() -> Result<ObservabilityGuard> {
        Self::from_env()?.install()
    }
}

/// Guard that owns the installed OpenTelemetry provider.
///
/// Dropping the guard attempts to flush and shut down telemetry exporters. Call
/// [`shutdown`](Self::shutdown) when you need deterministic shutdown earlier.
pub struct ObservabilityGuard {
    provider: Option<OtelProvider>,
}

impl ObservabilityGuard {
    /// The underlying Codex OpenTelemetry provider, when exporters are enabled.
    pub fn provider(&self) -> Option<&OtelProvider> {
        self.provider.as_ref()
    }

    /// Flush and shut down telemetry exporters.
    pub fn shutdown(&self) {
        if let Some(provider) = &self.provider {
            provider.shutdown();
        }
    }
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        if let Some(provider) = &self.provider {
            provider.shutdown();
        }
    }
}

/// Builder for SDK tracing and OpenTelemetry configuration.
///
/// The builder wraps Codex's [`OtelSettings`] with common SDK defaults and can
/// optionally install a global `tracing_subscriber`.
#[derive(Clone, Debug)]
pub struct ObservabilityBuilder {
    /// Codex OpenTelemetry settings passed to `OtelProvider`.
    settings: OtelSettings,
    /// Optional tracing env-filter expression.
    env_filter: Option<String>,
    /// Whether `install()` should register a global subscriber.
    install_subscriber: bool,
    /// Whether to include a human-readable formatting layer.
    fmt_layer: bool,
}

impl Default for ObservabilityBuilder {
    fn default() -> Self {
        Self {
            settings: OtelSettings {
                environment: "development".to_string(),
                service_name: "codex-sdk-rs".to_string(),
                service_version: env!("CARGO_PKG_VERSION").to_string(),
                codex_home: std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from(".")),
                exporter: OtelExporter::None,
                trace_exporter: OtelExporter::None,
                metrics_exporter: OtelExporter::None,
                runtime_metrics: false,
                span_attributes: BTreeMap::new(),
                tracestate: BTreeMap::new(),
            },
            env_filter: Some("info".to_string()),
            install_subscriber: true,
            fmt_layer: true,
        }
    }
}

impl ObservabilityBuilder {
    /// Current OpenTelemetry settings.
    pub fn settings(&self) -> &OtelSettings {
        &self.settings
    }

    /// Mutable access to the full OpenTelemetry settings.
    pub fn settings_mut(&mut self) -> &mut OtelSettings {
        &mut self.settings
    }

    /// Replace all OpenTelemetry settings.
    pub fn with_settings(mut self, settings: OtelSettings) -> Self {
        self.settings = settings;
        self
    }

    /// Load a dotenv file and then apply supported environment variables.
    pub fn with_dotenv(mut self, path: impl AsRef<Path>) -> Result<Self> {
        dotenvy::from_path(path.as_ref()).map_err(Error::observability)?;
        self = self.with_env()?;
        Ok(self)
    }

    /// Apply supported environment variables to this builder.
    ///
    /// This reads standard OTEL variables such as
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`, signal-specific OTLP endpoints,
    /// `OTEL_RESOURCE_ATTRIBUTES`, and SDK-specific logging/runtime metrics
    /// variables.
    pub fn with_env(mut self) -> Result<Self> {
        if env_bool("OTEL_SDK_DISABLED").unwrap_or(false) {
            return Ok(self.disabled());
        }

        if let Some(service_name) = first_env(&["OTEL_SERVICE_NAME"]) {
            self.settings.service_name = service_name;
        }
        if let Some(service_version) = first_env(&["OTEL_SERVICE_VERSION"]) {
            self.settings.service_version = service_version;
        }
        if let Some(environment) =
            first_env(&["CODEX_SDK_OTEL_ENVIRONMENT", "CODEX_ENV", "APP_ENV"])
        {
            self.settings.environment = environment;
        }
        if let Some(filter) = first_env(&["RUST_LOG", "CODEX_SDK_LOG"]) {
            self.env_filter = Some(filter);
        }
        if let Some(runtime_metrics) = env_bool("CODEX_SDK_OTEL_RUNTIME_METRICS") {
            self.settings.runtime_metrics = runtime_metrics;
        }

        self.apply_resource_attributes();
        self.settings.exporter = exporter_from_env(Signal::Logs)?;
        self.settings.trace_exporter = exporter_from_env(Signal::Traces)?;
        self.settings.metrics_exporter = exporter_from_env(Signal::Metrics)?;
        Ok(self)
    }

    /// Set the deployment environment attached to telemetry resources.
    pub fn environment(mut self, environment: impl Into<String>) -> Self {
        self.settings.environment = environment.into();
        self
    }

    /// Set the service name attached to telemetry resources.
    pub fn service_name(mut self, service_name: impl Into<String>) -> Self {
        self.settings.service_name = service_name.into();
        self
    }

    /// Set the service version attached to telemetry resources.
    pub fn service_version(mut self, service_version: impl Into<String>) -> Self {
        self.settings.service_version = service_version.into();
        self
    }

    /// Set the Codex home directory included in telemetry settings.
    pub fn codex_home(mut self, codex_home: impl Into<PathBuf>) -> Self {
        self.settings.codex_home = codex_home.into();
        self
    }

    /// Set the logs exporter.
    pub fn logs_exporter(mut self, exporter: OtelExporter) -> Self {
        self.settings.exporter = exporter;
        self
    }

    /// Set the traces exporter.
    pub fn traces_exporter(mut self, exporter: OtelExporter) -> Self {
        self.settings.trace_exporter = exporter;
        self
    }

    /// Set the metrics exporter.
    pub fn metrics_exporter(mut self, exporter: OtelExporter) -> Self {
        self.settings.metrics_exporter = exporter;
        self
    }

    /// Use the same exporter for logs, traces, and metrics.
    pub fn all_exporters(mut self, exporter: OtelExporter) -> Self {
        self.settings.exporter = exporter.clone();
        self.settings.trace_exporter = exporter.clone();
        self.settings.metrics_exporter = exporter;
        self
    }

    /// Configure logs, traces, and metrics to use an OTLP/HTTP endpoint.
    pub fn otlp_http(mut self, endpoint: impl Into<String>) -> Self {
        let exporter = OtelExporter::OtlpHttp {
            endpoint: endpoint.into(),
            headers: HashMap::new(),
            protocol: OtelHttpProtocol::Binary,
            tls: None,
        };
        self.settings.exporter = exporter.clone();
        self.settings.trace_exporter = exporter.clone();
        self.settings.metrics_exporter = exporter;
        self
    }

    /// Configure logs, traces, and metrics to use an OTLP/gRPC endpoint.
    pub fn otlp_grpc(mut self, endpoint: impl Into<String>) -> Self {
        let exporter = OtelExporter::OtlpGrpc {
            endpoint: endpoint.into(),
            headers: HashMap::new(),
            tls: None,
        };
        self.settings.exporter = exporter.clone();
        self.settings.trace_exporter = exporter.clone();
        self.settings.metrics_exporter = exporter;
        self
    }

    /// Disable all telemetry exporters.
    pub fn disabled(mut self) -> Self {
        self.settings.exporter = OtelExporter::None;
        self.settings.trace_exporter = OtelExporter::None;
        self.settings.metrics_exporter = OtelExporter::None;
        self
    }

    /// Enable or disable runtime metrics collection.
    pub fn runtime_metrics(mut self, enabled: bool) -> Self {
        self.settings.runtime_metrics = enabled;
        self
    }

    /// Add an attribute to all emitted spans/resources.
    pub fn span_attribute(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.settings
            .span_attributes
            .insert(key.into(), value.into());
        self
    }

    /// Add a tracestate field for distributed trace propagation.
    pub fn tracestate_field(
        mut self,
        member: impl Into<String>,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.settings
            .tracestate
            .entry(member.into())
            .or_default()
            .insert(key.into(), value.into());
        self
    }

    /// Set the tracing env-filter expression, such as `info,codex_sdk=debug`.
    pub fn env_filter(mut self, filter: impl Into<String>) -> Self {
        self.env_filter = Some(filter.into());
        self
    }

    /// Disable env-filter installation.
    pub fn no_env_filter(mut self) -> Self {
        self.env_filter = None;
        self
    }

    /// Choose whether [`install`](Self::install) installs a global subscriber.
    pub fn install_subscriber(mut self, install: bool) -> Self {
        self.install_subscriber = install;
        self
    }

    /// Enable or disable the human-readable formatting layer.
    pub fn fmt_layer(mut self, enabled: bool) -> Self {
        self.fmt_layer = enabled;
        self
    }

    /// Build the OpenTelemetry provider without installing a subscriber.
    pub fn build_provider(self) -> Result<ObservabilityGuard> {
        let provider =
            OtelProvider::from(&self.settings).map_err(Error::observability)?;
        Ok(ObservabilityGuard { provider })
    }

    /// Build the provider and install the configured tracing subscriber.
    pub fn install(self) -> Result<ObservabilityGuard> {
        let provider =
            OtelProvider::from(&self.settings).map_err(Error::observability)?;
        if self.install_subscriber {
            install_subscriber(provider.as_ref(), self.env_filter, self.fmt_layer)?;
        }
        Ok(ObservabilityGuard { provider })
    }

    fn apply_resource_attributes(&mut self) {
        let Some(attributes) = first_env(&["OTEL_RESOURCE_ATTRIBUTES"]) else {
            return;
        };

        for (key, value) in parse_key_value_list(&attributes).unwrap_or_default() {
            match key.as_str() {
                "service.name" => self.settings.service_name = value,
                "service.version" => self.settings.service_version = value,
                "deployment.environment" | "deployment.environment.name" => {
                    self.settings.environment = value;
                }
                _ => {
                    self.settings.span_attributes.insert(key, value);
                }
            }
        }
    }
}

fn install_subscriber(
    provider: Option<&OtelProvider>,
    env_filter: Option<String>,
    fmt_layer: bool,
) -> Result<()> {
    let env_filter = env_filter
        .as_deref()
        .map(EnvFilter::try_new)
        .transpose()
        .map_err(Error::observability)?;
    let fmt_layer = fmt_layer.then(tracing_subscriber::fmt::layer);

    match provider {
        Some(provider) => tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(provider.logger_layer())
            .with(provider.tracing_layer())
            .try_init()
            .map_err(Error::observability),
        None => tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .try_init()
            .map_err(Error::observability),
    }
}

fn exporter_from_env(signal: Signal) -> Result<OtelExporter> {
    let Some(endpoint) =
        first_env(&[signal.endpoint_key(), "OTEL_EXPORTER_OTLP_ENDPOINT"])
    else {
        return Ok(OtelExporter::None);
    };
    let protocol = first_env(&[signal.protocol_key(), "OTEL_EXPORTER_OTLP_PROTOCOL"])
        .unwrap_or_else(|| "http/protobuf".to_string());
    let headers = merged_headers(signal)?;

    match protocol.as_str() {
        "grpc" => Ok(OtelExporter::OtlpGrpc {
            endpoint,
            headers,
            tls: None,
        }),
        "http/protobuf" | "http" => Ok(OtelExporter::OtlpHttp {
            endpoint,
            headers,
            protocol: OtelHttpProtocol::Binary,
            tls: None,
        }),
        "http/json" => Ok(OtelExporter::OtlpHttp {
            endpoint,
            headers,
            protocol: OtelHttpProtocol::Json,
            tls: None,
        }),
        other => Err(Error::observability(format!(
            "unsupported OTEL protocol {other:?}"
        ))),
    }
}

fn merged_headers(signal: Signal) -> Result<HashMap<String, String>> {
    let mut headers = HashMap::new();
    for key in ["OTEL_EXPORTER_OTLP_HEADERS", signal.headers_key()] {
        if let Some(value) = first_env(&[key]) {
            for (header, header_value) in parse_key_value_list(&value)? {
                headers.insert(header, header_value);
            }
        }
    }
    Ok(headers)
}

fn parse_key_value_list(input: &str) -> Result<Vec<(String, String)>> {
    input
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            (!part.is_empty()).then_some(part)
        })
        .map(|part| {
            let (key, value) = part.split_once('=').ok_or_else(|| {
                Error::observability(format!(
                    "expected key=value entry in comma-separated list: {part:?}"
                ))
            })?;
            Ok((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn first_env(keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| std::env::var(key).ok())
        .filter(|value| !value.trim().is_empty())
}

fn env_bool(key: &str) -> Option<bool> {
    let value = std::env::var(key).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Signal {
    Logs,
    Traces,
    Metrics,
}

impl Signal {
    fn endpoint_key(self) -> &'static str {
        match self {
            Self::Logs => "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
            Self::Traces => "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            Self::Metrics => "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        }
    }

    fn protocol_key(self) -> &'static str {
        match self {
            Self::Logs => "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
            Self::Traces => "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
            Self::Metrics => "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL",
        }
    }

    fn headers_key(self) -> &'static str {
        match self {
            Self::Logs => "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
            Self::Traces => "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
            Self::Metrics => "OTEL_EXPORTER_OTLP_METRICS_HEADERS",
        }
    }
}
