//! M8 anchor（`LUM-1797`）：**后台宿主位** —— GitHub PR 快照刷新的装配点与停机句柄。
//!
//! # 这一片解决什么问题
//!
//! `mc_vcs_github::ghsnapshot::Manager` 是**长期后台 worker**（worker 池 + TTL sweeper +
//! 限流暂停 + 单地址串行），上游 `handler.go:513-519` 在 `NewHandler` 里造它、由
//! `cmd/server/main.go` 调 `h.PRRefresh.Start(ctx)`（`docs/61` §2.6）⇒ 它的宿主必须是
//! `apps/mc-server`，而不是 `mc-http` 的一层中间件（worker 的重试/退避/限流不属于请求面）。
//!
//! # 为什么宿主在 `apps/mc-server`（与 M5-9 / M7-0 同造型）
//!
//! M5-9 已把「后台任务宿主」定在这里（`apps/mc-server/src/scheduler/`），M7-0 又把渠道
//! 长连接宿主放进来（`apps/mc-server/src/channels.rs`）⇒ 本文件是**第三个**同类宿主，
//! 进**同一条停机链**。
//!
//! # 停机顺序（`docs/61` §2.6，固定，不许改）
//!
//! **先停渠道连接 → 再停 PR 刷新 → 再停调度器 → 最后停 actor**：
//! 渠道连接挂着不退会让 graceful shutdown 永远等在那里；PR 刷新的 worker 有在飞的 GraphQL
//! 请求要先收尾；调度器的在跑 handler 随后；actor 池最后停。
//!
//! # 装配判据 = **App 私钥存在**（`docs/61` §2.4 / §2.5）
//!
//! 缺 `GITHUB_APP_ID` / `GITHUB_APP_PRIVATE_KEY` ⇒ `ghsnapshot` 整体不装配
//! （`Client::disabled()`），页面访问与 webhook 都不会触发刷新 —— 这是**正常**路径
//! （「能连接」与「能浏览仓库」是两个独立判据）。
//!
//! # 锚点期的空跑（**显式**，不是静默失效）
//!
//! [`mc_vcs_github::ghsnapshot::Manager::start`] 的实现归 M8-5，anchor 期仍是 `todo!()`
//! ⇒ [`start`] **不调用它**；有密钥时只打一条 warn 并明说"宿主未接线"。绝不假装刷新已接上
//! （那会让「PR 快照不更新」变成运行期才发现的静默失效，正是 `docs/37` 反复登记的那类事故）。

use std::sync::Arc;

use mc_http::state::integrations::GithubKeys;
use mc_vcs_github::ghsnapshot::{Client, Manager};
use mc_vcs_github::port::GithubAppConfig;

/// 代码与制品面的装配结果：PR 刷新宿主 + 已配置判据。
///
/// 句柄是空的（anchor 期没有真 worker），但**类型先定死**：`main.rs` 的停机链是
/// 启动/停止顺序的唯一实现点，不该等到 M8-5 才改写。
pub struct IntegrationHandles {
    pr_refresh: Option<Arc<Manager>>,
    wired: bool,
    app_configured: bool,
}

impl std::fmt::Debug for IntegrationHandles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IntegrationHandles")
            .field("pr_refresh", &self.pr_refresh.is_some())
            .field("wired", &self.wired)
            .field("app_configured", &self.app_configured)
            .finish()
    }
}

impl IntegrationHandles {
    /// PR 刷新宿主（`None` = 未装配 / 锚点期）。
    pub fn pr_refresh(&self) -> Option<&Arc<Manager>> {
        self.pr_refresh.as_ref()
    }

    /// 宿主端口是否已接线（`false` = 锚点期，或 M8-5 还没接）。
    pub fn is_wired(&self) -> bool {
        self.wired
    }

    /// App 凭据是否配置（诊断用）。
    pub fn is_app_configured(&self) -> bool {
        self.app_configured
    }

    /// 停机：收掉 PR 刷新 worker（anchor 期是 no-op）。
    ///
    /// 未来在这里 `manager.shutdown().await`；顺序由 `main.rs` 固定为
    /// 「先停渠道连接 → 再停 PR 刷新 → 再停调度器 → 最后停 actor」。
    pub async fn shutdown(self) {
        if let Some(manager) = self.pr_refresh {
            manager.shutdown().await;
        }
    }
}

/// 装配并启动代码与制品面的后台宿主（`main.rs` 在**渠道宿主之后、调度器之前**调用）。
///
/// `keys` = GitHub App 的部署密钥读取口（`mc_http::state::integrations::GithubKeys`）。
///
/// **不**返回错误：装配期的失败（私钥 PEM 非法等）在 M8-5 起 worker 时才出现，
/// 而上游语义是「键在但非法 ⇒ 打一条可操作的 error、整体退化」，不是进程起不来。
pub fn start(keys: &GithubKeys) -> IntegrationHandles {
    let app_configured = keys.is_app_configured();
    let webhook_configured = keys.is_webhook_configured();

    if !app_configured {
        // 缺 App 凭据 ⇒ 整体不装配（**正常**路径）。
        tracing::info!(
            webhook_configured,
            "github app credentials are not configured; PR snapshot refresh disabled"
        );
        return IntegrationHandles {
            pr_refresh: None,
            wired: false,
            app_configured: false,
        };
    }

    // 有凭据但宿主未接线（M8-5）⇒ 只 warn + 明说，不起 worker。
    tracing::warn!(
        webhook_configured,
        "github app credentials are set but the snapshot host is not wired yet (M8-5); \
         no PR refresh worker will be started"
    );
    // 这里**故意不**构造真 Client/Manager：`Manager::start` 仍是 `todo!()`，
    // 调用它会让"配了密钥"的部署 panic（比"没接上"更糟，docs/61 §2.6）。
    let _ = (Client::disabled(), GithubAppConfig::DEFAULT_API_BASE);
    IntegrationHandles {
        pr_refresh: None,
        wired: false,
        app_configured: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 无 App 凭据 ⇒ 不装配、不报错。
    #[test]
    fn no_app_keys_means_nothing_is_assembled() {
        let handles = start(&GithubKeys::default());
        assert!(!handles.is_app_configured());
        assert!(!handles.is_wired());
        assert!(handles.pr_refresh().is_none());
    }

    /// 有凭据但宿主未接线 ⇒ 明说未接线（**不**假装刷新已接、**不** panic）。
    #[test]
    fn app_keys_without_host_report_unwired() {
        let keys = GithubKeys::from_env_with(|name| match name {
            "GITHUB_APP_ID" => Some("123".to_string()),
            "GITHUB_APP_PRIVATE_KEY" => Some("-----BEGIN PRIVATE KEY-----\nx\n".to_string()),
            _ => None,
        });
        let handles = start(&keys);
        assert!(handles.is_app_configured());
        assert!(!handles.is_wired());
        assert!(handles.pr_refresh().is_none(), "宿主未接线 ⇒ 不起 worker");
    }
}
