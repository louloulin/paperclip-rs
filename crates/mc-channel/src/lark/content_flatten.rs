//! lark 正文的**结构摊平** + 提及改写 + markdown 探测
//! （上游 `internal/integrations/lark/{content_flatten.go 172,mention.go 64,`
//! `markdown_detect.go 61}` 三个文件，`docs/60-M7-PLAN.md` §4.1 的 M7-12 行）。
//!
//! - **写者**：M7-12（`docs/60` §3.3；本片的写集勘误见 `docs/32` §29）。
//! - **三个上游文件合成一个本地文件**：三者是同一件事的三个面 —— 「`msg_type` 认不认识」
//!   （摊平）/「正文里的 `@_user_N` 占位怎么改写」（提及）/「正文像不像 markdown」（探测），
//!   合起来 <800 行（门 ⑩），拆开反而各自只剩骨架。
//! - **本文件不碰网络、不碰 DB**：全是纯函数（可逐条单测），下游是
//!   [`super::feishu_channel`] 的归一化与 [`super::enricher`] 的富上下文装配。
//!
//! # 两条入站路径共用同一个摊平器（上游注释逐字）
//!
//! | 路径 | 从哪里读 | 谁调 |
//! | --- | --- | --- |
//! | WS 事件（用户自己的 text / post） | [`super::ws_frame_decoder::LarkInboundEvent::content`] | [`super::feishu_channel`] |
//! | IM REST（引用回复的父消息 / 转发的子消息） | [`super::types::LarkMessage::content`] | [`super::enricher`] |
//!
//! 两条路径的 `mentions[]` **形状不同**（WS 是 `{open_id,union_id,user_id}` 嵌套对象，
//! REST 是裸 `open_id` 字串）⇒ 摊平器**对提及无感**，只把 `@_user_N` 占位原样透出；
//! 改写由 [`resolve_mentions`] 负责，两条路径各自先经 [`mentions_from_event`] /
//! [`mentions_from_rest`] 归一到 [`MentionRef`]。
//!
//! # 与上游的三处**形态**差异（登记 `docs/32` §29 的 D 项）
//!
//! 1. **markdown 探测手写，不引 `regex`**：上游用 9 条 `regexp` 常量；本 crate 的依赖面在
//!    M7-0 冻结（`mc-channel/Cargo.toml` 的注释）且 `regex` 不在其中 ⇒ 同一组模式改成
//!    按行扫描 + 区间扫描，**逐条**对应上游那 9 条（每条都有一条用例钉住）。
//! 2. **`\r\n` 的处理**：本文件按 `'\n'` 切行、**保留** `\r`，与 Go 的 `(?m)$` 一致
//!    （`\r` 不属于 `[ \t]` ⇒ `---\r` 的上游口径是**不**匹配）；`str::lines()` 会吞掉 `\r`，
//!    所以**不**用它。
//! 3. **提及的三字段进一个 [`MentionRef`]**：上游 `larkMention` 是匿名结构体 +
//!    `restMentionsToEvent` 的字段搬移；本仓让两条路径都先归一到同一个具名类型，
//!    于是 [`resolve_mentions`] / [`contains_mention`] 只有**一份**实现。
//!
//! # 凭据面（`docs/60` §2.3）
//!
//! 本文件**没有**任何凭据字段：输入是平台正文（用户自己的话），不是秘密。唯一需要留意的是
//! 这些正文会进日志的地方 —— 本文件自己**零** `tracing::*` 调用。

use serde::{Deserialize, Serialize};

use mc_core::channel::message::MessageKind;

use super::types::LarkMessageMention;
use super::ws_frame_decoder::LarkEventMention;

// =====================================================================
// 词表
// =====================================================================

