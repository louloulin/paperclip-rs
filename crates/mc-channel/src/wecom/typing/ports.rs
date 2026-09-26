//! 打字指示的**端口与值类型**：它需要的那一点点外部世界，以及 task 生命周期事件的本仓形态。
//!
//! 本文件是 `typing.rs` 的子模块：拆分依据是 `docs/60-M7-PLAN.md` §6.3 的强制拆分加上门 ⑩ 的
//! 800 行硬限（逐条清单见 `docs/32` §38 的 D9）。
//!
//! # 为什么"事件"是一个**值**而不是一根总线
//!
//! 上游 `Register(bus *events.Bus)` 订阅三个事件（`task:queued` / `task:failed` / `task:cancelled`），
//! 而**本仓没有进程内事件总线**（M7-17 的 §34.4 H1 逐字：`Outbound::handle_chat_done` /
//! `handle_inbox_new` 是**显式调用**入口）。所以这里落成一个**值** [`TaskEvent`]，而宿主按同一批
//! 字段显式调用 [`super::TypingIndicator::handle_task_queued`] 那三个方法。
//!
//! 逐条对应（上游从 `events.Event` 的信封 + `map[string]any` payload 读的那几个键）：

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::id::Id;

use super::super::outbound::{AgentTask, OutboundQueries};
use super::super::stream_store::RootResolver;
use super::super::strings::{copy_for, locale_for_destination, Locale};
use super::super::ws_frame::CHAT_TYPE_SINGLE_INT;
use crate::engine::commands::task_input_is_channel_ingested;

// =====================================================================
// 事件
// =====================================================================

/// 一条 task 生命周期事件的本仓形态（上游 `events.Event` 的信封 + payload 的那几个键）。
///
/// 三个消费者各读各的子集，但它们**共用**一个值：同一个 payload 形状在三个入口上被解三遍正是
/// 上游那条"两个发布方都要把同一个事实盖上两次"的来处。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskEvent {
    /// 这个 run 的 task id（上游信封的 `TaskID`，payload 的 `task_id` 是它的第二个来源）。
    ///
    /// 上游 `taskIDFromEvent` 先看信封再看 payload —— 本仓一个字段，那两条路合成一条。
    pub task_id: String,
    /// 这个 run 属于哪个 chat session（信封的 `ChatSessionID`，payload 的 `chat_session_id`）。
    pub chat_session_id: Option<String>,
    /// 这个 run 是不是**从 issue 起**的（`/issue`、autopilot、web UI 的重跑）。
    ///
    /// 上游逐字：一个 chat run 的 `issue_id` 是 **NULL**（`CreateChatTask` 就是这么写的），所以
    /// 事件上"`issue_id` 非空"说的就是"这不是一次会话里的轮次"。
    pub issue_id: Option<String>,
    /// 平台**已经**在重试这次尝试了（上游 `retry_pending`）。
    pub retry_pending: bool,
    /// 平台给出的脱敏原因（上游 payload 的 `error`）。
    pub error: Option<String>,
}

impl TaskEvent {
    /// 一条 `task:queued`。
    #[must_use]
    pub fn queued(task_id: impl Into<String>, chat_session_id: Option<impl Into<String>>) -> Self {
        Self {
            task_id: task_id.into(),
            chat_session_id: chat_session_id.map(Into::into),
            ..Self::default()
        }
    }

