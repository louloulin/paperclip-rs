//! GitHub 入站 webhook 的验签与分发入口 —— 上游 `HandleGitHubWebhook`
//! （`github.go:1056`）+ `verifyWebhookSignature`（`github.go:1096`）
//! （M8-0 anchor 建桩，**实现归 M8-4**）。
//!
//! # 凭据与路由口径（`docs/61` §1.5 / §2.5）
//!
//! `POST /api/webhooks/github` 是**公开块**路由（不挂会话 middleware），凭据是
//! `GITHUB_WEBHOOK_SECRET` + `X-Hub-Signature-256`（HMAC-SHA256）。缺密钥 / 验签失败 ⇒
//! **401**（**不是** 404：路由必须在，鉴权在 handler 内）。
//!
//! # 幂等
//!
//! 同一 webhook 重投 2 次只插 1 行 PR（M8-4 的 `DoD`，`docs/61` §6.5）。

use http::HeaderMap;

use crate::payload::GithubEventKind;

/// 校验 `X-Hub-Signature-256` —— **anchor 期是桩**，实现归 M8-4。
///
/// 契约：常量时间；`secret` 空 ⇒ 必失败（**不得**短路成 true）；非法十六进制 ⇒ `false`
/// 而不是 panic。实现可直接复用 `mc_vcs::signature::verify_hmac_sha256_hex`。
pub fn verify_webhook_signature(_secret: &str, _headers: &HeaderMap, _body: &[u8]) -> bool {
    todo!("M8-4：HMAC-SHA256 验签（docs/61 §2.7 第 3 条）")
}

/// 从请求头分类事件 —— 转发到 `payload::GithubEventKind::classify`。
///
/// **anchor 期是桩**（`payload.rs` 的分类未实现）。
pub fn event_kind_for_request(_headers: &HeaderMap) -> GithubEventKind {
    todo!("M8-4：读 X-GitHub-Event 并分类")
}
