//! skill 主面路由：列表 / 搜索 / 详情 / 创建 / 更新 / 删除（**6 个注册键，含双形态共 10 键**）。
//!
//! - **写者**：M6-2（`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.go`（`ListSkills` / `SearchSkills` / `GetSkill` /
//!   `CreateSkill` / `UpdateSkill` / `DeleteSkill`）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/skills/` 与 `/api/skills` | GET, POST | `router.go:2234-2235` |
//! | `/api/skills/search` | GET | `router.go:2236` |
//! | `/api/skills/:id/` 与 `/api/skills/:id` | GET, PUT, DELETE | `router.go:2239-2241` |
//!
//! ⚠️ 尾斜杠两组的**方法集合必须逐字相同**（漏一个 = `slash_alias_audit.py` 的
//! `MISSING_ALIAS` 硬失败；M6-0 已删掉 allowlist 里的豁免行，没有退路）。axum 未注册的形态是
//! **404 而不是 307**。`/api/skills/search` 与 `/api/skills/:id` 的**冲突顺序**也要照上游：
//! 静态段优先于参数段（matchit 0.7 会自动优先静态，但 `search` 若被当成 `:id` 就会 404）。
//!
//! - **本仓约定**：`load_skill_for_user` 走 `super::helpers`；409（同名冲突）由
//!   `mc_repos::skill::write` 的 `RepoError::Conflict` 映射。
//! - **不做什么**：不做支持文件与标签（`files.rs` / `labels.rs`）、不做导入/刷新
//!   （`import.rs` / `refresh.rs`）。
//!
//! **状态：M6-2 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 420 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/skills` 主面（M6-2 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
