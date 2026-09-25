//! composio 服务 —— 上游 `integrations/composio/service.go`（709 行）+ `dispatch.go`
//! （M8-0 anchor 建桩，**实现归 M8-6**）。
//!
//! 一次连接生命周期（M8-6 的 `DoD`，`docs/61` §6.5）：connect init（签 state + 生成会话 URL）
//! → callback（验 state + 落 `user_composio_connection`）→ 列表 / toolkit 目录 → 断开。
//!
//! ⚠️ 门 ⑩ 预飞把本文件排在 600–800 行（`docs/61` §6.3）⇒ toolkit / auth-config 解析必须
//! 分到 `catalog.rs`。

use crate::client::ComposioClient;
use crate::state::StateSigner;

/// composio 面统一错误（**不得**含 API key / state secret / bearer）。
#[derive(Debug, thiserror::Error)]
pub enum ComposioError {
    #[error("composio: transport error: {0}")]
    Transport(String),
    #[error("composio: unauthorized")]
    Unauthorized,
    #[error("composio: not configured")]
    NotConfigured,
    #[error("composio: malformed payload: {0}")]
    Malformed(String),
}

/// composio 的**装配配置**（四个「未配置」条件缺一即不装配，`docs/61` §2.5）。
///
/// 手写 `Debug`（脱敏）：只暴露「配了哪些」。
#[derive(Clone, Default)]
pub struct ComposioConfig {
    /// `COMPOSIO_API_KEY`。
    pub api_key: Option<String>,
    /// `COMPOSIO_STATE_SECRET` 或由 `JWT_SECRET` 派生。
    pub state_secret: Option<String>,
    /// `COMPOSIO_CALLBACK_BASE_URL` 或 `MULTICA_PUBLIC_URL`。
    pub callback_base_url: Option<String>,
    /// feature flag（`mc-feature-flags`）—— 与密钥的组合，两层都过才装配。
    pub feature_enabled: bool,
    /// API base（替身接缝）。
    pub api_base: Option<String>,
}

impl ComposioConfig {
    /// 四个条件缺一即 `false`（`docs/61` §2.5 的 composio 行）。
    pub fn is_configured(&self) -> bool {
        self.feature_enabled
            && self.api_key.is_some()
            && self.state_secret.is_some()
            && self.callback_base_url.is_some()
    }

    /// 缺了哪些条件（诊断；**不**回显值）。
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.feature_enabled {
            missing.push("feature_flag");
        }
        if self.api_key.is_none() {
            missing.push("COMPOSIO_API_KEY");
        }
        if self.state_secret.is_none() {
            missing.push("COMPOSIO_STATE_SECRET|JWT_SECRET");
        }
        if self.callback_base_url.is_none() {
            missing.push("COMPOSIO_CALLBACK_BASE_URL|MULTICA_PUBLIC_URL");
        }
        missing
    }
}

impl std::fmt::Debug for ComposioConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposioConfig")
            .field("configured", &self.is_configured())
            .field("missing", &self.missing())
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

/// composio 服务（5 条 HTTP 路由 + task 派发服务的共用生产者）。
pub struct ComposioService {
    client: ComposioClient,
    signer: StateSigner,
    config: ComposioConfig,
}

impl ComposioService {
    /// 装配（**anchor 期是桩**：由 M8-6 在 `is_configured()==false` 时返回一个
    /// `enabled()==false` 的空服务，而不是 panic）。
    pub fn new(_config: ComposioConfig) -> Self {
        todo!("M8-6：装配 composio 服务（docs/61 §4.1 的 M8-6 行）")
    }

    /// 是否已装配（缺任一条件 ⇒ `false` ⇒ 4 条会话路由 503）。
    pub fn enabled(&self) -> bool {
        self.config.is_configured()
    }

    pub fn client(&self) -> &ComposioClient {
        &self.client
    }

    pub fn signer(&self) -> &StateSigner {
        &self.signer
    }
}

impl std::fmt::Debug for ComposioService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposioService")
            .field("enabled", &self.enabled())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_requires_all_four_conditions() {
        let mut config = ComposioConfig::default();
        assert!(!config.is_configured());
        assert_eq!(config.missing().len(), 4);

        config.feature_enabled = true;
        config.api_key = Some("k".into());
        config.state_secret = Some("s".into());
        assert!(!config.is_configured(), "callback base is still missing");
        config.callback_base_url = Some("https://example.test".into());
        assert!(config.is_configured());
        assert!(config.missing().is_empty());

        // Debug 不回显密钥值。
        assert!(!format!("{config:?}").contains("\"k\""));
    }
}
