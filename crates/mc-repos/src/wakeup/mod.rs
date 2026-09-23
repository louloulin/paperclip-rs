//! issue wakeup 仓储（`issue_wakeup` 26 列 / `issue_wakeup_receipt` 10 列）。
//!
//! - **状态**：M5-0 anchor（`LUM-1563`）只建文件；**M5-6（`LUM-1565`）填实现**。
//! - **写者**：M5-6（**W**：`wakeup/**.rs` 整组；`docs/44` §3.2）。M5-7 / M5-8 只 **读**。
//! - **上游 SQL**：`db/queries/wakeup.sql`162 / 23 查询 + `db/queries/workspace_wakeup.sql`70 / 1 查询
//!   （workspace 级列表 = `GET /api/issue-wakeups` 的来源）。
//! - **行结构口径**：列序与类型逐字段对照 `mc_core::wakeup` 的头表
//!   （`event_types` 是 `text[] NOT NULL DEFAULT '{}'`、`payload` 是 jsonb、
//!   `revision` 既是乐观并发版本又是合并作用域）。
//! - **本仓约定**：裸 `Uuid` 字段 + 手写 `sqlx::FromRow` + `crate::workspace::map_sqlx_err`。
//!
//! # 事务 / 执行器口径（本片实测后定，M5-7/M5-8 照此调用）
//!
//! 上游服务层全程在一个 `BEGIN` 里跑「锁 workspace → 锁 issue → 锁 wakeup」，本仓沿用：
//! **改动型查询取 `&mut PgConnection`**（调用方 `let mut tx = pool.begin().await?;` 后传
//! `&mut *tx`），**纯读查询取 `&PgPool`**。不引入泛型 `Executor`：`issue_wakeup` 的写入语义
//! 强依赖同一事务里的行锁与 `revision` 快照，把执行器抽象掉只会让「跑在事务外」也能编译。
//!
//! # 容量上限按**约束名**识别（不要匹配文案）
//!
//! `530` 的 `guard_issue_wakeup_capacity()` 抛 `ERRCODE=23514` +
//! `CONSTRAINT='issue_wakeup_active_limit'` ⇒ [`is_active_limit_violation`] 只认这两项；
//! `55P03`（`lock_not_available`，来自 `FOR UPDATE NOWAIT`）⇒ [`is_source_busy`]。
//!
//! # 目录
//!
//! - [`issue`]：wakeup 本体 + 锁 + issue 级列表（上游 `wakeup.sql`）
//! - [`receipt`]：证据收据的写入 / 合并 / 消费（上游 `wakeup.sql:73-92`）
//! - [`listing`]：workspace 级列表与摘要（上游 `workspace_wakeup.sql` + `wakeup.sql:22`）
//! - `tests`：纯 Rust 单测（真库 e2e 在 `mc-http/tests/issues/wakeups.rs`）

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use thiserror::Error;
use uuid::Uuid;

use crate::RepoError;

pub mod issue;
pub mod listing;
pub mod receipt;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// 行结构
// ---------------------------------------------------------------------------

/// 生成 wakeup 面的主键（上游用 `dbid.NewV7()`：连续入队聚在窄主键区间）。
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

