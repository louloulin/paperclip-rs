//! 会话状态机：代际窗口、跨代围栏、以及**三个端口的适配**（`SessionBinder` / `Deduper` / `Auditor`）。
//!
//! - **写者**：M7-2（`docs/60` §3.3 的写集表）。
//! - **上游**：`channel/engine/session.go`（1,316 行）的**判决那一半**。SQL 事务那一半落在
//!   `mc_repos::channel::session`（行锁顺序与 CAS 只有真 PG 能判），本文件只留：会话隔离键的
//!   组装、代际窗口的纯状态机、崩溃恢复的计划、以及错误分类（产品性判决 vs 基础设施失败）。
//! - **代际语义（本片 `DoD` 的硬项）**：`channel_chat_context_generation` 是会话上下文的**版本**。
//!   三条落法：
//!   1. **读必须带 revision**：仓储侧没有"读当前代"的出口（`get_generation(session, revision)`），
//!      所以"老代际读到新代际上下文"在签名层面就不可能；
//!   2. [`ContextWindow::accepts`] 是同一条判据的纯形态：一个窗口只认**它自己那一代**的
//!      revision，被 `/clear` 取代之后必须放弃（迟到的媒体绑定 / 去抖回调靠它失败关闭）；
//!   3. 崩溃恢复用 [`plan_pending_contexts`]：**发起人缺失的老代际不恢复**（失败关闭，
//!      绝不冒充后来的发件人 —— 上游 `PendingContext.InitiatorUserID` 的注释逐字）。
//! - **去重命中 ≠ 错误**（`docs/60` §2.6 第 4 条 / 上游 `handler.go` 契约）：[`ChannelDeduper`]
//!   把"命中"映射成 `PipelineError::Duplicate`（产品性判决，Router 消费成 `dropped`），
//!   把 SQL 失败映射成 `EngineError::Infra`（基础设施失败，adapter 上报）。两种返回各有一条用例。
//! - **key 组装**：`BindingKeyPolicy` —— 隔离键（存 `channel_chat_id`）**不是**"回复到哪个会话"。
//!   线程化平台（Slack）必须按"channel + 线程根"隔离，否则一个频道里的两个 `@bot` 线程会塌成
//!   一个会话（上游 `EnsureSessionInput.BindingKey` 的注释逐字）；Feishu / Telegram 直接传 chat id。
//!
//! 行预算（门 ⑩）：`session.rs` ≤800 行；用例拆在 `session/tests.rs`。

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::session as session_repo;
use mc_repos::channel::session::{
    AppendOutcome, BindMediaRefsParams, ChannelChatSessionRepo, LarkChatSessionBindingRepo,
    NewChannelAppend, NewEnsureSession, NewStartRoute, PendingContextRow, StartRouteOutcome,
};
use mc_repos::RepoError;

use crate::engine::commands::{
    chat_title_source, derive_chat_title, derive_first_message_title, media_type_title,
    parse_issue_command,
};
use crate::engine::resolvers::{
    AppendParams, AppendResult, BindMediaParams, BindMediaResult, Deduper, EngineError,
    EngineResult, EnsureSessionParams, PendingContext, PipelineError, SessionBinder,
    StartSessionParams, StartSessionResult,
};

#[cfg(test)]
mod tests;

mod audit;

pub use audit::{drop_from_message, AuditStore, ChannelAuditor};

// =====================================================================
// 会话隔离键
// =====================================================================

/// 会话隔离键的组装策略（一台平台一种；平台的 wire 形态决定）。
///
/// - [`BindingKeyPolicy::chat_id`]：直接传平台 chat id（Feishu / Lark / Telegram / `WeCom` /
///   `DingTalk` 的既有行为）。
/// - [`BindingKeyPolicy::chat_id_plus_thread_root`]：群聊/频道里把**线程根**拼进键
///   （Slack 的 channel/thread 模型）：一个频道里两个 `@bot` 线程各自成一个会话。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BindingKeyPolicy {
    /// 平台 chat id 就是隔离键（默认）。
    #[default]
    ChatId,
    /// `chat_id + "#" + thread_root`（线程为空时退回 chat id）。
    ChatIdPlusThreadRoot,
}

