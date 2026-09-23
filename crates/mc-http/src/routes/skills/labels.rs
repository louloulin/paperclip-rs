//! skill 标签路由：列 / 挂 / 摘（**3 个注册键**）。
//!
//! - **写者**：M6-2（`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.go` 的标签段（`skill_to_label` 连接行）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/skills/:id/labels` | GET, POST | `router.go:2243-2244` |
//! | `/api/skills/:id/labels/:labelId` | DELETE | `router.go:2245` |
//!
//! - **口径**：标签**目录**是 `issue_label`（迁移 `162` 给它加了
//!   `resource_type IN ('issue','agent','skill')`），本文件只动 **`skill_to_label` 连接行**。
//!   所以「新建一个 skill 标签」是**管理面**的动作（M2 的 label CRUD），本文件只挑已存在的
//!   `label_id` 来挂 —— 上游也是这样分的（`POST` 的 body 是 `{"labelId": …}`，不是标签名）。
//! - **幂等**：重复挂同一个 `label_id` 撞 `PK(skill_id, label_id)` ⇒ 按上游折成成功或 409，
//!   不要漏成 500。
//! - **不做什么**：不建标签目录、不改标签的名字/颜色；不做跨资源类型的批量挂载。
//!
//! **状态：M6-2 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 180 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/skills/:id/labels*`（M6-2 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
