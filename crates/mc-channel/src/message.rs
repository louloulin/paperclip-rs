//! 入站入口 + 消息面的**纯函数与词表**：`InboundHandler`（上游 `channel/handler.go`）
//! 与历史读取 / 引用渲染 / 深链 / 成员文本四个叶子文件（上游 `channel/{history,quoted_message,
//! issue_link,member_text}.go`，共 156 行）。
//!
//! - **写者**：M7-0 建（`InboundHandler` 位）；**M7-1 填充**（`docs/60` §3.3）。
//! - 消息**信封**（`InboundMessage` / `OutboundMessage` / `MediaRef`…）在
//!   `mc_core::channel::message`（M7-0 落、各片只读）；本文件放的是**跨平台纯函数**与
//!   **按需历史读取**的词表 —— 它们不属于任何平台，也都不碰 DB / 网络。
//!
//! # 上游契约（逐字，别简化）
//!
//! 上游是 `func(ctx, InboundMessage) error`，由 engine **单点注入**给每个 adapter
//! （`Config.Handler`）：engine 的入站处理只写一遍，所有平台汇进它。adapter 拥有自己的
//! 接收循环并调用它；核心**从不**轮询 Channel。两条判据：
//!
//! 1. **非 nil error = 基础设施失败**（DB 挂了、dispatcher 配错…）。adapter 应当把它当
//!    "投递失败"上报，让 supervisor 的退避/重连接管。**不得**用于产品性结果。
//! 2. **nil = 消息已被接受并分类**。它仍可能因**正当的产品理由**被丢弃（dedup 命中、
//!    发件人未绑定、群过滤）—— 那**不是**错误。判决带来的任何出站回复（绑定卡 / 离线提示 /
//!    打字指示）是 handler 自己的责任，**脱离 adapter 的 ACK 路径**。
//!
//! `fire-and-classify`：除了 error 没有别的返回值 —— adapter 不因结果分支。
//!
//! # Rust 形态（本仓的等价物，登记 `docs/32` §10）
//!
//! 上游是函数值；本仓用**对象安全的 trait**（`Arc<dyn InboundHandler>`）：生命周期在 Rust 里
//! 必须显式，`Arc<dyn …>` 是最直白的表达，也让"同一个 handler 注入给 5 个 adapter"变成一次
//! `Arc::clone`。**取消语义**：本 crate 不引 `tokio-util`（依赖面一次定死）⇒ 取消由持有
//! `connect` 的任务被取消表达（见 `engine::supervisor`），handler 只处理"已经收到的"消息。

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;

use crate::channel::ChannelResult;

/// 每个 adapter 都会调用的**共享、平台无关**入站入口（见模块文档的契约）。
///
/// 判决语义：`Ok(())` = 已接受并分类（**包含**"按产品理由丢弃"）；`Err(_)` = 基础设施失败。
#[async_trait]
pub trait InboundHandler: Send + Sync {
    /// 处理一条归一化入站消息。
    async fn handle(&self, message: InboundMessage) -> ChannelResult<()>;
}

/// 注入进 [`crate::channel::ChannelConfig::handler`] 的共享句柄。
///
/// `Arc` 而不是 `Box`：同一个 handler 注入给 5 个平台的 adapter，且 engine 自己也要留一份
/// 引用（出站订阅 / 握手路径）。
pub type SharedInboundHandler = Arc<dyn InboundHandler>;

// =====================================================================
// 历史读取（上游 `channel/history.go`）
// =====================================================================

/// 取回消息的归一化作者类别，对齐 agent 已经熟悉的 `chat_message.role` 域。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRole {
    /// 人（或第三方 bot，例如告警 bot）的消息 —— agent 应当读的上下文。
    #[default]
    User,
    /// 本 bot 自己在会话里的历史消息。
    Assistant,
}

impl HistoryRole {
    /// wire 取值（与上游 `HistoryRole` 逐字一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    /// 解回枚举；未知取值 ⇒ `None`（别把坏数据默认成 `User`）。
    pub fn from_str_opt(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            _ => None,
        }
    }
}

