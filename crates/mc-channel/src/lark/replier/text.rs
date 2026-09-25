//! `replier.rs` 的**文案与回复落点的纯函数**（上游 `outcome_replier.go` 的后半段）。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - 拆出本文件是**门 ⑩**（单文件 800 行硬限）的要求，切点是「**投递** ∥ **文案**」：
//!   本文件里没有一条 `async fn`、没有网络、没有端口 —— 上游把这几段散在
//!   `outcome_replier.go` 里，本仓按"它们都是**纯函数**（可单独钉住）"收成一处。
//!
//! # 两条上游不变式（都有用例）
//!
//! 1. **消毒顺序不能反**：先 [`crate::message::break_markdown_link_adjacency`]（拆掉不可信文本
//!    里的链接邻接）**再**把 `<` 换成 `&lt;`。反过来的话，消毒自己产出的 `&lt;` 会被二次转义，
//!    而邻接的 `<url|x>` 会被当成实体保留下来。
//! 2. **深链只认 `issue_identifier`**：`#42` 那个回落值是**降级标签**（`issue_identifier` 空时
//!    的显示形态），永远不是可路由的标识符 ⇒ [`crate::message::issue_web_link`] 拿的是原值。

use mc_core::channel::message::{ChatType, InboundMessage};

use super::super::params::ReplyTarget;
use super::super::store::ChatSessionBinding;
use super::DispatchResult;

/// 回复落到哪里（上游 `inboundReplyTarget`；与 patcher 的 `threadReplyTarget` 逐条对齐）。
///
/// - 没有触发消息 id ⇒ 会话层发送；
/// - 在话题里 ⇒ 回复并留在话题内（`reply_in_thread`）；
/// - 普通群 ⇒ 回复触发那条消息；
/// - 其余（p2p） ⇒ 会话层发送。
#[must_use]
pub fn inbound_reply_target(message: &InboundMessage) -> ReplyTarget {
    if message.message_id.is_empty() {
        return ReplyTarget::default();
    }
    if !message.source.thread_id.is_empty() {
        return ReplyTarget {
            message_id: message.message_id.clone(),
            in_thread: true,
        };
    }
    if message.source.chat_type != ChatType::Group {
        return ReplyTarget::default();
    }
    ReplyTarget {
        message_id: message.message_id.clone(),
        in_thread: false,
    }
}

/// [`inbound_reply_target`] 的绑定行形态（出站面用同一套判决；两处**必须**一致）。
#[must_use]
pub fn inbound_reply_target_of_binding(binding: &ChatSessionBinding) -> ReplyTarget {
    crate::lark::outbound::thread_reply_target(binding)
}

/// `/issue` 新建文案（上游 `issueCreatedText`）。
#[must_use]
pub fn issue_created_text(result: &DispatchResult, app_url: &str) -> String {
    let identifier = issue_result_identifier(result);
    let title = issue_title_sanitized(result);
    let line = if title.is_empty() {
        format!("Created {identifier}")
    } else {
        format!("Created {identifier} — {title}")
    };
    with_issue_link(line, result, app_url)
}

/// `/issue` 活跃重复文案（上游 `issueDuplicateText`）。
#[must_use]
pub fn issue_duplicate_text(result: &DispatchResult, app_url: &str) -> String {
    let identifier = issue_result_identifier(result);
    let title = issue_title_sanitized(result);
    let line = if title.is_empty() {
        format!("Not created — active issue {identifier} already exists.")
    } else {
        format!("Not created — active issue {identifier} already exists: {title}")
    };
    with_issue_link(line, result, app_url)
}

/// issue 的展示标识符（上游逐字：标识符永远赢过裸编号；空 ⇒ `#<number>`）。
#[must_use]
pub fn issue_result_identifier(result: &DispatchResult) -> String {
    if result.issue_identifier.is_empty() {
        format!("#{}", result.issue_number)
    } else {
        result.issue_identifier.clone()
    }
}

/// 成员可控标题的消毒（上游 `strings.TrimSpace` + 两步消毒，**顺序不能反**）。
#[must_use]
pub fn issue_title_sanitized(result: &DispatchResult) -> String {
    let trimmed = result.issue_title.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    crate::message::break_markdown_link_adjacency(trimmed).replace('<', "&lt;")
}

/// 挂深链（上游 `channel.IssueWebLink(appURL, res.IssueWorkspaceSlug, res.IssueIdentifier)`）。
///
/// 深链的判据是 **`issue_identifier`**（不是上面那个 `#42` 降级显示值）：后者不是可路由的
/// 标识符。
fn with_issue_link(line: String, result: &DispatchResult, app_url: &str) -> String {
    let deep_link = crate::message::issue_web_link(
        app_url,
        &result.issue_workspace_slug,
        &result.issue_identifier,
    );
    if deep_link.is_empty() {
        return line;
    }
    format!("{line}\n{deep_link}")
}

/// `url.QueryEscape` 的等价物（上游用 Go 的 `url.QueryEscape`；空格 → `+`，逐字保留）。
///
/// base64url 令牌只含 `A-Za-z0-9-_`（`-` / `_` 是 unreserved）⇒ 实际不会被编码；保留 Go 的
/// 行为是为了将来换令牌字形时形态不漂移。
#[must_use]
pub fn url_encode(raw: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => {
                // `String` 的 `fmt::Write` 是 infallible ⇒ 这个 `write!` 永不失败。
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}
