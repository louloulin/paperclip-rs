//! skill 支持文件路由：读 / 覆盖 / 删单个文件（**3 个注册键**）。
//!
//! - **写者**：M6-2（`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.go` 的 `ListSkillFiles` / `PutSkillFiles` /
//!   `DeleteSkillFile`（+ `internal/skill/reserved.go` 的路径判定）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/skills/:id/files` | GET, PUT | `router.go:2246-2247` |
//! | `/api/skills/:id/files/:fileId` | DELETE | `router.go:2248` |
//!
//! - **三条硬语义**：
//!   1. `PUT` 是**整批替换**语义（不是逐文件合并）—— 上游的请求体是「当前这批文件」；
//!   2. 保留路径（`SKILL.md`）**不能**作为支持文件写入：它属于 `skill.content` 正文。
//!      判定用 `mc_skill::reserved::IsReservedContentPath`（纯函数），**不要**在本文件再抄一份；
//!   3. 二进制内容要按 `mc_skill::binary` 的判定**跳过并记录**（`skill_file.content` 是 TEXT，
//!      写二进制会直接撞 `SQLSTATE 22021`）。
//! - **路径安全**：`..` / 绝对路径 / 规范化后的越界要在**本层**拒（400），不要指望 DB。
//! - **不做什么**：不做单文件的大小上限之外的内容校验。
//!
//! **状态：M6-2 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 260 行以内。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `/api/skills/:id/files*`（M6-2 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
