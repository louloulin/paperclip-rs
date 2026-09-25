//! 入站路由：把一条 [`InboundMessage`] 变成"落地了什么"（上游 `engine/router.go`，1124 行）。
//!
//! - **写者**：M7-0 建（anchor 只落签名）；**M7-1 实现**（`docs/60` §3.3）。
//! - 它是**唯一**的共享 `InboundHandler`：按 `channel_type` 分发到该平台注册的
//!   [`ResolverSet`]，然后对**每个平台**跑同一条有序流水线。
//!
//! # 流水线（顺序有语义，别重排；前四步在**身份之前**）
//!
//! ```text
//! 0 归一化命令源（CommandText 空 ⇒ 取 Text）   5 身份 + 成员资格（未绑定 ⇒ needs_binding）
//! 1 命令分类（/new、/clear、/issue）           6 会话（/new 轮换 | /clear 记待开新会话 | 追加）
//! 2 路由到安装行（未命中 ⇒ invalid_event）     7 产物（/issue 建 issue 行）
//! 3 两阶段去重 claim（命中 ⇒ duplicate）       8 触发 run（去抖交给 RunTriggerer）
//! 4 群过滤（未 @bot ⇒ not_addressed_in_group） 9 脱离 ACK 的出站面（回复 / 打字 / 媒体）
//! ```
//!
//! # 与上游的三处**形态**差异（不是语义差异）
//!
//! 1. **端口化**：上游把 `IssueCreator` / `TaskEnqueuer` / `SessionReader` 接在 `service.*`
//!    上；本仓没有那个 service 层 ⇒ 三者是本 crate 的 trait（见 `resolvers.rs` 的对应表），
//!    实现归 M7-2…M7-20 各自的写集。
//! 2. **命令分类**：上游 `fresh_command.go` / `issue_command.go` 属 **M7-2** ⇒ 这里走
//!    [`CommandClassifier`] 端口，词表（[`CommandIntent`]）由 M7-1 一次落定。
//! 3. **去抖**：上游 `pendingBatcher` 同样属 M7-2 ⇒ run 触发走 [`RunTriggerer`]；
//!    媒体任务（脱离 ACK + 按会话保序）在本文件与 `router/media.rs` 实现。
//!
//! 不碰平台 wire、不直接写 SQL、不决定"未配置"的 HTTP 响应（那三类分别是 adapter /
//! `mc-repos` / route 层的事）。

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;

use crate::channel::ChannelResult;
use crate::engine::resolvers::{
    AppendParams, BindMediaParams, ChannelIssue, ChannelIssueCommand, ChannelIssueParams,
    ChatRunParams, CommandClassifier, CommandIntent, DropReason, EngineError, EngineResult,
    EnsureSessionParams, IssueCreator, Outcome, PipelineError, ResolverSet, RouteResult,
    RunTriggerer, SessionReader, StartSessionParams, WorkspaceIdentity,
};
use crate::message::InboundHandler;

mod media;
mod outbound;

use media::{MediaJob, MediaQueue};

/// 路由器的调参（上游 `RouterConfig` 的等价物）。
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// 单次脱离式回复 / 打字调用的上限。它跑在 connector ACK 路径**之外**，
    /// 所以必须严格短于平台 ACK 期限（Lark: 3s）。默认 2.5s。
    pub reply_timeout: Duration,
    /// 一条消息的脱离式媒体解析（下载 / 上传 / 附件绑定）预算，从 append 起算
    /// （必须与持久化的 fallback 一致）。默认 45s。
    pub media_timeout: Duration,
    /// 全局并发媒体解析上限（限突发内存与平台下载压力）；会话内保序不受它影响。默认 8。
    pub media_concurrency: usize,
    /// 路由重试上限（连接器报陈旧路由时的在进程重试；持续冲突必须暴露而不是永久重试）。
    pub max_route_change_retries: u32,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_millis(2500),
            media_timeout: Duration::from_secs(45),
            media_concurrency: 8,
            max_route_change_retries: 8,
        }
    }
}

/// 默认媒体预算（上游 `DefaultMediaTimeout`：导出给"结算延迟远大于流水线预算"的不变式断言用）。
pub const DEFAULT_MEDIA_TIMEOUT: Duration = Duration::from_secs(45);

