//! 跨宿主的**端口**（trait）—— `PrRefreshPort` 与 GitHub App 配置形状。
//!
//! # 为什么端口先定（`docs/61` §5 / §2.2）
//!
//! `ghsnapshot::Manager` 的宿主是 `apps/mc-server`（长期后台 worker：worker 池 + TTL
//! sweeper + 限流暂停），而 webhook handler / 页面访问的宿主是 `mc-http`。两者**不互相依赖**：
//! `mc-http` 只持有一个 `Arc<dyn PrRefreshPort>`，由 `apps/mc-server` 在装配时
//! `Arc::new(Manager::new(...))` 注入。于是「入站 webhook 能触发快照刷新」不需要把
//! worker 塞进请求路径，也不需要 `mc-http` 依赖后台装配。
//!
//! anchor 期 `port.rs` 定死 trait 与请求形状；实现在 M8-5。

use std::sync::Arc;

use mc_core::id::Id;
use serde::{Deserialize, Serialize};

/// 一次 PR 快照刷新的触发原因（上游三个触发面，`docs/61` §4.2 的 M8-5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshReason {
    /// 入站 webhook（M8-4）。
    Webhook,
    /// 用户打开 PR 卡片（页面访问）。
    PageView,
    /// TTL sweeper 的到期扫描。
    TtlSweep,
}

/// 一次刷新的定位键（幂等与去重都按它）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrRefreshRequest {
    pub workspace_id: Id,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: i32,
    /// 期望的 head SHA（webhook 知道时给出；不一致则丢弃结果）。
    pub head_sha: Option<String>,
    pub reason: RefreshReason,
}

/// 快照刷新端口（实现在 M8-5 的 `ghsnapshot::refresh::Manager`）。
///
/// ⚠️ 三个方法都**不得阻塞调用方的 ACK 路径**（上游把入队与执行分离）：
/// `enqueue` 只入队，真正的 GraphQL 调用在 worker 里。
pub trait PrRefreshPort: Send + Sync {
    /// 功能是否已配置（缺 App 私钥 ⇒ `false`，页面访问不触发刷新）。
    fn enabled(&self) -> bool;

    /// 入队一次刷新（webhook / TTL 用；不关心是否已有一单在飞）。
    fn enqueue(&self, request: PrRefreshRequest);

    /// 页面访问触发的**有节制**入队：已有在飞 / 刚刷过则返回 `false`
    /// （上游 `maybeEnqueueOnView` 的口径）。
    fn maybe_enqueue_on_view(&self, request: PrRefreshRequest) -> bool;
}

/// 供装配点使用的共享端口别名。
pub type SharedPrRefresh = Arc<dyn PrRefreshPort>;

/// M8-5（`LUM-1802`）：[`crate::ghsnapshot::refresh::Manager`] 的端口实现 —— **anchor 指定
/// 的落点**（`docs/32` §21.1）。
///
/// # 三条语义（与上游 `Manager.Enqueue` / `MaybeEnqueueOnView` 的对应）
///
/// 1. **不阻塞 ACK 路径**：两个方法都只做「纯内存记账 + `try_send`」，真 GraphQL 调用在 worker 里；
///    队列满 ⇒ 丢弃（留给 TTL sweep / 下一次事件兜底），**绝不在请求路径上摸数据库**。
/// 2. **去重键 = 端口请求键**：上游的定位键是 `(installation, owner, repo, number)`，而本仓
///    端口形状（anchor 冻结）带的是 `(workspace_id, owner, repo, number)` ⇒ 入队去重按后者、
///    **执行侧的 `installation` 地址串行**按前者（worker 里解析一次，见
///    `ghsnapshot::refresh` 的偏离 D2）。M8-4 的 `webhook/mirror.rs` 已把「每个绑定各入队一次」
///    登记为 D4（同一条语义）。
/// 3. **`maybe_enqueue_on_view` 的返回值 = 「受理入队」**，不是「一定会抓」：view TTL
///    （上游由 handler 传入 `fetchedAt`/`hasFetched`）在解析阶段判（偏离 D3）。
impl PrRefreshPort for crate::ghsnapshot::refresh::Manager {
    fn enabled(&self) -> bool {
        crate::ghsnapshot::refresh::Manager::enabled(self)
    }

    fn enqueue(&self, request: PrRefreshRequest) {
        let _ = crate::ghsnapshot::refresh::Manager::enqueue_request(self, &request);
    }

    fn maybe_enqueue_on_view(&self, request: PrRefreshRequest) -> bool {
        crate::ghsnapshot::refresh::Manager::maybe_enqueue_request_on_view(self, &request)
    }
}

