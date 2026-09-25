//! `Provider` trait 与错误类型 —— 上游 `internal/integrations/vcs.Provider` 的逐字复刻
//! （M8-0 anchor 落**契约**，实现归 M8-2）。
//!
//! # 为什么 trait 只有 6 个方法（`docs/61` §2.2 被否决备选第 1 条）
//!
//! GitHub **没有**被塞进这个 trait：它的 installation token 是一次**带缓存的 OAuth 交换**，
//! 不是 `validate_token` 那种「拿 token 试一次」；强行并入会让 trait 长出
//! `RefreshableToken` 这条只有 1/3 实现者用得上的方法（接口污染）。这也是
//! `mc-vcs-github` 独立成 crate 的唯一硬理由。

use std::sync::Arc;

use async_trait::async_trait;
use http::HeaderMap;
use mc_core::vcs::VcsProviderKind;

use crate::events::{Account, CIStatusEvent, EventKind, PullRequestEvent};

/// VCS provider 适配器的错误（上游是裸 `error` + 哨兵 `ErrUnauthorized`）。
#[derive(Debug, thiserror::Error)]
pub enum VcsError {
    /// 实例拒绝了 token（HTTP 401/403）—— 调用侧把它翻成「连接时校验失败」，与传输/实例
    /// 错误区分开（上游哨兵 `ErrUnauthorized`）。
    #[error("vcs: token unauthorized")]
    Unauthorized,
    /// 该 provider 的 kind 未注册（上游 `For` 返回 `(nil,false)`）。
    #[error("vcs: unknown provider kind `{0}`")]
    UnknownProvider(String),
    /// 载荷不是本 provider 能解析的形状。
    #[error("vcs: malformed payload: {0}")]
    Malformed(String),
    /// HTTP / 实例错误（**不得**含凭据，`docs/61` §2.4）。
    #[error("vcs: instance error: {0}")]
    Instance(String),
}

/// 每个 provider 的适配器（上游 `Provider`）。
///
/// 实现是**无状态**的、构造便宜的；registry 每种 kind 持一个实例
/// （见 [`crate::registry::Registry`]）。全部方法都经过 `Arc<dyn Provider>` 可用 ⇒
/// trait 必须 object-safe（`async_trait` 负责把 `validate_token` 装箱）。
#[async_trait]
pub trait Provider: Send + Sync {
    /// provider 种类（registry 的键）。
    fn kind(&self) -> VcsProviderKind;

    /// 从**入站** webhook 的请求头分类事件（未建模的返回 [`EventKind::Other`]）。
    fn event_kind(&self, headers: &HeaderMap) -> EventKind;

    /// 用连接上存的 secret 校验原始 body。
    ///
    /// - Forgejo/Gitea：HMAC-SHA256（`X-Gitea-Signature` / `X-Forgejo-Signature`）；
    /// - GitLab：**明文 token 比较**（`X-Gitlab-Token`）。
    ///
    /// ⚠️ 两路都必须**常量时间**（`docs/61` §2.7 第 3 条）。
    fn verify_signature(&self, secret: &str, headers: &HeaderMap, body: &[u8]) -> bool;

    /// 解码 PR / merge request 载荷。
    fn parse_pull_request(&self, body: &[u8]) -> Result<PullRequestEvent, VcsError>;

    /// 解码 commit-status / pipeline 载荷。
    fn parse_ci_status(&self, body: &[u8]) -> Result<CIStatusEvent, VcsError>;

    /// 确认 token 对 `instance_url` 可用并返回已认证身份；401/403 映射为
    /// [`VcsError::Unauthorized`]。
    async fn validate_token(&self, instance_url: &str, token: &str) -> Result<Account, VcsError>;
}

/// 供 registry 与装配点使用的共享 trait 对象别名。
pub type SharedProvider = Arc<dyn Provider>;
