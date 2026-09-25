//! Slack 的 `/issue`、`/new`、`/clear` **斜杠命令**端到端
//! （上游 `internal/integrations/slack/slash_command.go` 368 行 + `slash_control.go` 191 行）。
//!
//! - **写者**：M7-4（`docs/60-M7-PLAN.md` §3.3）。
//! - **为什么必须是真正的斜杠命令**（上游 MUL-3908 注释逐字）：Slack 会把**首字符是 `/`**
//!   的消息在客户端就截走当作斜杠命令，**永远不会**投给 app ⇒ `/issue` 的消息前缀形态在
//!   Slack 上根本不可用。把 `/issue` 注册进 app manifest，它才会以 `EventTypeSlashCommand`
//!   从**同一条** Socket Mode 连接到达。
//!
//! # `/issue` 是**快速创建**入口，它自己不建 issue
//!
//! 它把调用者的自然语言描述当 prompt，对着安装的 agent 排一个 quick-create 任务
//! （与 web 的"快速创建"弹窗**同一条**流水线）。agent 在后台把它变成一条规整的
//! `multica issue create` —— 于是 issue 拿到的是像样的标题 + 结构化描述，而不是用户打的
//! 那一行。因为创建是**异步**的，命令用 `response_url` 回一条**私密**（ephemeral）确认，
//! agent 完成后再以 Multica inbox 通知的形式回到调用者。**不**开 chat 会话。
//!
//! # 三条从上游逐字搬来的纪律
//!
//! 1. **ACK 已经在传输层做完了**（`socket.rs`：先 ACK 再判决）⇒ 本文件只负责"答复"，
//!    而且**从不**返回错误：每一种结局都是一条给用户看的消息。
//! 2. **控制命令（`/new` / `/clear`）按 Socket Mode 的 envelope id 去重**：
//!    重连重投同一个信封只能生效一次（上游 `deduper.Claim`）。
//! 3. **频道里不做控制命令**：`/new` / `/clear` 只在 **DM**（channel id 以 `D` 开头）里生效；
//!    频道用户必须用 `@Multica /new` 这种提及形态 —— 因为斜杠载荷**不带 `thread_ts`**，
//!    猜一个频道级的线程根会把无关的对话搬走（上游注释逐字）。
//!
//! # 与上游的三处形态差异（登记 `docs/32` §15）
//!
//! 1. **解析器复用 M7-3 的端口**：上游在 `slash_command.go` 里**又写了一份**安装 / 成员
//!    解析（"kept local so the proven inbound pipeline is untouched"）。本仓沿用
//!    M7-3 的 [`InstallationQueries`] / [`IdentityQueries`]，**一份**实现、一条纪律。
//!    副作用是"已撤销安装"走的是 `InstallationNotFound` 分支 —— 用户可见结果与上游**相同**
//!    （都是 [`SLASH_DISABLED_TEXT`]）。
//! 2. **控制命令的会话动作走 engine 端口**（[`SessionBinder`] + [`Deduper`]），不自己开
//!    `ChatSession`：上游的 `slackDMControlStarter` 直接驱动 SQL 事务，本仓的会话状态机
//!    在 M7-2 的 `engine/session.rs` 里，复用它才是唯一不漂移的写法。
//! 3. **`response_url` 是能力 URL**：它自带签名票据 ⇒ 不进日志、不进 `Debug`、
//!    错误文案也不回显它（凭据纪律同 `apps.connections.open` 的 `wss://`）。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage, Source};
use mc_core::id::Id;

use crate::engine::resolvers::{
    AppendParams, Deduper, EngineError, EnsureSessionParams, PipelineError, ResolvedInstallation,
    SessionBinder, StartSessionParams,
};
use crate::slack::inbound::TYPE_SLACK;
use crate::slack::replier::{url_encode, BindingMinter};
use crate::slack::resolvers::{IdentityQueries, InstallationQueries, InstallationRow};

