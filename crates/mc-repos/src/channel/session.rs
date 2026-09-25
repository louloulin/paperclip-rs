//! 会话绑定面：`channel_chat_session_binding` / `channel_chat_context_generation` + `lark_chat_session_binding`。
//!
//! - **写者**：M7-2（**W**；`docs/60-M7-PLAN.md` §3.3 的写集表）。
//! - **上游**：`internal/integrations/channel/engine/session.go`（1,316 行）的
//!   `ChatSession` 服务 + `db/queries/{channel,chat,workspace}.sql` 的那批语句。**落在本文件**
//!   （而不是 `mc-channel`）的原因是：上游那套语义**就是** SQL 事务里的行锁顺序与 CAS，
//!   逐条移植后它属于"表访问"这一层；`mc-channel` 侧只留**判决**（代际围栏、错误分类）。
//! - **语义（本片的硬项）**：
//!   1. **会话隔离键** `(installation_id, channel_chat_id)`：一个键一个 `chat_session`。
//!      `channel_chat_id` **不是**"回复到哪个会话"——平台自己的 chat id，或（线程化平台）
//!      chat id + 线程根，由**调用方**组装（`NewEnsureSession::binding_key` 的文档）。
//!   2. **代际（generation）**：`channel_chat_context_generation` 是会话上下文的**版本**。
//!      `/clear`（`force_fresh`）把 `binding.context_revision` +1 并开新代；老代的**回调**必须
//!      按自己的 `revision` 读写 —— 本文件的每个代际读写都**要求带 revision**，
//!      「旧代际不得读到新代际上下文」因此在**签名层面**成立（没有"读当前代际"的出口）。
//!   3. **路由代际（`route_revision`）**：`/new` 退休当前路由行（`retired_at`）并开下一行；
//!      `UNIQUE(installation_id, channel_chat_id) WHERE retired_at IS NULL` 保证**当前路由唯一**。
//!      append 先锁 `chat_session` 再锁**当前**路由行 ⇒ 拿着已被退休路由的 append 得到
//!      [`AppendOutcome::RouteChanged`]（**不是**静默写进老会话）。
//!   4. **两阶段幂等**：`claim_token` 有效时，`MarkChannelInboundDedupProcessed` 跑在**同一个
//!      事务**里；令牌被抢走（0 行）⇒ [`AppendOutcome::ClaimLost`]，整个事务回滚（不留半条消息）。
//! - **两套表并存（**不得**合并）**：`lark_chat_session_binding`（`109`）与泛化层并存；
//!   本文件两个 Repo 并列（R-M7-5 / `docs/60` §6.4）。
//! - **错误面（不改 `RepoError`）**：`repo` 的错误词表只有 `NotFound | Conflict | Db`，
//!   而本面需要区分三个**产品性**结果（路由变了 / 认领丢了 / 正常）⇒ 用返回枚举
//!   （[`AppendOutcome`] / [`StartRouteOutcome`]）表达，**不**把它们塞进 `Err`
//!   （与 `docs/60` §2.6 第 4 条"产品性丢弃不是基础设施失败"同一条纪律）。
//! - **本仓约定**：裸 `Uuid` + 手写/derive `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定、jsonb → `serde_json::Value`；跨工作区写 = 越权 ⇒ 写路径带 `workspace_id` 收窄。
//!
//! 行预算（门 ⑩）：`session.rs` ≤800 行；真库用例拆在 `session/tests.rs`（同 `#[cfg(test)]`）。

use chrono::{DateTime, Utc};
use mc_core::channel::message::{ChatType, MediaRef};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_db::Db;
use serde_json::Value as Json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::channel::media::ChannelMediaRepo;
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

#[cfg(test)]
mod tests;

/// `channel_chat_session_binding` 的列清单（`124` + `271` + `377` + `420`；17 列）。
pub const BINDING_COLUMNS: &str = "id, chat_session_id, installation_id, channel_type, \
                                   channel_chat_id, chat_type, last_message_id, last_thread_id, \
                                   config, created_at, pending_fresh, context_revision, \
                                   route_revision, retired_at, history_start_message_id, \
                                   history_end_message_id, history_boundary_pending";

/// `channel_chat_context_generation` 的列清单（`377` + 后续代际列；11 列）。
pub const GENERATION_COLUMNS: &str = "chat_session_id, revision, history_start_message_id, \
                                      history_end_message_id, history_boundary_pending, \
                                      pending_fresh, initiator_user_id, created_at, \
                                      last_message_id, last_thread_id, last_sender_id";

