//! github 仓储：7 张 GitHub App / PR 快照表按**面**分文件。
//!
//! - **状态**：M8-0 anchor 只落文件与边界（`LUM-1797` / `docs/61-M8-PLAN.md` §3.3）——
//!   本文件只有模块声明与下面的归属表；四个子模块都是 doc-only 桩。
//! - **表的落法**（7 张，**本波 0 新迁移**）：
//!
//! | 表 | 迁移 | 本模块的落点 | 写者 |
//! | --- | --- | --- | :-: |
//! | `github_installation` | `079` | `installation.rs` | M8-1 |
//! | `github_pending_installation` | `079` | `installation.rs` | M8-1 |
//! | `github_pull_request` | `079` | `pull_request.rs` | M8-1 |
//! | `issue_pull_request` | `079` | `pull_request.rs` | M8-1 |
//! | `github_pull_request_check_suite` | `091` | `check_suite.rs` | M8-4 |
//! | `github_pull_request_check_run` | `091` | `check_suite.rs` | M8-4 |
//! | `github_pending_check_suite` | `091` | `pending.rs` | M8-4 |
//!
//! - **读者关系（`docs/61` §3.3）**：M8-4 **读** M8-1 的 `installation.rs` / `pull_request.rs`；
//!   M8-5 **读** M8-1 的 `pull_request.rs`。反向不成立 ⇒ 无环。
//! - **凭据纪律**：本模块**不**持有 App 私钥或 installation token（那些在
//!   `mc-vcs-github`）；表里只有 GitHub 的公开数字标识与账号信息。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；列表查询带 `workspace_id` 收窄。
//!
//! | 子文件 | 写者 | 内容 |
//! | --- | :-: | --- |
//! | `installation.rs` | M8-1 | `github_installation` + `github_pending_installation` |
//! | `pull_request.rs` | M8-1 | `github_pull_request` upsert + `issue_pull_request` 关联账 |
//! | `check_suite.rs` | M8-4 | `github_pull_request_check_suite` + `github_pull_request_check_run` |
//! | `pending.rs` | M8-4 | `github_pending_check_suite`（乱序 check_suite 的暂存） |

pub mod check_suite;
pub mod installation;
pub mod pending;
pub mod pull_request;
