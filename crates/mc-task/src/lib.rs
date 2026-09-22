//! M3 anchor scaffold（LUM-1406）：task 领域层 crate —— **占位，无实现**。
//!
//! 由 M3-3（`feat/multica-rs-m3a-task-domain`）填充：
//!
//! - 状态机（真值 = `docs/15` §2.3 的事件表 + 上游迁移 `022`/`055`）：
//!   `queued → dispatched → running → {waiting_local_directory} → terminal_*`，
//!   `delegated_failure` 是 `terminal_failed` 的子类；
//! - 租约（`prepare-lease` 语义、`lease_expires_at`、`last_heartbeat_at`、过期扫描）；
//! - 重试（`attempt` / `max_attempts` / `parent_task_id` /
//!   `failure_reason ∈ {agent_error, timeout, runtime_offline, runtime_recovery, manual}`）；
//! - 取消与 `cancel-ack`；usage 结算**纯计算**（token/时长 → `task_usage` 行形状）；
//! - `TaskStore` 以 trait（port）形式给出：SQL 实现在 M3-6（`mc-repos`），
//!   内存实现**只允许出现在 `test_state()`**（plan1 §3.4）。
//!
//! 范围限制：不写 SQL、不写路由、不写 daemon 侧；不得引入本仓自造列
//! （`retry_count` / `source_task_id` 等 7 列，docs/15 §2.2）；不碰 `agent_task_queue` 的 DDL。
//!
//! scaffold 阶段本文件只有文档注释：一个类型都不定义。
