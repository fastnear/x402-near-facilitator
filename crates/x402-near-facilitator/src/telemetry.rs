use std::collections::HashMap;
use std::time::Duration;

use opentelemetry::metrics::{Counter, Gauge, Histogram, MeterProvider as _};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::VERSION;
use crate::config::{Environment, OtelConfig, read_secret};

#[allow(missing_debug_implementations)]
pub struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
    metrics: Metrics,
}

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct Metrics {
    requests: Counter<u64>,
    request_duration_seconds: Histogram<f64>,
    rpc_failovers: Counter<u64>,
    idempotency_replays: Counter<u64>,
    settlement_results: Counter<u64>,
    pending_settlements: Gauge<u64>,
    journal_state_rows: Gauge<u64>,
    oldest_pending_age_seconds: Gauge<f64>,
    sponsorship_budget_used_ratio: Gauge<f64>,
    relayer_balance_near: Gauge<f64>,
    relayer_quarantined: Gauge<u64>,
    gas_burnt: Histogram<u64>,
    sponsored_yocto_near: Histogram<f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("failed to load OTLP credentials")]
    Secret(#[source] crate::config::ConfigError),
    #[error("invalid OTLP header credential file")]
    Headers,
    #[error("failed to initialize OTLP exporter")]
    Exporter,
    #[error("tracing subscriber was already initialized")]
    Subscriber,
}

impl TelemetryGuard {
    pub fn initialize(
        environment: Environment,
        otel: Option<&OtelConfig>,
    ) -> Result<Self, TelemetryError> {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let Some(otel) = otel else {
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().json())
                .try_init()
                .map_err(|_| TelemetryError::Subscriber)?;
            return Ok(Self {
                provider: None,
                meter_provider: None,
                metrics: Metrics::new(&global::meter("x402-near-facilitator")),
            });
        };

        let headers_secret = read_secret(&otel.headers_file).map_err(TelemetryError::Secret)?;
        let headers = parse_headers(headers_secret.as_str())?;
        let traces_endpoint = signal_endpoint(&otel.endpoint, "traces");
        let metrics_endpoint = signal_endpoint(&otel.endpoint, "metrics");
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(traces_endpoint)
            .with_timeout(Duration::from_secs(5))
            .with_headers(headers)
            .build()
            .map_err(|_| TelemetryError::Exporter)?;
        let deployment = match environment {
            Environment::Mainnet => "mainnet",
            Environment::Testnet => "testnet",
        };
        let resource = Resource::builder()
            .with_service_name("x402-near-facilitator")
            .with_attributes([
                KeyValue::new("service.version", VERSION),
                KeyValue::new("deployment.environment.name", deployment),
            ])
            .build();
        let provider = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(exporter)
            .build();
        let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(metrics_endpoint)
            .with_timeout(Duration::from_secs(5))
            .with_headers(parse_headers(headers_secret.as_str())?)
            .build()
            .map_err(|_| TelemetryError::Exporter)?;
        let reader = PeriodicReader::builder(metric_exporter)
            .with_interval(Duration::from_secs(30))
            .build();
        let meter_provider = SdkMeterProvider::builder()
            .with_resource(resource)
            .with_reader(reader)
            .build();
        let tracer = provider.tracer("x402-near-facilitator");
        let meter = meter_provider.meter("x402-near-facilitator");
        let metrics = Metrics::new(&meter);
        global::set_tracer_provider(provider.clone());
        global::set_meter_provider(meter_provider.clone());
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().json())
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .try_init()
            .map_err(|_| TelemetryError::Subscriber)?;
        Ok(Self {
            provider: Some(provider),
            meter_provider: Some(meter_provider),
            metrics,
        })
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics.clone()
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = &self.provider {
            let _shutdown_result = provider.shutdown();
        }
        if let Some(provider) = &self.meter_provider {
            let _shutdown_result = provider.shutdown();
        }
    }
}