/// 合并转发消息的 `msg_type`（上游 `larkMsgTypeMergeForward`）。
///
/// 它自己的 `body.content` 是一个固定哨兵串；真正的被转发内容要靠 `GetMessage` 的额外
/// `items[]` 取回 ⇒ 摊平器只给它一个占位（真正的展开归 [`super::enricher`]）。
pub const MSG_TYPE_MERGE_FORWARD: &str = "merge_forward";

/// 出站提及后面跟的分隔符（上游 `mentionSeparator`）：一个空格而不是换行，
/// 让单行回答仍是一行。
pub const MENTION_SEPARATOR: &str = " ";

// =====================================================================
// 摊平：msg_type → 纯文本
// =====================================================================

/// 把 Lark 双重编码的 `body.content` 摊平成纯文本（上游 `flattenContent`）。
///
/// 非文本媒体类型渲染成**稳定的方括号占位**：agent 因此知道"挂了点什么"，而这条快路径
/// **不**下载二进制（真正的下载归 [`super::media`]，占位是它的持久兜底）。
/// `merge_forward` 在到达这里之前会被 [`super::enricher`] 截走（展开它需要一次 HTTP 往返）；
/// 这里的占位只兜住"转发里套转发"的嵌套形态。
#[must_use]
pub fn flatten_content(msg_type: &str, raw_content: &str) -> String {
    match msg_type {
        "text" => extract_text_body(raw_content),
        "post" => flatten_post_content(raw_content),
        "image" => "[Image]".to_string(),
        "file" => "[File]".to_string(),
        "audio" => "[Audio]".to_string(),
        "media" | "video" => "[Video]".to_string(),
        "sticker" => "[Sticker]".to_string(),
        "interactive" => "[interactive card]".to_string(),
        "share_chat" => "[Shared Chat]".to_string(),
        "share_user" => "[Shared User Card]".to_string(),
        "system" => "[System Message]".to_string(),
        MSG_TYPE_MERGE_FORWARD => "[forwarded messages]".to_string(),
        _ => String::new(),
    }
}

/// 把平台 `msg_type` 归一到跨平台的 [`MessageKind`]（上游 `channelMsgType`）。
///
/// 文本类的 Lark 类型（`text` / `post` / `merge_forward` / `interactive`）**全部**摊成
/// `Text` —— 人类可读的内容在 `Body` 里；媒体类型一一对应过去。
#[must_use]
pub fn message_kind(msg_type: &str) -> MessageKind {
    match msg_type {
        "image" => MessageKind::Image,
        "file" => MessageKind::File,
        "audio" => MessageKind::Audio,
        "media" | "video" => MessageKind::Video,
        "" | "text" | "post" | MSG_TYPE_MERGE_FORWARD | "interactive" => MessageKind::Text,
        _ => MessageKind::Unknown,
    }
}

/// `text` 类型正文的形状（上游 `extractTextBody` 里的匿名结构体，提到模块级以免
/// "在语句之后加 item"）。
#[derive(Deserialize)]
struct TextBody {
    #[serde(default)]
    text: String,
}

/// `text` 类型正文里的 `text` 字段（上游 `extractTextBody`）。
///
/// 空串 / 坏 JSON ⇒ 空串（不是错误：一条解不开的正文就是"没有可读内容"）。
#[must_use]
pub fn extract_text_body(content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    serde_json::from_str::<TextBody>(content)
        .map(|body| body.text)
        .unwrap_or_default()
}

// =====================================================================
// 富文本（`post`）
// =====================================================================

/// **接收侧**的 `post` 正文形状（上游 `larkPostContent`）。
///
/// ⚠️ 这**不是**发送 API 的 locale 包装形态（`{"zh_cn": {…}}`）：入站 `post` 的
/// `body.content` 直接解成 `{title, content}`。`content` 是**二维**数组 —— 外层是段落的有序
/// 列表，内层是段落内 span 的有序列表；段落之间的换行是**数组边界**带来的，不是一个 span。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct LarkPostContent {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub content: Vec<Vec<LarkPostSpan>>,
}

