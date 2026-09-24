//! 扫码安装会话：设备流（`begin` → 轮询 `status` → 终态）的**进程内**状态投影。
//!
//! - **写者**：M7-0 anchor（本片；`docs/60-M7-PLAN.md` §2.1 / §2.5）。落地后**各片只读**。
//! - **上游**：`internal/integrations/lark/install_session_store.go`（`InstallSessionState` /
//!   `InstallSessionOutcome` / `InstallSessionStore`）+ `install_session_redis_store.go`
//!   （多副本实现）+ `registration_service.go`（`RegistrationSessionStatus` 三态）。
//!
//! # 为什么是"进程内"（`docs/60` §2.5 的 R-M7-1）
//!
//! 上游这一层**本来**有 Redis 与进程内两份实现，且进程内那份是**官方降级路径**
//! （`router.go:742` 有明文 `slog.Warn("…no Redis; bind sessions are per-process…")`）。
//! 本仓没有 Redis 依赖且本波**不引入**，所以落进程内实现 + **单副本部署契约**。
//! 代价写清楚：**会话不跨进程存活** —— 起它的那个进程死了，别的副本会一直报 `pending`
//! 直到 `expires_at`，然后读路径报过期（这正是上游注释里 `MUL-7340` 那个事故的形状，
//! 上游靠共享状态解决；本仓靠单副本假设解决）。
//!
//! # 三个**故意**不在本结构里的东西
//!
//! 1. **`device_code` 不入库/不入共享状态**：它是承载凭据（谁拿到谁能完成授权），只有跑
//!    轮询循环的那个进程需要它 ⇒ 留在进程内存里（上游注释逐字如此）。所以本结构里
//!    **没有** `device_code` 字段。
//! 2. **`initiator_id` 不序列化给客户端**：它只用于状态读的授权（发起人，或 workspace
//!    的 owner/admin）。`serde(skip_serializing)` 把这条纪律写进类型。
//! 3. **不做"存在但不是你的"这种可区分错误**：未知 / 过期 / 跨 workspace 一律同一个
//!    `not found`（否则调用方能枚举别的 workspace 的会话 id）。本结构不含错误类型 ——
//!    仓储 port 的错误口径归 M7-14。
//!
//! # 终态是**first-writer-wins**
//!
//! 过期截止与轮询结果可能并发触发，用户必须看到他**已经看到过**的那个结果：输的那次写是
//! no-op，**不是**错误。所以 [`InstallSession::mark_terminal`] 返回 `bool`（是否由本次写成）。

use serde::{Deserialize, Serialize};

use crate::id::Id;
use crate::timestamp::Timestamp;

/// `begin` 会话的三态（上游 `lark.RegistrationSessionStatus`，wire 值逐字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationSessionStatus {
    /// 二维码已生成，后台轮询还在跑；前端按 `poll_interval_seconds` 继续轮询。
    Pending,
    /// 设备流拿到凭据**且**安装行 + 发起人绑定已提交；`installation_id` 已填。
    Success,
    /// 终态失败（过期 / 用户拒绝 / 协议错 / 后续 bot-info 或 DB 错）；`error_reason` 是稳定码。
    Error,
}

impl RegistrationSessionStatus {
    /// wire 字面量（前端按它分支）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Error => "error",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "success" => Some(Self::Success),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// 是不是终态（终态之后 `mark_terminal` 只认第一个写者）。
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// 安装会话的状态投影（上游 `InstallSessionState`）。
///
/// ⚠️ **`initiator_id` 不进响应**（[`InstallSession::public_view`] 是唯一的对外投影）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallSession {
    /// 会话 id（`GET …/install/{sessionId}/status` 的路径参数）。
    pub id: String,
    pub workspace_id: Id,
    /// 发起人：授权状态读的人（发起人本身，或 workspace owner/admin）。
    /// **不**序列化给客户端。
    #[serde(skip_serializing)]
    pub initiator_id: Id,
    pub status: RegistrationSessionStatus,
    /// 成功时的安装行 id。
    pub installation_id: Option<Id>,
    /// 失败的稳定错误码（前端据此选文案，**不解析** `error_message`）。
    pub error_reason: Option<String>,
    /// 失败的人类可读细节（可以进日志；不得含凭据）。
    pub error_message: Option<String>,
    /// 设备流凭据的截止时间：`pending` 超过它按**过期**报（否则会话会永远挂着 `pending`）。
    pub expires_at: Timestamp,
}

impl InstallSession {
    /// 新建一个 `pending` 会话（`begin` 的唯一合法初态）。
    pub fn pending(
        id: impl Into<String>,
        workspace_id: Id,
        initiator_id: Id,
        expires_at: Timestamp,
    ) -> Self {
        Self {
            id: id.into(),
            workspace_id,
            initiator_id,
            status: RegistrationSessionStatus::Pending,
            installation_id: None,
            error_reason: None,
            error_message: None,
            expires_at,
        }
    }

    /// 成功终态。
    #[must_use]
    pub fn succeeded(&self, installation_id: Id) -> Self {
        Self {
            status: RegistrationSessionStatus::Success,
            installation_id: Some(installation_id),
            error_reason: None,
            error_message: None,
            ..self.clone()
        }
    }

