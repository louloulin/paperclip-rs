#![forbid(unsafe_code)]
//! `pc-board-auth` 公共类型。

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// CLI auth challenge 状态枚举 —— 与 Node 字面量 1:1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChallengeStatus {
    Pending,
    Approved,
    Cancelled,
    Expired,
}

impl ChallengeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
}

/// 与 Node `CliAuthChallengeStatus` 同名类型别名 —— 公共 API 字段用。
pub type CliAuthChallengeStatus = ChallengeStatus;

/// CLI auth 申请的访问级别 —— 与 Node 字面量 1:1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliRequestedAccess {
    Board,
    InstanceAdminRequired,
}

impl CliRequestedAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Board => "board",
            Self::InstanceAdminRequired => "instance_admin_required",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "board" => Some(Self::Board),
            "instance_admin_required" => Some(Self::InstanceAdminRequired),
            _ => None,
        }
    }
}

/// Board 用户访问上下文 —— 对应 Node `resolveBoardAccess` 返回值。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoardAccess {
    pub user: Option<BoardUserSummary>,
    pub company_ids: Vec<Uuid>,
    pub memberships: Vec<BoardMembership>,
    pub is_instance_admin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardUserSummary {
    pub id: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardMembership {
    pub company_id: Uuid,
    pub membership_role: Option<String>,
    pub status: String,
}

/// board_api_keys 的精简投影（list/get 用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardApiKeyListItem {
    pub id: Uuid,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 创建 board api key 后返回的 DTO（含明文 token）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardApiKeyCreated {
    pub id: Uuid,
    pub name: String,
    pub token: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// cli_auth_challenges 的完整行投影（内部使用，对应 Node `ChallengeRow`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliAuthChallengeRow {
    pub id: Uuid,
    pub secret_hash: String,
    pub command: String,
    pub client_name: Option<String>,
    pub requested_access: String,
    pub requested_company_id: Option<Uuid>,
    pub pending_key_hash: String,
    pub pending_key_name: String,
    pub approved_by_user_id: Option<String>,
    pub approved_at: Option<chrono::DateTime<chrono::Utc>>,
    pub cancelled_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Round 687: challenge 关联的 board api key id（approve 时回填）。
    #[serde(default)]
    pub board_api_key_id: Option<Uuid>,
}

/// 创建 CLI auth challenge 后返回的 DTO（含明文 secret + pending token）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliAuthChallengeCreated {
    pub challenge: CliAuthChallengeRow,
    pub challenge_secret: String,
    pub pending_board_token: String,
}

/// CLI auth challenge 描述符（describe 路径返回给 board UI）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliAuthChallengeDescription {
    pub id: Uuid,
    pub status: ChallengeStatus,
    pub command: String,
    pub client_name: Option<String>,
    pub requested_access: String,
    pub requested_company_id: Option<Uuid>,
    pub requested_company_name: Option<String>,
    pub approved_at: Option<chrono::DateTime<chrono::Utc>>,
    pub cancelled_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub approved_by_user: Option<BoardUserSummary>,
}

/// 业务错误。
#[derive(Debug, Error)]
pub enum BoardAuthServiceError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("repository error: {0}")]
    Repo(String),
}

pub type BoardAuthServiceResult<T> = Result<T, BoardAuthServiceError>;

impl From<pc_repos::RepoError> for BoardAuthServiceError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Repo(e.to_string())
    }
}

impl From<sqlx::Error> for BoardAuthServiceError {
    fn from(e: sqlx::Error) -> Self {
        Self::Repo(format!("sqlx: {e}"))
    }
}