/// `post` 段落里的一个节点（上游 `larkPostSpan`）。
///
/// 只建模**承载可渲染文本**的字段；tag 集合是可扩展的 ⇒ 摊平器对认不出的 tag 取它的
/// `text`（有就取，没有就跳过），而不是整条失败。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct LarkPostSpan {
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub href: String,
    #[serde(default, rename = "user_id")]
    pub user_id: String,
    #[serde(default, rename = "user_name")]
    pub user_name: String,
    #[serde(default, rename = "image_key")]
    pub image_key: String,
    #[serde(default, rename = "file_key")]
    pub file_key: String,
    #[serde(default, rename = "file_name")]
    pub file_name: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "mime_type")]
    pub mime_type: String,
}

/// 把一条收到的 `post` 的 `body.content` 摊平成纯文本（上游 `flattenPostContent`）。
///
/// 标题（有就写）独占第一行，随后**一段一行**。段内 span 用一个空格连起来 —— 这是 Lark 自己
/// 的渲染口径（逻辑上分开的块读起来就是空格分隔的词）。
///
/// - 链接 span 渲染成 `text (href)`，URL 因此能活到 agent 的上下文里；
/// - `at` span 渲染成它的 `@_user_N` 占位，好让下游的 [`resolve_mentions`] 换成显示名
///   （占位缺席时退回 span 自带的 `user_name`）；
/// - 媒体 span 退化到 [`flatten_content`] 用的同一组方括号占位。
#[must_use]
pub fn flatten_post_content(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let Ok(doc) = serde_json::from_str::<LarkPostContent>(raw) else {
        return String::new();
    };

    let mut out = String::new();
    let write = |line: &str, out: &mut String| {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    };
    if !doc.title.is_empty() {
        write(&doc.title, &mut out);
    }
    for paragraph in &doc.content {
        write(&flatten_post_paragraph(paragraph), &mut out);
    }
    out.trim_end_matches('\n').to_string()
}

/// 一个段落内的 span 摊平（上游 `flattenPostParagraph`）。
#[must_use]
pub fn flatten_post_paragraph(spans: &[LarkPostSpan]) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(spans.len());
    for span in spans {
        match span.tag.as_str() {
            "a" => match (span.text.is_empty(), span.href.is_empty()) {
                (false, false) => parts.push(format!("{} ({})", span.text, span.href)),
                (false, true) => parts.push(span.text.clone()),
                (true, false) => parts.push(span.href.clone()),
                (true, true) => {}
            },
            "at" => {
                // 优先用 `@_user_N` 占位：下游 resolveMentions 才能把它换成显示名并剥掉
                // bot 自己那一份；占位缺席时退回 span 内联的 user_name。
                if !span.user_id.is_empty() {
                    parts.push(span.user_id.clone());
                } else if !span.user_name.is_empty() {
                    parts.push(format!("@{}", span.user_name));
                }
            }
            "img" => parts.push("[Image]".to_string()),
            "media" => parts.push("[Video]".to_string()),
            // `emotion` 的 emoji_type 是一个枚举键（例如 `SMILE`），不是显示文本 ——
            // 跳过它，别把键泄露出去。
            "emotion" => {}
            "hr" => parts.push("---".to_string()),
            // `text` / `code_block` 与认不出的 tag 是**同一个形态**（上游两处逐字相同）：
            // 有 `text` 就取它。这里合并成一支，不另设 `"text" | "code_block"` 臂。
            _ => {
                if !span.text.is_empty() {
                    parts.push(span.text.clone());
                }
            }
        }
    }
    parts.join(" ")
}

// =====================================================================
// 提及：两条入站路径的归一形态
// =====================================================================