/// `/issue`（上游 `issueSlashCommand`）。
pub const ISSUE_COMMAND: &str = "/issue";
/// `/new`。
pub const NEW_COMMAND: &str = "/new";
/// `/clear`。
pub const CLEAR_COMMAND: &str = "/clear";
/// 绑定页默认路径（上游 `BindingPath` 零值）。
pub const DEFAULT_BINDING_PATH: &str = "/slack/bind";

// ---- 面向用户的 ephemeral 文案（上游 const 块，逐字） ----
/// `/issue` 缺正文时的用法提示。
pub const SLASH_USAGE_TEXT: &str =
    "Tell me what to file, e.g. `/issue the login button does nothing on Safari`.";
/// 已排队。
pub const SLASH_QUEUED_TEXT: &str =
    "✅ On it — I'm turning that into an issue. You'll get a Multica notification when it's ready.";
/// 调用者不是 workspace 成员。
pub const SLASH_NOT_MEMBER_TEXT: &str =
    "You're not a member of this Multica workspace, so I can't file an issue for you.";
/// 没接绑定服务时的退路提示。
pub const SLASH_LINK_ACCOUNT_FALLBACK: &str =
    "Link your Slack account to Multica first, then try `/issue` again.";
/// 达到 issue 上限。
pub const SLASH_ISSUE_LIMIT_TEXT: &str =
    "⚠️ This workspace has reached its issue limit. Open Multica to view the available recovery options.";
/// 内部错误。
pub const SLASH_INTERNAL_ERROR_TEXT: &str =
    "⚠️ Something went wrong creating the issue. Please try again.";
/// 这个 app 没连到 Multica（或已断开）。
pub const SLASH_DISABLED_TEXT: &str =
    "This Slack app isn't connected to Multica (or was disconnected). Ask a workspace admin to reconnect it.";
/// `/new` 已开始。
pub const SLASH_NEW_STARTED_TEXT: &str = "✅ Started a new Multica chat.";
/// 频道里 `/new` 的引导。
pub const SLASH_NEW_THREAD_GUIDE_TEXT: &str =
    "In a channel, start the new chat from the target thread with `@Multica /new`.";
/// `/clear` 已生效。
pub const SLASH_CLEAR_STARTED_TEXT: &str = "✅ Cleared the agent context in this Multica chat.";
/// 频道里 `/clear` 的引导。
pub const SLASH_CLEAR_THREAD_GUIDE_TEXT: &str =
    "In a channel, clear the target thread's context with `@Multica /clear`.";

// =====================================================================
// 载荷
// =====================================================================

/// 一条斜杠命令载荷（上游 `slack.SlashCommand` 的契约子集）。
///
/// `response_url` 是**能力 URL** ⇒ [`SlashCommand::Debug`] 手写脱敏。
#[derive(Clone, PartialEq, Eq, Default)]
pub struct SlashCommand {
    pub command: String,
    pub text: String,
    pub api_app_id: String,
    pub team_id: String,
    pub channel_id: String,
    pub user_id: String,
    pub response_url: String,
}

impl fmt::Debug for SlashCommand {
    /// 手写脱敏：`response_url` 自带签名票据（等价于凭据）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SlashCommand")
            .field("command", &self.command)
            .field("text_len", &self.text.chars().count())
            .field("api_app_id", &self.api_app_id)
            .field("team_id", &self.team_id)
            .field("channel_id", &self.channel_id)
            .field("user_id", &self.user_id)
            .field(
                "response_url",
                &if self.response_url.is_empty() {
                    "<empty>"
                } else {
                    "<redacted>"
                },
            )
            .finish()
    }
}