/// `issue_wakeup` 行（26 列）——字段名即列名，`Serialize` 出来的键与上游 sqlc
/// 结构的 json tag 逐字一致（`id` / `workspace_id` / … / `updated_at`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WakeupRow {
    /// 主键。
    pub id: Uuid,
    /// 所属工作区（容量上限按它统计）。
    pub workspace_id: Uuid,
    /// 所属 issue。
    pub issue_id: Uuid,
    /// 被唤醒的 agent。
    pub agent_id: Uuid,
    /// 注册者（人类成员）。
    pub created_by: Uuid,
    /// 注册这条 wakeup 的 task（自触发抑制用）。
    pub source_task_id: Option<Uuid>,
    /// 注册它的评论（留痕）。
    pub parent_comment_id: Option<Uuid>,
    /// 唤醒时注入的指令正文。
    pub instruction: String,
    /// `event | at | every | cron`。
    pub kind: String,
    /// `once | continuous`。
    pub mode: String,
    /// 订阅的事件名集合（`text[] NOT NULL DEFAULT '{}'`）。
    pub event_types: Vec<String>,
    /// 主体过滤类型（`member | agent`，`531` 的 CHECK 只允许这两个）。
    pub filter_actor_type: Option<String>,
    /// 主体过滤 id。
    pub filter_actor_id: Option<Uuid>,
    /// agent 过滤（`509`）。
    pub filter_agent_id: Option<Uuid>,
    /// task 过滤（`509`）。
    pub filter_task_id: Option<Uuid>,
    /// `kind=every` 的间隔秒数。
    pub interval_seconds: Option<i64>,
    /// `kind=cron` 的 5 字段表达式。
    pub cron_expression: Option<String>,
    /// 调度时区（IANA）。
    pub timezone: String,
    /// 下次触发时间。
    pub next_fire_at: Option<DateTime<Utc>>,
    /// 是否启用。
    pub enabled: bool,
    /// 停用时间。
    pub disabled_at: Option<DateTime<Utc>>,
    /// 版本号（乐观并发 + receipt 合并作用域）。
    pub revision: i64,
    /// 最近派出的 task。
    pub last_task_id: Option<Uuid>,
    /// 最近一次失败信息。
    pub last_error: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for WakeupRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            issue_id: row.try_get("issue_id")?,
            agent_id: row.try_get("agent_id")?,
            created_by: row.try_get("created_by")?,
            source_task_id: row.try_get("source_task_id")?,
            parent_comment_id: row.try_get("parent_comment_id")?,
            instruction: row.try_get("instruction")?,
            kind: row.try_get("kind")?,
            mode: row.try_get("mode")?,
            event_types: row.try_get("event_types")?,
            filter_actor_type: row.try_get("filter_actor_type")?,
            filter_actor_id: row.try_get("filter_actor_id")?,
            filter_agent_id: row.try_get("filter_agent_id")?,
            filter_task_id: row.try_get("filter_task_id")?,
            interval_seconds: row.try_get("interval_seconds")?,
            cron_expression: row.try_get("cron_expression")?,
            timezone: row.try_get("timezone")?,
            next_fire_at: row.try_get("next_fire_at")?,
            enabled: row.try_get("enabled")?,
            disabled_at: row.try_get("disabled_at")?,
            revision: row.try_get("revision")?,
            last_task_id: row.try_get("last_task_id")?,
            last_error: row.try_get("last_error")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

impl WakeupRow {
    /// 是否事件驱动（只有 `event` 不由调度器推进 `next_fire_at`）。
    #[must_use]
    pub fn is_event(&self) -> bool {
        self.kind == "event"
    }

    /// 是否单次（`once` 消费后走 disabled 路径）。
    #[must_use]
    pub fn is_once(&self) -> bool {
        self.mode == "once"
    }
}