/// 一条提及的**路径无关**形态（上游 `larkMention` 的具名化）。
///
/// WS 事件带三个 id（[`LarkEventMention`]），IM REST 只带裸 `open_id`
/// （[`LarkMessageMention`]）⇒ 两条路径各自[归一](mentions_from_event)，其余代码只看本类型。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MentionRef {
    /// 正文里的占位键（`@_user_1` …）。
    pub key: String,
    /// 被提及者的按应用 id。
    pub open_id: String,
    /// 被提及者的跨应用稳定 id（REST 形状没有 ⇒ 空）。
    pub union_id: String,
    /// 显示名（**可能为空**）。
    pub name: String,
}

/// WS 事件形状 → [`MentionRef`]（上游 `larkMention` 就是它）。
#[must_use]
pub fn mentions_from_event(mentions: &[LarkEventMention]) -> Vec<MentionRef> {
    mentions
        .iter()
        .map(|mention| MentionRef {
            key: mention.key.clone(),
            open_id: mention.id.open_id.clone(),
            union_id: mention.id.union_id.clone(),
            name: mention.name.clone(),
        })
        .collect()
}

/// IM REST 形状 → [`MentionRef`]（上游 `restMentionsToEvent`）。
///
/// REST 的 `mentions[].id` 是**裸** `open_id` 字串 ⇒ `union_id` 留空（上游同）。
#[must_use]
pub fn mentions_from_rest(mentions: &[LarkMessageMention]) -> Vec<MentionRef> {
    mentions
        .iter()
        .map(|mention| MentionRef {
            key: mention.key.clone(),
            open_id: mention.id.clone(),
            union_id: String::new(),
            name: mention.name.clone(),
        })
        .collect()
}

/// 这条提及是不是 bot 自己（上游 `isBotMention`）。
///
/// 判据顺序**承重**（上游注释逐字）：
///
/// 1. `union_id` 已知 ⇒ **只**比 `union_id`。多 bot 群里两个 WS 视角给出的 `open_id`
///    结构上相反，只有 `union_id` 一致 ⇒ 用 `open_id` 比会把事件交给**错的** supervisor；
/// 2. `union_id` 未知（迁移 112 之前的安装、或通讯录范围受限的运维）⇒ 回落到按安装的
///    `open_id` 比较。这在多 bot 群里结构上是反的，但 p2p / 单 bot 场景够用，而且能让
///    **回填之前**的安装不硬失败（`union_id_backfill` 的存在理由）；
/// 3. 两个标识都空 ⇒ `false`（上游同：防住"一行安装两个标识都空却匹配上每一条提及"）。
#[must_use]
pub fn is_bot_mention(mention: &MentionRef, bot_open_id: &str, bot_union_id: &str) -> bool {
    if !bot_union_id.is_empty() {
        return mention.union_id == bot_union_id;
    }
    if bot_open_id.is_empty() {
        return false;
    }
    mention.open_id == bot_open_id
}

/// 提及数组里有没有 bot（上游 `containsMention`）—— 群聊 `addressed_to_bot` 的判据。
///
/// ⚠️ 空输入短路成 `false`（不是"匹配每一条"）。
#[must_use]
pub fn contains_mention(mentions: &[MentionRef], bot_open_id: &str, bot_union_id: &str) -> bool {
    if bot_union_id.is_empty() && bot_open_id.is_empty() {
        return false;
    }
    mentions
        .iter()
        .any(|mention| is_bot_mention(mention, bot_open_id, bot_union_id))
}