impl BindingKeyPolicy {
    /// 组装隔离键。
    pub fn compose(self, message: &InboundMessage) -> String {
        match self {
            Self::ChatId => message.source.chat_id.clone(),
            Self::ChatIdPlusThreadRoot => {
                if message.source.thread_id.is_empty() {
                    message.source.chat_id.clone()
                } else {
                    format!("{}#{}", message.source.chat_id, message.source.thread_id)
                }
            }
        }
    }
}

// =====================================================================
// 代际窗口（纯状态机）
// =====================================================================

/// 一代上下文的窗口（`channel_chat_context_generation` 一行的纯形态）。
///
/// 它存在的唯一理由是让"这一代还能不能被写 / 读"成为**纯函数**：迟到的媒体绑定、
/// 迟到的去抖回调都拿自己那一代的 revision 来问它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextWindow {
    revision: i64,
    /// 这一代的**第一条** agent 可见消息（`None` = 边界待定：裸 `/clear` 之后还没有真实游标）。
    history_start_message_id: Option<String>,
    /// 这一代的**最后**一条（收口时写入；`None` = 还开着）。
    history_end_message_id: Option<String>,
    boundary_pending: bool,
    pending_fresh: bool,
}

impl ContextWindow {
    /// 从仓储行构造。
    pub fn from_row(row: &session_repo::ChannelChatContextGenerationRow) -> Self {
        Self {
            revision: row.revision,
            history_start_message_id: row.history_start_message_id.clone(),
            history_end_message_id: row.history_end_message_id.clone(),
            boundary_pending: row.history_boundary_pending,
            pending_fresh: row.pending_fresh,
        }
    }

    /// 打开第 `revision` 代（第一代或 `/new` 开出来的新 Chat）。
    pub fn opened(revision: i64) -> Self {
        Self {
            revision,
            history_start_message_id: None,
            history_end_message_id: None,
            boundary_pending: false,
            pending_fresh: false,
        }
    }

    pub fn revision(&self) -> i64 {
        self.revision
    }

    pub fn history_start_message_id(&self) -> Option<&str> {
        self.history_start_message_id.as_deref()
    }

    pub fn history_end_message_id(&self) -> Option<&str> {
        self.history_end_message_id.as_deref()
    }

    /// 边界还没被平台游标定下来（裸 `/clear` 之后、下一条真实消息之前）。
    pub fn boundary_pending(&self) -> bool {
        self.boundary_pending
    }

    /// 这一代还带着"开新会话"的意图（任务入队时消费）。
    pub fn pending_fresh(&self) -> bool {
        self.pending_fresh
    }

    /// **跨代围栏**：一个带 `revision` 的回调/写入能不能落在这条窗口上。
    ///
    /// 这正是"旧代际不得读到新代际上下文"的纯判据：被 `/clear` 取代之后，
    /// 拿着老 revision 的迟到回调必须**放弃**，而不是把内容写进新代。
    pub fn accepts(&self, revision: i64) -> bool {
        self.revision == revision
    }

    /// 收口这一代并在它的 `history_end` 上写下触发消息（`None` = 边界仍然待定）。
    #[must_use]
    pub fn closed(&self, boundary_message_id: Option<&str>) -> Self {
        Self {
            revision: self.revision,
            history_start_message_id: self.history_start_message_id.clone(),
            history_end_message_id: boundary_message_id.map(str::to_string),
            boundary_pending: self.boundary_pending,
            pending_fresh: self.pending_fresh,
        }
    }

    /// 开下一代：老代在触发消息处收口；新代**有正文**才把触发消息当起点，
    /// 否则起点待定（裸 `/clear` / 原生斜杠命令没有公开游标）。
    #[must_use]
    pub fn advance(&self, trigger_message_id: Option<&str>, has_message_body: bool) -> Self {
        Self {
            revision: self.revision + 1,
            history_start_message_id: if has_message_body {
                trigger_message_id.map(str::to_string)
            } else {
                None
            },
            history_end_message_id: None,
            boundary_pending: !has_message_body,
            pending_fresh: true,
        }
    }