/// 「未配置」的空实现：`enabled() == false`、入队是 no-op、页面访问返回 `false`。
///
/// 用途有两个（`docs/61` §6.5 的 M8-0 专属 `DoD` 点名要它）：① 部署没有 App 私钥时的
/// **诚实退化**（不是假装刷新成功）；② trait 形状用例里构造 `Arc<dyn PrRefreshPort>`
/// 不需要真起 worker。
#[derive(Debug, Clone, Copy, Default)]
pub struct DisabledPrRefresh;

impl PrRefreshPort for DisabledPrRefresh {
    fn enabled(&self) -> bool {
        false
    }

    fn enqueue(&self, _request: PrRefreshRequest) {
        // 未配置 ⇒ 静默丢弃（上游 nil client 的语义）。
    }

    fn maybe_enqueue_on_view(&self, _request: PrRefreshRequest) -> bool {
        false
    }
}

/// GitHub App 的**四类部署密钥**的读取口形状（`docs/61` §2.4）。
///
/// ⚠️ 这不是 env 读取口 —— 生产读取口是 `mc_http::state` 的 `github` 字段
/// （`docs/61` §3.2 的共享件纪律：anchor 只在 `AppState::new` 内读 env）。本结构是
/// **装配参数的载体**，由 `apps/mc-server` 构造后交给 `ghsnapshot::Manager`。
///
/// 手写 `Debug`（脱敏）：只暴露「配了哪些」，不暴露值。
#[derive(Clone, Default)]
pub struct GithubAppConfig {
    /// `GITHUB_APP_ID`（JWT 的 `iss`）。
    pub app_id: Option<String>,
    /// `GITHUB_APP_PRIVATE_KEY`（PEM，PKCS#8）。
    pub private_key_pem: Option<String>,
    /// `GITHUB_WEBHOOK_SECRET`（webhook HMAC + state 签名，上游**刻意复用同一个** secret）。
    pub webhook_secret: Option<String>,
    /// `GITHUB_APP_SLUG`（安装引导 URL）。
    pub app_slug: Option<String>,
    /// REST / GraphQL 的 base（**离线替身的接缝**；默认 `https://api.github.com`）。
    pub api_base: String,
}

impl GithubAppConfig {
    /// 默认 base（上游 `githubAPIBase` / `defaultAPIBase` 的字面量）。
    pub const DEFAULT_API_BASE: &'static str = "https://api.github.com";

    /// 「能连接」的判据：App id + 私钥都配了（`isGitHubConfigured()` 的本地对应物之一）。
    pub fn is_app_configured(&self) -> bool {
        self.app_id.is_some() && self.private_key_pem.is_some()
    }

    /// 「webhook 可验签」的判据。
    pub fn is_webhook_configured(&self) -> bool {
        self.webhook_secret.is_some()
    }
}

impl std::fmt::Debug for GithubAppConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubAppConfig")
            .field("app_id", &self.app_id)
            .field(
                "private_key_pem",
                &self.private_key_pem.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "webhook_secret",
                &self.webhook_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("app_slug", &self.app_slug)
            .field("api_base", &self.api_base)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_port_degrades_honestly() {
        let port: Arc<dyn PrRefreshPort> = Arc::new(DisabledPrRefresh);
        assert!(!port.enabled());
        // 入队不 panic、不假装成功。
        port.enqueue(PrRefreshRequest {
            workspace_id: Id::new(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
            pr_number: 1,
            head_sha: None,
            reason: RefreshReason::Webhook,
        });
        assert!(!port.maybe_enqueue_on_view(PrRefreshRequest {
            workspace_id: Id::new(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
            pr_number: 1,
            head_sha: None,
            reason: RefreshReason::PageView,
        }));
    }

    #[test]
    fn app_config_debug_never_echoes_secrets() {
        let config = GithubAppConfig {
            app_id: Some("1".into()),
            private_key_pem: Some("-----BEGIN PRIVATE KEY-----\nsecret\n".into()),
            webhook_secret: Some("super-secret".into()),
            app_slug: Some("my-app".into()),
            api_base: GithubAppConfig::DEFAULT_API_BASE.into(),
        };
        let rendered = format!("{config:?}");
        // 只断言**值**不出现（字段名 `webhook_secret` 本身可以出现）。
        assert!(!rendered.contains("super-secret"));
        assert!(!rendered.contains("BEGIN PRIVATE KEY"));
        assert!(rendered.contains("<redacted>"));
    }
}
