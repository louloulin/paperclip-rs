//! skill 导入路由：`POST /api/skills/import`（**1 个注册键**）。
//!
//! - **写者**：M6-3（`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.go` 的导入段（`detectImportSource` / `fetchFromClawHub` /
//!   `fetchFromSkillsSh` / `fetchFromGitHub` / `finishSkillImport`）+ `skill_import_archive.go`。
//! - **两种入参形态**：① `{"source": "https://…"}`（或裸 slug，默认 clawhub）走**出网取件**；
//!   ② multipart 上传 zip 包。两者最终都汇到同一套「校验 → 入库」路径。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/skills/import` | POST | `router.go:2237` |
//!
//! - **出网面（本仓唯一需要出网的 skill 路径；所以取件放在 route 层，不在 `mc-skill`）**：
//!   - 源判定用 `mc_skill::source`（纯函数），**不要**在 handler 里再写一遍 host 匹配；
//!   - 总超时 **45s**（上游 `importFetchTimeout`），失败映射：取件不可用 ⇒ 502/503/504 家族，
//!     超上限 ⇒ **413**（`errImportCapExceeded`）；
//!   - `reqwest` 已在 `mc-http` 的依赖里（M6-0 声明），**不要**新增 HTTP 客户端依赖。
//! - **zip 面**：解包与两条上限用 `mc_skill::archive`（纯逻辑，内存内解包），
//!   文件数/体积超限 = **整包失败**，不截断。
//! - **入库**：走 `mc_repos::skill::import`（整包一个事务；`on_conflict` 四策略
//!   `fail`/`overwrite`/`rename`/`skip`，缺省 `fail`）。
//! - **不做什么**：不做 git clone（上游没有这条路径）；不做导入任务的异步化（同步返回）。
//!
//! **状态：M6-3 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 520 行以内。⚠️ 若逼近 800 行门，按「出网取件 / 入库」拆兄弟文件，
//! 并把拆分登记到 `docs/32` §9 的文件→写者表。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// `POST /api/skills/import`（M6-3 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
