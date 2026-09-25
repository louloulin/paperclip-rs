//! `WeCom` 智能机器人在 `inbox:new` 上推的那张卡片（上游
//! `internal/integrations/wecom/inbox_message.go`，**232 行**）。
//!
//! - **写者**：M7-19（`LUM-1784` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：用 markdown 推，因为 `aibot_send_msg` **不接受**
//!   `msgtype=text`。它单独成文件，是为了让出站处理器专注投递、而这个模块拥有**措辞与链接构造**。
//!
//! # 本片接的是 M7-17 的交接 H2/D7（`docs/32` §34.2 / §34.4）
//!
//! M7-17 落了投递路径 + [`InboxRenderer`] 端口，但**没有**渲染器：没有渲染器时这条推送
//! **不投递**（失败关闭）。本文件交付渲染器 [`InboxCardRenderer`]。
//!
//! # 两处形态差异（逐条登记，`docs/32` §36）
//!
//! 1. **端口形状勘误 + 收敛（D2）**：M7-17 的 [`InboxPush`] 里**没有** `title` / `body`，而
//!    上游 `buildInboxMarkdown` 的整张卡片就是标题 + 正文 + 深链 ⇒ 少了这两个字段，卡片会
//!    **缺标题与正文**（是降级，不是等价）。本片把这两个字段补进 [`InboxPush`]（`from_payload`
//!    一并抽出来），所以本文件实现的是**等价**的渲染器，不是降级版。
//! 2. **app URL 是参数，不是 env 读在函数里（D4）**：上游 `inboxAppURL()` 在渲染函数内部直接读
//!    `WECOM_APP_URL` / `MULTICA_APP_URL` / `FRONTEND_ORIGIN`。本仓的惯例（同
//!    [`crate::wecom::replier::WeComOutboundReplier::new`]）是把 app 主机做成**装配参数**：
//!    一个纯函数不该读进程环境，否则它不可测、也无法按部署覆写。env 解析本身保留为
//!    [`inbox_app_url_from_env`]，宿主在装配时调一次。
//!
//! # 链接纪律（上游逐字，别丢）
//!
//! - **只接受 `https://`**：一个非 HTTPS 的覆写被**静默丢掉**，免得一个配错的环境变量把一条
//!   `http://` 地址漏进用户聊天；
//! - 没配 app URL ⇒ **整段链接省掉**：宁可发一条只有标题的卡，也不发一条坏链。

use std::fmt;

use serde_json::Value;

use crate::wecom::markdown::break_member_links;
use crate::wecom::outbound::{InboxPush, InboxRenderer};

/// `aibot` markdown 正文的长度上限：超过约 4096 个字符 `WeCom` 会**整条**拒帧。这里用 4000，
/// 给前缀与链接后缀留余量（上游 `inboxMarkdownMaxLen`）。
pub const INBOX_MARKDOWN_MAX_LEN: usize = 4000;

/// `/inbox` 的路径段。
pub const INBOX_PATH: &str = "inbox";

/// 通知类型的中文显示名（上游 `inboxTypeLabels`，十三项逐字）。
///
/// 保留在本文件里，是为了让 wecom **不去**伸手进 `cmd/server` 拿它；两张表靠约定一致。
#[must_use]
pub fn inbox_type_labels() -> &'static [(&'static str, &'static str)] {
    &[
        ("issue_assigned", "任务指派"),
        ("mentioned", "提及你"),
        ("status_changed", "状态变更"),
        ("comment_added", "新评论"),
        ("new_comment", "新评论"),
        ("reaction_added", "表情反应"),
        ("task_failed", "task 失败"),
        ("unassigned", "取消指派"),
        ("assignee_changed", "指派人变更"),
        ("priority_changed", "优先级变更"),
        ("due_date_changed", "截止日期变更"),
        ("start_date_changed", "开始日期变更"),
    ]
}

/// 上游 `inboxTypeLabel`：认识的类型用它的显示名，别的一律「新消息」。
#[must_use]
pub fn inbox_type_label(item_type: &str) -> &'static str {
    inbox_type_labels()
        .iter()
        .find(|(key, _)| *key == item_type)
        .map_or("新消息", |(_, label)| *label)
}

/// 本模块读的三个环境变量（按上游的优先级顺序）。
pub const INBOX_APP_URL_ENV_VARS: [&str; 3] =
    ["WECOM_APP_URL", "MULTICA_APP_URL", "FRONTEND_ORIGIN"];

/// 上游 `inboxAppURL` 的**判定那一半**：只接受 `https://`，末尾的 `/` 去掉。
///
/// `None` = 这个值不可用（空 / 不是 HTTPS）⇒ 调用方继续试下一个，或者在链路末端**整段省掉链接**。
#[must_use]
pub fn resolve_inbox_app_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || !trimmed.starts_with("https://") {
        return None;
    }
    Some(trimmed.trim_end_matches('/').to_string())
}