impl SlashCommand {
    /// 从 Socket Mode 的 `slash_commands` 帧载荷解出。
    ///
    /// 缺 `command` ⇒ `None`（认不出的载荷不是错误，丢弃即可 —— 上游同）。
    #[must_use]
    pub fn from_payload(payload: &serde_json::Value) -> Option<Self> {
        let field = |key: &str| {
            payload
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let command = field("command");
        if command.is_empty() {
            return None;
        }
        Some(Self {
            command,
            text: field("text"),
            api_app_id: field("api_app_id"),
            team_id: field("team_id"),
            channel_id: field("channel_id"),
            user_id: field("user_id"),
            response_url: field("response_url"),
        })
    }

    /// 是不是本处理器认的三条命令之一（上游 `HandleEnvelope` 的第一道门）。
    #[must_use]
    pub fn recognised(&self) -> bool {
        let command = self.command.trim();
        command.eq_ignore_ascii_case(ISSUE_COMMAND)
            || command == NEW_COMMAND
            || command == CLEAR_COMMAND
    }

    /// 是不是控制命令（`/new` / `/clear`）。
    #[must_use]
    pub fn is_control(&self) -> bool {
        let command = self.command.trim();
        command == NEW_COMMAND || command == CLEAR_COMMAND
    }

    /// 频道里还是 DM 里（上游判据是 channel id 是否以 `D` 开头）。
    #[must_use]
    pub fn is_direct_message(&self) -> bool {
        self.channel_id.starts_with('D')
    }
}

// =====================================================================
// 端口
// =====================================================================

/// ephemeral 答复（上游注入的 `respond`；默认实现 POST 到 `response_url`）。
#[async_trait]
pub trait EphemeralResponder: Send + Sync {
    /// POST 一条私密消息到命令的 `response_url`（**不需要** bot token）。
    async fn respond(&self, response_url: &str, text: &str) -> Result<(), String>;
}

/// 生产实现：POST `{"response_type":"ephemeral","text":…}`。
#[derive(Debug, Default, Clone)]
pub struct HttpResponder;

#[async_trait]
impl EphemeralResponder for HttpResponder {
    async fn respond(&self, response_url: &str, text: &str) -> Result<(), String> {
        // 错误文案**不**带 URL（它是能力 URL）。
        reqwest::Client::new()
            .post(response_url)
            .json(&serde_json::json!({
                "response_type": "ephemeral",
                "text": text,
            }))
            .send()
            .await
            .map(|_| ())
            .map_err(|_| "slack: response_url POST failed".to_string())
    }
}

/// quick-create 入队（上游 `quickCreateEnqueuer`）。
#[async_trait]
pub trait QuickCreateEnqueuer: Send + Sync {
    /// 把调用者的 prompt 交给安装的 agent（**不**带 squad / 项目 / 父 issue / 附件）。
    async fn enqueue_quick_create(
        &self,
        params: &QuickCreateParams,
    ) -> Result<(), QuickCreateError>;
}

/// quick-create 的入参（上游 `EnqueueQuickCreateTask` 的渠道子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickCreateParams {
    pub workspace_id: Id,
    pub requester_id: Id,
    pub agent_id: Id,
    pub prompt: String,
}

/// quick-create 的失败（上游用 `errors.As(*IssueLimitReachedError)` 分辨的那一条是产品性的）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuickCreateError {
    /// workspace 达到 issue 上限（产品性：回"去 Multica 看恢复选项"）。
    #[error("slack: workspace reached its issue limit")]
    IssueLimitReached,
    /// 其它失败（回泛化的内部错误）。
    #[error("slack: quick create failed: {message}")]
    Other { message: String },
}

/// DM 会话控制（上游 `slashControlStarter`）。
///
/// 生产实现是 [`SessionControlStarter`]（复用 M7-2 的 engine 端口）。
#[async_trait]
pub trait ControlStarter: Send + Sync {
    /// `/new`：轮换会话路由并（正文非空时）开一条新会话里的任务。
    async fn start_dm_chat(
        &self,
        installation: &ResolvedInstallation,
        user_id: Id,
        command: &SlashCommand,
        envelope_id: &str,
    ) -> Result<(), EngineError>;

    /// `/clear`：开新上下文代际（正文非空时随同一提交入队）。
    async fn clear_dm_context(
        &self,
        installation: &ResolvedInstallation,
        user_id: Id,
        command: &SlashCommand,
        envelope_id: &str,
    ) -> Result<(), EngineError>;
}