/// 一条归一化的历史消息（与平台无关；agent 读到的是一份均匀的列表）。
///
/// 只在"channel 概览里作为线程头的行"上带 `thread_id` / `reply_count` / `latest_reply`。
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct HistoryMessage {
    /// 平台消息 id（Slack `ts`、Feishu `message_id`）。
    pub id: String,
    /// 发送者的可读标签（"Alice" / "Bot"，解析不出时的位置兜底 "User 2"）。
    pub author: String,
    /// 平台原生发送者 id，可能为空。
    pub author_id: String,
    pub role: HistoryRole,
    /// 被 adapter 摊平成纯文本的正文。
    pub text: String,
    /// 平台时间戳字符串（同一平台内可字典序排序；同时充当分页游标）。
    pub ts: String,
    /// 传给 `chat thread <id>` 读这个线程的标识；仅概览行上的线程头有。
    pub thread_id: String,
    /// 线程回复数（没有则 0）。
    pub reply_count: i32,
    /// 最近一条回复的平台时间戳（知道才有）。
    pub latest_reply: String,
}

/// 一页归一化历史：消息**由旧到新**排序，读起来像会话本身。
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct HistoryPage {
    /// 历史来自哪个平台（`slack`）；会话没绑渠道时为空（纯 Web 会话）。
    pub channel_type: String,
    /// THREAD 读取时：这些消息属于哪个线程；概览读取为空。
    pub thread_id: String,
    pub messages: Vec<HistoryMessage>,
    /// 非空 = 传给 `before` 翻更老一页的不透明游标；空 = 没有更老的了。
    pub next_cursor: String,
}

/// 一次历史读取的调参（平台中立；各 reader 映射到自己的分页原语）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryOptions {
    /// 上限；reader 会按平台上限做钳制，`<= 0` 用合理默认。
    pub limit: i32,
    /// 不透明游标（上一页的 [`HistoryPage::next_cursor`]），只返回**严格更老**的消息。
    pub before: String,
    /// `[after, until)` 的服务端权威代际边界（调用方不能从 URL 设）。
    pub after: String,
    pub until: String,
    pub boundary_pending: bool,
    /// 任务的不可变渠道上下文代际；0 = 老式或直聊读取，没有代际边界。
    pub context_revision: i64,
}

// =====================================================================
// 三个纯函数（上游 `quoted_message.go` / `issue_link.go` / `member_text.go`）
// =====================================================================

/// 把用户选中的那一条历史消息渲染成普通 Markdown（上游 `FormatQuotedMessage`）。
///
/// adapter 在选中正文与当前消息**还分开**时调它；没有消费者需要去用户散文里扫 adapter 私有信封。
/// 发送者名字是**纯文本不是 Markdown**：转义它同时防止 `[Image]` 一类名字变成 adapter 的媒体标记。
pub fn format_quoted_message(sender: &str, body: &str) -> String {
    let body = body.replace("\r\n", "\n");
    let body = body.trim();
    if body.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = body
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                ">".to_string()
            } else {
                format!("> {line}")
            }
        })
        .collect();
    let quote = lines.join("\n");
    let sender = sender.split_whitespace().collect::<Vec<_>>().join(" ");
    if sender.is_empty() {
        return quote;
    }
    let escaped = sender
        .replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('`', "\\`")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('&', "&amp;");
    format!("> **{escaped}:**\n>\n{quote}")
}

/// 构造渠道 issue 结果的 Multica Web 深链（上游 `IssueWebLink`）。
///
/// Web 路由是 workspace 作用域的 `/{workspaceSlug}/issues/{identifier}`：缺 slug 的链接解析不到
/// issue，不值得发出去；任一部分为空就返回空串，调用方**不带链接**回复。这是**故意**的：
/// 忘了接 slug 的 adapter 会**响亮地**丢掉链接，而不是又悄悄发一次不可路由的老形态
/// `/issues/{identifier}`。
pub fn issue_web_link(app_url: &str, workspace_slug: &str, identifier: &str) -> String {
    if app_url.is_empty() || workspace_slug.is_empty() || identifier.is_empty() {
        return String::new();
    }
    format!(
        "{}/{}/issues/{}",
        app_url.trim_end_matches('/'),
        percent_encode(workspace_slug),
        percent_encode(identifier)
    )
}