/// 上游 `inboxAppURL` 的**读取那一半**：按 [`INBOX_APP_URL_ENV_VARS`] 的顺序取第一个可用的。
///
/// 读取口做成参数（`reader: impl Fn(&str) -> Option<String>`）而不是直接 `std::env::var`：
/// 用例因此可以验"非 HTTPS 被静默丢掉、优先级是真的"这两条，而不必改进程环境。
#[must_use]
pub fn inbox_app_url_from_env(reader: impl Fn(&str) -> Option<String>) -> Option<String> {
    INBOX_APP_URL_ENV_VARS
        .iter()
        .filter_map(|name| reader(name))
        .find_map(|value| resolve_inbox_app_url(&value))
}

/// 上游 `buildInboxMarkdown`：把一条收件箱条目渲染成 `aibot` 友好的 markdown 卡片。
///
/// 形态（逐字）：
///
/// ```text
/// **[{类型}] {标题}**
/// {正文}
/// [查看详情]({appURL}/{slug|workspaceID}/inbox?issue={issueID})
/// ```
///
/// 没配 app URL 时**整段链接省掉**；正文里的成员文本先过
/// [`break_member_links`]（见 [`crate::wecom::markdown`]）。
///
/// 返回空串 = 这条条目没有可发送的正文（既没标题也没类型）⇒ 调用方**不投递**（失败关闭）。
#[must_use]
pub fn build_inbox_markdown(item: &Value, app_url: &str, workspace_id: &str, slug: &str) -> String {
    let title = item
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    if title.is_empty() && item_type.is_empty() {
        return String::new();
    }
    let body = inbox_item_body(item);
    let link = inbox_item_link(item, app_url, workspace_id, slug);

    // 标题与正文是**成员写的**。它们落进一张收件人有充分理由信任的卡片 —— 它来自机器人而不是
    // 作者本人 —— 所以里面的链接语法绝不能渲染。上过线的形态：一条标题
    // "[click here](http://evil.example)" 到达时是一条能点的链接；一条正文带着
    // "[重置密码]: https://evil.example" 加 "[重置密码]" 到达时也是一条。
    //
    // 逐字段过而不是过成品，正是下面那段长度预算需要的 —— 而且不损失任何覆盖：成员文本的每一行
    // 都从卡片自己那一行的行首开始（标题第一行除外，而那里的 "**[" 前缀让成员站不到块位置
    // 起始处）。扫描本来就把每个字段的起点当成行首，所以那一处是被守了两遍而不是一遍。
    //
    // 在**任何长度测量之前**做：每次断点插入一个空格，而一篇布满 "](" 的正文会长出一半。
    // 量的是涨完之后的那份文本，否则卡片会超上限发出去，而 `WeCom` 会**整条**拒掉它、同时
    // 告诉发送方"已投递"。
    let title = break_member_links(title);
    let body = break_member_links(&body);

    let label = inbox_type_label(item_type);
    let prefix = format!("**[{label}] {title}**");
    let suffix = if link.is_empty() {
        String::new()
    } else {
        format!("\n[查看详情]({link})")
    };
    let mut assembled = prefix.clone();
    if !body.is_empty() {
        assembled.push('\n');
        assembled.push_str(&body);
    }
    assembled.push_str(&suffix);
    if rune_count(&assembled) <= INBOX_MARKDOWN_MAX_LEN {
        return assembled;
    }
    // 只截**正文**。前缀与链接必须完整活下来，用户才还拿得到"查看详情"这个入口。
    //
    // 下面两处裁剪都不可能把一个 "]" 重新放到 "(" 或 ":" 旁边：`truncate_runes` 只丢掉一个后缀，
    // 所以它造不出 `break_member_links` 已经分开的一对；而这里在成员文本之后拼进去的每个字面量
    // 都以 "."、"*" 或 "\n" 开头 —— 绝不是 "(" 或 ":"。丢掉一个后缀也补不出一条引用定义：
    // 它只能把某个定义的目标切掉。
    let room = INBOX_MARKDOWN_MAX_LEN.saturating_sub(rune_count(&prefix) + rune_count(&suffix) + 4);
    if room > 0 {
        return format!("{prefix}\n{}...{suffix}", truncate_runes(&body, room));
    }
    // 正文一点位置都没有，意味着**前缀本身**就是问题 —— 一个足够长的标题能把
    // 前缀+后缀独自推过上限，而原样返回意味着 `WeCom` 拒掉整帧、发送方却被告知已投递。
    // 截短标题，让发出去的东西**总是**放得下。
    let fixed = rune_count(&prefix).saturating_sub(rune_count(&title));
    let title_room = INBOX_MARKDOWN_MAX_LEN.saturating_sub(fixed + rune_count(&suffix) + 3);
    format!(
        "**[{label}] {}...**{suffix}",
        truncate_runes(&title, title_room)
    )
}

