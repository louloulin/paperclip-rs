//! composio 仓储：`user_composio_connection` 一张表。
//!
//! - **状态**：M8-0 anchor 只落文件与边界（`LUM-1797` / `docs/61-M8-PLAN.md` §3.3）。
//! - **表的落法**（1 张，**本波 0 新迁移**）：
//!
//! | 表 | 迁移 | 本模块的落点 |
//! | --- | --- | --- |
//! | `user_composio_connection` | `127` | `connection.rs` |
//!
//! - **归属**：连接属于**用户**，不属于 workspace（`docs/61` §1.1 第 4 簇）⇒ 任何
//!   「按 workspace 查连接」的写法都是越权。
//! - **凭据纪律**：`connected_account_id` / `composio_user_id` 是**外部标识**（非密钥）；
//!   bearer 只在 `mc-composio::service` 的会话 URL 里，**不进**本模块。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；`UNIQUE (user_id, connected_account_id)`。
//!
//! | 子文件 | 写者 | 内容 |
//! | --- | :-: | --- |
//! | `connection.rs` | M8-6 | `user_composio_connection` CRUD（connect / list / disconnect） |

pub mod connection;
