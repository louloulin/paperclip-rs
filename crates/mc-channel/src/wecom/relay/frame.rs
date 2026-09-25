//! `relay` 的**帧与事件 id**（上游 `relayFrame` / 三个 `relayKind` / `relayEventID` /
//! `relayInboxEventID` / `dedupeKey`）。
//!
//! 本文件是 `relay.rs` 的子模块：拆分依据是门 ⑩ 的 800 行硬限（逐条清单见 `docs/32` §34 的 D12）。

use mc_core::id::Id;

// =====================================================================
// 帧
// =====================================================================

/// `relayFrame` 的 `kind` 那一格（上游三个字符串常量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayKind {
    /// 一条回答（上游 `relayKindReply`）。
    #[default]
    Reply,
    /// 一条收件箱推送（上游 `relayKindInbox`）。
    Inbox,
    /// **不带话地结束一轮**（上游 `relayKindSeal`），见 [`RelayFrame::seal`]。
    Seal,
}

impl RelayKind {
    /// wire / metric 标签用的字面量（与帧上的 `kind` 同字）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Inbox => "inbox",
            Self::Seal => "seal",
        }
    }
}

/// 一个在两个副本之间**在飞**的投递（上游 `relayFrame`）。
///
/// 它驮的是**标识**而不是渲染好的载荷，凡是租约持有者自己读得到的东西都按 id 传 ——
/// 于是附件由**要发它的那个副本**去取，而不是走中继。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RelayFrame {
    pub kind: RelayKind,
    #[serde(default)]
    pub installation_id: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub chat_type: i32,
    #[serde(default)]
    pub content: String,
    /// [`RelayKind::Seal`] 帧驮的是"哪一种结束"的**名字**，而不是它的话。
    ///
    /// 话是**那一轮的**，而那一轮在持有者身上：它的 locale 在气泡被画出来时就捕获了，
    /// 所以只有持有者能用提问者读的语言说出那句话。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub seal_reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session_id: String,
    /// 握着 socket 的那个副本也发这一轮的文件（见 `relay/relayed.rs`）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub carries_files: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde 的 `skip_serializing_if` 只收 `fn(&T) -> bool`
fn is_false(value: &bool) -> bool {
    !*value
}

impl RelayFrame {
    /// 一条回答帧。
    #[must_use]
    #[allow(clippy::too_many_arguments)] // 上游逐字：这八个字段就是帧的形状
    pub fn reply(
        installation_id: String,
        chat_id: String,
        chat_type: i32,
        content: &str,
        task_id: &str,
        message_id: &str,
        workspace_id: &str,
        session_id: &str,
    ) -> Self {
        Self {
            kind: RelayKind::Reply,
            installation_id,
            chat_id,
            chat_type,
            content: content.to_string(),
            task_id: task_id.to_string(),
            message_id: message_id.to_string(),
            workspace_id: workspace_id.to_string(),
            session_id: session_id.to_string(),
            ..Self::default()
        }
    }

    /// 一条收件箱推送帧。
    #[must_use]
    pub fn inbox(
        installation_id: String,
        chat_id: String,
        chat_type: i32,
        content: String,
    ) -> Self {
        Self {
            kind: RelayKind::Inbox,
            installation_id,
            chat_id,
            chat_type,
            content,
            ..Self::default()
        }
    }

    /// 一帧"结束这一轮，但不带话"。
    ///
    /// 它结束一轮而**不带话** —— 那正是回答帧表达不了的那件事：每一个无话的结束（一次取消、
    /// 一次没什么可说的完成、一条只有文件的回答）过去都没有途径到达握着气泡的副本，
    /// 于是离了租约它就让一个转圈在整个协议窗口的剩余时间里宣称工作还在进行。
    /// 别的东西都不会结束它：扫描不写帧，`on_settled` 需要一个未绑定的轮次，
    /// 而下一个问题开它自己的气泡。
    ///
    /// 它**不是**一条正文为空的回答，而这个区别就是重点：一条轮次已经不在的回答会落到普通推送，
    /// 于是一次"全部取消"会让那句**这次处理已取消**出现在部署里每一个聊里；
    /// 而一个没有对应轮次的封印帧**什么都不做**。
    #[must_use]
    pub fn seal(reason: &str, task_id: &str, session_id: &str, carries_files: bool) -> Self {
        Self {
            kind: RelayKind::Seal,
            seal_reason: reason.to_string(),
            task_id: task_id.to_string(),
            session_id: session_id.to_string(),
            carries_files,
            ..Self::default()
        }
    }

    /// 编码成线上字节（上游 `json.Marshal`）。
    ///
    /// # Errors
    ///
    /// 序列化失败（不可能：全是字符串与整数）。
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|error| error.to_string())
    }

    /// 从线上字节解出来（上游 `json.Unmarshal`）。
    ///
    /// # Errors
    ///
    /// 字节不是一帧。
    pub fn decode(raw: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(raw).map_err(|error| error.to_string())
    }
}

// =====================================================================
// 事件的 id（幂等的键）
// =====================================================================

/// 上游 `relayEventID`：每一条 claim 的键。
///
/// 从**那一轮**派生而不是铸一个，于是一次重发布（发布的重试、重放的流条目、第二个订阅者）
/// 是同一条 claim，不会变成聊天里的第二条消息。
#[must_use]
pub fn relay_event_id(event_type: &str, task_id: Id) -> String {
    format!("wecom:{event_type}:{}", task_id.0)
}

/// 上游 `relayInboxEventID`：收件箱推送的同一条规则（它没有 task）。
#[must_use]
pub fn relay_inbox_event_id(item_id: &str, recipient_id: &str) -> String {
    format!("wecom:inbox:{item_id}:{recipient_id}")
}

/// 上游 `dedupeKey`：claim 在存储里的键。
#[must_use]
pub fn dedupe_key(event_id: &str) -> String {
    format!("wecom:outbound:claim:{event_id}")
}
