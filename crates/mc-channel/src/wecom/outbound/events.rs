//! `outbound` 的**事件与预算**：宿主投影进来的 `chat:done` / `inbox:new` 两个信封、
//! 收件箱卡片的渲染端口，以及出站路径上的那份预算。
//!
//! 本文件是 `outbound.rs` 的子模块：拆分依据是门 ⑩ 的 800 行硬限（逐条清单见 `docs/32` §34 的 D12）。

use std::time::{Duration, Instant};

use mc_core::id::Id;

use mc_core::channel::message::ChatType;

use crate::wecom::ws_frame::aibot_chat_type_from_channel;
use crate::wecom::ws_sender::Deadline;

use super::{Outbound, INBOX_BUDGET};

// =====================================================================
// 预算（上游的 `context.Context` 那一半）
// =====================================================================

/// 一次投递的**截止时刻**（上游在出站路径上用 `ctx` 表达的那一件事）。
///
/// `WeCom` 的发送侧已经把调用方的预算表示成 [`Deadline`]（`Option<Instant>`，见
/// `ws_sender.rs` 的模块文档）⇒ 本结构是它的**有界版本**：出站路径上的每一次投递都从
/// 一个明确的、会到点的预算开始。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryBudget {
    deadline: Instant,
}

impl DeliveryBudget {
    /// 从现在起 `budget` 之内的预算。
    #[must_use]
    pub fn lasting(budget: Duration) -> Self {
        Self {
            deadline: Instant::now() + budget,
        }
    }

    /// 一个**指定**截止时刻的预算（用例把"已经花掉大半"这件事直接写出来，不睡真觉）。
    #[must_use]
    pub fn at(deadline: Instant) -> Self {
        Self { deadline }
    }

    /// 还剩多少。
    #[must_use]
    pub fn remaining(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }

    /// 到点了吗。
    #[must_use]
    pub fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// 转成发送侧要的那个形态。
    #[must_use]
    pub fn as_deadline(self) -> Deadline {
        Some(self.deadline)
    }

    /// 上游 `fallbackBudget`（**判据在 [`crate::wecom::seal::fallback_budget`]**，见交接 H2 的
    /// 收敛）：给普通消息一份**气泡不可能已经花掉**的预算。这里只做转发，不做判断。
    #[must_use]
    pub fn fallback(self, now: Instant) -> Self {
        crate::wecom::seal::fallback_budget(self, now)
    }
}

// =====================================================================
// 收件箱面（上游 `handleInboxNew` / `buildInboxMarkdown`）
// =====================================================================

/// 一条 `inbox:new` 的归一化投影（上游 `events.Event` 的 payload 那两层的 map 形态）。
///
/// # `title` / `body` 是 M7-19 补进来的（端口形状勘误，`docs/32` §37 的 D2）
///
/// M7-17 的这一份投影只抽了路由需要的字段，而**渲染**要的那张卡（上游 `buildInboxMarkdown`）
/// 是「标题 + 正文 + 深链」 —— 少了这两个字段，卡片会缺标题与正文，那是**降级**而不是等价。
/// 本片（第一个真正需要渲染的片）把形状补齐，于是 `InboxCardRenderer` 交出的是等价实现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxPush {
    /// 通知自己的 id。**投递幂等就靠它**：两个副本读到同一条重放的帧时，
    /// 它们的 claim 落在**同一条通知**上，而不是各自读到它的那一刻。
    pub item_id: String,
    pub item_type: String,
    pub issue_id: String,
    pub recipient_type: String,
    pub recipient_id: String,
    pub workspace_id: String,
    /// 通知标题（**成员写的**文本；卡片会把它过 [`super::super::markdown::break_member_links`]）。
    pub title: String,
    /// 通知正文（同上）。缺省空串。
    pub body: String,
}