/// `GET /api/issues/:id/wakeups` 的行（上游 `ListIssueWakeupsRow`）：26 列 + 4 个别名列。
///
/// 三个 `filter_*_id` 是**掩码后**的值（`CASE WHEN … THEN w.filter_*_id END`）：调用者看不到
/// 不可访问的 agent/task 过滤时，上游返回 `null` 而不是把 id 泄漏出去。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueWakeupView {
    /// 主键。
    pub id: Uuid,
    /// 所属工作区。
    pub workspace_id: Uuid,
    /// 所属 issue。
    pub issue_id: Uuid,
    /// 被唤醒的 agent。
    pub agent_id: Uuid,
    /// 注册者。
    pub created_by: Uuid,
    /// 注册它的 task。
    pub source_task_id: Option<Uuid>,
    /// 注册它的评论。
    pub parent_comment_id: Option<Uuid>,
    /// 指令正文。
    pub instruction: String,
    /// 类型。
    pub kind: String,
    /// 单次 / 持续。
    pub mode: String,
    /// 订阅事件集合。
    pub event_types: Vec<String>,
    /// 主体过滤类型。
    pub filter_actor_type: Option<String>,
    /// 主体过滤 id（掩码后）。
    pub filter_actor_id: Option<Uuid>,
    /// 主体过滤展示名（`COALESCE(agent.name, user.name, '')`）。
    pub filter_actor_name: String,
    /// agent 过滤 id（掩码后）。
    pub filter_agent_id: Option<Uuid>,
    /// task 过滤 id（掩码后）。
    pub filter_task_id: Option<Uuid>,
    /// `every` 的间隔秒数。
    pub interval_seconds: Option<i64>,
    /// `cron` 表达式。
    pub cron_expression: Option<String>,
    /// 时区。
    pub timezone: String,
    /// 下次触发时间。
    pub next_fire_at: Option<DateTime<Utc>>,
    /// 是否启用。
    pub enabled: bool,
    /// 停用时间。
    pub disabled_at: Option<DateTime<Utc>>,
    /// 版本号。
    pub revision: i64,
    /// 最近派出的 task。
    pub last_task_id: Option<Uuid>,
    /// 最近一次失败信息。
    pub last_error: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 被唤醒 agent 的展示名。
    pub agent_name: String,
    /// agent 过滤的展示名（掩码后为空）。
    pub filter_agent_name: Option<String>,
    /// 最近那个 task 的状态。
    pub last_task_status: Option<String>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for IssueWakeupView {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            issue_id: row.try_get("issue_id")?,
            agent_id: row.try_get("agent_id")?,
            created_by: row.try_get("created_by")?,
            source_task_id: row.try_get("source_task_id")?,
            parent_comment_id: row.try_get("parent_comment_id")?,
            instruction: row.try_get("instruction")?,
            kind: row.try_get("kind")?,
            mode: row.try_get("mode")?,
            event_types: row.try_get("event_types")?,
            filter_actor_type: row.try_get("filter_actor_type")?,
            filter_actor_id: row.try_get("filter_actor_id")?,
            filter_actor_name: row.try_get("filter_actor_name")?,
            filter_agent_id: row.try_get("filter_agent_id")?,
            filter_task_id: row.try_get("filter_task_id")?,
            interval_seconds: row.try_get("interval_seconds")?,
            cron_expression: row.try_get("cron_expression")?,
            timezone: row.try_get("timezone")?,
            next_fire_at: row.try_get("next_fire_at")?,
            enabled: row.try_get("enabled")?,
            disabled_at: row.try_get("disabled_at")?,
            revision: row.try_get("revision")?,
            last_task_id: row.try_get("last_task_id")?,
            last_error: row.try_get("last_error")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            agent_name: row.try_get("agent_name")?,
            filter_agent_name: row.try_get("filter_agent_name")?,
            last_task_status: row.try_get("last_task_status")?,
        })
    }
}