/// 把正文里的 `@_user_N` 占位换成可读文本（上游 `resolveMentions`）。
///
/// bot **自己**那一份被**剥掉**：分派已经用 `addressed_to_bot` 路由过这条事件，
/// 再把 `@<bot>` 顶在每条消息前面只会让聊天记录与下游 LLM 上下文更吵而没有信息。
/// 其他参与者渲染成 `@<显示名>`；名字为空时**保留占位**（防御性 —— 实际上 Lark 总会给名字）。
///
/// 替换是**单趟词法扫描**，不是朴素 `replace`，两个理由：
///
/// - **前缀撞车**：一个群里提了十一个人会同时出现 `@_user_1` 与 `@_user_10`；
///   对 `@_user_1` 做 `replace` 会改坏 `@_user_10` 的子串。按 key **长度降序**排，
///   在每个扫描位置试最长匹配，长占位因此总是赢。
/// - **空白保真**：剥 bot 提及时只动**紧邻**它的一个空格 —— 占位之后那个空格，或者
///   （没有的话）输出里已经写进去的那个尾空格。制表符、缩进、代码块、表格竖线等
///   用户消息里**有意**的空白逐字保留。
///
/// `bot_open_id` / `bot_union_id` 传空串 = "**不**剥任何提及"（富上下文装配器就那样调：
/// 引用 / 转发的消息是历史上下文，不是新的触发，所有 `@` 都该留成可读的 `@名字`）。
#[must_use]
pub fn resolve_mentions(
    text: &str,
    mentions: &[MentionRef],
    bot_open_id: &str,
    bot_union_id: &str,
) -> String {
    if text.is_empty() || mentions.is_empty() {
        return text.to_string();
    }
    // 过滤空 key，并按长度降序（稳定排序：同长度的保持原序，便于用例逐字比对）。
    let mut sorted: Vec<&MentionRef> = mentions.iter().filter(|m| !m.key.is_empty()).collect();
    sorted.sort_by_key(|mention| std::cmp::Reverse(mention.key.len()));

    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        let matched = sorted
            .iter()
            .find(|mention| bytes[index..].starts_with(mention.key.as_bytes()));
        let Some(mention) = matched else {
            // 逐字节推进：占位是纯 ASCII，多字节 UTF-8 字符不会与任何 key 前缀相等，
            // 所以按字节前进是安全的（不会切坏一个字符）。
            out.push(bytes[index]);
            index += 1;
            continue;
        };
        let mut end = index + mention.key.len();
        if is_bot_mention(mention, bot_open_id, bot_union_id) {
            // 剥掉：吃掉紧邻的一个空格（优先占位之后那个；否则回退掉已经写出去的那个
            // 尾空格），接缝处因此不会留下双空格或悬空的前导空格。
            // 制表符 / 换行 / 其它字符一律不动。
            if end < bytes.len() && bytes[end] == b' ' {
                end += 1;
            } else if out.last() == Some(&b' ') {
                out.pop();
            }
        } else if !mention.name.is_empty() {
            out.push(b'@');
            out.extend_from_slice(mention.name.as_bytes());
        } else {
            // 认不出的提及：保留占位，让 agent 至少看到一个稳定的 token。
            out.extend_from_slice(mention.key.as_bytes());
        }
        index = end;
    }
    // 占位与其替换物都是合法 UTF-8，且扫描按字节推进时从不切开多字节字符 ⇒ 这里必成功。
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

// =====================================================================
// 出站提及（上游 `mention.go`；消费方是 M7-13 的出站面）
// =====================================================================

/// 出站提及要用的 `open_id`（上游 `safeMentionOpenID`）。
///
/// 真正的飞书 id 是 `ou_` + 十六进制 ⇒ 带引号、尖括号或空白的值不是该嵌进去的 id。
/// 返回 `None` 让调用方**不带提及地发出**，而不是发出坏标记或畸形卡 JSON。
#[must_use]
pub fn safe_mention_open_id(open_id: &str) -> Option<&str> {
    if open_id.is_empty() {
        return None;
    }
    let unsafe_char = |c: char| {
        matches!(
            c,
            '<' | '>' | '"' | '\'' | '`' | '\\' | ' ' | '\t' | '\r' | '\n'
        )
    };
    if open_id.contains(unsafe_char) {
        return None;
    }
    Some(open_id)
}

