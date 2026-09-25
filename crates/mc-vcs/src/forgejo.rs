//! Forgejo/Gitea（wire 同一套）的 provider 适配器 —— **M8-0 anchor 建桩，填充归 M8-2**
//! （`LUM-1799` / `docs/61-M8-PLAN.md` §3.3）。
//!
//! 上游对应物是 `internal/integrations/vcs/forgejo.go`（257 行）：
//! `X-Gitea-Signature` / `X-Forgejo-Signature` 的 HMAC-SHA256 验签（用
//! [`crate::signature::verify_hmac_sha256_hex`]）、PR / commit-status 载荷解析、
//! `/api/v1/user` 的 token 校验。
//!
//! ⚠️ anchor 期本文件是**桩**：结构体已可构造（形状用例要用 `Arc<dyn Provider>`），
//! 但所有方法 `todo!()` ⇒ **调用它一定 panic**。这是刻意的：让它静默返回会让
//! 「provider 还没接上」变成运行期才发现的事。

use async_trait::async_trait;
use http::HeaderMap;
use mc_core::vcs::VcsProviderKind;

use crate::events::{Account, CIStatusEvent, EventKind, PullRequestEvent};
use crate::provider::{Provider, VcsError};
use crate::registry::Registry;

/// Forgejo（以及 wire 兼容的 Gitea）适配器。
#[derive(Debug, Clone, Copy, Default)]
pub struct ForgejoProvider;

#[async_trait]
impl Provider for ForgejoProvider {
    fn kind(&self) -> VcsProviderKind {
        VcsProviderKind::Forgejo
    }

    fn event_kind(&self, _headers: &HeaderMap) -> EventKind {
        todo!("M8-2：按 X-Gitea-Event / X-Forgejo-Event 分类（docs/61 §4.1 的 M8-2 行）")
    }

    fn verify_signature(&self, _secret: &str, _headers: &HeaderMap, _body: &[u8]) -> bool {
        todo!("M8-2：HMAC-SHA256 验签（signature::verify_hmac_sha256_hex）")
    }

    fn parse_pull_request(&self, _body: &[u8]) -> Result<PullRequestEvent, VcsError> {
        todo!("M8-2：解析 Forgejo PR 载荷")
    }

    fn parse_ci_status(&self, _body: &[u8]) -> Result<CIStatusEvent, VcsError> {
        todo!("M8-2：解析 Forgejo commit-status 载荷")
    }

    async fn validate_token(&self, _instance_url: &str, _token: &str) -> Result<Account, VcsError> {
        todo!("M8-2：GET {{instance_url}}/api/v1/user，401/403 ⇒ ErrUnauthorized")
    }
}

/// 把 Forgejo 适配器注册进 registry。
///
/// **anchor 期是空体**（`docs/61` §5）：M8-2 在这里
/// `registry.register(Arc::new(ForgejoProvider))`，并让 Gitea 复用同一 wire 实现
/// （但注册成**两个** kind，见 [`VcsProviderKind::Gitea`]）。
pub fn register(_registry: &mut Registry) {}