/// 隔开不可信文本里的标准行内 Markdown `](` 链接邻接（上游 `BreakMarkdownLinkAdjacency`）。
///
/// 平台原生标记需要自己的守卫；不含 `](` 的文本**逐字节**原样返回。
pub fn break_markdown_link_adjacency(text: &str) -> String {
    text.replace("](", "] (")
}

/// 路径段的百分号编码（上游用的是 `url.PathEscape`）。
///
/// 只编码 path segment 里必须编码的字节（`/`、空格、`?`、`#`、`%` 与不可打印字节），
/// 与 `url.PathEscape` 的可读性一致（它不编码 `-_.~` 与字母数字）。
fn percent_encode(segment: &str) -> String {
    const KEEP: &[u8] = b"-_.~!$&'()*+,;=:@";
    let mut out = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        if byte.is_ascii_alphanumeric() || KEEP.contains(byte) {
            out.push(*byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `InboundHandler` 的对象安全形态：`Arc<dyn ...>` 可共享、可 `Arc::clone`。
    #[tokio::test]
    async fn inbound_handler_is_object_safe_and_shareable() {
        struct Counting(std::sync::atomic::AtomicUsize);

        #[async_trait]
        impl InboundHandler for Counting {
            async fn handle(&self, _message: InboundMessage) -> ChannelResult<()> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        }
        let handler: SharedInboundHandler =
            Arc::new(Counting(std::sync::atomic::AtomicUsize::new(0)));
        let clone = Arc::clone(&handler);
        assert!(Arc::ptr_eq(&handler, &clone));
    }

    /// 历史词表的 wire 取值与往返。
    #[test]
    fn history_role_round_trips() {
        for role in [HistoryRole::User, HistoryRole::Assistant] {
            assert_eq!(HistoryRole::from_str_opt(role.as_str()), Some(role));
        }
        assert_eq!(HistoryRole::from_str_opt("system"), None);
        assert_eq!(HistoryRole::User.as_str(), "user");
    }

    /// 引用渲染：空正文 / 空发送者 / 多行 / 发送者转义。
    #[test]
    fn quoted_message_renders_and_escapes() {
        assert_eq!(format_quoted_message("Alice", "   "), "");
        assert_eq!(format_quoted_message("", "hi"), "> hi");
        assert_eq!(
            format_quoted_message("Alice", "a\r\nb\n\nc"),
            "> **Alice:**\n>\n> a\n> b\n>\n> c"
        );
        // 名字里的 Markdown 元字符被转义（`[Image]` 不会变成媒体标记）。
        assert_eq!(
            format_quoted_message("[Image]", "x"),
            "> **\\[Image\\]:**\n>\n> x"
        );
        // 空白折叠（`strings.Fields` + `Join` 的等价物）。
        assert_eq!(format_quoted_message("  A   B ", "x"), "> **A B:**\n>\n> x");
    }

    /// 深链：三段都非空才给链接，且 slug / identifier 做 path 编码。
    #[test]
    fn issue_web_link_requires_all_three_parts() {
        assert_eq!(issue_web_link("", "acme", "ABC-1"), "");
        assert_eq!(issue_web_link("https://x", "", "ABC-1"), "");
        assert_eq!(issue_web_link("https://x", "acme", ""), "");
        assert_eq!(
            issue_web_link("https://x/", "acme", "ABC-1"),
            "https://x/acme/issues/ABC-1"
        );
        // 空格与斜杠必须编码，否则深链会指向另一个路径。
        assert_eq!(
            issue_web_link("https://x", "a b", "A/B"),
            "https://x/a%20b/issues/A%2FB"
        );
    }

    /// 链接邻接隔断：不含 `](` 的文本原样返回。
    #[test]
    fn break_markdown_link_adjacency_is_conservative() {
        assert_eq!(break_markdown_link_adjacency("plain text"), "plain text");
        assert_eq!(
            break_markdown_link_adjacency("[x](y)"),
            "[x] (y)",
            "标准行内链接邻接要断开"
        );
    }
}
