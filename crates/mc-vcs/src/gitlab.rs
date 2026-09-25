//! GitLab 的 provider 适配器 —— **M8-0 anchor 建桩，填充归 M8-2**
//! （`LUM-1799` / `docs/61-M8-PLAN.md` §3.3）。
//!
//! 上游对应物是 `internal/integrations/vcs/gitlab.go`（248 行）：
//! `X-Gitlab-Token` 的**明文 token 比较**（本仓刻意收紧为常量时间，见
//! [`crate::signature::verify_plaintext_token`]）、`X-Gitlab-Event` 分类、
//! GitLab "merge request" → 同一 `PullRequestEvent` 的映射、`/api/v4/user` 的 token 校验。
//!
//! ⚠️ anchor 期本文件是**桩**（所有方法 `todo!()`）。

use async_trait::async_trait;
use http::HeaderMap;
use mc_core::vcs::VcsProviderKind;

use crate::events::{Account, CIStatusEvent, EventKind, PullRequestEvent};
use crate::provider::{Provider, VcsError};
use crate::registry::Registry;

/// GitLab 适配器。
#[derive(Debug, Clone, Copy, Default)]
pub struct GitLabProvider;

#[async_trait]
impl Provider for GitLabProvider {
    fn kind(&self) -> VcsProviderKind {
        VcsProviderKind::GitLab
    }

    fn event_kind(&self, _headers: &HeaderMap) -> EventKind {
        todo!("M8-2：按 X-Gitlab-Event 分类（Merge Request Hook / Pipeline Hook / …）")
    }

    fn verify_signature(&self, _secret: &str, _headers: &HeaderMap, _body: &[u8]) -> bool {
        todo!("M8-2：X-Gitlab-Token 明文常量时间比较（signature::verify_plaintext_token）")
    }

    fn parse_pull_request(&self, _body: &[u8]) -> Result<PullRequestEvent, VcsError> {
        todo!("M8-2：解析 GitLab merge request 载荷（映射进同一 PullRequestEvent）")
    }

    fn parse_ci_status(&self, _body: &[u8]) -> Result<CIStatusEvent, VcsError> {
        todo!("M8-2：解析 GitLab pipeline / commit-status 载荷")
    }

    async fn validate_token(&self, _instance_url: &str, _token: &str) -> Result<Account, VcsError> {
        todo!("M8-2：GET {{instance_url}}/api/v4/user，401/403 ⇒ ErrUnauthorized")
    }
}

/// 把 GitLab 适配器注册进 registry。
///
/// **anchor 期是空体**：M8-2 在这里 `registry.register(Arc::new(GitLabProvider))`。
pub fn register(_registry: &mut Registry) {}