/// `lark_chat_session_binding` 的列清单（`109` + `122` 的 last_* 两列；8 列）。
pub const LARK_BINDING_COLUMNS: &str = "id, chat_session_id, installation_id, lark_chat_id, \
                                        lark_chat_type, created_at, last_lark_message_id, \
                                        last_lark_thread_id";

/// `chat_message.message_kind`：Router **同步**处理的控制面轮次。
///
/// 公开 Chat 投影与任务批次密封都跳过它 —— agent 不能在之后的轮次里**再执行一遍**命令
/// （上游 `channelCommandMessageKind` 逐字）。
pub const CHANNEL_COMMAND_MESSAGE_KIND: &str = "channel_command";

// =====================================================================
// 行结构
// =====================================================================

/// `channel_chat_session_binding` 的一行（17 列）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelChatSessionBindingRow {
    pub id: Uuid,
    pub chat_session_id: Uuid,
    pub installation_id: Uuid,
    /// **存储口径**（Lark 是 `feishu`）。
    pub channel_type: String,
    /// 会话隔离键（见模块文档第 1 条）。
    pub channel_chat_id: String,
    pub chat_type: String,
    pub last_message_id: Option<String>,
    pub last_thread_id: Option<String>,
    pub config: Json,
    pub created_at: DateTime<Utc>,
    pub pending_fresh: bool,
    pub context_revision: i64,
    pub route_revision: i64,
    pub retired_at: Option<DateTime<Utc>>,
    pub history_start_message_id: Option<String>,
    pub history_end_message_id: Option<String>,
    pub history_boundary_pending: bool,
}

impl ChannelChatSessionBindingRow {
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    pub fn chat_session_id(&self) -> Id {
        Id(self.chat_session_id)
    }

    pub fn installation_id(&self) -> Id {
        Id(self.installation_id)
    }

    /// 平台判别式（解存储口径）。
    pub fn kind(&self) -> Option<ChannelKind> {
        ChannelKind::from_storage_str(&self.channel_type)
    }

    /// 当前路由（未退休）。
    pub fn is_current(&self) -> bool {
        self.retired_at.is_none()
    }

    /// 群聊 / 直聊（`CHECK` 保证只有两个取值）。
    pub fn chat_type(&self) -> Option<ChatType> {
        ChatType::from_str_opt(&self.chat_type)
    }
}

/// `channel_chat_context_generation` 的一行（11 列）。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct ChannelChatContextGenerationRow {
    pub chat_session_id: Uuid,
    pub revision: i64,
    pub history_start_message_id: Option<String>,
    pub history_end_message_id: Option<String>,
    pub history_boundary_pending: bool,
    pub pending_fresh: bool,
    pub initiator_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub last_message_id: Option<String>,
    pub last_thread_id: Option<String>,
    pub last_sender_id: Option<String>,
}

impl ChannelChatContextGenerationRow {
    pub fn chat_session_id(&self) -> Id {
        Id(self.chat_session_id)
    }

    /// 发起人快照（**可空**：老数据可能缺失 ⇒ 恢复时失败关闭，不冒充后来的发件人）。
    pub fn initiator_user_id(&self) -> Option<Id> {
        self.initiator_user_id.map(Id)
    }
}

/// `lark_chat_session_binding` 的一行（8 列）。
///
/// ⚠️ 遗留表：`installation_id` 有 `REFERENCES lark_installation(id) ON DELETE CASCADE`。
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct LarkChatSessionBindingRow {
    pub id: Uuid,
    pub chat_session_id: Uuid,
    pub installation_id: Uuid,
    pub lark_chat_id: String,
    pub lark_chat_type: String,
    pub created_at: DateTime<Utc>,
    pub last_lark_message_id: Option<String>,
    pub last_lark_thread_id: Option<String>,
}

impl LarkChatSessionBindingRow {
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    pub fn chat_session_id(&self) -> Id {
        Id(self.chat_session_id)
    }
}

/// 尚有未认领输入的代际（上游 `PendingContext`；`initiator` 可空）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingContextRow {
    pub revision: i64,
    pub initiator_user_id: Option<Uuid>,
}

impl PendingContextRow {
    pub fn initiator_user_id(&self) -> Option<Id> {
        self.initiator_user_id.map(Id)
    }
}

// =====================================================================
// 入参 / 出参
// =====================================================================

