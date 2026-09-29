//! `workspace_invitation` 表的 DB-backed 仓储。
//!
//! 对应 multica server `internal/repo/invitation.go`，覆盖：
//! - 创建邀请（生成 32 字节 hex token，TTL 7 天）
//! - 按 token / id / workspace / email 查询
//! - 收件人接受（原子事务：标记 `accepted_at` + 创建 `member` row）
//! - 收件人拒绝 / 管理员撤销（软删除 via `revoked_at`）
//! - 速率限制：单 workspace 1h 内邀请计数
//!
//! 设计要点：
//! - `accept` 在单事务中完成：避免「已经接受但 member 行未插入」导致再次接受失败
//! - `accept` 走 UNIQUE(`workspace_id`, `user_id`) 冲突 → 幂等返回已存在 member
//! - `revoked_at` 与 `accepted_at` 互斥：再次 accept 时检查 `revoked_at`
//!
//! Token 形态：32 字节随机 → 43 字符 base64url（无 padding），URL 安全，无歧义字符。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use mc_core::id::Id;
use mc_core::member::WorkspaceMember;
use mc_core::workspace::WorkspaceRole;

/// 默认 token 字节长度（32 → base64url 后约 43 字符）。
pub const INVITATION_TOKEN_BYTES: usize = 32;
/// 默认 TTL：7 天。
pub const INVITATION_DEFAULT_TTL_DAYS: i64 = 7;
/// 创建邀请时给收件人 member 的默认 role。
pub const INVITATION_DEFAULT_ROLE: WorkspaceRole = WorkspaceRole::Member;

/// 数据库行映射。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct InvitationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub email: String,
    pub role: String,
    pub invited_by_user_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl InvitationRow {
    /// 转换为领域 `Id` 形态。
    pub fn id(&self) -> Id {
        Id(self.id)
    }
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }
    pub fn invited_by_user_id(&self) -> Id {
        Id(self.invited_by_user_id)
    }
    pub fn role(&self) -> WorkspaceRole {
        match self.role.as_str() {
            "admin" => WorkspaceRole::Admin,
            "owner" => WorkspaceRole::Owner,
            "guest" => WorkspaceRole::Guest,
            _ => WorkspaceRole::Member,
        }
    }
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
    pub fn is_accepted(&self) -> bool {
        self.accepted_at.is_some()
    }
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        !self.is_revoked() && !self.is_accepted() && !self.is_expired(now)
    }
}

/// 创建邀请的输入。
#[derive(Debug, Clone)]
pub struct NewInvitation {
    pub workspace_id: Id,
    pub email: String,
    pub role: WorkspaceRole,
    pub invited_by_user_id: Id,
    /// 可选 TTL 覆盖（秒）。None → 默认 7 天。
    pub ttl_secs: Option<i64>,
}

/// 接受邀请的返回结果。
#[derive(Debug, Clone)]
pub struct AcceptOutcome {
    pub member: WorkspaceMember,
    /// 已经被先前接受过（UNIQUE 冲突幂等）—— 当前调用方未实际写入 member。
    pub already_accepted: bool,
}

#[derive(Clone)]
pub struct InvitationRepo {
    pool: PgPool,
}
// ---------------------------------------------------------------------------
// 子模块（门 ⑩ 第 8 批拆分；0 行为变更）
// ---------------------------------------------------------------------------

mod repo;

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod tests;