    /// 下一条真实消息把待定的起点钉下来（老代与绑定行在同一事务里被收口）。
    #[must_use]
    pub fn resolve_history_start(&self, message_id: &str) -> Self {
        if !self.boundary_pending {
            return self.clone();
        }
        Self {
            revision: self.revision,
            history_start_message_id: Some(message_id.to_string()),
            history_end_message_id: self.history_end_message_id.clone(),
            boundary_pending: false,
            pending_fresh: self.pending_fresh,
        }
    }
}

/// 一个待恢复代的计划（崩溃恢复：进程内的去抖窗口丢了，靠持久化的代际重建）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingContextPlan {
    pub revision: i64,
    /// 发起人快照；**可空** ⇒ 见 [`PendingContextPlan::is_recoverable`]。
    pub initiator_user_id: Option<Id>,
}

impl PendingContextPlan {
    /// **失败关闭**：老数据没有发起人快照 ⇒ **不**恢复这一代，绝不冒充后来的发件人
    /// （上游 `PendingContext` 的注释逐字）。
    pub fn is_recoverable(&self) -> bool {
        self.initiator_user_id.is_some()
    }
}

/// 把仓储的待恢复行转成计划（按代际升序；上游 `ListUnownedChannelChatContextRevisions`）。
pub fn plan_pending_contexts(rows: &[PendingContextRow]) -> Vec<PendingContextPlan> {
    let mut plans: Vec<PendingContextPlan> = rows
        .iter()
        .map(|row| PendingContextPlan {
            revision: row.revision,
            initiator_user_id: row.initiator_user_id(),
        })
        .collect();
    plans.sort_by_key(|plan| plan.revision);
    plans
}

/// 把计划映射成 Router 用的词表（`AppendResult::pending_contexts`）。
pub fn pending_contexts(plans: &[PendingContextPlan]) -> Vec<PendingContext> {
    plans
        .iter()
        .map(|plan| PendingContext {
            revision: plan.revision,
            initiator_user_id: plan.initiator_user_id,
        })
        .collect()
}

// =====================================================================
// 去重端口（`Deduper`）
// =====================================================================

/// 两阶段幂等的行访问接缝（网关：`mc-repos` 的两套表 ↔ engine 的错误词表）。
///
/// `claim` 的语义**逐字**来自上游：`Ok(None)` = 这张表**已经有主**（终态，或新鲜的在飞认领）
/// ⇒ 调用方按 `duplicate` 丢弃；`Err` = 基础设施失败。
#[async_trait]
pub trait DedupStore: Send + Sync {
    /// `Ok(Some(token))` = 认领到手（新令牌）；`Ok(None)` = 已被处理 / 正在处理。
    async fn claim(&self, installation_id: Id, message_id: &str) -> EngineResult<Option<Id>>;

