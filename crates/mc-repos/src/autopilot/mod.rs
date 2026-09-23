//! autopilot 仓储：行结构 + 共享 SELECT（`autopilot` / `autopilot_trigger` / `autopilot_run` /
//! `webhook_delivery` / `autopilot_collaborator` / `autopilot_subscriber` / `autopilot_rule_version`）。
//!
//! - **状态**：M5-0 anchor（`LUM-1563`）只建文件，**0 查询、0 类型**（`docs/44` §5.3）。
//! - **写者**：M5-1（**W**：本文件 + 行结构 + 共享 SELECT）。其余切片只 **读**
//!   （`docs/44` §3.2）：M5-2/3/4/5/8 不得在本文件加查询 —— 各自加在自己的文件里。
//! - **上游 SQL**：`db/queries/autopilot.sql`810 / 58 查询。
//! - **拆文件的原因**（`docs/44` §5.3）：`autopilot.sql` 的 58 个查询如果挤在一个文件里，
//!   单文件门 ⑩（800 行）与「一格一写者」都过不去 ⇒ 按 §3.2 的七格拆。
//! - **本仓约定**（照 `mc_repos::agent` / `mc_repos::project` 抄，不要另立）：
//!   - 行结构用**裸 `Uuid` / `Option<...>`**，不直接拿 `mc_core` 的领域类型去 `sqlx::FromRow`；
//!   - `sqlx::FromRow` 一律**手写**（`mc_core::Id` 没有 sqlx 的 Decode/Encode 实现）；
//!   - 错误经 `crate::workspace::map_sqlx_err` 归一；
//!   - 一律**运行时 builder + 参数绑定**（不用 compile-time 宏 ⇒ 构建期不需要数据库）；
//!   - jsonb 列用 `serde_json::Value`，bytea 列用 `Option<Vec<u8>>`。
//! - **列口径**：`autopilot` 16 列、`autopilot_trigger` 19 列、`autopilot_run` 18 列、
//!   `webhook_delivery` 28 列、`autopilot_collaborator` 5 列、`autopilot_subscriber` 4 列、
//!   `autopilot_rule_version` 7 列（逐字段对照见 `mc_core::autopilot` 的头表）。
pub mod delivery;
pub mod ingress;
pub mod quota;
pub mod run;
pub mod trigger;
pub mod write;