    /// 一条 `task:failed`。
    #[must_use]
    pub fn failed(
        task_id: impl Into<String>,
        chat_session_id: Option<impl Into<String>>,
        error: Option<impl Into<String>>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            chat_session_id: chat_session_id.map(Into::into),
            issue_id: None,
            retry_pending: false,
            error: error.map(Into::into),
        }
    }

    /// 一条 `task:cancelled`。
    #[must_use]
    pub fn cancelled(
        task_id: impl Into<String>,
        chat_session_id: Option<impl Into<String>>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            chat_session_id: chat_session_id.map(Into::into),
            ..Self::default()
        }
    }

    /// 把它标成"平台已经在重试这次尝试了"（上游 `FailTask` 给那个 payload 盖的章）。
    #[must_use]
    pub fn with_retry_pending(mut self, pending: bool) -> Self {
        self.retry_pending = pending;
        self
    }

    /// 把它标成一条 issue / autopilot 的 run（不是会话里的轮次）。
    #[must_use]
    pub fn with_issue(mut self, issue_id: impl Into<String>) -> Self {
        self.issue_id = Some(issue_id.into());
        self
    }

    /// 信封上那个 task id（空 = 没有名字）。
    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// 这次 run 该在哪一轮上收尾 —— 信封优先，缺席就是缺席（上游 `sessionIDFromEvent` 的第二个
    /// 来源在 payload 上，而本仓两者已经是**一个**字段）。
    #[must_use]
    pub fn session_id(&self) -> Option<Id> {
        self.chat_session_id
            .as_deref()
            .and_then(|raw| Id::parse(raw).ok())
    }

    /// 上游 `retryPending`：`FailTask` 已经为这次尝试造了重试子任务吗。
    #[must_use]
    pub fn retry_pending(&self) -> bool {
        self.retry_pending
    }

    /// 上游"是不是一次会话里的轮次"的判据：`issue_id` 为空。
    #[must_use]
    pub fn is_chat_turn(&self) -> bool {
        self.issue_id.as_deref().is_none_or(str::is_empty)
    }
}

/// 平台那条失败告知的前缀（上游 `taskFailedPrefix`，**逐字**）。
///
/// 上游逐字：它把一条失败告知与一条回答在聊天里**分开**。
pub const TASK_FAILED_PREFIX: &str = "⚠️ ";

/// 上游 `taskFailedContent`：这个 payload 要不要说点什么，说的是什么。
///
/// - `retry_pending` ⇒ **空**：一次平台已经在重试的尝试不是一个结局（上游逐字：
///   `taskFailedFields` 连错误文本都扣住了）。
/// - 没有 `error` / 它只是空白 ⇒ 空（调用方退到文案包里的 `stream_failed`）。
/// - 其余 ⇒ `⚠️ ` + 平台给的那段脱敏文本。
#[must_use]
pub fn task_failed_content(event: &TaskEvent) -> String {
    if event.retry_pending() {
        return String::new();
    }
    let Some(message) = event.error.as_deref() else {
        return String::new();
    };
    if message.trim().is_empty() {
        return String::new();
    }
    format!("{TASK_FAILED_PREFIX}{message}")
}

/// 一次失败的 run 在聊天里说什么（上游 `failureText`）。
///
/// 平台自己给的那段脱敏原因优先 —— 那是 web 转录里显示的那段，也是"上下文超出模型限制"这类
/// 能告诉一个人下一步做什么的话；文案包的 `stream_failed` 是没有原因时的兜底。
#[must_use]
pub fn failure_text(event: &TaskEvent, locale: Locale) -> String {
    let reason = task_failed_content(event);
    if reason.is_empty() {
        return copy_for(locale).stream_failed.to_string();
    }
    reason
}

// =====================================================================
// 端口
// =====================================================================

/// 打字指示要的**两**条读（上游 `taskLookup` + `engine.ChannelProvenanceQueries`）。
///
/// 与 [`super::super::outbound::ports::OutboundQueries`] 的关系：那一个面更宽（投递行、安装状态、
/// 成员绑定、slug），而本片只要这两条 ⇒ 落成一条窄 trait，于是本模块的用例不必造一个宽替身。
#[async_trait]
pub trait TaskQueries: Send + Sync {
    /// 这个 task 那一行（上游 `taskLookup.GetAgentTask`）。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn get_agent_task(&self, task_id: Id) -> Result<Option<AgentTask>, String>;

    /// 入站批次里是否有渠道递进来的消息（上游 `TaskHasChannelIngestedMessages`）。
    ///
    /// # Errors
    ///
    /// 读库失败。
    async fn task_has_channel_ingested_messages(&self, task_id: Id) -> Result<bool, String>;
}

/// 上游的两个接口由同一个 `*db.Queries` 满足 ⇒ 本仓同款：一个 `Arc<dyn OutboundQueries>` 就是
/// 一个 [`TaskQueries`]（它就是 M7-17 那条转发 impl 的**同一个**对象）。
#[async_trait]
impl TaskQueries for Arc<dyn OutboundQueries> {
    async fn get_agent_task(&self, task_id: Id) -> Result<Option<AgentTask>, String> {
        OutboundQueries::get_agent_task(self.as_ref(), task_id).await
    }

