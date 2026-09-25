//! vcs 仓储：4 张 VCS 表按**面**分文件（上游 `internal/integrations/vcs` 的查询面）。
//!
//! - **状态**：M8-0 anchor 只落文件与边界（`LUM-1797` / `docs/61-M8-PLAN.md` §3.3）——
//!   本文件只有模块声明与下面的归属表；三个子模块都是 doc-only 桩，由 **M8-2** 填自己的
//!   文件（**一个文件一个写者**）。
//! - **表的落法**（4 张，**本波 0 新迁移**，全部已在 `migrations/upstream/**`；
//!   `docs/61` §6.4 的清点）：
//!
//! | 表 | 迁移 | 本模块的落点 |
//! | --- | --- | --- |
//! | `vcs_connection` | `216` | `connection.rs` |
//! | `vcs_pull_request` | `216` | `pull_request.rs` |
//! | `issue_vcs_pull_request` | `216` | `pull_request.rs` |
//! | `vcs_commit_status` | `216` | `commit_status.rs` |
//!
//! - **凭据纪律（本模块的硬约束）**：`access_token_encrypted` / `webhook_secret_encrypted`
//!   是 `secretbox` 密文（`mc_secrets::secretbox`，**M7-0 建、M8 只读**）—— 本模块**不**
//!   解封、**不**读明文、**不**把密文塞进 `Debug`；明文入库即失败（M8-2 的 `DoD`）。
//! - **本仓约定**（与 `mc_repos::channel` / `mc_repos::skill` 同款，**抄不要另立**）：
//!   裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定、
//!   jsonb → `serde_json::Value`、bytea → `Vec<u8>`；列表查询必须带 `workspace_id` 收窄
//!   （跨工作区读 = 越权）。
//! - **不做什么**：不做领域转换（行 → `mc_core::vcs::*` 的转换在调用侧）；不写迁移。
//!
//! | 子文件 | 写者 | 内容 |
//! | --- | :-: | --- |
//! | `connection.rs` | M8-2 | `vcs_connection` 的 CRUD + webhook secret 轮换 |
//! | `pull_request.rs` | M8-2 | `vcs_pull_request` 的 upsert + `issue_vcs_pull_request` 关联账 |
//! | `commit_status.rs` | M8-2 | `vcs_commit_status` 的单调 upsert |

pub mod commit_status;
pub mod connection;
pub mod pull_request;