/// [`ChannelChatSessionRepo::ensure_session`] 的入参。
///
/// `binding_key` 是**会话隔离键**（存进 `channel_chat_id`）：Feishu 传 chat id；
/// Slack 对 channel/thread 传"channel id + 线程根"的复合键，于是一个 Slack channel 里两个
/// `@bot` 线程**不会**塌成一个会话（Hermes 模型）。**线程化平台不得**原样透传平台 chat id。
///
/// `binding_config` 是键本身带不了的平台路由（例如复合键下的真实 `channel_id`），
/// 落在绑定行上供出站路径读回；空 = `{}`。
#[derive(Debug, Clone)]
pub struct NewEnsureSession {
    pub workspace_id: Id,
    pub agent_id: Id,
    pub installation_id: Id,
    pub kind: ChannelKind,
    pub chat_type: ChatType,
    pub binding_key: String,
    pub binding_config: Json,
    /// 会话创建者：p2p 是那个人，群聊是安装者（由调用方决定）。
    pub creator: Id,
}

/// [`ChannelChatSessionRepo::start_route`] 的入参（`/new` 的事务化实现）。
#[derive(Debug, Clone)]
pub struct NewStartRoute {
    pub session: NewEnsureSession,
    /// `/new` 命令的**发起人**（`session.creator` 仍是新 Chat 的所有者）。
    pub initiator: Id,
    /// 持久化正文（agent 可见）。
    pub body: String,
    /// 首条用户消息的标题（标题策略在 `mc-channel`，见 `docs/60` §2.1）。
    pub first_title: String,
    pub message_id: String,
    pub thread_id: String,
    pub sender_channel_id: String,
    pub claim_token: Option<Id>,
    pub media_pending_seconds: f64,
    pub persist_message: bool,
    pub history_boundary_pending: bool,
}

/// [`ChannelChatSessionRepo::start_route`] 的成功结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRouteResult {
    pub session_id: Id,
    pub binding_id: Id,
    pub route_revision: i64,
    pub first_message_id: Option<Id>,
    pub context_revision: i64,
    pub pending_contexts: Vec<PendingContextRow>,
    pub dedup_marked: bool,
    pub initial_title: String,
}

/// `/new` 的三种判决（产品性结果，**不**走 `Err`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartRouteOutcome {
    Started(Box<StartRouteResult>),
    /// 当前路由在读取与加锁之间被换掉了 ⇒ Router 重新解析并重试（`ErrRouteChanged`）。
    RouteChanged,
    /// 认领令牌在飞行中被抢走 ⇒ 整个事务回滚（`ErrClaimLost`）。
    ClaimLost,
}

/// [`ChannelChatSessionRepo::append_message`] 的入参。
#[derive(Debug, Clone)]
pub struct NewChannelAppend {
    pub session_id: Id,
    pub sender: Id,
    pub installation_id: Id,
    /// 完整存储正文（含平台富化）。
    pub body: String,
    /// 首条消息的标题（只在会话还"隐式"时用）。
    pub first_title: String,
    /// 这条消息是控制面命令（`/issue`）⇒ 不进 agent 输入、不换标题、不当 reply target。
    pub is_command: bool,
    pub message_id: String,
    pub thread_id: String,
    pub sender_channel_id: String,
    /// 去重键（空 ⇒ 退回 `message_id`）。
    pub dedup_message_id: String,
    pub claim_token: Option<Id>,
    pub media_pending_seconds: f64,
    /// `/clear` 一类要求开新代际。
    pub force_fresh: bool,
    pub has_media: bool,
}

/// append 的结果数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendResultData {
    pub message_id: Option<Id>,
    pub context_revision: i64,
    pub pending_contexts: Vec<PendingContextRow>,
    /// binder 在**自己的事务里** Mark 了去重行 ⇒ Router 跳过流水线后的 finalize。
    pub dedup_marked: bool,
    /// 这次提交把一个隐式渠道会话变成了公开 Chat。
    pub became_visible: bool,
    /// 首次落下的标题（`None` = 没动标题）。
    pub initial_title: Option<String>,
    pub binding_id: Id,
    pub route_revision: i64,
}

/// append 的三种判决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended(Box<AppendResultData>),
    RouteChanged,
    ClaimLost,
}