/// 上游 `inboxItemBody`：从收件箱条目里取正文。
///
/// 上游的 Go 形态允许 `*string`（可空的 JSON 字段）、`string`、或缺失；`serde_json` 里
/// `*string` 与 `null` 是同一个东西，所以这里两种取值 + 缺失就够了。
#[must_use]
pub fn inbox_item_body(item: &Value) -> String {
    item.get("body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 上游 `inboxItemLink`：构造 `{appURL}/{slug|wsUUID}/inbox?issue={issueID}` 深链。
///
/// 没配 app URL ⇒ 返回空串，调用方据此**整段省掉**链接。
#[must_use]
pub fn inbox_item_link(item: &Value, app_url: &str, workspace_id: &str, slug: &str) -> String {
    let Some(app_url) = resolve_inbox_app_url(app_url) else {
        return String::new();
    };
    let segment = if slug.is_empty() { workspace_id } else { slug };
    let mut link = format!("{app_url}/{}/{INBOX_PATH}", path_escape(segment));
    if let Some(issue_id) = inbox_item_issue_id(item) {
        link.push_str("?issue=");
        link.push_str(&query_escape(&issue_id));
    }
    link
}

/// 上游 `inboxItemIssueID`：有 `issue_id` 就取出来；聊天类通知没有 `issue_id` ⇒ `None`，
/// 链接于是省掉那个查询参数。
#[must_use]
pub fn inbox_item_issue_id(item: &Value) -> Option<String> {
    item.get("issue_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// 上游 `truncateRunes`：把 `text` 裁到至多 `max_runes` 个 rune。
///
/// 按 rune 而不是按字节，是为了裁剪**永远**不会劈开一个汉字。
#[must_use]
pub fn truncate_runes(text: &str, max_runes: usize) -> String {
    if max_runes == 0 {
        return String::new();
    }
    text.chars().take(max_runes).collect()
}

/// `url.PathEscape` 的等价物（路径段：`/`、空格、`?`、`#`、`%` 与不可打印字节要编码）。
///
/// 与 [`crate::message`] 的 `percent_encode` 同源 —— 那一份是私有的，而重复一个 12 行的
/// 表比给它开一个 `pub` 更好（同 [`crate::wecom::replier::url_encode`] 的先例）。
#[must_use]
pub fn path_escape(segment: &str) -> String {
    const KEEP: &[u8] = b"-_.~!$&'()*+,;=:@";
    percent_encode(segment, KEEP)
}

/// `url.QueryEscape` 的等价物（查询值：保留集比路径段小）。
#[must_use]
pub fn query_escape(value: &str) -> String {
    const KEEP: &[u8] = b"-_.~";
    percent_encode(value, KEEP)
}

fn percent_encode(value: &str, keep: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || keep.contains(byte) {
            out.push(char::from(*byte));
        } else {
            // 写进 `String` 不会失败（`fmt::Write for String` 的 `write_str` 是 infallible）。
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// 上游那条长度预算数的**rune**（`utf8.RuneCountInString`），不是字节。
fn rune_count(text: &str) -> usize {
    text.chars().count()
}

// =====================================================================
// 端口实现（M7-17 的 `InboxRenderer`）
// =====================================================================

/// 收件箱卡片的**生产**渲染器（[`InboxRenderer`] 端口 + 本文件的卡片）。
///
/// app 主机在装配时进来（见模块文档的 D4），所以渲染本身是纯的、可测的。
pub struct InboxCardRenderer {
    app_url: String,
}

impl fmt::Debug for InboxCardRenderer {
    /// 手写：app 主机是**部署配置**而不是凭据，但它仍然不该被无脑打进日志（它的形态随环境而变）
    /// ⇒ 只报"配没配"。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InboxCardRenderer")
            .field("app_url_configured", &!self.app_url.is_empty())
            .finish()
    }
}

impl InboxCardRenderer {
    /// 用一个 app 主机装配（空串 = 没配 ⇒ 卡片不带链接，但仍会发）。
    #[must_use]
    pub fn new(app_url: impl Into<String>) -> Self {
        Self {
            app_url: resolve_inbox_app_url(&app_url.into()).unwrap_or_default(),
        }
    }

    /// 从部署环境装配（[`inbox_app_url_from_env`] 的读取口是 `std::env::var`）。
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(inbox_app_url_from_env(|name| std::env::var(name).ok()).unwrap_or_default())
    }

    /// app 主机的**可用**形态（`None` = 没配 / 不是 HTTPS）。
    #[must_use]
    pub fn app_url(&self) -> Option<&str> {
        if self.app_url.is_empty() {
            None
        } else {
            Some(&self.app_url)
        }
    }
}

impl InboxRenderer for InboxCardRenderer {
    fn render(&self, push: &InboxPush, workspace_slug: &str) -> Option<String> {
        // 把投影重新装成 `buildInboxMarkdown` 的那个条目形状，于是卡片的形态只有**一处**
        // 实现 —— 端口实现与纯函数不可能漂开。
        let item = serde_json::json!({
            "title": push.title,
            "body": push.body,
            "type": push.item_type,
            "issue_id": push.issue_id,
        });
        let rendered =
            build_inbox_markdown(&item, &self.app_url, &push.workspace_id, workspace_slug);
        if rendered.is_empty() {
            None
        } else {
            Some(rendered)
        }
    }
}

#[cfg(test)]
mod tests;
