//! Wakeup 领域类型（事件唤醒 + 时间唤醒；M5-0 anchor **重写**）。
//!
//! 类型来源 = `migrations/upstream/**` 的真实列 + `contracts/upstream-schema.json`
//! 的真库快照（见 [`crate::autopilot`] 的同一说明）。
//!
//! | 实体 | 表 | 建表迁移 | 追加迁移 | 列数 |
//! | --- | --- | --- | --- | ---: |
//! | [`Wakeup`] | `issue_wakeup` | `509` | `531` | 26 |
//! | [`WakeupReceipt`] | `issue_wakeup_receipt` | `509` | `528` | 10 |
//!
//! ⚠️ `docs/44-M5-PLAN.md` §5.1 只读到 `509`，**漏了 5 个后续迁移**：
//!
//! - `528_wakeup_receipt_coalescing`：receipt 加 `coalesce_key`（**合并语义**，见下）。
//! - `529_wakeup_pending_event`：唯一偏索引 `issue_wakeup_pending_event_idx`
//!   `(wakeup_id, revision, coalesce_key) WHERE processed_at IS NULL AND coalesce_key IS NOT NULL`。
//! - `530_wakeup_bounded_capture`：**库侧捕获函数** `capture_issue_wakeup(...)` +
//!   容量触发器 `guard_issue_wakeup_capacity()`。
//! - `531_wakeup_actor_filter`：`issue_wakeup` 加 `filter_actor_type` / `filter_actor_id`
//!   + `issue_wakeup_actor_filter` 约束（**NOT VALID**）。§5.1 的列清单里没有这两个列。
//! - `532_wakeup_actor_capture`：`capture_issue_wakeup` 覆盖定义，加入 actor 过滤。
//!
//! ⇒ `issue_wakeup` 的最终列是 26 个（不是 24），`issue_wakeup_receipt` 是 10 个（不是 9）。
//!
//! # 旧 stub 错在哪（重写前的实测）
//!
//! | 旧 stub | 真值 |
//! | --- | --- |
//! | `source ∈ {event,time,manual}` | `kind ∈ {event,at,every,cron}`（**没有** `manual`） |
//! | 单一 `source` 表达一切 | `kind` **×** `mode ∈ {once,continuous}` 两个**正交**维度 |
//! | `event_type: Option<String>` | `event_types text[] NOT NULL DEFAULT '{}'`（多值） |
//! | `due_at` | `next_fire_at`；另有 `interval_seconds` / `cron_expression` / `timezone` |
//! | `status: String`（`pending/active/settled/cancelled`） | `enabled: bool` + `disabled_at` + `revision` |
//! | `actor_type` / `actor_id` | 两组过滤：`filter_agent_id` / `filter_task_id`（`509`）**与** `filter_actor_type` / `filter_actor_id`（`531`） |
//! | `run_id` | `last_task_id` |
//! | `receipt_id` / `coalesced_count` | **不在本表** ⇒ receipt 侧的 `coalesce_key` + 唯一索引合并 |
//! | 缺 `instruction` / `created_by` / `source_task_id` / `parent_comment_id` / `last_error` | 都是真列（`instruction` 是**核心语义列**） |
//! | `WakeupEventCapture{event_kind, captured_at, actor_type, actor_id}` | 真表是 [`WakeupReceipt`]：`event_key` + `event_type` + `created_at` / `processed_at`，actor 在 `payload`（evidence）里 |
//!
//! # 事件捕获是**库侧**语义（M5-6 必须照抄，不能自创）
//!
//! `capture_issue_wakeup(p_issue, p_type, p_key, p_agent, p_task, p_payload)`（`532` 版）在**同一事务**里：
//!
//! 1. **早退**：该 issue 上没有 `enabled AND kind='event' AND p_type = ANY(event_types)` 的 wakeup ⇒ 什么都不做。
//! 2. **issue 终态早退**：issue 必须存在且 `status NOT IN ('done','cancelled')`，且没有
//!    `category IN ('done','closed')` 的状态定义与之同名 ⇒ 否则什么都不做。
//! 3. **evidence 形状**（版本 1，M5-6 的 `wakeup/evidence.rs` 要逐键实现）：
//!    `event_id, event_type, version, occurred_at, workspace_id, issue_id, source_task_id,
//!    agent_id, actor_type, actor_id` + 调用方 `p_payload`（后者**只带引用与字段名**，
//!    不带评论正文/附件 URL/任意 metadata 值）。
//!    `actor_type` 的兜底顺序：`multica.actor_type` 会话变量 → `p_agent` 非空则 `agent` → 否则 `system`；
//!    `actor_id` 兜底：会话变量 → `p_agent`。
//! 4. **命中规则**（逐条 AND）：`filter_agent_id` / `filter_task_id` 相等过滤；
//!    `filter_actor_type` / `filter_actor_id` 与 evidence 的 `actor_type` / `actor_id` 相等过滤；
//!    **自触发抑制**：`p_task IS DISTINCT FROM source_task_id`（注册本次唤醒的那个 task 触发的
//!    事件不算，避免自唤醒）；**环路抑制**：`agent_task_queue.context->>'wakeup_id' == wakeup.id`
//!    的 task 触发的事件也不算。
//! 5. **合并**（`coalesce_key = event_type`）：同一 `(wakeup_id, revision, event_type)` 只保留
//!    **一条 pending** receipt。合并时 receipt **换 `id`**（`DO UPDATE SET id=EXCLUDED.id`，
//!    让读后没有 `FOR UPDATE` 的旧 dispatcher 只能消费自己看到的那一版，而不是吞掉没看到的证据）、
//!    `payload.coalesced_count` +1、`payload.first_occurred_at` 保留首次时间。
//! 6. 命中 `issue_wakeup_receipt_key_idx` 的 `unique_violation` 视为「已保留的 receipt 处理过同一
//!    来源事实」被吞掉；**其它**唯一性冲突原样抛出。
//!
//! 容量（`530` 的触发器，**库侧强制**，应用层只负责把报错转成可读错误）：
//! 单个 issue 最多 [`MAX_ENABLED_WAKEUPS_PER_ISSUE`] 条 enabled wakeup，
//! 单个 workspace 最多 [`MAX_ENABLED_WAKEUPS_PER_WORKSPACE`] 条。
//! 触发条件是 `BEFORE INSERT OR UPDATE OF enabled,issue_id,workspace_id`，且同 issue/workspace
//! 的 enabled→enabled 更新被短路（不重算容量）；判错时抛 `ERRCODE=23514` +
//! `CONSTRAINT='issue_wakeup_active_limit'`，所以 M5-6 可以按约束名而不是按文案匹配。
//!
//! # 约定
//!
//! - `kind = 'at'` 用 `next_fire_at`；`every` 用 `interval_seconds`；`cron` 用
//!   `cron_expression` + `timezone`（5 字段、无秒，与 `autopilot_trigger` 同一语义）。
//!   **哪个字段该有值属于校验面**（M5-6 的行为），库侧只有 `kind` 的封闭枚举，没有交叉 CHECK。
//! - `event_types` 只在 `kind='event'` 时有意义；`disabled_at` 只在 `enabled = false` 时有值。
//! - `revision` 是**乐观并发 + 合并作用域**的双重键：receipt 的唯一索引含它，
//!   所以**改动 wakeup 配置要 `revision` +1**，老 revision 的 pending receipt 不再被新证据合并。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// 单个 issue 上允许的 enabled wakeup 上限（`530` 的触发器硬编码 32）。
pub const MAX_ENABLED_WAKEUPS_PER_ISSUE: i64 = 32;

