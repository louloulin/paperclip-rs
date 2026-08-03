//! Paperclip 遥测层。
//!
//! 单一职责：提供结构化日志与 tracing subscriber 初始化。
//! 不持有任何业务状态，不依赖其他 crate。

use serde::Serialize;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 启动横幅字段（与原 server `startup-banner.ts` 对齐）。
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
            service_name: "paperclip-server".into(),
            json_output: true,
            default_level: tracing::Level::INFO,
        }
    }
}

/// 初始化全局 tracing subscriber。
///
/// 默认输出 JSON 到 stdout；可通过 `RUST_LOG` 环境变量覆盖级别。
pub fn init(opts: &TelemetryOptions) -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(opts.default_level.to_string()));

    let registry = tracing_subscriber::registry().with(env_filter);

    if opts.json_output {
        let layer = fmt::layer()
            .with_target(true)
            .with_thread_ids(false)
            .with_file(false)
            .with_line_number(false)
            .json()
            .with_current_span(true)
            .with_span_list(false);
        registry.with(layer).try_init()?;
    } else {
        let layer = fmt::layer().with_target(true).compact();
        registry.with(layer).try_init()?;
    }

    tracing::info!(service = %opts.service_name, "telemetry initialized");
    Ok(())
}

/// 记录启动横幅。
pub fn log_banner(banner: &StartupBanner) {
    tracing::info!(
        service = %banner.service,
        version = %banner.version,
        build_time = %banner.build_time,
        commit = %banner.commit,
        mode = %banner.mode,
        "startup banner"
    );
    // 同时打到 stdout，便于容器日志收集
    println!("{banner}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_serializes_to_json() {
        let b = StartupBanner {
            service: "paperclip-server".into(),
            version: "0.1.0".into(),
            build_time: "2026-08-02T00:00:00Z".into(),
            commit: "dev".into(),
            mode: "development",
        };
        let json = serde_json::to_value(&b).unwrap();
        assert_eq!(json["service"], "paperclip-server");
        assert_eq!(json["mode"], "development");
    }

    #[test]
    fn default_options_have_expected_values() {
        let opts = TelemetryOptions::default();
        assert_eq!(opts.default_level, tracing::Level::INFO);
        assert!(opts.json_output);
    }
}

#[cfg(feature = "otlp")]
pub mod otlp;

#[cfg(feature = "otlp")]
pub use otlp::{build_otlp_provider, install_global, OtlpConfig};
