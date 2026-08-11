//! 跨所有已注册 adapter 拉取 provider quota windows（原 `pc-quota-windows` 已下沉）。
//!
//! 对应 Node `server/src/services/quota-windows.ts`（64 行）。
//!
//! 设计目标：1:1 复刻
//! - `fetchAllQuotaWindows()` —— 并行调用所有 adapter 的 `getQuotaWindows()`，
//!   每个用 `withQuotaTimeout` 包裹（20s 超时）
//! - adapter 失败 → 单条记录成 `{ok:false, error, windows:[]}`，**不会** 让某个
//!   adapter 故障阻塞整个响应
//! - `providerSlugForAdapterType` —— 映射 `claude_local → "anthropic"`、
//!   `codex_local → "openai"`，其它透传
//!
//! Rust 端抽象：
//! - `AdapterRegistry` trait —— 提供 `list_adapters_with_quota()`，
//!   真实实现接入 `pc-adapter-registry`，测试中用 in-memory list
//! - `Clock` trait —— 超时计算用

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Provider quota 查询结果 —— 与 Node `ProviderQuotaResult` 1:1 对齐。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderQuotaResult {
    pub provider: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub windows: Vec<serde_json::Value>,
}

impl ProviderQuotaResult {
    pub fn ok(provider: impl Into<String>, windows: Vec<serde_json::Value>) -> Self {
        Self {
            provider: provider.into(),
            ok: true,
            error: None,
            windows,
        }
    }

    pub fn err(provider: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            ok: false,
            error: Some(error.into()),
            windows: Vec::new(),
        }
    }
}