impl InboxPush {
    /// 上游 `handleInboxNew` 的两层 map 投影（`payload["item"]`）。
    ///
    /// `None` = 这条 payload 没有投递所需的形状（缺 `item`、或收件人不是成员）。
    #[must_use]
    pub fn from_payload(payload: &serde_json::Value) -> Option<Self> {
        let item = payload.get("item")?.as_object()?;
        let text = |key: &str| {
            item.get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Some(Self {
            item_id: item_id_of(item),
            item_type: text("type"),
            issue_id: text("issue_id"),
            recipient_type: text("recipient_type"),
            recipient_id: text("recipient_id"),
            workspace_id: text("workspace_id"),
            // 渲染要的那两个字段（M7-19 的 D2）：缺失时是空串，卡片照发但少一段 —— 与
            // 上游 `item["title"].(string)` 取不到值时同义。
            title: text("title"),
            body: text("body"),
        })
    }

    /// 只有**成员**收件人走机器人（agent 不经聊天渠道收任何东西）。上游逐字。
    #[must_use]
    pub fn is_member_recipient(&self) -> bool {
        self.recipient_type == "member"
    }
}

/// 上游 `itemIDOf`：收件箱条目自己的 id。
///
/// 老 payload 没有 id ⇒ 回落到"类型 + 它属于的那个 issue"：比 id 弱，但对**一条**通知仍然
/// 稳定，而 claim 要的就是稳定。
fn item_id_of(item: &serde_json::Map<String, serde_json::Value>) -> String {
    if let Some(id) = item.get("id").and_then(serde_json::Value::as_str) {
        if !id.is_empty() {
            return id.to_string();
        }
    }
    let text = |key: &str| {
        item.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    };
    format!("{}:{}", text("type"), text("issue_id"))
}

/// 把一条收件箱通知渲染成机器人要发的那张卡（上游 `buildInboxMarkdown`，
/// 落点 = **M7-19** 的 `inbox_message.rs`）。
///
/// `slug` 是 Web 深链的 workspace 片段（best-effort；缺它就退到 URL 里的 uuid）。
pub trait InboxRenderer: Send + Sync {
    /// 渲染这条通知；`None` = 这条通知没有可发送的正文 ⇒ 不投递（失败关闭）。
    fn render(&self, push: &InboxPush, workspace_slug: &str) -> Option<String>;
}

// =====================================================================
// 事件（上游 `events.Event` 的聊天面投影）
// =====================================================================

/// 一条 `chat:done` 的归一化投影（上游 `events.Event` + `protocol.ChatDonePayload`）。
///
/// 本仓没有进程内事件总线 ⇒ 宿主把总线上的事情投影成这个值再调
/// [`Outbound::handle_chat_done`]（D1）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatDone {
    /// 这次完成属于哪个 `chat_session`（空 = issue / autopilot 的任务，本订阅者不管）。
    pub chat_session_id: String,
    /// 信封上的 task id。上游 `service.broadcastChatDone` 填的是 payload 那一份，
    /// 信封常常是空的 —— 所以两个都要看，见 [`ChatDone::task_id`]。
    pub envelope_task_id: String,
    /// payload 上的 task id。
    pub payload_task_id: String,
    /// 回答正文（上游 `chatDoneContent` 的 `Content`）。
    pub content: String,
    /// 产出这条回答的 assistant 消息 id（附件按它绑定）。
    pub message_id: String,
    pub workspace_id: String,
    /// 上游 `e.Type`（`chat:done`）：只用于事件 id 与日志。
    pub event_type: String,
}

impl ChatDone {
    /// 上游 `taskIDFromEvent`：**信封优先**，然后 payload。
    ///
    /// 规则只有一处（origin 门与气泡 take 必须谈**同一个** run：两条会打架的规则会让门清掉
    /// task A、而 take 消费掉绑在 task B 上的轮次）。
    #[must_use]
    pub fn task_id(&self) -> &str {
        if self.envelope_task_id.is_empty() {
            &self.payload_task_id
        } else {
            &self.envelope_task_id
        }
    }

    /// 上游 `chatDoneTaskID`：能把 task id 解成 `Id` 吗。
    #[must_use]
    pub fn parsed_task_id(&self) -> Option<Id> {
        parse_uuid(self.task_id())
    }

    /// 上游 `chatDoneContent`：正文。
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }
}

/// 解一个 uuid 字符串（上游 `util.ParseUUID` + `Valid`）。
///
/// 本仓的 `Id` 是 `Uuid` 的 newtype ⇒ 这里的 `None` 覆盖上游的"解析失败"与"零值"两种。
#[must_use]
pub fn parse_uuid(raw: &str) -> Option<Id> {
    uuid::Uuid::parse_str(raw.trim())
        .ok()
        .map(Id)
        .filter(|id| !id.0.is_nil())
}

/// 上游 `util.UUIDToString`：只用于日志与帧字段。
#[must_use]
pub fn uuid_string(id: Id) -> String {
    id.0.to_string()
}

// =====================================================================
// 两个投递入口（`impl Outbound`）
// =====================================================================

