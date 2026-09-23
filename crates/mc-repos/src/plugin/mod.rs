//! plugin 仓储：`plugin_installation` / `plugin_storage` / `plugin_secret` / `plugin_invocation`
//! / `plugin_hook_schedule` / `plugin_package*` 的行结构 + SQL。
//!
//! - **状态**：M6-0 anchor 只落文件与边界（`LUM-1665`）—— 本文件只有模块声明与下面的归属表；
//!   七个子模块都是 doc-only 桩，由 M6-5 / M6-6 / M6-7 / M6-8 各自填自己的文件。
//! - **表清单**（八张，全部已存在，**本波 0 新迁移**；逐字段对照 `mc_core::plugin` 的头表）：
//!
//! | 表 | 迁移 | 列数 | 备注 |
//! | --- | :-: | :-: | --- |
//! | `plugin_installation` | `344` + `362` + `369` + `392` | 15 | `enabled` 是唯一开关（**没有** `status` 列）；`mcp_approvals` JSONB 见 `369` |
//! | `plugin_storage` | `344` | 8 | `scope_type IN ('workspace','user')`；软配额（1000 键 / 5 MiB，无淘汰） |
//! | `plugin_secret` | `344` | 6 | `ciphertext` 是 **BYTEA**（`nonce‖ct‖tag`，见 `mc-plugin-host::credentials`） |
//! | `plugin_invocation` | `362` + `399` + `402` | 13 | `trigger` 五态（`399` 补 `schedule`）；`attempt 1..10` |
//! | `plugin_hook_schedule` | `399` | 12 | `generation` 用于换代失效（老回调不能打到新代） |
//! | `plugin_package` | `392` | 7 | `plugin_key` 3..255、`name` 1..160 |
//! | `plugin_package_version` | `392` | 9 | `digest` 纯 hex（CHECK `char_length = 64`），**不可变** |
//! | `plugin_package_file` | `392` | 7 | `content` **BYTEA**；`sha256` 纯 hex |
//!
//! - **本仓约定**（与 `mc_repos::skill` 同款，抄不要另立）：裸 `Uuid` + 手写 `sqlx::FromRow`、
//!   `map_sqlx_err`、运行时 builder + 参数绑定、jsonb → `serde_json::Value`、bytea → `Vec<u8>`。
//! - **不做什么**：
//!   - 不做 manifest 的**结构**（那是 `mc-plugin-host::manifest`，JSONB 原样存取，别在这里
//!     再把 manifest 解析成 Rust 结构）；
//!   - 不做 scope / capability 判定（`mc-plugin-host::{scope,capabilities}`）；
//!   - 不新写「append-only 触发器」那套（老 14 表设计已由 `344` 整体 drop，别复活）。
//!
//! | 子文件 | 写者 | 内容 |
//! | --- | :-: | --- |
//! | `installation.rs` | M6-5 | 安装行的 CRUD / 启停 / 配置 / 令牌哈希 |
//! | `package.rs` | M6-5 | 插件包与版本、文件的读写（`plugin_package*`） |
//! | `skill.rs` | M6-5 | 插件贡献的 skill（按 `skill.plugin_installation_id` 关联） |
//! | `mcp_approval.rs` | M6-6 | `mcp_approvals` JSONB 的读写与交叉校验 |
//! | `invocation_read.rs` | M6-6 | `plugin_invocation` 的列表/详情读面 |
//! | `storage.rs` | M6-7 | `plugin_storage` 的读写（公开 API + bridge 的 storage 面共用） |
//! | `hook.rs` | M6-8 | `plugin_hook_schedule` + `plugin_invocation` 的写侧（hook 引擎 / job） |

pub mod hook;
pub mod installation;
pub mod invocation_read;
pub mod mcp_approval;
pub mod package;
pub mod skill;
pub mod storage;
