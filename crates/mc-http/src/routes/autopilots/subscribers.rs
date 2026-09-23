//! M5-0 anchor：autopilot **协作者 / 订阅者**写面 —— **空 router 占位**。
//!
//! - **写者**：M5-2（`docs/44` §3.2）。切片只实现本文件的 `router()`。
//! - **路由**（`router.go` L2108–L2109）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 8 | POST | `/api/autopilots/:id/collaborators` | `AddAutopilotCollaborator` | 59 (+17) |
//! | 9 | DELETE | `/api/autopilots/:id/collaborators/:userId` | `RemoveAutopilotCollaborator` | 35 |
//!
//! - **两条都是单形态**（plain 子路由）⇒ 不要加尾斜杠别名。
//! - **必须同事务加锁**：上游 `lockAndValidateAutopilotSubscribers`39 是 `FOR SHARE`/`FOR UPDATE`
//!   语义，`parseAutopilotSubscribers`33 负责解析请求体 ⇒ 校验与写入要在**同一事务**内，
//!   否则并发加协作者会超员（需要一条**真库**并发测试兜底，`docs/44` §6.2 的 `DoD`）。
//! - **`user_type` 只有 `'member'`**（`120` / `128` 的 CHECK，"Members-only for now"）⇒ 用
//!   `mc_core::autopilot::AutopilotUserType`（单变体），不要用 `AutopilotActorType`。
//! - **无外键**：`autopilot_collaborator` / `autopilot_subscriber` 的主体列没有 FK ⇒
//!   「用户存在且是本 workspace 成员」要在这里校验（仓储侧写在 `mc_repos::autopilot::write`）。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M5-2 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
