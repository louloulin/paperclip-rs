//! Multica 遥测层。
//!
//! 单一职责：提供结构化日志与 tracing subscriber 初始化。
//! 不持有任何业务状态，不依赖其他 crate。

use serde::Serialize;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub mod redact;

pub use redact::{redact_log, Redactor};

/// 启动横幅字段（与 multica `startup-banner.ts` 对齐）。
#[derive(Debug, Serialize, Clone)]
pub struct StartupBanner {
    pub service: String,
    pub version: String,
    pub build_time: String,
    pub commit: String,
    pub mode: &'static str,
}

impl std::fmt::Display for StartupBanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "┌──────────────────────────────────────────────┐")?;
        writeln!(f, "  service:   {}", self.service)?;
        writeln!(f, "  version:   {}", self.version)?;
        writeln!(f, "  build:     {}", self.build_time)?;
        writeln!(f, "  commit:    {}", self.commit)?;
        writeln!(f, "  mode:      {}", self.mode)?;
        writeln!(f, "└──────────────────────────────────────────────┘")
    }
}

/// 遥测初始化选项。
#[derive(Debug, Clone)]
pub struct TelemetryOptions {
    pub service_name: String,
    pub json_output: bool,
    pub default_level: tracing::Level,
}

impl Default for TelemetryOptions {
    fn default() -> Self {
        Self {
            service_name: "multica-server".into(),
            json_output: true,
            default_level: tracing::Level::INFO,
        }
    }
}

/// 初始化全局 tracing subscriber（幂等）。失败返回错误。
pub fn init(opts: &TelemetryOptions) -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(opts.default_level.to_string()));

    let registry = tracing_subscriber::registry().with(env_filter);

    if opts.json_output {
        let layer = fmt::layer()
            .with_target(true)
            .with_level(true)
            .with_thread_ids(false)
            .with_thread_names(false)
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(false);
        registry.with(layer).try_init()?;
    } else {
        let layer = fmt::layer().with_target(true).with_level(true);
        registry.with(layer).try_init()?;
    }
    Ok(())
}

/// 输出启动横幅到 tracing banner。
pub fn log_banner(banner: &StartupBanner) {
    tracing::info!("\n{}", banner);
}

/// 可选 OTLP exporter 安装（feature `otlp` 启用时）。
#[cfg(feature = "otlp")]
pub fn install_global(cfg: &OtlpConfig) -> anyhow::Result<()> {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::trace::TracerProvider;
    use opentelemetry_sdk::Resource;

    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(&cfg.endpoint);

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(Resource::new(vec![opentelemetry::KeyValue::new(
            "service.name",
            cfg.service_name.clone(),
        )]))
        .build();
    let tracer = provider.tracer(cfg.service_name.clone());
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    tracing_subscriber::registry().with(layer).try_init()?;
    Ok(())
}

#[cfg(feature = "otlp")]
#[derive(Debug, Clone)]
pub struct OtlpConfig {
    pub service_name: String,
    pub service_version: String,
    pub endpoint: String,
}

#[cfg(feature = "otlp")]
impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            service_name: "multica-server".into(),
            service_version: "0.1.0".into(),
            endpoint: "http://127.0.0.1:4317".into(),
        }
    }
}

/// Instance telemetry 客户端 stub：把遥测事件 append 到一个本地 outbox，
/// 由后台 actor 周期 flush 到 instance telemetry endpoint。
pub mod global {
    use serde::Serialize;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, Serialize)]
    pub struct TelemetryEvent {
        pub kind: String,
        pub payload: serde_json::Value,
        pub ts: chrono::DateTime<chrono::Utc>,
    }

    pub trait TelemetrySink: Send + Sync {
        fn handle(&self, event: TelemetryEvent);
    }

    #[derive(Default)]
    pub struct NoopSink;

    impl TelemetrySink for NoopSink {
        fn handle(&self, _event: TelemetryEvent) {}
    }

    static SINK: Mutex<Option<Arc<dyn TelemetrySink>>> = Mutex::new(None);

    /// 安装全局 sink。后续 `record` 调用转给它。
    pub fn install(sink: Arc<dyn TelemetrySink>) {
        if let Ok(mut guard) = SINK.lock() {
            *guard = Some(sink);
        }
    }

    /// 提交一条 telemetry 事件。
    pub fn record(kind: impl Into<String>, payload: serde_json::Value) {
        let event = TelemetryEvent {
            kind: kind.into(),
            payload,
            ts: chrono::Utc::now(),
        };
        if let Ok(guard) = SINK.lock() {
            if let Some(sink) = guard.as_ref() {
                sink.handle(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_displays_all_fields() {
        let b = StartupBanner {
            service: "multica-server".into(),
            version: "0.1.0".into(),
            build_time: "now".into(),
            commit: "abc".into(),
            mode: "production",
        };
        let s = format!("{b}");
        assert!(s.contains("multica-server"));
        assert!(s.contains("0.1.0"));
        assert!(s.contains("production"));
    }

    #[test]
    fn telemetry_options_default() {
        let o = TelemetryOptions::default();
        assert_eq!(o.service_name, "multica-server");
        assert!(o.json_output);
    }
}