/// 单个 workspace 允许的 enabled wakeup 上限（`530` 的触发器硬编码 1000）。
pub const MAX_ENABLED_WAKEUPS_PER_WORKSPACE: i64 = 1000;

/// `issue_wakeup.kind` ∈ `{event,at,every,cron}`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeupKind {
    /// 由 issue 上的事件触发（靠库侧 `capture_issue_wakeup` 建 receipt）。
    Event,
    /// 在 `next_fire_at` 触发一次。
    At,
    /// 每 `interval_seconds` 触发。
    Every,
    /// 按 `cron_expression` + `timezone` 触发。
    Cron,
}

impl WakeupKind {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::At => "at",
            Self::Every => "every",
            Self::Cron => "cron",
        }
    }

    /// 是否由库侧事件捕获驱动（只有这一种不需要调度器推进 `next_fire_at`）。
    #[must_use]
    pub const fn is_event_driven(self) -> bool {
        matches!(self, Self::Event)
    }
}

/// `issue_wakeup.mode` ∈ `{once,continuous}`——与 [`WakeupKind`] **正交**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeupMode {
    /// 触发一次后失效。
    Once,
    /// 持续有效（`revision` 或 `enabled` 才会终止它）。
    Continuous,
}

impl WakeupMode {
    /// DB `CHECK` 的取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Continuous => "continuous",
        }
    }
}