    async fn task_has_channel_ingested_messages(&self, task_id: Id) -> Result<bool, String> {
        OutboundQueries::task_has_channel_ingested_messages(self.as_ref(), task_id).await
    }
}

/// 把一次收尾的 task id 解回它所属那一轮的 **root** task id（上游 `roundTaker` 的
/// `taskLookup` → `ChatInputTaskID`）。
///
/// 这是 M7-17 的 §34.4 **H4** 里"同片还要实现 `RootResolver`"那一条的落点：上游那个查询就是
/// `GetAgentTask(id).ChatInputTaskID`，而本仓的 [`TaskQueries`] 已经把它投影出来了
/// （[`AgentTask::chat_input_task_id`]）。
pub struct TaskLookupRoots {
    tasks: Arc<dyn TaskQueries>,
}

impl TaskLookupRoots {
    /// 用一个 task 面造。
    #[must_use]
    pub fn new(tasks: Arc<dyn TaskQueries>) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl RootResolver for TaskLookupRoots {
    /// 一个 task 所属的输入批次。
    ///
    /// 上游逐字：`chat_input_task_id` 是一个**自动重试 clone 继承它父亲那一个**的列 ⇒ 这条查询
    /// 把 clone 解回拥有它输入批次的**那一轮**。查不着（没有行、id 解析不了、读库失败）就是
    /// `None` —— 上游只记一条 debug 日志，因为那正是"这一轮不属于一个被释放的尝试"的正常形态。
    async fn root_task_id(&self, task_id: &str) -> Option<String> {
        let id = Id::parse(task_id).ok()?;
        match self.tasks.get_agent_task(id).await {
            Ok(Some(task)) => task.chat_input_task_id.map(|root| root.to_string()),
            Ok(None) | Err(_) => None,
        }
    }
}

/// 一次 run 的输入来自哪里（上游 `originOf` 的三格）。
///
/// 三格各要不同的动作：`NotOurs` **释放**那一轮（绑着它的 run 是别人的，永远不会来收尾）；
/// `Unknown` 什么都不说、也**不**释放（一个够不着的数据库不是"这个 run 属于别处"的证据，
/// 而在一次失败的读上把轮次交出去会丢掉一个这个聊自己的回答还在路上的气泡）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginVerdict {
    /// 查不出来（上游 `originUnknown`）。
    Unknown,
    /// 不是本 adapter 的（上游 `originNotOurs`）。
    NotOurs,
    /// 是本 adapter 的（上游 `originOurs`）。
    Ours,
}

/// 一条投递行读出来的东西（上游 `taskRouting` 的三格 —— 上游逐字：**它有三个答案而不是两个**，
/// 把前两个压成一个正是让 origin 门够不着的原因）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRouting {
    /// 这次 run 什么都没有归档。它**可能**是一个拿着这个房间轮次的第一方 run，而那正是 origin 门
    /// 存在的原因 ⇒ 这是唯一一个值得再花读的答案。
    NoRow,
    /// 有一行，但它不是一个活的 `WeCom` 地址（别的平台的 run，或者我们的在一次被撤销的安装上）。
    /// 两种情况下本订阅者都什么都不说，而且它**不**花掉 task 行那次读。
    Silent,
    /// 一个活的 `WeCom` 地址。**这一行本身就是来源证明** —— 一个在 Multica 里打的 run 永远不会有
    /// 它 ⇒ 这个答案跳过 origin 门。
    Ours,
}

/// 一个目的地用哪种语言收尾（上游 `languageLookup` + `localeFor`）。
///
/// 上游按**目的地**定语言：1:1 读提问者自己的 Multica 档案，群聊读**部署**的。本片给出这条端口
/// 与它的兜底实现 [`DeploymentLanguage`]；真正的档案回查要 `channel_user_binding` + `user` 两条
/// 读，而那是**宿主**的接线（见 `docs/32` §38 的 D8 与交接 H2）。
pub trait LanguageLookup: Send + Sync {
    /// 一条气泡该用哪种语言收尾。
    fn locale_for(&self, installation_id: Id, chat_type: i32, sender_id: &str) -> Locale;
}

