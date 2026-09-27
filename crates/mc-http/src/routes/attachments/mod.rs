//! 附件面聚合：**6 行** = 5 缺口 + 1 占位升级
//! （**写者 M10-B1** / `LUM-2112` / `docs/64` §4.2 第 1 行、`§6.5` 的 M10-B1 行）。
//!
//! | 文件 | 注册键 | 上游 handler（pin `f41fae6b08fb`） |
//! | --- | :-: | --- |
//! | `attachments/read.rs` | `GET /api/attachments/{id}` | `file.go:671` `GetAttachmentByID` |
//! | `attachments/download.rs` | `.../{id}/content` · `.../{id}/download` · `.../{id}/signed-download` | `file.go:1285` / `:849` / `attachment_capability.go:198` |
//! | `attachments/delete.rs` | `DELETE /api/attachments/{id}` | `file.go:1413` `DeleteAttachment` |
//! | **（`routes/issues/mod.rs` 的那一行）** | `GET /api/issues/{id}/attachments` | `file.go:642` `ListAttachments` |
//!
//! 账：3 + 1 + 1 + 1 = **6**；其中 `known_gap` **5**（`/api/issues/{id}/attachments`
//! 在 `base` 上已是 501 占位 ⇒ 本片是**占位升级**）。复算见 `docs/64` §10 命令 2。
//!
//! ## ⚠️ 本目录是**新目录** ⇒ 需要的两处「让新文件可见」的接线（写集审计第 ② 类）
//!
//! M10-0 的 anchor（`LUM-2102`）**只**预声明了 `probes` / `config` 两个面
//! （`routes/mod.rs:136-146` 与 `mount.rs:121-126`）—— 本目录**不在**它的委托范围
//! （`docs/64` §4.1 A 面 vs §4.2 B 面是两张表，anchor 不该为 B 面包）。
//! `base` 实测：`ls crates/mc-http/src/routes/attachments/` = **不存在**；
//! `grep -c attachments routes/{mod,mount}.rs` = `0` / `0`。
//!
//! ⇒ **两条最小冻结破例外**（cycle `LUM-2364` 逐条裁定，**各只加不改**）：
//!
//! | 文件 | base 行数 | 本片动作 |
//! | --- | ---: | --- |
//! | `crates/mc-http/src/routes/mod.rs` | 183 | **+1 行** `pub mod attachments;` |
//! | `crates/mc-http/src/routes/mount.rs` | 569 | **+1 行** `.merge(...)`（`router()` 内追加）+ 文件**末尾**新增一个合并点函数 |
//!
//! 两条都逐字登记在 `docs/32-M3-DAEMON-FACE.md` 的 `## 50.` / `### 9.20`。
//!
//! ## 三条接线纪律（与前几波同款）
//!
//! 1. 同 path+method **重复注册 ⇒ axum 启动时 panic**（`docs/15` §9.6.6）。
//!    ⚠️ 本片最可能踩的一条：`/api/issues/:id/attachments` 在 `issues/mod.rs` 注册，
//!    **绝不可**在本目录再注册一次。
//! 2. 路径参数必须写 `:name`（matchit 0.7 把 `{name}` 当**字面量** ⇒ 编译通过且恒 404）。
//! 3. 形态：上游 6 条**全是 plain 注册** ⇒ **只注册无尾斜杠**那一形态
//!    （`docs/64` §1.4 实测 `dual-form required: 0`；补尾斜杠 = `EXTRA_ALIAS` 硬失败，
//!    本波 `slash-alias-allowlist.tsv` 是 **0 数据行**、**没有**豁免退路）。
//!
//! ## 本片与别处的交集（**只读**清单）
//!
//! - `crates/mc-repos/src/attachment.rs`（**本片新建**）—— 4 个查询见它的模块头。
//! - `crates/mc-http/src/routes/issues/{mod,context}.rs` —— **只读** `resolve_workspace`
//!   / `load_issue` / `issue_repo` / `WorkspaceQuery`（先例 = M9-3 的 onboarding 面：
//!   **不新写** workspace 解析、**不新写**角色判定，`docs/62` §9.7 的判例）。
//! - `crates/mc-http/src/routes/invitations.rs` —— **只读** `require_workspace_member` /
//!   `require_workspace_admin` / `not_found`（同上）。
//! - `crates/mc-http/src/routes/config.rs` —— **只读** `cdn_signed: false`（互为断言）。
//! - `crates/mc-storage` —— **只读** `Storage::get`（provider 路由 + 桶/键）。
//! - **M10-B2**（`POST /api/upload-file` + `GET /uploads/*`，stage 4）—— 写侧；它落地时
//!   要一起复核 `download.rs::split_object_ref` 的 `(bucket, key)` 约定。

pub mod delete;
pub mod download;
pub mod download_pure;
pub mod read;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// 附件面聚合 router（5 条新键；第 6 条在 `routes/issues/mod.rs`）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(read::router())
        .merge(download::router())
        .merge(delete::router())
}

// 本片的证据面（门 ⑤ 零库 + 门 ⑥ 真库）。拆成 `tests/{support,db,download}.rs` 三个子模块
// 是门 ⑩ 的 800 行上限要求（先例 = `routes/onboarding/tests.rs`，M9-3）。
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