impl Metrics {
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::new(&global::meter("x402-near-facilitator-tests"))
    }

    fn new(meter: &opentelemetry::metrics::Meter) -> Self {
        Self {
            requests: meter.u64_counter("x402_requests_total").build(),
            request_duration_seconds: meter.f64_histogram("x402_request_duration_seconds").build(),
            rpc_failovers: meter.u64_counter("x402_rpc_failovers_total").build(),
            idempotency_replays: meter.u64_counter("x402_idempotency_replays_total").build(),
            settlement_results: meter.u64_counter("x402_settlement_results_total").build(),
            pending_settlements: meter.u64_gauge("x402_pending_settlements").build(),
            journal_state_rows: meter.u64_gauge("x402_journal_state_rows").build(),
            oldest_pending_age_seconds: meter.f64_gauge("x402_oldest_pending_age_seconds").build(),
            sponsorship_budget_used_ratio: meter
                .f64_gauge("x402_sponsorship_budget_used_ratio")
                .build(),
            relayer_balance_near: meter.f64_gauge("x402_relayer_balance_near").build(),
            relayer_quarantined: meter.u64_gauge("x402_relayer_quarantined").build(),
            gas_burnt: meter.u64_histogram("x402_settlement_gas_burnt").build(),
            sponsored_yocto_near: meter.f64_histogram("x402_sponsored_yocto_near").build(),
        }
    }

    pub fn record_request(&self, operation: &'static str, result: &'static str, seconds: f64) {
        let attributes = [
            KeyValue::new("operation", operation),
            KeyValue::new("result", result),
        ];
        self.requests.add(1, &attributes);
        self.request_duration_seconds.record(seconds, &attributes);
    }

    pub fn record_rpc_failover(&self, operation: &'static str) {
        self.rpc_failovers
            .add(1, &[KeyValue::new("operation", operation)]);
    }

    pub fn record_idempotency_replay(&self) {
        self.idempotency_replays.add(1, &[]);
    }

    pub fn record_pending_settlements(&self, count: u64) {
        self.pending_settlements.record(count, &[]);
    }

    pub fn record_journal_state(&self, state: &'static str, count: u64) {
        self.journal_state_rows
            .record(count, &[KeyValue::new("state", state)]);
    }

    pub fn record_oldest_pending_age(&self, seconds: f64) {
        self.oldest_pending_age_seconds.record(seconds, &[]);
    }

    pub fn record_budget_used_ratio(&self, ratio: f64) {
        self.sponsorship_budget_used_ratio.record(ratio, &[]);
    }

    pub fn record_relayer(&self, balance_near: f64, quarantined: bool) {
        self.relayer_balance_near.record(balance_near, &[]);
        self.relayer_quarantined.record(u64::from(quarantined), &[]);
    }

    pub fn record_settlement_result(&self, result: &'static str, reason: &'static str) {
        self.settlement_results.add(
            1,
            &[
                KeyValue::new("result", result),
                KeyValue::new("reason", reason),
            ],
        );
    }

    pub fn record_settlement_cost(&self, gas_burnt: u64, sponsored_yocto_near: f64) {
        self.gas_burnt.record(gas_burnt, &[]);
        self.sponsored_yocto_near.record(sponsored_yocto_near, &[]);
    }
}

fn parse_headers(value: &str) -> Result<HashMap<String, String>, TelemetryError> {
    let mut headers = HashMap::new();
    for pair in value.split(',') {
        let (name, value) = pair.split_once('=').ok_or(TelemetryError::Headers)?;
        let name = name.trim();
        let value = value.trim();
        if name.is_empty()
            || value.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            || value.bytes().any(|byte| matches!(byte, b'\r' | b'\n'))
        {
            return Err(TelemetryError::Headers);
        }
        if headers.insert(name.to_owned(), value.to_owned()).is_some() {
            return Err(TelemetryError::Headers);
        }
    }
    if headers.is_empty() {
        return Err(TelemetryError::Headers);
    }
    Ok(headers)
}

fn signal_endpoint(base: &url::Url, signal: &str) -> String {
    let mut endpoint = base.clone();
    endpoint.set_path(&format!("/v1/{signal}"));
    endpoint.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_honeycomb_headers_without_exposing_them() {
        let headers =
            parse_headers("x-honeycomb-team=secret,x-honeycomb-dataset=x402-near-mainnet");
        assert_eq!(headers.as_ref().map(HashMap::len).ok(), Some(2));
    }

    #[test]
    fn rejects_duplicate_or_multiline_headers() {
        assert!(parse_headers("x-key=a,x-key=b").is_err());
        assert!(parse_headers("x-key=a\ninjected").is_err());
    }

    #[test]
    fn builds_distinct_otlp_http_signal_endpoints() {
        let base =
            url::Url::parse("https://api.honeycomb.io").unwrap_or_else(|_| std::process::abort());
        assert_eq!(
            signal_endpoint(&base, "traces"),
            "https://api.honeycomb.io/v1/traces"
        );
        assert_eq!(
            signal_endpoint(&base, "metrics"),
            "https://api.honeycomb.io/v1/metrics"
        );
    }

    #[test]
    fn header_parse_errors_do_not_echo_secret_material() {
        let secret = "never-emit-this-secret";
        let error = parse_headers(&format!("x-key={secret}\ninjected"));
        let debug = format!("{error:?}");
        assert!(!debug.contains(secret));
    }
}