// =====================================================================
// 生产者：控制命令的会话动作（复用 engine 端口）
// =====================================================================

/// 用 engine 的 [`SessionBinder`] + [`Deduper`] 实现的控制命令（见模块文档差异 2）。
pub struct SessionControlStarter {
    session: Arc<dyn SessionBinder>,
    dedup: Arc<dyn Deduper>,
}

impl fmt::Debug for SessionControlStarter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionControlStarter")
            .field("session", &"<dyn SessionBinder>")
            .field("dedup", &"<dyn Deduper>")
            .finish()
    }
}

impl SessionControlStarter {
    /// 装配。
    #[must_use]
    pub fn new(session: Arc<dyn SessionBinder>, dedup: Arc<dyn Deduper>) -> Self {
        Self { session, dedup }
    }

    /// 从斜杠载荷造一条"合成的"入站消息（上游 `StartSession` 用的那几个字段）。
    fn synthetic(command: &SlashCommand) -> InboundMessage {
        InboundMessage {
            event_id: format!("slash:{}", command.command),
            message_id: String::new(),
            source: Source {
                channel_type: TYPE_SLACK,
                chat_id: command.channel_id.clone(),
                chat_type: ChatType::P2p,
                sender_id: command.user_id.clone(),
                sender_stable_id: String::new(),
                thread_id: String::new(),
            },
            kind: mc_core::channel::message::MessageKind::Text,
            text: command.text.trim().to_string(),
            command_text: command.text.trim().to_string(),
            has_selected_context: false,
            media_refs: Vec::new(),
            reply_to: None,
            addressed_to_bot: false,
            force_fresh: true,
            skip_agent_run: false,
            raw: serde_json::Value::Null,
        }
    }
}

#[async_trait]
impl ControlStarter for SessionControlStarter {
    async fn start_dm_chat(
        &self,
        installation: &ResolvedInstallation,
        user_id: Id,
        command: &SlashCommand,
        envelope_id: &str,
    ) -> Result<(), EngineError> {
        if envelope_id.is_empty() {
            return Err(EngineError::infra("slack /new: missing socket envelope id"));
        }
        // 去重所有权是围栏：重连重投同一个信封只能生效一次。
        let claim = self.dedup.claim(installation.id, envelope_id).await?;
        let body = command.text.trim().to_string();
        let mut message = Self::synthetic(command);
        message.message_id = envelope_id.to_string();
        let result = self
            .session
            .start_session(StartSessionParams {
                installation: installation.clone(),
                creator: user_id,
                sender: user_id,
                message,
                claim_token: Some(claim),
                media_pending_seconds: 0.0,
                persist_message: !body.is_empty(),
            })
            .await;
        if result.is_err() {
            // 没提交 ⇒ 放掉围栏（上游 `context.WithoutCancel` 的等价物是"尽力释一把"）。
            let _ = self
                .dedup
                .release(installation.id, envelope_id, claim)
                .await;
        }
        result.map(|_| ())
    }

    async fn clear_dm_context(
        &self,
        installation: &ResolvedInstallation,
        user_id: Id,
        command: &SlashCommand,
        envelope_id: &str,
    ) -> Result<(), EngineError> {
        if envelope_id.is_empty() {
            return Err(EngineError::infra(
                "slack /clear: missing socket envelope id",
            ));
        }
        let claim = self.dedup.claim(installation.id, envelope_id).await?;
        let body = command.text.trim().to_string();
        let mut message = Self::synthetic(command);
        message.message_id = envelope_id.to_string();
        let attempt = async {
            let session_id = self
                .session
                .ensure_session(EnsureSessionParams {
                    installation: installation.clone(),
                    sender: user_id,
                    message: message.clone(),
                })
                .await?;
            if body.is_empty() {
                // 裸 `/clear`：只记下"待开新会话"，没有正文入库。
                self.session
                    .mark_pending_fresh(session_id, envelope_id)
                    .await
            } else {
                self.session
                    .append_message(AppendParams {
                        session_id,
                        sender: user_id,
                        installation_id: installation.id,
                        message,
                        claim_token: Some(claim),
                        media_pending_seconds: 0.0,
                    })
                    .await
                    .map(|_| ())
            }
        }
        .await;
        if attempt.is_err() {
            let _ = self
                .dedup
                .release(installation.id, envelope_id, claim)
                .await;
        }
        attempt
    }
}