impl Outbound {
    /// 上游 `handleInboxNew`：一条成员通知经智能机器人推送。
    ///
    /// 任何一次错过（收件人不是成员、没有 `WeCom` 绑定、没有活发送者、发送失败）都是 no-op，
    /// 成员照常从应用内收件箱拿到通知。
    pub async fn handle_inbox_new(&self, push: &InboxPush) -> bool {
        if !push.is_member_recipient() {
            return false;
        }
        if push.recipient_id.is_empty() || push.workspace_id.is_empty() {
            return false;
        }
        let budget = DeliveryBudget::lasting(INBOX_BUDGET);
        self.try_deliver_inbox(budget, push).await
    }

    /// 上游 `tryDeliverInbox`：投递核心。返回 `true` **当且仅当**机器人推了这条通知。
    pub async fn try_deliver_inbox(&self, budget: DeliveryBudget, push: &InboxPush) -> bool {
        // 没有渲染器 ⇒ 这条推送不投递（D7）：上游的 `buildInboxMarkdown` 是 M7-19 的产物，
        // 而本片**不许**伪造一张卡片。放在最前面：一次没有渲染器的部署不该花任何一次读。
        let Some(renderer) = self.inbox.as_ref() else {
            return false;
        };
        let (Some(recipient_id), Some(workspace_id)) = (
            parse_uuid(&push.recipient_id),
            parse_uuid(&push.workspace_id),
        ) else {
            return false;
        };
        let binding = match self
            .q
            .find_binding_for_member(workspace_id, recipient_id)
            .await
        {
            Ok(Some(binding)) => binding,
            Ok(None) => return false, // 没有绑定 ⇒ 经机器人无可投递
            Err(message) => {
                tracing::warn!(
                    error = message.as_str(),
                    workspace_id = push.workspace_id.as_str(),
                    recipient_id = push.recipient_id.as_str(),
                    "wecom outbound: lookup member binding failed"
                );
                return false;
            }
        };
        let Some(senders) = self.senders.as_ref() else {
            return false;
        };
        let sender = senders.get(binding.installation_id);
        // slug 是 best-effort：查不到就用 URL 里的 workspace uuid。
        let slug = self
            .q
            .workspace_slug(workspace_id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let Some(content) = renderer.render(push, &slug) else {
            return false;
        };
        let single = aibot_chat_type_from_channel(ChatType::P2p);
        // 机器人推送的收件箱通知是**单聊**推送（`chat_type=1`）：绑定行的 `channel_user_id`
        // 就是机器人域内的 T-* userid，`WeCom` 把它当成一条 `chat_type=1` 发送的 chatid。
        if let Some(sender) = sender {
            if sender
                .send_text(
                    &binding.channel_user_id,
                    single,
                    &content,
                    budget.as_deadline(),
                )
                .await
                .is_err()
            {
                tracing::warn!(
                    installation_id = %uuid_string(binding.installation_id),
                    recipient_id = push.recipient_id.as_str(),
                    "wecom outbound: inbox push failed"
                );
                return false;
            }
            return true;
        }
        // 本副本没有 socket。与回复路径同形：交给握着 socket 的那个副本。
        // 一条收件箱推送与一条回答一样是用户可见的，把它留在本地正是"即使回复可路由、
        // 单副本约束也不得不留下"的来处。
        if self.route_frame(
            &crate::wecom::relay::RelayFrame::inbox(
                uuid_string(binding.installation_id),
                binding.channel_user_id.clone(),
                single,
                content,
            ),
            &crate::wecom::relay::relay_inbox_event_id(&push.item_id, &push.recipient_id),
        ) {
            tracing::debug!(
                installation_id = %uuid_string(binding.installation_id),
                "wecom outbound: routed an inbox push to the replica holding the socket"
            );
            return true;
        }
        // 记日志，**不**进回复计数器：它们的单位是**agent 回复**，而一条收件箱通知记进去会
        // 表现为"这个 adapter 欠某人一条回复而且没送到" —— 那正是中继那条收件箱路径避免的
        // 单位错误，也是"送达/丢弃"比可以被读成一个结局的来路。成员仍然从应用内收件箱拿到它，
        // 这正是"少一次机器人推送是降级而不是丢失"的来由。
        tracing::warn!(
            installation_id = %uuid_string(binding.installation_id),
            recipient_id = push.recipient_id.as_str(),
            "wecom outbound: inbox push not delivered and not routable"
        );
        false
    }
}