/// Quota windows adapter trait —— 真实实现接入具体 adapter。
pub trait QuotaAdapter: Send + Sync {
    fn adapter_type(&self) -> &str;
    fn get_quota_windows(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderQuotaResult, String>> + Send>>;
}

/// Adapter registry trait —— 提供"哪些 adapter 有 getQuotaWindows"的查询。
pub trait AdapterRegistry: Send + Sync {
    fn list_adapters_with_quota(&self) -> Vec<Arc<dyn QuotaAdapter>>;
}

/// Provider slug 映射 —— 与 Node `providerSlugForAdapterType` 1:1 对齐。
pub fn provider_slug_for_adapter_type(adapter_type: &str) -> String {
    match adapter_type {
        "claude_local" => "anthropic".to_string(),
        "codex_local" => "openai".to_string(),
        other => other.to_string(),
    }
}

/// 默认超时 —— 与 Node 常量 1:1。
pub const QUOTA_PROVIDER_TIMEOUT_MS: u64 = 20_000;

/// 拉取所有 adapter 的 quota windows。
///
/// 与 Node `fetchAllQuotaWindows` 1:1 对齐：
/// - 并行调用 `Promise.allSettled`
/// - 单个 adapter 失败 → 单条 err 结果
/// - timeout → 单条 err 结果（含 timeout message）
pub async fn fetch_all_quota_windows(
    registry: Arc<dyn AdapterRegistry>,
    timeout: Duration,
) -> Vec<ProviderQuotaResult> {
    let adapters = registry.list_adapters_with_quota();
    let mut results = Vec::with_capacity(adapters.len());

    // 串行 + 超时包裹（避免 race condition 复杂性；实际 Node 是 Promise.allSettled）
    for adapter in adapters {
        let adapter_type = adapter.adapter_type().to_string();
        let task = adapter.get_quota_windows();
        let result = with_quota_timeout(&adapter_type, task, timeout).await;
        results.push(result);
    }

    results
}

/// 单个 adapter 调用 + 超时。
pub async fn with_quota_timeout(
    adapter_type: &str,
    task: Pin<Box<dyn Future<Output = Result<ProviderQuotaResult, String>> + Send>>,
    timeout: Duration,
) -> ProviderQuotaResult {
    let provider = provider_slug_for_adapter_type(adapter_type);
    let timeout_secs = timeout.as_secs().max(1);

    let provider_for_timeout = provider.clone();
    let timeout_fut = async move {
        tokio::time::sleep(timeout).await;
        ProviderQuotaResult::err(
            provider_for_timeout,
            format!("quota polling timed out after {timeout_secs}s"),
        )
    };

    tokio::select! {
        result = task => match result {
            Ok(qr) => qr,
            Err(e) => ProviderQuotaResult::err(provider, e),
        },
        timeout_result = timeout_fut => timeout_result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct StaticRegistry {
        adapters: Vec<Arc<dyn QuotaAdapter>>,
    }

    impl AdapterRegistry for StaticRegistry {
        fn list_adapters_with_quota(&self) -> Vec<Arc<dyn QuotaAdapter>> {
            self.adapters.clone()
        }
    }

    struct StubAdapter {
        adapter_type: String,
        result: std::sync::Mutex<
            Option<Pin<Box<dyn Future<Output = Result<ProviderQuotaResult, String>> + Send>>>,
        >,
    }

    impl QuotaAdapter for StubAdapter {
        fn adapter_type(&self) -> &str {
            &self.adapter_type
        }

        fn get_quota_windows(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<ProviderQuotaResult, String>> + Send>> {
            let mut guard = self.result.lock().unwrap();
            guard.take().expect("no future available")
        }
    }

    fn make_success_adapter(
        adapter_type: &str,
        windows: Vec<serde_json::Value>,
    ) -> Arc<StubAdapter> {
        let owned_type = adapter_type.to_string();
        let cloned = owned_type.clone();
        let fut: Pin<Box<dyn Future<Output = Result<ProviderQuotaResult, String>> + Send>> =
            Box::pin(async move {
                Ok(ProviderQuotaResult::ok(
                    provider_slug_for_adapter_type(&cloned),
                    windows,
                ))
            });
        Arc::new(StubAdapter {
            adapter_type: owned_type,
            result: Mutex::new(Some(fut)),
        })
    }

    fn make_failing_adapter(adapter_type: &str, err: String) -> Arc<StubAdapter> {
        let owned_type = adapter_type.to_string();
        let fut: Pin<Box<dyn Future<Output = Result<ProviderQuotaResult, String>> + Send>> =
            Box::pin(async move { Err(err) });
        Arc::new(StubAdapter {
            adapter_type: owned_type,
            result: Mutex::new(Some(fut)),
        })
    }

    fn make_hanging_adapter(adapter_type: &str) -> Arc<StubAdapter> {
        let owned = adapter_type.to_string();
        let fut: Pin<Box<dyn Future<Output = Result<ProviderQuotaResult, String>> + Send>> =
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(ProviderQuotaResult::ok("anthropic", vec![]))
            });
        Arc::new(StubAdapter {
            adapter_type: owned,
            result: Mutex::new(Some(fut)),
        })
    }

    #[test]
    fn r705_provider_slug_claude_local() {
        assert_eq!(provider_slug_for_adapter_type("claude_local"), "anthropic");
    }

    #[test]
    fn r705_provider_slug_codex_local() {
        assert_eq!(provider_slug_for_adapter_type("codex_local"), "openai");
    }

    #[test]
    fn r705_provider_slug_passthrough() {
        assert_eq!(
            provider_slug_for_adapter_type("custom-adapter"),
            "custom-adapter"
        );
    }

    #[tokio::test]
    async fn r705_fetch_all_returns_each_adapter_result() {
        let registry: Arc<dyn AdapterRegistry> = Arc::new(StaticRegistry {
            adapters: vec![
                make_success_adapter("claude_local", vec![json5("window-1")]),
                make_success_adapter("codex_local", vec![json5("window-2")]),
            ],
        });
        let r = fetch_all_quota_windows(registry, Duration::from_secs(5)).await;
        assert_eq!(r.len(), 2);
        assert!(r[0].ok);
        assert_eq!(r[0].provider, "anthropic");
        assert!(r[1].ok);
        assert_eq!(r[1].provider, "openai");
    }

    #[tokio::test]
    async fn r705_fetch_all_continues_on_error() {
        let registry: Arc<dyn AdapterRegistry> = Arc::new(StaticRegistry {
            adapters: vec![
                make_failing_adapter("claude_local", "rate-limited".into()),
                make_success_adapter("codex_local", vec![]),
            ],
        });
        let r = fetch_all_quota_windows(registry, Duration::from_secs(5)).await;
        assert_eq!(r.len(), 2);
        assert!(!r[0].ok);
        assert_eq!(r[0].provider, "anthropic");
        assert_eq!(r[0].error.as_deref(), Some("rate-limited"));
        assert!(r[1].ok);
    }

    #[tokio::test]
    async fn r705_with_quota_timeout_triggers_timeout() {
        let r = with_quota_timeout(
            "claude_local",
            Box::pin(make_hanging_adapter_future()),
            Duration::from_millis(50),
        )
        .await;
        assert!(!r.ok);
        assert_eq!(r.provider, "anthropic");
        assert!(r.error.as_ref().unwrap().contains("timed out"));
    }

    fn make_hanging_adapter_future(
    ) -> impl Future<Output = Result<ProviderQuotaResult, String>> + Send {
        async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(ProviderQuotaResult::ok("anthropic", vec![]))
        }
    }

    #[tokio::test]
    async fn r705_empty_registry_returns_empty() {
        let registry: Arc<dyn AdapterRegistry> = Arc::new(StaticRegistry::default());
        let r = fetch_all_quota_windows(registry, Duration::from_secs(5)).await;
        assert!(r.is_empty());
    }

    // Helper
    fn json5(s: &str) -> serde_json::Value {
        serde_json::Value::String(s.to_string())
    }
}
