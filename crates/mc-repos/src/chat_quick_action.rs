//! M4-4（LUM-1475）：`chat_quick_action` 仓储 —— `POST /api/chat/sessions/:id/
//! quick-actions/regenerate`（上游 `router.go` #10）的落库面。
//!
//! | 本模块方法 | 上游 query |
//! | --- | --- |
//! | [`ChatQuickActionRepo::latest_regenerable_reply`] | `GetLatestAssistantChatMessageForSession`（`chat.sql:1694`） |
//! | [`ChatQuickActionRepo::has_active_chat_task_for_session`] | `HasActiveChatTaskForSession`（`chat.sql:1310`） |
//!
//! ⚠️ **anchor scaffold 的表判断是错的**（本片实测，登记在 `docs/45` §`known_gap`）：
//! scaffold 的模块文档说「上游真值：表 `quick_action`（`migrations/upstream/237_quick_action.up.sql`，
//! 15 列）；查询面 `quick_action.sql`（8 条 query）」。实测：**`quick_action` 表是
//! *issue* 快捷动作**（`237` 的头顶注释：「Issue Quick Actions (MUL-5465) … posts a
//! `quick_action` comment」），与 chat 的 quick actions **无关**；`quick_action.sql` 的 8 条
//! query 全部服务于 issue 面（本仓由 `crate::issue` 系列承担）。chat 的 quick actions 存在
//! **`chat_message.quick_actions` JSONB 列**（`migrations/upstream/235_…`）里，生成侧的
//! service 是 `service/chat_quick_actions*.go`（790 行，走 daemon ⇒ 本片只做**门禁**与
//! 落库读面，生成侧登记为 follow-up）。
//!
//! **可达性说明**：本部署没有 chat quick-actions provider，所以服务层的**第一句**可用性检查
//! （`QuickActions == nil || !Enabled()`）总是先失败 ⇒ 三个目标态 409（`NoTurn` / `Stale` /
//! `Busy`）与 202 成功在路由上**不可达**。本文件仍把这两条 SQL **真做**（不是留空），并用
//! `#[ignore]` 的真库测试钉住它们：一旦 M6/M7 装上 provider，门禁链的后半段就是现成的。
//! ⇒ 路由侧只落「可用性 403」这一条出口（`docs/45` 的偏离 D-1）。
//!
//! 约定与 M1/M2/M3 各 Repo 一致（见 `crate::task` / `crate::chat_session`）：
//! - `Row` 用原始 `Uuid`/`String` 字段（`mc_core::Id` 没有 sqlx impl ⇒ 手写 `FromRow`）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，不允许静默跳过）
//!
//! 本文件**只读**：quick actions 的写入（生成侧）与 daemon 侧落库都随 provider 一起落地。

use uuid::Uuid;

use crate::chat_message::ChatMessageRow;
use crate::workspace::map_sqlx_err;
use crate::Result;

/// 在飞态字面量（上游 `HasActiveChatTaskForSession` 的 `status IN (...)` 原文）。
///
/// `crate::chat_task::support::PENDING_STATUSES` 是同一串字面量的 chat 派发面副本；
/// 两处都照上游原文写，因为两个模块各自与上游文件对齐（上游也把同一串抄了七遍）。
const PENDING_STATUSES: &str =
    "'queued', 'dispatched', 'running', 'waiting_local_directory', 'deferred'";

/// chat quick-actions 的只读仓储。
#[derive(Debug, Clone)]
pub struct ChatQuickActionRepo {
    db: mc_db::Db,
}

impl ChatQuickActionRepo {
    /// 用连接池构造。
    pub fn new(db: mc_db::Db) -> Self {
        Self { db }
    }

    /// 上游 `GetLatestAssistantChatMessageForSession`（`chat.sql:1694`）：本会话最近的
    /// **可重生成** assistant 轮。
    ///
    /// `task_id IS NOT NULL` 是过滤条件而非巧合：daemon 的 suggest 补充以 `task_id` 为键，
    /// 且 resume 需要一个真实完成的轮去续。`onboarding_opening` 这类产品写的行没有 task，
    /// 因此天然不在候选里。
    pub async fn latest_regenerable_reply(
        &self,
        session_id: Uuid,
    ) -> Result<Option<ChatMessageRow>> {
        sqlx::query_as::<_, ChatMessageRow>(
            "SELECT * FROM chat_message \
             WHERE chat_session_id = $1 AND role = 'assistant' AND task_id IS NOT NULL \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `HasActiveChatTaskForSession`（`chat.sql:1310`）：本会话是否已有在飞轮。
    ///
    /// 与 `HasPendingChatTurnForSession` 的唯一差别是**不过滤**
    /// `regenerate_quick_actions_for IS NULL` —— 背景重生成轮虽然对 UI 不可见，但它确实在
    /// 占用这个会话，所以它算「忙」。
    pub async fn has_active_chat_task_for_session(&self, session_id: Uuid) -> Result<bool> {
        let sql = format!(
            "SELECT EXISTS (SELECT 1 FROM agent_task_queue \
             WHERE chat_session_id = $1 AND status IN ({PENDING_STATUSES}))"
        );
        sqlx::query_scalar(&sql)
            .bind(session_id)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

impl crate::RepoWithDb for ChatQuickActionRepo {
    fn db(&self) -> &mc_db::Db {
        &self.db
    }
}
