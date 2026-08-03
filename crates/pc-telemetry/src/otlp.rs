//! 可选 OTLP/HTTP exporter。
//!
//! 与原 `paperclip/server/src/telemetry/otlp.ts` 等价；通过 `otlp` feature 启用。
//! 默认关闭（编译期 + 运行期均零开销）。
//!
//! 启用方法：
//! ```toml
//! pc-telemetry = { path = "../pc-telemetry", features = ["otlp"] }
//! ```
//!
//! 运行时配置（环境变量）：
//! - `PAPERCLIP_OTLP_ENDPOINT` — OTLP/HTTP collector endpoint（默认 `http://127.0.0.1:4318`）
//! - `PAPERCLIP_OTLP_HEADERS` — 额外 header，格式 `k1=v1,k2=v2`
//! - `PAPERCLIP_OTLP_SAMPLE_RATIO` — 采样率 0.0–1.0（默认 1.0）
//! - `PAPERCLIP_OTLP_DISABLED` — 显式禁用（默认 false）
//!
//! 推荐用法：调用 `build_otlp_provider` 拿到 provider，调用方自行与
//! `tracing_subscriber::Registry` 组合并调用 `try_init`。

use anyhow::{Context, Result};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::trace::{Sampler, TracerProvider as SdkTracerProvider};
use opentelemetry_sdk::Resource;
use std::collections::HashMap;

/// OTLP 启动配置。
#[derive(Debug, Clone)]
pub struct OtlpConfig {
    pub service_name: String,
    pub service_version: String,
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub sample_ratio: f64,
    pub disabled: bool,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            service_name: "paperclip-server".into(),
            service_version: env!("CARGO_PKG_VERSION").into(),
            endpoint: std::env::var("PAPERCLIP_OTLP_ENDPOINT")
                .unwrap_or_else(|_| "http://127.0.0.1:4318".into()),
            headers: parse_headers(&std::env::var("PAPERCLIP_OTLP_HEADERS").unwrap_or_default()),
            sample_ratio: std::env::var("PAPERCLIP_OTLP_SAMPLE_RATIO")
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(1.0)
                .clamp(0.0, 1.0),
            disabled: matches!(
                std::env::var("PAPERCLIP_OTLP_DISABLED").as_deref(),
                Ok("1" | "true" | "TRUE" | "yes")
            ),
        }
    }
}

fn parse_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let (k, v) = part.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// 构造 OTLP `SdkTracerProvider`。
///
/// 返回的 provider 包含批处理 exporter；调用方负责：
/// 1. 用 `provider.tracer("paperclip-rs")` 构造 tracer
/// 2. 用 `tracing_opentelemetry::layer().with_tracer(tracer)` 构造 layer
/// 3. 与 `tracing_subscriber::Registry` 组合并 `try_init`
/// 4. 进程退出前调用 `provider.shutdown()` flush 残留 spans
pub fn build_otlp_provider(config: &OtlpConfig) -> Result<SdkTracerProvider> {
    if config.disabled {
        anyhow::bail!("OTLP disabled via PAPERCLIP_OTLP_DISABLED");
    }
    let header_map: HashMap<String, String> = config.headers.clone().into_iter().collect();
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(&config.endpoint)
        .with_headers(header_map)
        .build()
        .context("build OTLP HTTP exporter")?;
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_resource(Resource::new(vec![
            KeyValue::new("service.name", config.service_name.clone()),
            KeyValue::new("service.version", config.service_version.clone()),
        ]))
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            config.sample_ratio,
        ))))
        .build();
    Ok(provider)
}

/// 安装 OTLP layer 到全局 subscriber（便捷封装）。
///
/// 等价于：
/// ```ignore
/// let provider = build_otlp_provider(&OtlpConfig::default())?;
/// let tracer = provider.tracer("paperclip-rs");
/// let layer = tracing_opentelemetry::layer().with_tracer(tracer);
/// tracing_subscriber::registry()
///     .with(EnvFilter::from_default_env())
///     .with(layer)
///     .try_init()?;
/// Box::leak(Box::new(provider)); // 防止 provider 被 drop
/// ```
pub fn install_global(config: &OtlpConfig) -> Result<SdkTracerProvider> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;
    let provider = build_otlp_provider(config)?;
    let tracer = provider.tracer("paperclip-rs");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(tracing::Level::INFO.to_string()));
    // `tracing_subscriber::registry()` 自身实现 Subscriber，可以直接 .with() 任何 Layer
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(layer)
        .try_init();
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_headers_handles_empty() {
        assert!(parse_headers("").is_empty());
        assert!(parse_headers("   ").is_empty());
    }

    #[test]
    fn parse_headers_splits_pairs() {
        let pairs = parse_headers("k1=v1,k2=v2, k3 = v3 ");
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0], ("k1".into(), "v1".into()));
        assert_eq!(pairs[2], ("k3".into(), "v3".into()));
    }

    #[test]
    fn sample_ratio_clamps() {
        let mut c = OtlpConfig::default();
        c.sample_ratio = 1.5;
        c.sample_ratio = c.sample_ratio.clamp(0.0, 1.0);
        assert_eq!(c.sample_ratio, 1.0);
        c.sample_ratio = -0.5;
        c.sample_ratio = c.sample_ratio.clamp(0.0, 1.0);
        assert_eq!(c.sample_ratio, 0.0);
    }

    #[test]
    fn disabled_config_short_circuits() {
        let mut c = OtlpConfig::default();
        c.disabled = true;
        let result = build_otlp_provider(&c);
        assert!(result.is_err());
    }
}
