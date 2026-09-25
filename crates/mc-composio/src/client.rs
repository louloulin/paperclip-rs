//! composio SDK 的 HTTP 客户端 —— 上游 `pkg/composio` 的 `Options{APIKey}` 面
//! （M8-0 anchor 落形状与 `api_base` 接缝，**实现归 M8-6**）。
//!
//! # `api_base` 是离线替身的接缝（R-M8-1，`docs/61` §4.2）
//!
//! 上游 composio SDK 的 API base 可注入 ⇒ 本地 HTTP 替身按 `toolkits` / `auth_configs` /
//! `connected_accounts` 的形状答，端到端断言链（connect → callback → 落库 → toolkits）
//! 不需要真连 composio。
//!
//! # 凭据纪律
//!
//! `COMPOSIO_API_KEY` 是 `x-api-key` 头的值，**不派生 `Debug`**（手写脱敏）。

use std::time::Duration;

use crate::service::ComposioError;

/// composio API 的默认 base（上游 `pkg/composio` 的默认端点 —— 本仓只钉形状，
/// 若上游 SDK 以后改端点，改这一处）。
pub const DEFAULT_API_BASE: &str = "https://backend.composio.dev/api/v3";

/// 用 `x-api-key` 认证的 composio 客户端。
///
/// ⚠️ anchor 期所有方法 `todo!()`（实现归 M8-6）。
pub struct ComposioClient {
    /// `COMPOSIO_API_KEY`；未配置为 `None`（⇒ 整体不装配）。
    pub(crate) api_key: Option<String>,
    /// API base（替身接缝）。
    pub(crate) api_base: String,
    /// 连接池。
    #[allow(dead_code)]
    pub(crate) http: reqwest::Client,
}

impl ComposioClient {
    /// 构造（未配置 api key 也可构造，只是 [`ComposioClient::enabled`] 为 `false`）。
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key,
            api_base: DEFAULT_API_BASE.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
        }
    }

    /// 自定义 base（**测试/替身唯一入口**）。
    #[must_use]
    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into();
        self
    }

    /// 是否配了 API key。
    pub fn enabled(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// 列出 toolkit 目录 —— **anchor 期是桩**，实现归 M8-6。
    ///
    /// # Errors
    ///
    /// 传输失败 / 401 / 载荷非法时返回 [`ComposioError`]。
    pub async fn list_toolkits(
        &self,
    ) -> Result<Vec<mc_core::composio::ComposioToolkit>, ComposioError> {
        todo!("M8-6：GET {{api_base}}/toolkits（docs/61 §4.2 的 M8-6 行）")
    }
}

impl std::fmt::Debug for ComposioClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposioClient")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_debug_is_redacted_and_base_is_overridable() {
        let client = ComposioClient::new(Some("ak_live_secret".into()));
        assert!(client.enabled());
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("ak_live_secret"));
        assert_eq!(
            client.with_api_base("http://127.0.0.1:9").api_base(),
            "http://127.0.0.1:9"
        );
        assert!(!ComposioClient::new(None).enabled());
    }
}