/// 兜底：**一律**用部署语言。
///
/// 它对群聊是**等价**的（群聊本来就没有共享档案）；对 1:1 是**降级**（一个把档案设成英文的读者
/// 会拿到部署语言的文案）。降级而不是错误：一个语言不对的气泡仍然说了一件有用的事。
#[derive(Debug, Clone, Copy, Default)]
pub struct DeploymentLanguage;

impl LanguageLookup for DeploymentLanguage {
    fn locale_for(&self, _installation_id: Id, chat_type: i32, _sender_id: &str) -> Locale {
        // 群聊 ⇒ 部署语言；1:1 但没有档案读数 ⇒ 也是部署语言（"缺席不是选择"，
        // `strings.rs::locale_for_destination` 逐字）。
        locale_for_destination(chat_type == CHAT_TYPE_SINGLE_INT, None)
    }
}

/// 一次收尾的 task id 能不能解成一个 id（上游 `util.ParseUUID` 的那一半）。
#[must_use]
pub fn parse_task_id(task_id: &str) -> Option<Id> {
    if task_id.is_empty() {
        return None;
    }
    Id::parse(task_id).ok()
}

/// 上游 `originOf` 那一次判定：这次 run 的输入来自渠道吗。
///
/// `engine::task_input_is_channel_ingested` 是**它自己的**判据（M7-2 的纯函数：`chat_input_task_id`
/// 为 NULL ⇒ 默认投递，否则查那个批次所有者的 `channel_ingested` 章）⇒ 本函数只负责把那两步读
/// 拼起来、并把三种失败各记一条 WARN。
pub async fn origin_of(
    tasks: Option<&dyn TaskQueries>,
    session_id: Option<Id>,
    task_id: &str,
) -> OriginVerdict {
    if task_id.is_empty() {
        // 两个 task:failed 的发布方在生产里都会带一个 ⇒ 这是**没有任何真实东西**会产出的 payload
        // 形态，而它命名不了一个可归因的 run。
        refuse_unknown_origin(session_id, task_id, "no task id on the event");
        return OriginVerdict::Unknown;
    }
    let Some(tasks) = tasks else {
        refuse_unknown_origin(session_id, task_id, "no task lookup configured");
        return OriginVerdict::Unknown;
    };
    let Some(id) = parse_task_id(task_id) else {
        refuse_unknown_origin(session_id, task_id, "unparseable task id");
        return OriginVerdict::Unknown;
    };
    let task = match tasks.get_agent_task(id).await {
        Ok(Some(task)) => task,
        Ok(None) => {
            refuse_unknown_origin(session_id, task_id, "no task row");
            return OriginVerdict::Unknown;
        }
        Err(error) => {
            refuse_unknown_origin(
                session_id,
                task_id,
                &format!("cannot read the task row: {error}"),
            );
            return OriginVerdict::Unknown;
        }
    };
    let ingested = if task.chat_input_task_id.is_none() {
        // 判据自己那条短路（上游 `TaskInputIsChannelIngested` 的第一个分支：一个 NULL 所有者是
        // "封口之前的渠道任务"⇒ 默认投递）⇒ **不花**第二次读。
        false
    } else {
        match tasks.task_has_channel_ingested_messages(id).await {
            Ok(ingested) => ingested,
            Err(error) => {
                refuse_unknown_origin(
                    session_id,
                    task_id,
                    &format!("cannot read the channel-ingested stamp: {error}"),
                );
                return OriginVerdict::Unknown;
            }
        }
    };
    if task_input_is_channel_ingested(task.chat_input_task_id, ingested) {
        OriginVerdict::Ours
    } else {
        OriginVerdict::NotOurs
    }
}

/// 记一条"本进程拒绝把一次失败告知放进某个 `WeCom` 房间"。
///
/// WARN 而不是 debug：它对提问者是一个**真实**的结局 —— 他们的一次运行结束了，而他们没被告知 ——
/// 而且它是"origin 门依赖的那个数据库不再应答"的**唯一**信号。
pub fn refuse_unknown_origin(session_id: Option<Id>, task_id: &str, reason: &str) {
    tracing::warn!(
        chat_session_id = %session_id.map_or_else(|| "-".to_string(), |id| id.to_string()),
        task_id,
        reason,
        "wecom typing: refusing to announce a failed run whose origin cannot be established"
    );
}
