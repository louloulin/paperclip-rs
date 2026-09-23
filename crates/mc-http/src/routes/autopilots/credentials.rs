//! M5-0 anchor：trigger **凭据**路由 —— **空 router 占位**。
//!
//! - **写者**：M5-3（`docs/44` §3.2；§6.3 明确从 `trigger.rs` 拆出，约 168 行）。
//! - **路由**（`router.go` L2113–L2114）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 13 | POST | `/api/autopilots/:id/triggers/:triggerId/rotate-webhook-token` | `RotateAutopilotTriggerWebhookToken` | 69 |
//! | 14 | PUT | `/api/autopilots/:id/triggers/:triggerId/signing-secret` | `SetAutopilotTriggerSigningSecret` | 65 |
//!
//! - **两条都是单形态**（plain 子路由）⇒ 不要加尾斜杠别名。
//! - **凭据只出 hint**：响应与日志都用 `signingSecretHint`15 / `redactWebhookSecrets`19 的语义
//!   （真值在 `mc_autopilot::credential`），**绝不出明文**；日志通道走 `mc-telemetry` 的
//!   redaction（`docs/33` §12.2）。
//! - **写敏感值的两条路由**（⑨/安全面）：需要 403/404 判负与「只写不回显」的断言。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M5-3 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