/// 没有该 `channel_type` 的 `ResolverSet` —— **基础设施**错误（上游 `ErrNoResolverSet` 逐字：
/// 它必须是错误，让 adapter 上报而不是静默丢消息）。
pub const NO_RESOLVER_SET: &str = "channel router: no resolver set for channel type";

/// 渠道无关的入站路由器（上游 `Router`）：**唯一**的共享 `InboundHandler`
/// （每个 adapter 在接收循环里调 [`Router::route`]）。
pub struct Router {
    sets: RwLock<HashMap<ChannelKind, Arc<ResolverSet>>>,
    classifier: Arc<dyn CommandClassifier>,
    trigger: Arc<dyn RunTriggerer>,
    reader: Arc<dyn SessionReader>,
    issues: Arc<dyn IssueCreator>,
    config: RouterConfig,
    media: Arc<MediaQueue>,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Router")
            .field("kinds", &self.kinds())
            .field("reply_timeout", &self.config.reply_timeout)
            .field("media_timeout", &self.config.media_timeout)
            .field("media_concurrency", &self.config.media_concurrency)
            .finish_non_exhaustive()
    }
}

impl Router {
    /// 装配（不启动）；注册平台用 [`Router::register`]。
    pub fn new(
        classifier: Arc<dyn CommandClassifier>,
        trigger: Arc<dyn RunTriggerer>,
        reader: Arc<dyn SessionReader>,
        issues: Arc<dyn IssueCreator>,
        config: RouterConfig,
    ) -> Self {
        let media = Arc::new(MediaQueue::new(config.media_concurrency));
        Self {
            sets: RwLock::new(HashMap::new()),
            classifier,
            trigger,
            reader,
            issues,
            config,
            media,
        }
    }

    /// 绑定一个平台的端口包（boot 时调，`route` 之前）。重复注册同一个 kind 是
    /// **last-writer-wins**（与 `Registry` 同语义：部署方能覆盖内置实现）。
    pub fn register(&self, kind: ChannelKind, set: ResolverSet) {
        let mut sets = self
            .sets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.insert(kind, Arc::new(set));
    }

    /// 已注册的 kind（**字典序**：诊断与测试都要确定性）。
    pub fn kinds(&self) -> Vec<ChannelKind> {
        let sets = self
            .sets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut kinds: Vec<ChannelKind> = sets.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }

