//! M5-0 anchor：autopilot **写面**（create / update / delete）—— **空 router 占位**。
//!
//! - **写者**：M5-2（`docs/44` §3.2）。切片只实现本文件的 `router()`。
//! - **路由**（`router.go` L2105–L2107）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 5 | POST | `/api/autopilots/`（+ 无斜杠别名） | `CreateAutopilot` | 159 |
//! | 6 | PATCH | `/api/autopilots/:id/`（+ 无斜杠别名） | `UpdateAutopilot` | 260 |
//! | 7 | DELETE | `/api/autopilots/:id/`（+ 无斜杠别名） | `DeleteAutopilot` | 64 |
//!
//! - **三态补丁**：#6 的 `UpdateAutopilot`260 是「缺失 / `null` / 有值」三态大户 ⇒ 本地用
//!   `Option<Option<T>>`（`routes/issues/mod.rs` 已有 `#![allow(clippy::option_option)]` 先例）。
//! - **字段口径**：`autopilot` 表**没有** `priority`（`058` DROP）与 `concurrency_policy`
//!   （`043` DROP）——旧桩与计划里列过它们，是错的（见 `mc_core::autopilot` 的「旧 stub 错在哪」表）。
//! - **规则版本**：只有 `autopilotRuleSubstantiveChange`13 判为实质变更才 append
//!   `autopilot_rule_version`（`186`，append-only）；不是每次 PATCH 都写。
//! - **assignee 校验**在 `../assignee.rs`（M5-2），**权限**在 `../access.rs`（M5-1）。
//! - **门 ⑩ 预判**（§6.3）：本文件 + 拆出的 `subscribers.rs`(183) / `assignee.rs`(135)
//!   三者合计约 650 行 ⇒ **anchor 已拆好**，切片不要再往本文件堆协作者/订阅者逻辑。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 空切片：scaffold 占位，等 M5-2 填入真实路由。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