// =====================================================================
// 处理器
// =====================================================================

/// `/issue`、`/new`、`/clear` 的端到端处理器（上游 `SlashCommandProcessor`）。
pub struct SlashCommandProcessor {
    installations: Arc<dyn InstallationQueries>,
    identities: Arc<dyn IdentityQueries>,
    tasks: Arc<dyn QuickCreateEnqueuer>,
    control: Option<Arc<dyn ControlStarter>>,
    binding: Option<Arc<dyn BindingMinter>>,
    responder: Arc<dyn EphemeralResponder>,
    /// web app 主机（绑定链接要它；空串 ⇒ 退回纯文本提示）。
    app_url: String,
    binding_path: String,
}

impl fmt::Debug for SlashCommandProcessor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SlashCommandProcessor")
            .field("installations", &"<dyn InstallationQueries>")
            .field("identities", &"<dyn IdentityQueries>")
            .field("tasks", &"<dyn QuickCreateEnqueuer>")
            .field("control", &self.control.is_some())
            .field("binding", &self.binding.is_some())
            .field("responder", &"<dyn EphemeralResponder>")
            .field("app_url", &self.app_url)
            .field("binding_path", &self.binding_path)
            .finish()
    }
}

/// 装配袋（上游 `SlashCommandConfig`）。
pub struct SlashCommandDeps {
    pub installations: Arc<dyn InstallationQueries>,
    pub identities: Arc<dyn IdentityQueries>,
    pub tasks: Arc<dyn QuickCreateEnqueuer>,
    pub control: Option<Arc<dyn ControlStarter>>,
    pub binding: Option<Arc<dyn BindingMinter>>,
    pub responder: Arc<dyn EphemeralResponder>,
    /// web app 主机（`MULTICA_APP_URL`，回落 `FRONTEND_ORIGIN`；本仓由调用方注入）。
    pub app_url: String,
    pub binding_path: Option<String>,
}

impl SlashCommandProcessor {
    /// 装配（默认答复器 = POST `response_url`）。
    #[must_use]
    pub fn new(deps: SlashCommandDeps) -> Self {
        Self::with_responder(deps, Arc::new(HttpResponder))
    }

    /// 装配（注入答复器；用例捕获答复而**不**真打 Slack）。
    #[must_use]
    pub fn with_responder(deps: SlashCommandDeps, responder: Arc<dyn EphemeralResponder>) -> Self {
        let raw = deps
            .binding_path
            .unwrap_or_else(|| DEFAULT_BINDING_PATH.to_string());
        let binding_path = if raw.starts_with('/') {
            raw
        } else {
            format!("/{raw}")
        };
        Self {
            installations: deps.installations,
            identities: deps.identities,
            tasks: deps.tasks,
            control: deps.control,
            binding: deps.binding,
            responder,
            app_url: deps.app_url.trim_end_matches('/').to_string(),
            binding_path,
        }
    }

    /// 完整路径：判决 → 答复（上游 `HandleEnvelope`）。
    ///
    /// **从不返回错误**：每一种结局都是一条用户可见的消息（除了"没有正文要回"）。
    pub async fn handle(&self, command: &SlashCommand, envelope_id: &str) {
        if !command.recognised() {
            return;
        }
        let text = if command.is_control() {
            self.process_control(command, envelope_id).await
        } else {
            self.process_issue(command).await
        };
        if text.is_empty() || command.response_url.is_empty() {
            return;
        }
        if let Err(message) = self.responder.respond(&command.response_url, &text).await {
            tracing::warn!(
                app_id = command.api_app_id,
                error = message,
                "slack slash command: response_url reply failed"
            );
        }
    }