    /// 该 kind 注册了吗。
    pub fn has_kind(&self, kind: ChannelKind) -> bool {
        self.sets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&kind)
    }

    /// 某个 kind 的端口包（测试与诊断用）。
    pub fn resolver_set(&self, kind: ChannelKind) -> Option<Arc<ResolverSet>> {
        self.sets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&kind)
            .cloned()
    }

    /// 配置快照。
    pub fn config(&self) -> &RouterConfig {
        &self.config
    }

    /// 路由一条入站消息（[`InboundHandler`] 的契约：`Ok` 含"按产品理由丢弃"，`Err` = 基础设施失败）。
    pub async fn route(&self, message: InboundMessage) -> ChannelResult<()> {
        let kind = message.source.channel_type;
        let Some(set) = self.resolver_set(kind) else {
            tracing::error!(
                channel_type = kind.as_str(),
                "channel router: no resolver set"
            );
            return Err(crate::channel::ChannelError::InvalidConfig {
                kind: kind.as_str().to_string(),
                reason: NO_RESOLVER_SET.to_string(),
            });
        };

        match self.dispatch(&set, message).await {
            Ok(result) => {
                tracing::debug!(
                    channel_type = kind.as_str(),
                    outcome = result.outcome.as_str(),
                    drop_reason = result.drop_reason.map(DropReason::as_str),
                    "channel router: dispatch outcome"
                );
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    channel_type = kind.as_str(),
                    code = error.code_hint(),
                    "channel router: dispatch failed"
                );
                Err(error.into_channel_error())
            }
        }
    }

    /// 停机排空：冲刷 run 触发窗口 + 等媒体任务收尾。
    pub async fn drain(&self) -> bool {
        self.media.drain().await;
        match self.trigger.drain().await {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(code = error.code_hint(), "channel router: run drain failed");
                false
            }
        }
    }

    /// 流水线本体（上游 `dispatch` + `processClaimed` 的合并形态）。
    ///
    /// `too_many_lines`：这**就是**上游那一条流水线（顺序本身是语义），拆函数只会把
    /// "谁在第几步"藏进调用图里 —— 与 `docs/60` §4.1 的六步一一对应是它的价值。
    #[allow(clippy::too_many_lines)]
    async fn dispatch(
        &self,
        set: &Arc<ResolverSet>,
        message: InboundMessage,
    ) -> EngineResult<RouteResult> {
        let mut message = message;
        // 0. 命令源归一化：`command_text` 空 ⇒ 取（可能已被 adapter 富化的）`text`。
        if message.command_text.is_empty() {
            message.command_text = message.text.clone();
        }

        // 1. 命令分类（语义分叉的唯一一处）。
        let classified = self.classifier.classify(&message.command_text);
        let mut start_chat = false;
        let mut bare_fresh = false;
        let mut issue_command: Option<ChannelIssueCommand> = None;
        match classified {
            CommandIntent::NewChat { body } => {
                start_chat = true;
                // 只有未被 adapter 改写的正文才整体替换（富化过的正文不能丢）。
                if message.text == message.command_text {
                    message.text = body.clone();
                }
                // 被消费掉的 /new 源不得再被下游当成 /issue。
                message.command_text = body;
            }
            CommandIntent::FreshSession { body } => {
                // adapter 可能已经自己剥过指令（`force_fresh` 已置位）。
                let adapter_already_stripped = message.force_fresh;
                message.force_fresh = true;
                bare_fresh = body.trim().is_empty();
                if !adapter_already_stripped {
                    message.text = body;
                }
            }
            CommandIntent::Issue { title, description } => {
                issue_command = Some(ChannelIssueCommand { title, description });
            }
            CommandIntent::None => {}
        }

        // 2. 路由到安装行（在这些丢弃分支上还没有可挂 claim 的安装）。
        let installation = match set.installation.resolve_installation(&message).await {
            Ok(installation) => installation,
            Err(error) if is_pipeline(&error, &PipelineError::InstallationNotFound) => {
                return self
                    .drop_with(set, None, &message, DropReason::InvalidEvent)
                    .await;
            }
            Err(error) => return Err(error),
        };
        if !installation.active {
            return self
                .drop_with(
                    set,
                    Some(installation.id),
                    &message,
                    DropReason::RevokedInstallation,
                )
                .await;
        }

        // 3. 两阶段去重 claim（空 MessageID = 没有可去重的键 ⇒ 跳过）。
        let mut claim_token: Option<Id> = None;
        if !message.message_id.is_empty() {
            match set.dedup.claim(installation.id, &message.message_id).await {
                Ok(token) => claim_token = Some(token),
                Err(error) if is_pipeline(&error, &PipelineError::Duplicate) => {
                    return self
                        .drop_with(set, Some(installation.id), &message, DropReason::Duplicate)
                        .await;
                }
                Err(error) => return Err(error),
            }
        }

        let result = self
            .process_claimed(
                set,
                &message,
                &installation,
                claim_token,
                bare_fresh,
                start_chat,
                issue_command,
            )
            .await;

        let (result, finalize_mark) = match result {
            Ok(pair) => pair,
            Err(error) => {
                if let Some(token) = claim_token {
                    // 基础设施失败 ⇒ 释放 claim，让重投还能处理它。
                    let _ = set
                        .dedup
                        .release(installation.id, &message.message_id, token)
                        .await;
                }
                return Err(error);
            }
        };

        // claim 落在终态：`mark`（幂等）。
        if finalize_mark {
            if let Some(token) = claim_token {
                if let Err(error) = set
                    .dedup
                    .mark(installation.id, &message.message_id, token)
                    .await
                {
                    tracing::warn!(
                        installation_id = %installation.id,
                        code = error.code_hint(),
                        "channel router: dedup mark failed"
                    );
                }
            }
        }

        // 9a. 出站面（脱离 ACK 路径）。
        Router::emit_typing(set, &installation, &message, &result);
        Router::emit_reply(set, &installation, &message, &result);
        Ok(result)
    }

    /// `too_many_lines` / `too_many_arguments`：同上，这是流水线的后半段（会话 → 产物 →
    /// 触发）；参数就是那一步的输入，收进结构只会把顺序藏起来。
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    async fn process_claimed(
        &self,
        set: &Arc<ResolverSet>,
        message: &InboundMessage,
        installation: &crate::engine::resolvers::ResolvedInstallation,
        claim_token: Option<Id>,
        bare_fresh: bool,
        start_chat: bool,
        issue_command: Option<ChannelIssueCommand>,
    ) -> EngineResult<(RouteResult, bool)> {
        // 4. 群过滤（在身份之前：未绑定用户的群闲聊不该刷绑定卡）。
        if message.source.chat_type.is_group() && !message.addressed_to_bot {
            return Ok((
                self.drop(
                    set,
                    Some(installation.id),
                    message,
                    DropReason::NotAddressedInGroup,
                )
                .await,
                true,
            ));
        }

        // 5. 身份 + 成员资格。
        let identity = match set.identity.resolve_sender(installation, message).await {
            Ok(identity) => identity,
            Err(error) if is_pipeline(&error, &PipelineError::SenderUnbound) => {
                self.audit(set, Some(installation.id), message, DropReason::UnboundUser)
                    .await;
                return Ok((
                    RouteResult {
                        outcome: Outcome::NeedsBinding,
                        drop_reason: Some(DropReason::UnboundUser),
                        installation_id: Some(installation.id),
                        sender: message.source.sender_id.clone(),
                        ..RouteResult::default()
                    },
                    true,
                ));
            }
            Err(error) if is_pipeline(&error, &PipelineError::SenderNotMember) => {
                return Ok((
                    self.drop(
                        set,
                        Some(installation.id),
                        message,
                        DropReason::NonWorkspaceMember,
                    )
                    .await,
                    true,
                ));
            }
            Err(error) => return Err(error),
        };

        // 6. 会话（群聊创建者是**安装者**，不是发件人）。
        let session_creator = if message.source.chat_type.is_group() {
            installation.installer_user_id
        } else {
            identity.user_id
        };

        let has_media = set
            .media
            .as_ref()
            .is_some_and(|media| media.has_media(message));
        let has_selected_context = message.has_selected_context && !message.text.trim().is_empty();
        let persist_message = !message.command_text.is_empty() || has_selected_context || has_media;
        let resolve_media = has_media && !is_issue_usage(issue_command.as_ref());
        let media_pending_seconds = if resolve_media {
            self.config.media_timeout.as_secs_f64()
        } else {
            0.0
        };
        let deadline = tokio::time::Instant::now() + self.config.media_timeout;

        let mut retries = 0_u32;
        let (session_id, append) = loop {
            let attempt = if start_chat {
                set.session
                    .start_session(StartSessionParams {
                        installation: installation.clone(),
                        creator: session_creator,
                        sender: identity.user_id,
                        message: message.clone(),
                        claim_token,
                        media_pending_seconds,
                        persist_message,
                    })
                    .await
                    .map(|started| (started.session_id, started.append))
            } else {
                let existing = set
                    .session
                    .ensure_session(EnsureSessionParams {
                        installation: installation.clone(),
                        sender: session_creator,
                        message: message.clone(),
                    })
                    .await;
                match existing {
                    Ok(session_id) if bare_fresh => {
                        // 裸 /clear：只记"待开新会话"，没有正文入库。
                        match set
                            .session
                            .mark_pending_fresh(session_id, &message.message_id)
                            .await
                        {
                            Ok(()) => {
                                let result = RouteResult {
                                    outcome: Outcome::FreshPending,
                                    installation_id: Some(installation.id),
                                    chat_session_id: Some(session_id),
                                    sender: message.source.sender_id.clone(),
                                    ..RouteResult::default()
                                };
                                return Ok((result, true));
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Ok(session_id) => set
                        .session
                        .append_message(AppendParams {
                            session_id,
                            sender: identity.user_id,
                            installation_id: installation.id,
                            message: message.clone(),
                            claim_token,
                            media_pending_seconds,
                        })
                        .await
                        .map(|appended| (session_id, appended)),
                    Err(error) => Err(error),
                }
            };

            match attempt {
                Ok(pair) => break pair,
                Err(error) if is_pipeline(&error, &PipelineError::RouteChanged) => {
                    retries += 1;
                    if retries >= self.config.max_route_change_retries {
                        return Err(EngineError::infra(format!(
                            "channel route did not stabilize after {retries} retries"
                        )));
                    }
                    tracing::info!(
                        channel_type = message.source.channel_type.as_str(),
                        event_id = message.event_id,
                        attempt = retries,
                        "channel route changed; retrying inbound"
                    );
                }
                Err(error) => return Err(error),
            }
        };

        // 6b. /new 且没有要持久化的正文 ⇒ 只轮换路由。
        if start_chat && !persist_message {
            let result = RouteResult {
                outcome: Outcome::ChatStarted,
                installation_id: Some(installation.id),
                chat_session_id: Some(session_id),
                channel_binding_id: append.binding_id,
                channel_route_revision: append.route_revision,
                sender: message.source.sender_id.clone(),
                ..RouteResult::default()
            };
            return Ok((result, true));
        }

        let mut result = RouteResult {
            outcome: Outcome::Ingested,
            installation_id: Some(installation.id),
            chat_session_id: Some(session_id),
            channel_binding_id: append.binding_id,
            channel_route_revision: append.route_revision,
            sender: message.source.sender_id.clone(),
            ..RouteResult::default()
        };

        // 7. 产物：/issue（正文已经持久化 ⇒ 从这里起的所有错误都不再 Release）。
        let parsed = append.issue_command.clone().or(issue_command);
        if let Some(command) = parsed {
            if command.title.trim().is_empty() {
                result.outcome = Outcome::IssueUsage;
                result.issue_usage_had_media = has_media;
                // 重复命中 / 用法提示都不消费媒体：只做空收尾把 pending 标记清掉。
                self.enqueue_media(MediaJob::new(
                    set,
                    installation,
                    &identity,
                    message,
                    session_id,
                    append.message_id,
                    deadline,
                ));
                return Ok((result, !append.dedup_marked));
            }
            let identity_meta = self
                .reader
                .workspace_identity(installation.workspace_id)
                .await
                .unwrap_or_default();
            let created = self
                .issues
                .create_issue(ChannelIssueParams {
                    workspace_id: installation.workspace_id,
                    title: command.title.clone(),
                    description: command.description.clone(),
                    agent_id: installation.agent_id,
                    creator_user_id: identity.user_id,
                    origin_type: set.origin_type.clone(),
                    origin_session_id: session_id,
                    assigned_run_fire_at: None,
                })
                .await?;
            result.issue = Some(ChannelIssue {
                id: created.issue.id,
                number: created.issue.number,
                title: created.issue.title.clone(),
            });
            result.issue_identifier = issue_identifier(&identity_meta, created.issue.number);
            result.issue_workspace_slug = identity_meta.slug.clone();
            result.issue_duplicate = created.duplicate;
            // issue 命令是**终态**：不再排普通 chat run（否则 agent 会把命令再执行一遍）。
            // 重复命中时不消费媒体（没有新 issue 会用它）：只做空收尾。
            let media_job = if created.duplicate {
                MediaJob::new(
                    set,
                    installation,
                    &identity,
                    message,
                    session_id,
                    append.message_id,
                    deadline,
                )
            } else {
                MediaJob::new(
                    set,
                    installation,
                    &identity,
                    message,
                    session_id,
                    append.message_id,
                    deadline,
                )
                .map(|job| job.with_issue(created.issue.id, &command))
                .map(|job| job.remote(resolve_media))
            };
            self.enqueue_media(media_job);
            return Ok((result, !append.dedup_marked));
        }

        // 8. 触发 run（去抖归 RunTriggerer；`skip_agent_run` 只留产物）。
        if !message.skip_agent_run {
            self.trigger
                .schedule_chat_run(ChatRunParams {
                    installation: installation.clone(),
                    session_id,
                    initiator_user_id: identity.user_id,
                    channel_binding_id: result.channel_binding_id,
                    route_revision: result.channel_route_revision,
                    force_fresh: message.force_fresh,
                    context_revision: append.context_revision,
                })
                .await?;
            result.run_scheduled = true;
        }

        self.enqueue_media(
            MediaJob::new(
                set,
                installation,
                &identity,
                message,
                session_id,
                append.message_id,
                deadline,
            )
            .map(|job| job.remote(resolve_media)),
        );
        // 追加路径：binder 若没在自己的事务里 Mark，就在流水线后补一次（上游同款兜底）。
        Ok((result, !append.dedup_marked))
    }

    /// 丢弃 = 审计一行 + 返回判决（**不是**错误）。
    async fn drop(
        &self,
        set: &ResolverSet,
        installation_id: Option<Id>,
        message: &InboundMessage,
        reason: DropReason,
    ) -> RouteResult {
        self.audit(set, installation_id, message, reason).await;
        RouteResult::dropped(reason, installation_id)
    }

    async fn drop_with(
        &self,
        set: &ResolverSet,
        installation_id: Option<Id>,
        message: &InboundMessage,
        reason: DropReason,
    ) -> EngineResult<RouteResult> {
        Ok(self.drop(set, installation_id, message, reason).await)
    }

    /// 审计是**尽力而为**：写失败不能改变判决（上游 `_ = set.Audit.RecordDrop`）。
    async fn audit(
        &self,
        set: &ResolverSet,
        installation_id: Option<Id>,
        message: &InboundMessage,
        reason: DropReason,
    ) {
        if let Err(error) = set
            .audit
            .record_drop(installation_id, message, reason)
            .await
        {
            tracing::warn!(
                reason = reason.as_str(),
                code = error.code_hint(),
                "channel router: drop audit failed"
            );
        }
    }

    /// 媒体解析：**脱离 ACK 路径**、**按会话保序**、受全局并发与自己的预算约束。
    fn enqueue_media(&self, job: Option<MediaJob>) {
        let Some(job) = job else {
            return;
        };
        if self.media.is_stopping() {
            return;
        }
        let queue = Arc::clone(&self.media);
        let session = queue.session_lock(job.session_id);
        let sem = Arc::clone(&queue.sem);
        queue.inflight.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            // 会话内保序：等前一条；自己的预算耗尽就跳过远端解析（只做空收尾）。
            let budget = job
                .deadline
                .saturating_duration_since(tokio::time::Instant::now());
            let guard = tokio::time::timeout(budget, session.lock()).await.ok();
            let expired = guard.is_none() || tokio::time::Instant::now() >= job.deadline;
            // 全局并发槽：预算内没抢到就不占（远端解析同样跳过）。
            let _permit = if expired || !job.resolve_remote {
                None
            } else {
                let remaining = job
                    .deadline
                    .saturating_duration_since(tokio::time::Instant::now());
                tokio::time::timeout(remaining, sem.acquire())
                    .await
                    .ok()
                    .and_then(std::result::Result::ok)
            };
            let resolved = if job.resolve_remote && !expired {
                job.media.resolve_media(
                    &job.installation,
                    &job.identity,
                    job.session_id,
                    job.chat_message_id,
                    &job.message,
                )
            } else {
                // 预算耗尽 ⇒ 只做空收尾：refs 清空，正文里的占位文本保留。
                let mut copy = job.message.clone();
                copy.media_refs.clear();
                copy
            };
            let outcome = job
                .set
                .session
                .bind_media(BindMediaParams {
                    message_id: job.chat_message_id,
                    session_id: job.session_id,
                    workspace_id: job.installation.workspace_id,
                    sender: job.identity.user_id,
                    issue_id: job.issue_id,
                    issue_description_base: job.issue_description_base.clone(),
                    issue_command_text: job.issue_command_text.clone(),
                    body: resolved.text,
                    media_refs: resolved.media_refs,
                })
                .await;
            if let Err(error) = outcome {
                // 绝不在行内删任何东西：上传对象都有先于 PUT 写的意图账本行，对账器事后收尾。
                tracing::warn!(
                    channel_type = job.message.source.channel_type.as_str(),
                    event_id = job.message.event_id,
                    code = error.code_hint(),
                    "channel router: media attachment binding failed"
                );
            }
            queue.finish_one();
        });
    }
}

#[async_trait]
impl InboundHandler for Router {
    async fn handle(&self, message: InboundMessage) -> ChannelResult<()> {
        self.route(message).await
    }
}

fn is_pipeline(error: &EngineError, want: &PipelineError) -> bool {
    matches!(error, EngineError::Pipeline(got) if got == want)
}

fn is_issue_usage(command: Option<&ChannelIssueCommand>) -> bool {
    command.is_some_and(|command| command.title.trim().is_empty())
}

fn issue_identifier(identity: &WorkspaceIdentity, number: i64) -> String {
    if identity.issue_prefix.is_empty() {
        format!("#{number}")
    } else {
        format!("{}-{number}", identity.issue_prefix)
    }
}

#[cfg(test)]
mod tests;