    /// 失败终态：`reason` 是稳定码，`message` 是可读细节。
    #[must_use]
    pub fn failed(&self, reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: RegistrationSessionStatus::Error,
            installation_id: None,
            error_reason: Some(reason.into()),
            error_message: Some(message.into()),
            ..self.clone()
        }
    }

    /// 读路径的过期判定：**`pending` 且已过 `expires_at`** ⇒ 报过期（终态不再看时间）。
    ///
    /// 这就是"拥有它的进程死了也不会永远 `pending`"的那条保险。
    pub fn is_expired_pending_at(&self, now: Timestamp) -> bool {
        self.status == RegistrationSessionStatus::Pending && now > self.expires_at
    }

    /// 按 `now` 归一化后的**对外**状态：过期的 `pending` 呈现为 [`Self::Error`] +
    /// `error_reason = "expired"`（读路径投影，不改存储）。
    pub fn effective_status_at(&self, now: Timestamp) -> RegistrationSessionStatus {
        if self.is_expired_pending_at(now) {
            RegistrationSessionStatus::Error
        } else {
            self.status
        }
    }

    /// 写终态：**first-writer-wins**（已在终态 ⇒ 什么都不改，返回 `false`）。
    ///
    /// 返回 `true` = 本次写生效；`false` = 输了竞态，**不是**错误（上游契约逐字）。
    pub fn mark_terminal(&mut self, outcome: InstallSession) -> bool {
        if self.status.is_terminal() {
            return false;
        }
        let id = self.id.clone();
        let workspace_id = self.workspace_id;
        let initiator_id = self.initiator_id;
        let expires_at = self.expires_at;
        *self = InstallSession {
            id,
            workspace_id,
            initiator_id,
            expires_at,
            ..outcome
        };
        true
    }

    /// 对外投影（**去掉 `initiator_id`**）：状态读响应只带这五个字段。
    pub fn public_view(&self, now: Timestamp) -> serde_json::Value {
        serde_json::json!({
            "session_id": self.id,
            "status": self.effective_status_at(now).as_str(),
            "installation_id": self.installation_id.map(Id::as_string),
            "error_reason": self.error_reason,
            "error_message": self.error_message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn future() -> Timestamp {
        Timestamp::from(chrono::Utc::now() + chrono::Duration::minutes(5))
    }

    fn session() -> InstallSession {
        InstallSession::pending("sess-1", Id::new(), Id::new(), future())
    }

    /// 三态与上游 wire 取值逐字一致，且只有 `pending` 不是终态。
    #[test]
    fn status_vocabulary_matches_upstream() {
        for status in [
            RegistrationSessionStatus::Pending,
            RegistrationSessionStatus::Success,
            RegistrationSessionStatus::Error,
        ] {
            assert_eq!(
                RegistrationSessionStatus::from_str_opt(status.as_str()),
                Some(status)
            );
        }
        assert_eq!(RegistrationSessionStatus::from_str_opt("cancelled"), None);
        assert!(!RegistrationSessionStatus::Pending.is_terminal());
        assert!(RegistrationSessionStatus::Success.is_terminal());
        assert!(RegistrationSessionStatus::Error.is_terminal());
    }

    /// `pending` 过期按 error 呈现；终态**不再**看时间。
    #[test]
    fn expired_pending_is_reported_as_error() {
        let now = Timestamp::now();
        let expired = InstallSession::pending("s", Id::new(), Id::new(), now);
        assert!(
            !expired.is_expired_pending_at(now),
            "expires_at == now 仍 pending"
        );
        let after = Timestamp::from(now.as_datetime() + chrono::Duration::seconds(1));
        assert!(expired.is_expired_pending_at(after));
        assert_eq!(
            expired.effective_status_at(after),
            RegistrationSessionStatus::Error
        );

        // 终态（成功）过了截止时间也仍是成功 —— 只有 pending 会"过期成 error"。
        let done = expired.succeeded(Id::new());
        assert!(!done.is_expired_pending_at(after));
        assert_eq!(
            done.effective_status_at(after),
            RegistrationSessionStatus::Success
        );
    }

    /// 终态 first-writer-wins：输的那次写是 no-op，且**不**覆盖先到的结果。
    #[test]
    fn terminal_outcome_is_first_writer_wins() {
        let mut live = session();
        let installed = Id::new();
        assert!(live.mark_terminal(live.succeeded(installed)));
        assert_eq!(live.status, RegistrationSessionStatus::Success);
        assert_eq!(live.installation_id, Some(installed));

        // 第二次（过期路径）必须被忽略，且用户仍看到他已看到的成功结果。
        assert!(!live.mark_terminal(live.failed("expired", "session expired")));
        assert_eq!(live.status, RegistrationSessionStatus::Success);
        assert_eq!(live.installation_id, Some(installed));
        assert!(live.error_reason.is_none());

        // 反向：先失败，后成功也进不来。
        let mut failed = session();
        assert!(failed.mark_terminal(failed.failed("user_denied", "denied")));
        assert!(!failed.mark_terminal(failed.succeeded(Id::new())));
        assert_eq!(failed.status, RegistrationSessionStatus::Error);
        assert_eq!(failed.error_reason.as_deref(), Some("user_denied"));
    }

    /// 对外投影**不含** `initiator_id`；`serde` 也不序列化它。
    #[test]
    fn public_view_hides_the_initiator() {
        let live = session();
        let initiator = live.initiator_id.as_string();
        let view = live.public_view(Timestamp::now());
        assert_eq!(view["session_id"], "sess-1");
        assert_eq!(view["status"], "pending");
        assert!(view["installation_id"].is_null());
        assert!(
            !view.to_string().contains(&initiator),
            "状态投影里不得出现发起人 id"
        );

        let encoded = serde_json::to_string(&live).expect("serialize");
        assert!(!encoded.contains(&initiator));
    }
}
