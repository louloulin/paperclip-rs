//! issue wakeup 面（`issue_wakeup` / `issue_wakeup_receipt`）—— 服务层。
//!
//! - **写者**：M5-6（`wakeup/**` 整组）。
//! - **上游**：`handler/issue_wakeup.go`320 + `wakeup_actor.go`63（≈64）+
//!   `service/issue_wakeup.go`831（`Validate`108 / `save`221 / `dispatch`185 / `CheckClaim`32 / `Tick`35）
//!   以及 `service/issue_wakeup_evidence.go`135（136）、`db/queries/wakeup.sql`162/23 查询 +
//!   `workspace_wakeup.sql`70/1 查询。
//! - **两个正交维度**：`kind ∈ {event, at, every, cron}` × `mode ∈ {once, continuous}`。
//!   旧桩把它们压成了一个 `source` 枚举（`mc_core::wakeup` 的「旧 stub 错在哪」表）。
//! - **`revision` 是两件事**：乐观并发的版本号 **且** 合并作用域
//!   （`issue_wakeup_receipt` 的唯一索引含 `revision`）⇒ **配置变更必须 bump `revision`**，
//!   否则旧的 pending receipt 会继续生效。
//! - **DB 侧还有 5 个迁移**（`528`–`532`：合并 / pending 事件 / 有界捕获 / actor 过滤 / actor 捕获）
//!   —— 本波 0 新迁移，但要**照抄** `capture_issue_wakeup()` 的语义，见 [`evidence`]。
//! - **路由**：8 条里 7 条挂在 `/api/issues/{id}/wakeups*` 下（M5-0 已把 501 占位**搬进**
//!   `mc-http/src/routes/issues/wakeups.rs`，本片只填实现、不改注册位置）；
//!   `GET /api/issue-wakeup-summaries` 本片**新增注册**（#29，本波唯一的真 `known_gap` 键）。
//!
//! # 本片的两处落点偏离（`docs/44` §3.2 / §5.2 之外，已在交付注释登记）
//!
//! 1. **cron 解析落在本片**：`service/cron.go` 的移植原定在 `src/trigger.rs`（M5-3），但 C 波未启动
//!    ⇒ 本片在 [`schedule`] 内自带等价的最小 5 字段解析器（`Validate` 的 `cron` 分支必须能算出
//!    `next_fire_at`，否则 `kind=cron` 无法创建）。M5-3 落地时应改为复用 `trigger.rs` 的实现，
//!    并把 [`schedule`] 删掉（形态与实测依据见 `mc-autopilot/src/lib.rs` 的选型段）。
//! 2. **错误类型放本模块**：`src/error.rs` 是 M5-1 的写集（并发片）⇒ [`WakeupError`] 定义在
//!    [`service`] 外的本文件，M5-1 落地后若要收拢到 `error.rs` 可以直接搬。

pub mod evidence;
pub mod schedule;
pub mod service;

use thiserror::Error;

/// wakeup 面的服务层错误（上游 `ErrWakeupInput` / `ErrWakeupConflict` / `ErrWakeupForbidden` 三哨兵
/// + 两个按库错误分类出来的特例）。
///
/// **Display 文案是契约**：上游 handler 的 `wakeupError` 把 `err.Error()` 直接写进 400/409 响应体
/// ⇒ 这里逐字复刻（`invalid wakeup: …` / `wakeup changed; refresh and retry` / `wakeup permission denied`）。
/// 只有 `Capacity` / `SourceBusy` 例外：上游用固定的 code+message（`wakeup_capacity_exceeded` /
/// `wakeup_source_busy`），HTTP 层直接写常量，不读 Display。
#[derive(Debug, Error)]
pub enum WakeupError {
    /// 上游 `ErrWakeupInput`：`invalid wakeup: <具体原因>`。
    #[error("invalid wakeup: {0}")]
    Input(String),
    /// 上游 `ErrWakeupConflict`（409）。
    #[error("wakeup changed; refresh and retry")]
    Conflict,
    /// 上游 `ErrWakeupForbidden`（403）。
    #[error("wakeup permission denied")]
    Forbidden,
    /// 上游 `pgx.ErrNoRows`（404 `wakeup not found`）。
    #[error("wakeup not found")]
    NotFound,
    /// `530` 的容量触发器（→ 400 `wakeup_capacity_exceeded`，message 取库的原文）。
    #[error("wakeup capacity exceeded: {0}")]
    Capacity(String),
    /// `55P03`：`LockWakeupSourceTask` 的 `FOR UPDATE NOWAIT` 拿不到锁（→ 409 `wakeup_source_busy`）。
    #[error("wakeup_source_busy")]
    SourceBusy,
    /// 派发路径的 owner fence 拿不到行 —— 上游在这里 `return pgx.ErrNoRows`，
    /// 由 `Tick` 写进 `last_error`。Display 复刻库文案，便于与上游日志逐字对照。
    #[error("no rows in result set")]
    NotDispatchable,
    /// 其余库错误（→ 500）。
    #[error("{0}")]
    Db(String),
}

impl WakeupError {
    /// `invalid wakeup: <msg>`。
    pub fn input(msg: impl Into<String>) -> Self {
        Self::Input(msg.into())
    }
}

impl From<mc_repos::RepoError> for WakeupError {
    fn from(err: mc_repos::RepoError) -> Self {
        match err {
            mc_repos::RepoError::NotFound => Self::NotFound,
            mc_repos::RepoError::Conflict => Self::Conflict,
            mc_repos::RepoError::Db(msg) => Self::Db(msg),
        }
    }
}

impl From<mc_repos::wakeup::WakeupRepoError> for WakeupError {
    fn from(err: mc_repos::wakeup::WakeupRepoError) -> Self {
        match err {
            mc_repos::wakeup::WakeupRepoError::Capacity(message) => Self::Capacity(message),
            mc_repos::wakeup::WakeupRepoError::SourceBusy => Self::SourceBusy,
            mc_repos::wakeup::WakeupRepoError::Repo(inner) => inner.into(),
        }
    }
}

/// `sqlx` 错误按**约束名 / SQLSTATE**分类（分类逻辑复用 `mc-repos`，不在服务层匹配文案）。
impl From<sqlx::Error> for WakeupError {
    fn from(err: sqlx::Error) -> Self {
        if mc_repos::wakeup::is_active_limit_violation(&err) {
            return Self::Capacity(err.to_string());
        }
        if mc_repos::wakeup::is_source_busy(&err) {
            return Self::SourceBusy;
        }
        match err {
            sqlx::Error::RowNotFound => Self::NotFound,
            other => Self::Db(other.to_string()),
        }
    }
}