    /// `/new` / `/clear`（上游 `processControl`）。
    async fn process_control(&self, command: &SlashCommand, envelope_id: &str) -> String {
        let Some(installation) = self.resolve_installation(command).await else {
            return SLASH_DISABLED_TEXT.to_string();
        };
        if !installation.active {
            return SLASH_DISABLED_TEXT.to_string();
        }
        let user_id = match self.resolve_user(&installation, &command.user_id).await {
            Ok(user_id) => user_id,
            Err(SenderProblem::Unbound) => {
                return self.binding_text(&installation, &command.user_id).await;
            }
            Err(SenderProblem::NotMember) => return SLASH_NOT_MEMBER_TEXT.to_string(),
            Err(SenderProblem::Infra) => return SLASH_INTERNAL_ERROR_TEXT.to_string(),
        };
        // 斜杠载荷不带 `thread_ts`：频道里猜一个线程根会把无关对话搬走 ⇒ 只引导。
        if !command.is_direct_message() {
            return if command.command.trim() == CLEAR_COMMAND {
                SLASH_CLEAR_THREAD_GUIDE_TEXT.to_string()
            } else {
                SLASH_NEW_THREAD_GUIDE_TEXT.to_string()
            };
        }
        let Some(control) = &self.control else {
            return SLASH_INTERNAL_ERROR_TEXT.to_string();
        };
        let outcome = if command.command.trim() == CLEAR_COMMAND {
            control
                .clear_dm_context(&installation, user_id, command, envelope_id)
                .await
        } else {
            control
                .start_dm_chat(&installation, user_id, command, envelope_id)
                .await
        };
        match outcome {
            Ok(()) => {
                if command.command.trim() == CLEAR_COMMAND {
                    SLASH_CLEAR_STARTED_TEXT.to_string()
                } else {
                    SLASH_NEW_STARTED_TEXT.to_string()
                }
            }
            Err(error) => match error {
                // 去重命中 ⇒ 这条命令**已经**生效过，答复照旧（幂等）。
                EngineError::Pipeline(PipelineError::Duplicate | PipelineError::ClaimLost) => {
                    if command.command.trim() == CLEAR_COMMAND {
                        SLASH_CLEAR_STARTED_TEXT.to_string()
                    } else {
                        SLASH_NEW_STARTED_TEXT.to_string()
                    }
                }
                other => {
                    tracing::warn!(
                        outcome = "session_control_failed",
                        command = command.command,
                        channel_type = "slack",
                        app_id = command.api_app_id,
                        code = other.code_hint(),
                        "slack slash command: session control failed"
                    );
                    SLASH_INTERNAL_ERROR_TEXT.to_string()
                }
            },
        }
    }

    /// `/issue`（上游 `process`）。
    async fn process_issue(&self, command: &SlashCommand) -> String {
        let prompt = command.text.trim();
        if prompt.is_empty() {
            return SLASH_USAGE_TEXT.to_string();
        }
        let Some(installation) = self.resolve_installation(command).await else {
            return SLASH_DISABLED_TEXT.to_string();
        };
        if !installation.active {
            return SLASH_DISABLED_TEXT.to_string();
        }
        let user_id = match self.resolve_user(&installation, &command.user_id).await {
            Ok(user_id) => user_id,
            Err(SenderProblem::Unbound) => {
                return self.binding_text(&installation, &command.user_id).await;
            }
            Err(SenderProblem::NotMember) => return SLASH_NOT_MEMBER_TEXT.to_string(),
            Err(SenderProblem::Infra) => {
                tracing::warn!(
                    app_id = command.api_app_id,
                    "slack slash command: resolve user failed"
                );
                return SLASH_INTERNAL_ERROR_TEXT.to_string();
            }
        };
        // 把原始自然语言 prompt 交给安装的 agent；**不加** squad / 项目 / 父 issue / 附件。
        match self
            .tasks
            .enqueue_quick_create(&QuickCreateParams {
                workspace_id: installation.workspace_id,
                requester_id: user_id,
                agent_id: installation.agent_id,
                prompt: prompt.to_string(),
            })
            .await
        {
            Ok(()) => SLASH_QUEUED_TEXT.to_string(),
            Err(QuickCreateError::IssueLimitReached) => SLASH_ISSUE_LIMIT_TEXT.to_string(),
            Err(QuickCreateError::Other { .. }) => {
                tracing::warn!(
                    app_id = command.api_app_id,
                    "slack slash command: enqueue quick-create failed"
                );
                SLASH_INTERNAL_ERROR_TEXT.to_string()
            }
        }
    }