/// 事件证据里的 `actor_type` ∈ `{member,agent,system}`。
///
/// 注意 `system` 是 `capture_issue_wakeup` 的**兜底值**（既没有 `multica.actor_type` 会话变量、
/// 也没有 `p_agent` 时），**不可能**出现在 `issue_wakeup.filter_actor_type` 里：
/// `531` 的 `issue_wakeup_actor_filter` 约束只允许 `filter_actor_type ∈ {member,agent}`。
/// ⇒ 用 actor 过滤的规则**永远不会**被 `system` 事件唤醒（这是正确的，不是缺陷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeupActorType {
    /// 工作区成员。
    Member,
    /// 工作区内的 agent。
    Agent,
    /// 无主体可归因的系统事件（只在 evidence 的 `actor_type` 里）。
    System,
}

impl WakeupActorType {
    /// 取值字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Agent => "agent",
            Self::System => "system",
        }
    }

    /// 是否可用于 `filter_actor_type`（`531` 的 CHECK 只允许 `member` / `agent`）。
    #[must_use]
    pub const fn is_filterable(self) -> bool {
        matches!(self, Self::Member | Self::Agent)
    }
}

/// `issue_wakeup` 行（26 列）——issue 上的一条唤醒规则（事件或时间）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wakeup {
    /// 主键。
    pub id: Id,
    /// 所属工作区（容量上限按它统计）。
    pub workspace_id: Id,
    /// 所属 issue（容量上限按它统计；issue 删除时的清理属应用层）。
    pub issue_id: Id,
    /// 被唤醒的 agent。
    pub agent_id: Id,
    /// 注册者（不是被唤醒者）。
    pub created_by: Id,
    /// 注册这条 wakeup 的那个 task（自触发抑制用）。
    pub source_task_id: Option<Id>,
    /// 注册它的评论（留痕）。
    pub parent_comment_id: Option<Id>,
    /// 唤醒时注入给 agent 的指令正文（**核心语义列**）。
    pub instruction: String,
    /// 唤醒类型。
    pub kind: WakeupKind,
    /// 单次还是持续（与 `kind` 正交）。
    pub mode: WakeupMode,
    /// 订阅的事件名集合（`NOT NULL`，默认空数组；只在 `kind = event` 时有意义）。
    pub event_types: Vec<String>,
    /// 主体过滤：类型（可空；实际只能 `member`/`agent`，且**必须**与 `filter_actor_id` 同时有值、
    /// 且 `kind = event`——见 `531` 的 `issue_wakeup_actor_filter`，该约束 **NOT VALID**）。
    pub filter_actor_type: Option<WakeupActorType>,
    /// 主体过滤：id。
    pub filter_actor_id: Option<Id>,
    /// agent 过滤（`509`）。
    pub filter_agent_id: Option<Id>,
    /// task 过滤（`509`）。
    pub filter_task_id: Option<Id>,
    /// `kind = every` 的间隔秒数。
    pub interval_seconds: Option<i64>,
    /// `kind = cron` 的 5 字段表达式（无秒）。
    pub cron_expression: Option<String>,
    /// 调度时区（IANA 名；`NOT NULL DEFAULT 'UTC'`）。
    pub timezone: String,
    /// 下次触发时间（调度器推进；`kind = event` 时为 `NULL`）。
    pub next_fire_at: Option<Timestamp>,
    /// 是否启用（`509` 的两态就是它，不是旧 stub 的四态 `status`）。
    pub enabled: bool,
    /// 停用时间。
    pub disabled_at: Option<Timestamp>,
    /// 版本号：乐观并发 + **合并作用域**（receipt 唯一索引含它）。
    pub revision: i64,
    /// 最近一次派出去的 task。
    pub last_task_id: Option<Id>,
    /// 最近一次失败信息。
    pub last_error: Option<String>,
    /// 创建时间（默认 `clock_timestamp()`）。
    pub created_at: Timestamp,
    /// 更新时间（默认 `clock_timestamp()`）。
    pub updated_at: Timestamp,
}