/// `GET /api/issue-wakeup-summaries` 的行（上游 `ListWorkspaceWakeupSummaryRowsRow`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WakeupSummaryRow {
    /// issue。
    pub issue_id: Uuid,
    /// wakeup。
    pub id: Uuid,
    /// 被唤醒的 agent。
    pub agent_id: Uuid,
    /// agent 展示名。
    pub agent_name: String,
    /// 类型。
    pub kind: String,
    /// 单次 / 持续。
    pub mode: String,
    /// 订阅事件集合。
    pub event_types: Vec<String>,
    /// 主体过滤类型。
    pub filter_actor_type: Option<String>,
    /// 主体过滤 id（掩码后）。
    pub filter_actor_id: Option<Uuid>,
    /// 主体过滤展示名。
    pub filter_actor_name: String,
    /// task 过滤 id（掩码后）。
    pub filter_task_id: Option<Uuid>,
    /// agent 过滤展示名（掩码后为空）。
    pub filter_agent_name: Option<String>,
    /// `every` 的间隔秒数。
    pub interval_seconds: Option<i64>,
    /// `cron` 表达式。
    pub cron_expression: Option<String>,
    /// 时区。
    pub timezone: String,
    /// 下次触发时间。
    pub next_fire_at: Option<DateTime<Utc>>,
    /// 该 issue 上的 enabled wakeup 总数（含未进预览的那些）。
    pub active_count: i64,
    /// 其中 `kind='event'` 的数量。
    pub event_count: i64,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for WakeupSummaryRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            issue_id: row.try_get("issue_id")?,
            id: row.try_get("id")?,
            agent_id: row.try_get("agent_id")?,
            agent_name: row.try_get("agent_name")?,
            kind: row.try_get("kind")?,
            mode: row.try_get("mode")?,
            event_types: row.try_get("event_types")?,
            filter_actor_type: row.try_get("filter_actor_type")?,
            filter_actor_id: row.try_get("filter_actor_id")?,
            filter_actor_name: row.try_get("filter_actor_name")?,
            filter_task_id: row.try_get("filter_task_id")?,
            filter_agent_name: row.try_get("filter_agent_name")?,
            interval_seconds: row.try_get("interval_seconds")?,
            cron_expression: row.try_get("cron_expression")?,
            timezone: row.try_get("timezone")?,
            next_fire_at: row.try_get("next_fire_at")?,
            active_count: row.try_get("active_count")?,
            event_count: row.try_get("event_count")?,
        })
    }
}

/// `issue_wakeup_receipt` 行（10 列）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WakeupReceiptRow {
    /// 主键（合并时轮换）。
    pub id: Uuid,
    /// 所属 wakeup。
    pub wakeup_id: Uuid,
    /// 合并作用域版本。
    pub revision: i64,
    /// 事件键（合并后保留首次那次的键）。
    pub event_key: String,
    /// 事件名（同时也是 `coalesce_key`）。
    pub event_type: String,
    /// evidence 载荷。
    pub payload: serde_json::Value,
    /// 合并键（`528`；`NULL` 表示不参与合并）。
    pub coalesce_key: Option<String>,
    /// 消费它的 task。
    pub task_id: Option<Uuid>,
    /// 处理完成时间（`NULL` = 待处理）。
    pub processed_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for WakeupReceiptRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            wakeup_id: row.try_get("wakeup_id")?,
            revision: row.try_get("revision")?,
            event_key: row.try_get("event_key")?,
            event_type: row.try_get("event_type")?,
            payload: row.try_get("payload")?,
            coalesce_key: row.try_get("coalesce_key")?,
            task_id: row.try_get("task_id")?,
            processed_at: row.try_get("processed_at")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

// ---------------------------------------------------------------------------
// 错误识别（按约束名 / SQLSTATE，不匹配文案）
// ---------------------------------------------------------------------------

/// 库侧容量触发器（`530`）报的错：`23514` + `issue_wakeup_active_limit`。
///
/// 上游 `wakeupError` 用 `pgErr.ConstraintName == "issue_wakeup_active_limit"` 判它，
/// 本仓照抄（**不**匹配错误文案，文案会随迁移变）。
#[must_use]
pub fn is_active_limit_violation(err: &sqlx::Error) -> bool {
    constraint_name(err).is_some_and(|name| name == "issue_wakeup_active_limit")
}

/// `FOR UPDATE NOWAIT` 拿不到锁（`55P03`）——上游映射成 409 `wakeup_source_busy`。
#[must_use]
pub fn is_source_busy(err: &sqlx::Error) -> bool {
    sqlstate(err).as_deref() == Some("55P03")
}