    /// 把命令的 `api_app_id`（+ 事件 team）映射到安装（上游 `resolveInstallation`）。
    async fn resolve_installation(&self, command: &SlashCommand) -> Option<ResolvedInstallation> {
        let row = self
            .installations
            .find_active_by_app_id(&command.api_app_id)
            .await
            .ok()
            .flatten()?;
        if !crate::slack::config::installation_serves_team(&row.config, &command.team_id) {
            return None;
        }
        Some(resolved_from(&row))
    }

    /// 平台用户 id → 绑定的 Multica 用户，并**重校验** workspace 成员资格。
    async fn resolve_user(
        &self,
        installation: &ResolvedInstallation,
        channel_user_id: &str,
    ) -> Result<Id, SenderProblem> {
        let binding = self
            .identities
            .find_user_binding(installation.id, channel_user_id)
            .await
            .map_err(|_| SenderProblem::Infra)?;
        let Some(binding) = binding else {
            return Err(SenderProblem::Unbound);
        };
        let member = self
            .identities
            .is_workspace_member(installation.workspace_id, binding.multica_user_id())
            .await
            .map_err(|_| SenderProblem::Infra)?;
        if !member {
            return Err(SenderProblem::NotMember);
        }
        Ok(binding.multica_user_id())
    }

    /// 铸绑定令牌 + 拼「绑定账号」提示（上游 `bindingText`）。
    async fn binding_text(
        &self,
        installation: &ResolvedInstallation,
        channel_user_id: &str,
    ) -> String {
        let (Some(binding), false) = (&self.binding, self.app_url.is_empty()) else {
            return SLASH_LINK_ACCOUNT_FALLBACK.to_string();
        };
        let Ok(token) = binding
            .mint(installation.workspace_id, installation.id, channel_user_id)
            .await
        else {
            tracing::warn!(
                installation_id = %installation.id,
                "slack slash command: mint binding token failed"
            );
            return SLASH_LINK_ACCOUNT_FALLBACK.to_string();
        };
        let bind_url = format!(
            "{}{}?token={}",
            self.app_url,
            self.binding_path,
            url_encode(&token.raw)
        );
        // 包成显式 Slack 链接：base64url 令牌里的 `_`/`-` 不会被 mrkdwn 折成斜体（上游逐字）。
        format!(
            "👋 To file issues, link your Slack account to Multica: <{bind_url}|link your account>\n(This link expires in 15 minutes.)"
        )
    }
}

/// 发件人解析的产品判决（与 [`PipelineError`] 同源，收窄成三个分支）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SenderProblem {
    Unbound,
    NotMember,
    Infra,
}

/// 安装行 → engine 的解析结果（`platform` 带上 adapter 自己的行）。
fn resolved_from(row: &InstallationRow) -> ResolvedInstallation {
    ResolvedInstallation {
        id: row.id,
        workspace_id: row.workspace_id,
        agent_id: row.agent_id,
        installer_user_id: row.installer_user_id,
        active: row.is_active(),
        kind: TYPE_SLACK,
        platform: Some(Arc::new(row.clone())),
    }
}

/// 本 adapter 的命令字面量（诊断用）。
#[must_use]
pub fn commands() -> [&'static str; 3] {
    [ISSUE_COMMAND, NEW_COMMAND, CLEAR_COMMAND]
}

#[cfg(test)]
mod tests;
