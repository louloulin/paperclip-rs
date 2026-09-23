//! M5-0 anchor：autopilot **trigger** 写面（create / update / delete）—— **空 router 占位**。
//!
//! - **写者**：M5-3（`docs/44` §3.2）。切片只实现本文件的 `router()`；凭据两条在
//!   `../credentials.rs`（同片，已按 §6.3 拆好防门 ⑩）。
//! - **路由**（`router.go` L2110–L2112）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 10 | POST | `/api/autopilots/:id/triggers` | `CreateAutopilotTrigger` | 202 (+56) |
//! | 11 | PATCH | `/api/autopilots/:id/triggers/:triggerId/`（+ 无斜杠别名） | `UpdateAutopilotTrigger` | 172 |
//! | 12 | DELETE | `/api/autopilots/:id/triggers/:triggerId/`（+ 无斜杠别名） | `DeleteAutopilotTrigger` | 74 |
//!
//! - **#11/#12 是双形态**（`Route(":triggerId") + Patch("/") / Delete("/")`）；#10 单形态。
//! - **cron 校验落在 `mc_autopilot::trigger`**（5 字段手写解析器，选型记录见该 crate 的 `lib.rs`）；
//!   本文件只做参数落库 + 调 `computeNextRun`75 算 `next_run_at`。**不要**在路由层写第二份
//!   cron 解析（`cron-preview`(#2) 与 M5-8 的 job 共用同一个解析器）。
//! - **时区**：`autopilot_trigger.timezone` 是 `TEXT NULL DEFAULT 'UTC'`（**可空** ⇒ 空值时按
//!   `UTC` 解包，语义与 `issue_wakeup.timezone` 的 NOT NULL 不同）；校验用
//!   `mc_autopilot::trigger` 的 `Timezone`（IANA）。
//! - **token 铸造**：`createWebhookTriggerWithMintedToken`56 生成 + 唯一冲突重试；
//!   路径形态由 `webhookPathForToken`4 定 ⇒ 与 M5-5 的 ingress 入口**逐字一致**
//!   （`POST /api/webhooks/autopilots/{token}`）。
//! - **provider 闭集** `{generic, github}`（`isAllowedWebhookProvider`9 归本片）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M5-3 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
