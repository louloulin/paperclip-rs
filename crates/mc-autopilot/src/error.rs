//! crate 级错误类型与 HTTP 映射。
//!
//! - **写者**：M5-1（框架）。其余切片加自己的错误变体时**只准加变体**，不改既有签名。
//! - **上游**：上游没有集中的 error 类型（handler 各自 `writeError` + 状态码）；本地统一收敛到
//!   本文件，再由 `mc-http` 的 `ApiError` 转响应。
//! - **要覆盖的状态码语义**：400（参数/三态补丁非法）、403（`autopilotWriteByOwnership` /
//!   `memberCanWriteAutopilot` 判负）、404（`loadAutopilotInWorkspace` 判负 —— 非成员一律 404，
//!   与 `invitations::require_workspace_member` 一致）、409（订阅者并发锁 / token 唯一性冲突）。
//! - **纪律**：`AutopilotError → ApiError` 的映射是**契约**（M5-2..M5-6 都吃它），落地时要和
//!   `docs/44` §1.1 的路由级状态码逐条对齐；不要在切片里各自拼 `StatusCode`。
//!
//! # 本地映射表（`http_status()`，M5-2..M5-6 直接复用）
//!
//! | 变体 | 状态码 | 说明 |
//! | --- | ---: | --- |
//! | [`AutopilotError::Validation`] | 400 | 参数/三态补丁非法；带 `code` 时走扁平错误体 |
//! | [`AutopilotError::Cron`] | 400 | `invalid_cron` / `invalid_timezone`（`CronError::code()`） |
//! | [`AutopilotError::Forbidden`] | 403 | 写门拒绝，`code` 是机器可读的拒绝码 |
//! | [`AutopilotError::NotFound`] | 404 | 行不存在**或**跨工作区（上游两者同形） |
//! | [`AutopilotError::Conflict`] | 409 | 并发/唯一性冲突（订阅者锁、token 唯一性） |
//! | [`AutopilotError::Internal`] | 500 | 其余（DB 故障等） |
//!
//! # 与 `mc_errors::Error` 的关系
//!
//! `mc-errors` **不在 M5-1 的写集里**（`docs/44` §3.2）⇒ 不往那里加变体。HTTP 侧把
//! `AutopilotError` 折成本仓既有的嵌套错误体（`{"error":{"code":…,"message":…}}`），只有上游
//! **显式**用 `writeErrorCode` 写扁平体 `{"error":msg,"code":code}` 的端点在 `mc-http` 侧另建
//! 自有响应类型（见 `routes/autopilots/dto.rs` 的 `CronPreviewError`）。

use crate::cron::CronError;

/// 领域层错误（服务层用；HTTP 状态码由 [`AutopilotError::http_status`] 给出）。
#[derive(Debug, Clone, thiserror::Error)]
pub enum AutopilotError {
    /// 入参非法（400）。`code` 给出时，HTTP 侧走**扁平**错误体（上游 `writeErrorCode`）。
    #[error("{message}")]
    Validation {
        /// 人类可读的原因。
        message: String,
        /// 机器可读的稳定码（`None` = 走本仓嵌套错误体）。
        code: Option<String>,
    },
    /// 写门拒绝（403）。`code` 是上游那五个稳定拒绝码之一。
    #[error("{message}")]
    Forbidden {
        /// 稳定拒绝码。
        code: String,
        /// 人类可读的原因。
        message: String,
    },
    /// 资源不存在（404）—— 含**跨工作区**的同形情形。
    #[error("{message}")]
    NotFound {
        /// 人类可读的原因。
        message: String,
    },
    /// 并发/唯一性冲突（409）。
    #[error("{message}")]
    Conflict {
        /// 人类可读的原因。
        message: String,
    },
    /// 其余（500）。
    #[error("{message}")]
    Internal {
        /// 人类可读的原因。
        message: String,
    },
    /// cron 解析/推算失败（400；`invalid_cron` / `invalid_timezone`）。
    #[error(transparent)]
    Cron(#[from] CronError),
}

impl AutopilotError {
    /// 400（走嵌套错误体）。
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            code: None,
        }
    }

    /// 400 + 扁平错误体用的稳定码。
    pub fn validation_coded(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            code: Some(code.into()),
        }
    }

    /// 403 + 稳定拒绝码。
    pub fn forbidden(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Forbidden {
            code: code.into(),
            message: message.into(),
        }
    }

    /// 404。
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound {
            message: message.into(),
        }
    }

    /// 409。
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict {
            message: message.into(),
        }
    }

    /// 500。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }

    /// 机器可读码（没有就是 `None`）。
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Validation { code, .. } => code.as_deref(),
            Self::Forbidden { code, .. } => Some(code.as_str()),
            Self::Cron(err) => Some(err.code()),
            Self::NotFound { .. } | Self::Conflict { .. } | Self::Internal { .. } => None,
        }
    }

    /// HTTP 状态码（映射表见模块文档）。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Validation { .. } | Self::Cron(_) => 400,
            Self::Forbidden { .. } => 403,
            Self::NotFound { .. } => 404,
            Self::Conflict { .. } => 409,
            Self::Internal { .. } => 500,
        }
    }

    /// 可读消息。
    #[must_use]
    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl From<mc_repos::RepoError> for AutopilotError {
    /// 仓储错误折叠：`NotFound` → 404、`Conflict` → 409、`Db` → 500。
    fn from(err: mc_repos::RepoError) -> Self {
        match err {
            mc_repos::RepoError::NotFound => Self::not_found("autopilot not found"),
            mc_repos::RepoError::Conflict => Self::conflict("autopilot state conflict"),
            mc_repos::RepoError::Db(message) => Self::internal(message),
        }
    }
}

impl From<sqlx::Error> for AutopilotError {
    fn from(err: sqlx::Error) -> Self {
        Self::internal(err.to_string())
    }
}
