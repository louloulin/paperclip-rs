//! M4-4：Mika 引路（上游 `StartMikaOnboarding` handler 的落库面）。
//!
//! | 本文件方法 | 上游 |
//! | --- | --- |
//! | [`ChatTaskRepo::chat_session_has_user_message`] | `ChatSessionHasUserMessage`（`chat.sql:1514`） |
//! | [`ChatTaskRepo::user_onboarding_profile`] | `user.timezone` / `user.onboarding_questionnaire` 直读 |
//! | [`ChatTaskRepo::workspace_name`] | `workspace.name` 直读 |
//! | [`ChatTaskRepo::start_mika_onboarding`] | `TaskService.OpenMikaOnboardingChat`（`task.go:2530`） |
//!
//! `OpenMikaOnboardingChat` 在**一把会话锁**下写两条行：隐藏的 kickoff（role=user、
//! `message_kind='onboarding_kickoff'`、无 task）与会员读到的开场白（role=assistant、
//! `created_at = kickoff + 1µs`）。两者必须同事务，且 opening 的时间戳必须显式推导 ——
//! `now()` 是**事务**时间戳，两行会落在同一微秒，而会话列表的 `ORDER BY created_at DESC
//! LIMIT 1` 没有并列打破器（id 是随机 uuid），并列时可能选中 kickoff（其 kind 让
//! `buildChatLastMessage` 返回 nil）⇒ 一个 onboarding 完全成功的会话会报告「没有最后一条
//! 消息」，UI 上的 "Start with Mika" 恢复卡片会重新冒出来。差 1 微秒让顺序成为全序。
//!
//! **有意偏离**（`docs/45` §`known_gap`）：`user` / `workspace` 的两个读不走上游仓储
//! （`UserRepo` / `WorkspaceRepo` 都不暴露 `timezone` / `onboarding_questionnaire` /
//! `name`），按 `crate::chat_session` 的先例在仓储内原样写最小 SELECT。
//! `publishChat`（开场白广播）属 LUM-1506，本片不发。

use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::chat_message::{ChatMessageRow, MESSAGE_COLUMNS};
use crate::workspace::map_sqlx_err;
use crate::RepoWithDb;
use crate::Result;

use super::support::{OnboardingOpenResult, StartOnboardingOutcome};
use super::ChatTaskRepo;

/// `user` 表里引路文案要的两列。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserOnboardingRow {
    /// IANA 时区名（可空；`profile_block` 里 `%q` 渲染，未知时区另给一行提示）。
    pub timezone: Option<String>,
    /// 问卷答案 JSONB（列 `NOT NULL DEFAULT '{}'`）。
    pub onboarding_questionnaire: JsonValue,
}

impl ChatTaskRepo {
    /// 上游 `ChatSessionHasUserMessage`（`chat.sql:1514`）：**不做** `message_kind` 过滤 ——
    /// kickoff 行本身就是 role='user'，这正是「已经开过门」能被检出的原因。
    pub async fn chat_session_has_user_message(&self, session_id: Uuid) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM chat_message \
             WHERE chat_session_id = $1 AND role = 'user')",
        )
        .bind(session_id)
        .fetch_one(self.db().pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 读引路文案需要的用户属性（时区 + 问卷）。
    pub async fn user_onboarding_profile(
        &self,
        user_id: Uuid,
    ) -> Result<Option<UserOnboardingRow>> {
        sqlx::query_as::<_, UserOnboardingRow>(
            "SELECT timezone, onboarding_questionnaire FROM \"user\" WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(self.db().pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 读 workspace 名（开场白模板的 `{workspace}` 槽位）。
    pub async fn workspace_name(&self, workspace_id: Uuid) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT name FROM workspace WHERE id = $1")
            .bind(workspace_id)
            .fetch_optional(self.db().pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `TaskService.OpenMikaOnboardingChat`（`task.go:2530`）。
    ///
    /// 锁与锁顺序都与发送路径一致（会话先锁），因此「开门」与「首次发送」/「runtime 重绑」
    /// 会串行化而不是死锁。`AlreadyStarted` / `SessionArchived` 是**正常结局**，不是错误。
    pub async fn start_mika_onboarding(
        &self,
        session_id: Uuid,
        kickoff: &str,
        opening: &str,
    ) -> Result<StartOnboardingOutcome> {
        let mut tx = self.db().pool().begin().await.map_err(map_sqlx_err)?;

        let locked: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM chat_session WHERE id = $1 FOR UPDATE")
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if locked.is_none() {
            return Ok(StartOnboardingOutcome::SessionArchived);
        }

        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM chat_session WHERE id = $1")
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if status.as_deref() != Some("active") {
            return Ok(StartOnboardingOutcome::SessionArchived);
        }

        // kickoff 是 role='user'，所以这道检查同时也是「重复开门」的幂等闸。
        let has_user_message: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM chat_message \
             WHERE chat_session_id = $1 AND role = 'user')",
        )
        .bind(session_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        if has_user_message {
            return Ok(StartOnboardingOutcome::AlreadyStarted);
        }

        let kickoff_row: ChatMessageRow = sqlx::query_as(&format!(
            "INSERT INTO chat_message (chat_session_id, role, content, message_kind, id) \
             VALUES ($1, 'user', $2, 'onboarding_kickoff', $3) RETURNING {MESSAGE_COLUMNS}"
        ))
        .bind(session_id)
        .bind(kickoff)
        .bind(Uuid::now_v7())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        let opening_row: ChatMessageRow = sqlx::query_as(&format!(
            "INSERT INTO chat_message (chat_session_id, role, content, message_kind, \
                                       created_at, id) \
             VALUES ($1, 'assistant', $2, 'onboarding_opening', \
                     $3::timestamptz + interval '1 microsecond', $4) \
             RETURNING {MESSAGE_COLUMNS}"
        ))
        .bind(session_id)
        .bind(opening)
        .bind(kickoff_row.created_at)
        .bind(Uuid::now_v7())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        sqlx::query("UPDATE chat_session SET updated_at = now() WHERE id = $1")
            .bind(session_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;

        tx.commit().await.map_err(map_sqlx_err)?;

        Ok(StartOnboardingOutcome::Started(Box::new(
            OnboardingOpenResult {
                kickoff: kickoff_row,
                opening: opening_row,
            },
        )))
    }
}