/// 唯一键冲突（`23505`）。
#[must_use]
pub fn is_unique_violation(err: &sqlx::Error) -> bool {
    sqlstate(err).as_deref() == Some("23505")
}

/// 冲突的唯一索引名（`23505` 时才有值）——`capture_issue_wakeup` 想区分
/// `issue_wakeup_receipt_key_idx`（重复事件，吞掉）与其它唯一冲突（重抛）。
#[must_use]
pub fn unique_violation_constraint(err: &sqlx::Error) -> Option<&str> {
    if is_unique_violation(err) {
        constraint_name(err)
    } else {
        None
    }
}

fn sqlstate(err: &sqlx::Error) -> Option<Cow<'_, str>> {
    match err {
        sqlx::Error::Database(db) => db.code(),
        _ => None,
    }
}

fn constraint_name(err: &sqlx::Error) -> Option<&str> {
    match err {
        sqlx::Error::Database(db) => db.constraint(),
        _ => None,
    }
}

/// 仓储错误：RowNotFound → [`RepoError::NotFound`]，其余走 [`crate::workspace::map_sqlx_err`]。
///
/// 单独包一层是为了让 `FOR UPDATE` 拿不到行（上游 `pgx.ErrNoRows` → 404）与真错误分开。
pub(crate) fn map_wakeup_err(err: sqlx::Error) -> RepoError {
    crate::workspace::map_sqlx_err(err)
}

// ---------------------------------------------------------------------------
// 写路径特有的失败（容量触发器 / NOWAIT 锁）
// ---------------------------------------------------------------------------

/// 只有 `issue_wakeup` 的**写路径**会踩到的两个特例 —— 它们必须与普通 `RepoError` 分开，
/// 否则 HTTP 层拿不到上游要的两种响应形态（400 `wakeup_capacity_exceeded` /
/// 409 `wakeup_source_busy`）：
///
/// - [`Self::Capacity`]：`530` 的 `guard_issue_wakeup_capacity()` 抛 `23514` +
///   `CONSTRAINT='issue_wakeup_active_limit'`；message 取库的原文（上游原样写给客户端）；
/// - [`Self::SourceBusy`]：`LockWakeupSourceTask` 的 `FOR UPDATE NOWAIT` 报 `55P03`。
///
/// 其余一律 [`Self::Repo`]，由调用方按 [`RepoError`] 判 404/409/500。
#[derive(Debug, Error)]
pub enum WakeupRepoError {
    /// 活跃 wakeup 数量超上限（`32`/`1000`，常量在 `mc_core::wakeup`）。
    #[error("{0}")]
    Capacity(String),
    /// 源 run 正在变更（`NOWAIT` 拿不到锁），注册方可重试。
    #[error("source run is changing")]
    SourceBusy,
    /// 其它仓储错误。
    #[error(transparent)]
    Repo(#[from] RepoError),
}

impl WakeupRepoError {
    /// 转成 [`RepoError`]（丢掉两个特例的区分）——给只用 `RepoError` 的老调用点用。
    #[must_use]
    pub fn into_repo_error(self) -> RepoError {
        match self {
            Self::Capacity(message) => RepoError::Db(message),
            Self::SourceBusy => RepoError::Db("source run is changing".to_string()),
            Self::Repo(inner) => inner,
        }
    }
}

/// 写路径的 sqlx 错误分类（**按约束名 / SQLSTATE**，不匹配文案）。
pub(crate) fn map_wakeup_write_err(err: sqlx::Error) -> WakeupRepoError {
    if is_active_limit_violation(&err) {
        let message = err
            .as_database_error()
            .map_or_else(|| err.to_string(), |db| db.message().to_string());
        return WakeupRepoError::Capacity(message);
    }
    if is_source_busy(&err) {
        return WakeupRepoError::SourceBusy;
    }
    WakeupRepoError::Repo(crate::workspace::map_sqlx_err(err))
}