/// 在 `body` 前面加一个原生提及，用于 `msg_type=text` 那条路径（上游 `prependTextMention`）。
///
/// `open_id` 为空 / 不安全 ⇒ **原样返回** `body`：一条没有提及的回答只是小退步，
/// 一条**错**的提及才是要避免的故障（#8234）。
#[must_use]
pub fn prepend_text_mention(open_id: &str, body: &str) -> String {
    match safe_mention_open_id(open_id) {
        Some(id) => format!("<at user_id=\"{id}\"></at>{MENTION_SEPARATOR}{body}"),
        None => body.to_string(),
    }
}

/// [`prepend_text_mention`] 的 schema-2.0 卡片 markdown 版（同一个提及拼成 `id=` 且不加引号）。
#[must_use]
pub fn prepend_markdown_mention(open_id: &str, body: &str) -> String {
    match safe_mention_open_id(open_id) {
        Some(id) => format!("<at id={id}></at>{MENTION_SEPARATOR}{body}"),
        None => body.to_string(),
    }
}

// =====================================================================
// markdown 探测（上游 `markdown_detect.go`）
// =====================================================================
//
// 上游那 9 条 `regexp` 逐条对应下面的手写判据（差异见模块文档第 1 条）：
//
//   `(?m)^#{1,6}[ \t]`                     → `is_heading`
//   `(?m)^[ \t]*[-*+][ \t]`                → `is_unordered_list`
//   `(?m)^[ \t]*\d+\.[ \t]`                → `is_ordered_list`
//   `(?m)^>[ \t]`                          → `is_blockquote`
//   `(?m)^[ \t]*(?:---|\*\*\*|___)[ \t]*$` → `is_thematic_break`
//   `\*\*[^*\n]+\*\*`                      → `contains_bold`
//   `__[^_\n]+__`                          → `contains_bold_underscore`
//   `(?m)^[ \t]*\|.+\|[ \t]*$`             → `is_table_row`
//   `\[[^\]\n]+\]\([^)\n]+\)`              → `contains_link`

/// agent 的回答像不像 markdown（上游 `containsMarkdown`）。
///
/// 上游注释逐字：每条判据都**故意保守** —— 宁可误报（把纯文本走 markdown 卡，它照样渲染得
/// 挺好）也不要漏报（把 `**bold**` 原样留在用户的聊天记录里）。
///
/// 快路径（反引号 / 星号 / 竖线 / 井号）先走；只有命中才跑剩下的扫描。空串直接 `false`
/// （空回答不该被包进卡片）。
#[must_use]
pub fn contains_markdown(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    // 围栏代码块 —— 强信号，便宜的 `contains`。
    if text.contains("```") {
        return true;
    }
    // 行内代码：只认**成对**且中间有非空白字符的那一对。裸反引号（例如行文里引用一个按键）
    // 不该触发。上游同样只做**一次**探测（不循环）。
    if let Some(open) = text.find('`') {
        if let Some(close) = text[open + 1..].find('`') {
            if close > 0 {
                return true;
            }
        }
    }
    if contains_bold(text, b'*') || contains_bold(text, b'_') {
        return true;
    }
    if contains_link(text) {
        return true;
    }
    for line in text.split('\n') {
        if is_heading(line)
            || is_unordered_list(line)
            || is_ordered_list(line)
            || is_blockquote(line)
            || is_thematic_break(line)
            || is_table_row(line)
        {
            return true;
        }
    }
    false
}

/// `#{1,6}` 后跟一个空格 / 制表符（`^#{1,6}[ \t]`）。
fn is_heading(line: &str) -> bool {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return false;
    }
    matches!(line.as_bytes().get(hashes), Some(b' ' | b'\t'))
}

/// `^[ \t]*[-*+][ \t]`。
fn is_unordered_list(line: &str) -> bool {
    let rest = line.trim_start_matches([' ', '\t']);
    let mut chars = rest.chars();
    match chars.next() {
        Some('-' | '*' | '+') => matches!(chars.next(), Some(' ' | '\t')),
        _ => false,
    }
}