    /// 落定认领；`false` = 令牌被抢走。
    async fn mark(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<bool>;

    /// 放掉在飞认领（基础设施失败后让重投能立刻再拿）。
    async fn release(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<bool>;
}

#[async_trait]
impl DedupStore for mc_repos::channel::dedup::ChannelInboundDedupRepo {
    async fn claim(&self, installation_id: Id, message_id: &str) -> EngineResult<Option<Id>> {
        let row = self
            .claim(installation_id, message_id)
            .await
            .map_err(infra)?;
        Ok(row.map(|row| row.claim_token()))
    }

    async fn mark(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<bool> {
        self.mark_processed(installation_id, message_id, claim_token)
            .await
            .map_err(infra)
    }

    async fn release(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<bool> {
        self.release(installation_id, message_id, claim_token)
            .await
            .map_err(infra)
    }
}

#[async_trait]
impl DedupStore for mc_repos::channel::dedup::LarkInboundDedupRepo {
    async fn claim(&self, installation_id: Id, message_id: &str) -> EngineResult<Option<Id>> {
        let row = self
            .claim(installation_id, message_id)
            .await
            .map_err(infra)?;
        Ok(row.map(|row| row.claim_token()))
    }

    async fn mark(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<bool> {
        self.mark_processed(installation_id, message_id, claim_token)
            .await
            .map_err(infra)
    }

    async fn release(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<bool> {
        self.release(installation_id, message_id, claim_token)
            .await
            .map_err(infra)
    }
}

/// `Deduper` 的通用实现（一台平台一组；lark 用遗留表，其余用泛化表）。
pub struct ChannelDeduper {
    store: Arc<dyn DedupStore>,
    kind: ChannelKind,
}

impl std::fmt::Debug for ChannelDeduper {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelDeduper")
            .field("kind", &self.kind)
            .field("store", &"<dyn DedupStore>")
            .finish()
    }
}

impl ChannelDeduper {
    /// 用泛化去重表（`channel_inbound_message_dedup`）。
    pub fn generalized(
        repo: mc_repos::channel::dedup::ChannelInboundDedupRepo,
        kind: ChannelKind,
    ) -> Self {
        Self {
            store: Arc::new(repo),
            kind,
        }
    }

    /// 用 lark **遗留**去重表（`lark_inbound_message_dedup`）。
    pub fn lark(repo: mc_repos::channel::dedup::LarkInboundDedupRepo) -> Self {
        Self {
            store: Arc::new(repo),
            kind: ChannelKind::Lark,
        }
    }

    /// 任意接缝（用例注入替身）。
    pub fn with_store(store: Arc<dyn DedupStore>, kind: ChannelKind) -> Self {
        Self { store, kind }
    }

    /// 平台判别式（诊断）。
    pub fn kind(&self) -> ChannelKind {
        self.kind
    }
}

#[async_trait]
impl Deduper for ChannelDeduper {
    async fn claim(&self, installation_id: Id, message_id: &str) -> EngineResult<Id> {
        // ⚠️ 命中是**产品性判决**：`Duplicate` 会被 Router 消费成 `dropped` + 审计一行，
        // **不是** 抛给 adapter 的错误（`docs/60` §2.6 第 4 条）。
        match self.store.claim(installation_id, message_id).await? {
            Some(token) => Ok(token),
            None => Err(PipelineError::Duplicate.into()),
        }
    }

    async fn mark(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<()> {
        if self
            .store
            .mark(installation_id, message_id, claim_token)
            .await?
        {
            return Ok(());
        }
        // 令牌被抢走：等价于"这条已经被别人处理了"。
        Err(PipelineError::ClaimLost.into())
    }

    async fn release(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<()> {
        // 令牌不匹配 = 有意为之的 fenced no-op（上游 `ReleaseChannelInboundDedup` 的 0 行）
        // ⇒ **不报错**：重投时该行还会被重新认领。
        let _ = self
            .store
            .release(installation_id, message_id, claim_token)
            .await?;
        Ok(())
    }
}

// =====================================================================
// 会话绑定端口（`SessionBinder`）
// =====================================================================

/// 会话状态机的配置。
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionBinderConfig {
    /// 隔离键策略（线程化平台用 [`BindingKeyPolicy::ChatIdPlusThreadRoot`]）。
    pub binding_key: BindingKeyPolicy,
}

/// 会话绑定端口的上游实现：纯判决（本文件）+ 事务语句（`mc_repos::channel::session`）。
pub struct ChannelSessionBinder {
    repo: Arc<ChannelChatSessionRepo>,
    /// lark 遗留绑定面（`lark_chat_session_binding`）；只有 lark 的 adapter 会读它。
    lark: Option<Arc<LarkChatSessionBindingRepo>>,
    kind: ChannelKind,
    config: SessionBinderConfig,
}

impl std::fmt::Debug for ChannelSessionBinder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelSessionBinder")
            .field("kind", &self.kind)
            .field("config", &self.config)
            .field("has_lark_legacy", &self.lark.is_some())
            .finish_non_exhaustive()
    }
}

impl ChannelSessionBinder {
    /// 构造（不含 lark 遗留面）。
    pub fn new(
        repo: Arc<ChannelChatSessionRepo>,
        kind: ChannelKind,
        config: SessionBinderConfig,
    ) -> Self {
        Self {
            repo,
            lark: None,
            kind,
            config,
        }
    }

    /// 同时接上 lark **遗留**绑定面（`ChannelKind::Lark` 的 adapter 需要它）。
    #[must_use]
    pub fn with_lark_legacy(mut self, repo: Arc<LarkChatSessionBindingRepo>) -> Self {
        self.lark = Some(repo);
        self
    }

    /// 平台判别式（诊断）。
    pub fn kind(&self) -> ChannelKind {
        self.kind
    }

    /// 配置快照。
    pub fn config(&self) -> SessionBinderConfig {
        self.config
    }

    /// 组装一次 append 需要的全部入参（**判决**在这里：标题、命令判定、去重键）。
    fn append_input(params: &AppendParams) -> NewChannelAppend {
        let message = &params.message;
        let is_command = parse_issue_command(message.command_source_text()).is_some();
        let has_media = params.media_pending_seconds > 0.0;
        let first_title = derive_first_message_title(
            &chat_title_source(&message.text, &message.command_text, message.force_fresh),
            has_media,
        );
        NewChannelAppend {
            session_id: params.session_id,
            sender: params.sender,
            installation_id: params.installation_id,
            body: message.text.clone(),
            first_title,
            is_command,
            message_id: message.message_id.clone(),
            thread_id: message.source.thread_id.clone(),
            sender_channel_id: message.source.sender_id.clone(),
            dedup_message_id: message.dedup_message_id().to_string(),
            claim_token: params.claim_token,
            media_pending_seconds: params.media_pending_seconds,
            force_fresh: message.force_fresh,
            has_media,
        }
    }

    /// 一个带 `revision` 的回调（迟到的媒体绑定 / 去抖触发）还能不能落在这条绑定行上。
    pub fn generation_is_current(
        binding: &session_repo::ChannelChatSessionBindingRow,
        callback_revision: i64,
    ) -> bool {
        binding.is_current() && binding.context_revision == callback_revision
    }

    /// 该会话的待恢复代际计划（崩溃恢复；与去重/媒体无关）。
    pub async fn pending_context_plans(
        &self,
        session_id: Id,
    ) -> EngineResult<Vec<PendingContextPlan>> {
        let rows = self
            .repo
            .list_unowned_context_revisions(session_id)
            .await
            .map_err(infra)?;
        Ok(plan_pending_contexts(&rows))
    }

    /// 读某个代际的窗口（**必须**带 revision：没有"读当前代"的出口）。
    pub async fn context_window(
        &self,
        session_id: Id,
        revision: i64,
    ) -> EngineResult<Option<ContextWindow>> {
        Ok(self
            .repo
            .get_generation(session_id, revision)
            .await
            .map_err(infra)?
            .map(|row| ContextWindow::from_row(&row)))
    }

    /// lark 遗留绑定面（只有 lark 的 adapter 需要）。
    pub fn lark_legacy(&self) -> Option<&Arc<LarkChatSessionBindingRepo>> {
        self.lark.as_ref()
    }
}

#[async_trait]
impl SessionBinder for ChannelSessionBinder {
    async fn ensure_session(&self, params: EnsureSessionParams) -> EngineResult<Id> {
        let input = NewEnsureSession {
            workspace_id: params.installation.workspace_id,
            agent_id: params.installation.agent_id,
            installation_id: params.installation.id,
            kind: params.installation.kind,
            chat_type: params.message.source.chat_type,
            binding_key: self.config.binding_key.compose(&params.message),
            binding_config: serde_json::Value::Null,
            creator: params.sender,
        };
        self.repo.ensure_session(&input).await.map_err(infra)
    }

    async fn start_session(&self, params: StartSessionParams) -> EngineResult<StartSessionResult> {
        let message = &params.message;
        let is_command = parse_issue_command(message.command_source_text()).is_some();
        let has_media = params.media_pending_seconds > 0.0;
        let first_title = derive_first_message_title(
            &chat_title_source(&message.text, &message.command_text, message.force_fresh),
            has_media,
        );
        let outcome = self
            .repo
            .start_route(&NewStartRoute {
                session: NewEnsureSession {
                    workspace_id: params.installation.workspace_id,
                    agent_id: params.installation.agent_id,
                    installation_id: params.installation.id,
                    kind: params.installation.kind,
                    chat_type: message.source.chat_type,
                    binding_key: self.config.binding_key.compose(message),
                    binding_config: serde_json::Value::Null,
                    creator: params.creator,
                },
                initiator: params.sender,
                body: message.text.clone(),
                first_title,
                message_id: message.message_id.clone(),
                thread_id: message.source.thread_id.clone(),
                sender_channel_id: message.source.sender_id.clone(),
                claim_token: params.claim_token,
                media_pending_seconds: params.media_pending_seconds,
                persist_message: params.persist_message && !is_command,
                history_boundary_pending: false,
            })
            .await
            .map_err(infra)?;
        match outcome {
            StartRouteOutcome::Started(started) => Ok(StartSessionResult {
                session_id: started.session_id,
                binding_id: Some(started.binding_id),
                route_revision: started.route_revision,
                append: AppendResult {
                    message_id: started.first_message_id,
                    issue_command: None,
                    dedup_marked: started.dedup_marked,
                    context_revision: started.context_revision,
                    pending_contexts: pending_contexts(&plan_pending_contexts(
                        &started.pending_contexts,
                    )),
                    initial_title: started.initial_title.clone(),
                    became_visible: true,
                    binding_id: Some(started.binding_id),
                    route_revision: started.route_revision,
                },
            }),
            StartRouteOutcome::RouteChanged => Err(PipelineError::RouteChanged.into()),
            StartRouteOutcome::ClaimLost => Err(PipelineError::ClaimLost.into()),
        }
    }

    async fn mark_pending_fresh(&self, session_id: Id, message_id: &str) -> EngineResult<()> {
        match self
            .repo
            .mark_pending_fresh(session_id, message_id, None)
            .await
            .map_err(infra)?
        {
            AppendOutcome::Appended(_) => Ok(()),
            AppendOutcome::RouteChanged => Err(PipelineError::RouteChanged.into()),
            AppendOutcome::ClaimLost => Err(PipelineError::ClaimLost.into()),
        }
    }

    async fn append_message(&self, params: AppendParams) -> EngineResult<AppendResult> {
        let input = Self::append_input(&params);
        let issue_command = parse_issue_command(params.message.command_source_text()).map(
            |(title, description)| crate::engine::resolvers::ChannelIssueCommand {
                title,
                description,
            },
        );
        match self.repo.append_message(&input).await.map_err(infra)? {
            AppendOutcome::Appended(appended) => Ok(AppendResult {
                message_id: appended.message_id,
                issue_command,
                dedup_marked: appended.dedup_marked,
                context_revision: appended.context_revision,
                pending_contexts: pending_contexts(&plan_pending_contexts(
                    &appended.pending_contexts,
                )),
                initial_title: appended.initial_title.unwrap_or_default(),
                became_visible: appended.became_visible,
                binding_id: Some(appended.binding_id),
                route_revision: appended.route_revision,
            }),
            AppendOutcome::RouteChanged => Err(PipelineError::RouteChanged.into()),
            AppendOutcome::ClaimLost => Err(PipelineError::ClaimLost.into()),
        }
    }

    async fn bind_media(&self, params: BindMediaParams) -> EngineResult<BindMediaResult> {
        let media_title = params.media_refs.first().map(|media| {
            if media.filename.is_empty() {
                media_type_title(media.message_kind).to_string()
            } else {
                derive_chat_title(&media.filename)
            }
        });
        let outcome = self
            .repo
            .bind_media(&BindMediaRefsParams {
                message_id: params.message_id,
                session_id: params.session_id,
                workspace_id: params.workspace_id,
                sender: params.sender,
                issue_id: params.issue_id,
                issue_description_base: params.issue_description_base,
                issue_command_text: params.issue_command_text,
                body: params.body,
                media_refs: params.media_refs,
                media_title,
            })
            .await
            .map_err(infra)?;
        Ok(BindMediaResult {
            initial_title: outcome.initial_title.unwrap_or_default(),
            title_source: outcome.title_source,
        })
    }
}

// =====================================================================
// 小工具
// =====================================================================

/// 仓储错误 → engine 的**基础设施**失败（产品性判决由调用点在 `Ok(..)` 里表达）。
///
/// `needless_pass_by_value`：所有调用点都是 `Result::map_err(infra)` —— 那里交出来的就是
/// **所有权**，改成 `&RepoError` 反而要在每个调用点写闭包。
#[allow(clippy::needless_pass_by_value)]
pub(super) fn infra(error: RepoError) -> EngineError {
    EngineError::infra(error.to_string())
}

fn non_empty_owned(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}