/// [`ChannelChatSessionRepo::bind_media`] 的入参。
#[derive(Debug, Clone)]
pub struct BindMediaRefsParams {
    pub message_id: Option<Id>,
    pub session_id: Id,
    pub workspace_id: Id,
    pub sender: Id,
    /// `/issue` 轮次里媒体归 issue；否则归 `message_id`。
    pub issue_id: Option<Id>,
    pub issue_description_base: Option<String>,
    pub issue_command_text: String,
    pub body: String,
    pub media_refs: Vec<MediaRef>,
    /// 媒体首轮标题（由调用方从附件名算出；`None` = 不初始化）。
    pub media_title: Option<String>,
}

/// [`ChannelChatSessionRepo::bind_media`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BindMediaResult {
    pub initial_title: Option<String>,
    pub title_source: String,
    /// 真的落库并绑定到消息上的附件数（被对账器接管的 key 不算）。
    pub linked: usize,
}

// =====================================================================
// 仓储
// =====================================================================

/// 会话绑定面仓储（`channel_chat_session_binding` + `channel_chat_context_generation`）。
#[derive(Clone)]
pub struct ChannelChatSessionRepo {
    db: Db,
    media: ChannelMediaRepo,
}

impl ChannelChatSessionRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self {
            media: ChannelMediaRepo::new(db.clone()),
            db,
        }
    }

    /// 读档：当前路由行（未退休）。
    pub async fn get_current_binding(
        &self,
        installation_id: Id,
        binding_key: &str,
    ) -> Result<Option<ChannelChatSessionBindingRow>> {
        let sql = format!(
            "SELECT {BINDING_COLUMNS} FROM channel_chat_session_binding \
             WHERE installation_id = $1 AND channel_chat_id = $2 AND retired_at IS NULL"
        );
        sqlx::query_as::<_, ChannelChatSessionBindingRow>(&sql)
            .bind(installation_id.0)
            .bind(binding_key)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 反查：这个 `chat_session` 当前的渠道路由（`UNIQUE(chat_session_id)` 保证至多一行）。
    pub async fn get_current_binding_by_session(
        &self,
        session_id: Id,
    ) -> Result<Option<ChannelChatSessionBindingRow>> {
        let sql = format!(
            "SELECT {BINDING_COLUMNS} FROM channel_chat_session_binding \
             WHERE chat_session_id = $1 AND retired_at IS NULL"
        );
        sqlx::query_as::<_, ChannelChatSessionBindingRow>(&sql)
            .bind(session_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 该 `chat_session` 的**全部**代际行（含已退休；诊断 / 交付面用），按 `route_revision` 升序。
    pub async fn list_bindings_by_session(
        &self,
        session_id: Id,
    ) -> Result<Vec<ChannelChatSessionBindingRow>> {
        let sql = format!(
            "SELECT {BINDING_COLUMNS} FROM channel_chat_session_binding \
             WHERE chat_session_id = $1 ORDER BY route_revision ASC"
        );
        sqlx::query_as::<_, ChannelChatSessionBindingRow>(&sql)
            .bind(session_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 读某个代际行（**必须**带 revision ⇒ "老代际读新代际上下文"没有入口）。
    pub async fn get_generation(
        &self,
        session_id: Id,
        revision: i64,
    ) -> Result<Option<ChannelChatContextGenerationRow>> {
        let sql = format!(
            "SELECT {GENERATION_COLUMNS} FROM channel_chat_context_generation \
             WHERE chat_session_id = $1 AND revision = $2"
        );
        sqlx::query_as::<_, ChannelChatContextGenerationRow>(&sql)
            .bind(session_id.0)
            .bind(revision)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `ListUnownedChannelChatContextRevisions`：还有未认领输入的代际（崩溃恢复用）。
    pub async fn list_unowned_context_revisions(
        &self,
        session_id: Id,
    ) -> Result<Vec<PendingContextRow>> {
        let rows: Vec<(i64, Option<Uuid>)> = sqlx::query_as(
            "WITH pending AS ( \
                 SELECT DISTINCT COALESCE(channel_context_revision, 1)::bigint AS context_revision \
                 FROM chat_message \
                 WHERE chat_session_id = $1 AND role = 'user' AND task_id IS NULL \
                   AND message_kind != $2 \
             ) \
             SELECT pending.context_revision, generation.initiator_user_id \
             FROM pending \
             LEFT JOIN channel_chat_context_generation AS generation \
               ON generation.chat_session_id = $1 \
              AND generation.revision = pending.context_revision \
             ORDER BY pending.context_revision",
        )
        .bind(session_id.0)
        .bind(CHANNEL_COMMAND_MESSAGE_KIND)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|(revision, initiator)| PendingContextRow {
                revision,
                initiator_user_id: initiator,
            })
            .collect())
    }

    /// `pending_fresh` 位（绑定行口径）：任务入队时读取并消费。
    pub async fn binding_pending_fresh(&self, session_id: Id) -> Result<Option<bool>> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT pending_fresh FROM channel_chat_session_binding \
             WHERE chat_session_id = $1 AND retired_at IS NULL",
        )
        .bind(session_id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.map(|(pending,)| pending))
    }

    /// `ClearChannelChatSessionPendingFreshForRevision`：消费掉**该代际**的 fresh 标记。
    ///
    /// 带 revision 是判据的一部分：一次晚到的消费不该把新代际的 fresh 意图抹掉。
    pub async fn clear_pending_fresh_for_revision(
        &self,
        session_id: Id,
        revision: i64,
    ) -> Result<u64> {
        sqlx::query(
            "UPDATE channel_chat_session_binding SET pending_fresh = FALSE \
             WHERE chat_session_id = $1 AND context_revision = $2",
        )
        .bind(session_id.0)
        .bind(revision)
        .execute(self.db.pool())
        .await
        .map(|done| done.rows_affected())
        .map_err(map_sqlx_err)
    }

    /// 确保 `(installation, binding_key)` 的会话存在，返回 `chat_session.id`。
    ///
    /// 首次接触时在**一个事务**里建会话 + 绑定行 + 第 1 代；两个并发首消息的竞争由
    /// `UNIQUE(installation_id, channel_chat_id)` 仲裁，输的一方**重读**赢家的行。
    pub async fn ensure_session(&self, input: &NewEnsureSession) -> Result<Id> {
        if let Some(existing) = self
            .get_current_binding(input.installation_id, &input.binding_key)
            .await?
        {
            return Ok(existing.chat_session_id());
        }
        match self.create_session_and_binding(input).await {
            Ok(id) => Ok(id),
            Err(crate::RepoError::Conflict) => {
                let winner = self
                    .get_current_binding(input.installation_id, &input.binding_key)
                    .await?
                    .ok_or(crate::RepoError::Conflict)?;
                Ok(winner.chat_session_id())
            }
            Err(other) => Err(other),
        }
    }

    async fn create_session_and_binding(&self, input: &NewEnsureSession) -> Result<Id> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        // 锁顺序固定：workspace (FOR KEY SHARE) → chat_session → binding → generation。
        let locked: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM workspace WHERE id = $1 FOR KEY SHARE")
                .bind(input.workspace_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if locked.is_none() {
            return Err(crate::RepoError::NotFound);
        }
        let session_id = Id::new();
        sqlx::query(
            "INSERT INTO chat_session (id, workspace_id, agent_id, creator_id, title) \
             VALUES ($1, $2, $3, $4, '')",
        )
        .bind(session_id.0)
        .bind(input.workspace_id.0)
        .bind(input.agent_id.0)
        .bind(input.creator.0)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        let sql = format!(
            "WITH next_route AS ( \
                 SELECT COALESCE(MAX(route_revision) + 1, 1)::bigint AS route_revision \
                 FROM channel_chat_session_binding AS existing \
                 WHERE existing.installation_id = $2 AND existing.channel_chat_id = $4 \
             ), binding AS ( \
                 INSERT INTO channel_chat_session_binding \
                 (chat_session_id, installation_id, channel_type, channel_chat_id, chat_type, \
                  config, route_revision) \
                 SELECT $1, $2, $3, $4, $5, $6, next_route.route_revision FROM next_route \
                 RETURNING {BINDING_COLUMNS} \
             ), generation AS ( \
                 INSERT INTO channel_chat_context_generation (chat_session_id, revision) \
                 SELECT chat_session_id, context_revision FROM binding \
             ) \
             SELECT * FROM binding"
        );
        let row = sqlx::query_as::<_, ChannelChatSessionBindingRow>(&sql)
            .bind(session_id.0)
            .bind(input.installation_id.0)
            .bind(input.kind.storage_str())
            .bind(&input.binding_key)
            .bind(input.chat_type.as_str())
            .bind(binding_config(&input.binding_config))
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        debug_assert_eq!(row.chat_session_id(), session_id);
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(session_id)
    }
}

impl RepoWithDb for ChannelChatSessionRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

// 门 ⑩ 的拆分（`session.rs` 一度 1,695 行 > 800 硬限）：**一个写口一个文件**，事务边界不变。
mod append;
mod lark;
mod media;
mod route;
mod tx;

pub use lark::LarkChatSessionBindingRepo;

use tx::binding_config;