/// `issue_wakeup_receipt` 行（10 列）——一条**待处理的事件证据**。
///
/// 生命周期：`processed_at IS NULL` = 待处理。同一 `(wakeup_id, revision, coalesce_key)` 的
/// pending 行**只有一条**（`529` 的唯一偏索引），新证据走 `532` 的 upsert 合并进它、
/// 并把行 `id` 换掉。`task_id` 是派出去消费它的任务；`payload` 是 evidence（见模块文档第 3 点）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WakeupReceipt {
    /// 主键（合并时会换新值，见模块文档）。
    pub id: Id,
    /// 所属 wakeup。
    pub wakeup_id: Id,
    /// 证据对应的 wakeup 版本（与 `Wakeup::revision` 配合做作用域）。
    pub revision: i64,
    /// 事件键：**首次**那次的键在合并后被保留（用于重复抑制）。
    pub event_key: String,
    /// 事件名（同时也是 `coalesce_key`：每种事件类型只保留一条 pending receipt）。
    pub event_type: String,
    /// evidence 载荷（`jsonb`，版本 1 的键见模块文档第 3 点）。
    pub payload: serde_json::Value,
    /// 合并键（`528`；`NULL` 表示不参与合并，此时同 `(wakeup_id, revision)` 可以有多条 pending）。
    pub coalesce_key: Option<String>,
    /// 消费这条 receipt 的 task。
    pub task_id: Option<Id>,
    /// 处理完成时间（`NULL` = 仍待处理）。
    pub processed_at: Option<Timestamp>,
    /// 创建时间（默认 `clock_timestamp()`）。
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_strings_match_db_checks() {
        assert_eq!(WakeupKind::Event.as_str(), "event");
        assert_eq!(WakeupKind::Cron.as_str(), "cron");
        assert_eq!(WakeupMode::Continuous.as_str(), "continuous");
        assert_eq!(WakeupActorType::System.as_str(), "system");
        assert!(!WakeupActorType::System.is_filterable());
        assert!(WakeupActorType::Agent.is_filterable());
        assert!(WakeupKind::Event.is_event_driven());
        assert!(!WakeupKind::Every.is_event_driven());
    }

    /// 容量常量必须与 `530` 的触发器一致（库侧是权威执行点）。
    #[test]
    fn capacity_constants_match_migration_530() {
        assert_eq!(MAX_ENABLED_WAKEUPS_PER_ISSUE, 32);
        assert_eq!(MAX_ENABLED_WAKEUPS_PER_WORKSPACE, 1000);
    }
}