/// `^[ \t]*\d+\.[ \t]`。
fn is_ordered_list(line: &str) -> bool {
    let rest = line.trim_start_matches([' ', '\t']);
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    let tail = &rest[digits..];
    let mut chars = tail.chars();
    match chars.next() {
        Some('.') => matches!(chars.next(), Some(' ' | '\t')),
        _ => false,
    }
}

/// `^>[ \t]`（**不**允许前导空白 —— 上游逐字）。
fn is_blockquote(line: &str) -> bool {
    let mut chars = line.chars();
    match chars.next() {
        Some('>') => matches!(chars.next(), Some(' ' | '\t')),
        _ => false,
    }
}

/// `^[ \t]*(?:---|\*\*\*|___)[ \t]*$`（三者**恰好**三个字符，多一个就不匹配）。
fn is_thematic_break(line: &str) -> bool {
    let rest = line.trim_start_matches([' ', '\t']);
    let marker = ["---", "***", "___"]
        .into_iter()
        .find(|candidate| rest.starts_with(candidate));
    match marker {
        Some(marker) => rest[marker.len()..].trim_matches([' ', '\t']).is_empty(),
        None => false,
    }
}

/// `^[ \t]*\|.+\|[ \t]*$`（两端都要有竖线，中间至少一个字符）。
fn is_table_row(line: &str) -> bool {
    let rest = line.trim_start_matches([' ', '\t']);
    if !rest.starts_with('|') {
        return false;
    }
    let body = rest[1..].trim_end_matches([' ', '\t']);
    // 去掉尾部空白后必须以 `|` 收尾，且那之前还有内容（`.+`）。
    body.len() >= 2 && body.ends_with('|') && !body[..body.len() - 1].is_empty()
}

/// `\*\*[^*\n]+\*\*`（`marker = '*'`）与 `__[^_\n]+__`（`marker = '_'`）。
///
/// 两侧标记之间的内容**不含** marker、**不含**换行，且至少一个字符。
fn contains_bold(text: &str, marker: u8) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index + 1 < bytes.len() {
        if bytes[index] != marker || bytes[index + 1] != marker {
            index += 1;
            continue;
        }
        let start = index + 2;
        let mut end = start;
        while end < bytes.len() && bytes[end] != marker && bytes[end] != b'\n' {
            end += 1;
        }
        // `[^marker\n]+` 要至少一个字符 ⇒ end > start；闭合必须是同一行的两个 marker。
        if end > start && end + 1 < bytes.len() && bytes[end] == marker && bytes[end + 1] == marker
        {
            return true;
        }
        // 只前进一格：`***bold***` 这类三个标记连排的形态里，能成的匹配从**第二个**标记
        // 起算（上游的 regex 引擎会试每一个起点）⇒ 跳两格会漏掉它。
        index += 1;
    }
    false
}

/// `\[[^\]\n]+\]\([^)\n]+\)` —— `[text](url)` 形态的链接 / 图片。
fn contains_link(text: &str) -> bool {
    let bytes = text.as_bytes();
    for open in 0..bytes.len() {
        if bytes[open] != b'[' {
            continue;
        }
        let mut label_end = open + 1;
        while label_end < bytes.len() && bytes[label_end] != b']' && bytes[label_end] != b'\n' {
            label_end += 1;
        }
        // label 至少一个字符，且必须真的以 `]` 收尾。
        if label_end == open + 1 || label_end >= bytes.len() || bytes[label_end] != b']' {
            continue;
        }
        if bytes.get(label_end + 1) != Some(&b'(') {
            continue;
        }
        let mut url_end = label_end + 2;
        while url_end < bytes.len() && bytes[url_end] != b')' && bytes[url_end] != b'\n' {
            url_end += 1;
        }
        if url_end > label_end + 2 && url_end < bytes.len() && bytes[url_end] == b')' {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests;
