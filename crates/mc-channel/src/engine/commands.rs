//! 命令解析与标题：`/new` / `/clear` / `/issue` 的**唯一**分类器 + 渠道 Chat 的确定性标题。
//!
//! - **写者**：M7-2（`docs/60` §3.3 的写集表）。
//! - **上游**（M7-2 的三个上游文件，逐条移植）：
//!   `channel/engine/fresh_command.go`（99 行）、`channel/engine/issue_command.go`（150 行）、
//!   `channel/engine/title.go`（63 行）+ `internal/chattitle/title.go`、`provenance.go`（34 行）。
//! - **为什么命令解析在共享 engine 而不是各 adapter**（上游注释逐字）：命令是**跨平台产品行为**
//!   —— 每个到达 Router 的渠道都拿到同一份 `/clear` / `/new` / `/issue` 语义，adapter 只负责
//!   归一化平台的传输细节（bot mention、富媒体布局）。解析若下沉到 adapter，20 个切片就会有
//!   20 份"什么算 `/issue`"。
//! - **形态纪律（三条，逐条有用例）**：
//!   1. **大小写敏感**：`/Issue` / `/ISSUE` **不**触发（避免把句子里提到的 `/issue` 升格成命令）；
//!   2. **只认第一个非空行**：正文前面有若干空行仍算命令；第一非空行不是命令前缀就**不是**命令，
//!      哪怕后面的行里有 `/issue`；
//!   3. **必须是完整 token**：`/issuetracker` / `/clearness` / `/newness` 都不匹配。
//!      ⇒ `/clear` 与 `/issue` 在**同一第一行**上互斥（这也是上游注释里明写的一条）。
//! - **标题**：`derive_chat_title` 是"确定性回退"（首个非空行 → 去 Markdown → 折叠空白 →
//!   30 个码点截断 + 单个省略号）。**按 Unicode 码点**计数（不是 UTF-16 码元），
//!   与 Go 的 `[]rune` 切片行为一致。
//! - **不做正则**：本 crate 没有 `regex` 依赖（anchor 把依赖面一次定死），
//!   上游的四个正则在这里是手写扫描 ⇒ 不引入新包，且行为逐字对齐（见 `commands/tests.rs`）。
//!
//! 行预算（门 ⑩）：`commands.rs` ≤800 行；向量用例拆在 `commands/tests.rs`。

use mc_core::channel::message::MessageKind;
use mc_core::id::Id;

use crate::engine::resolvers::{CommandClassifier, CommandIntent};

/// 命令令牌（逐字；大小写敏感）。
const FRESH_SESSION_PREFIX: &str = "/clear";
const NEW_CHAT_PREFIX: &str = "/new";
const ISSUE_PREFIX: &str = "/issue";

/// 确定性标题的码点上限（上游 `deterministicTitleLimit`）。
pub const DETERMINISTIC_TITLE_LIMIT: usize = 30;

/// 媒体占位行（上游 `withoutMediaPlaceholderLines` 的四条，逐字）。
pub const MEDIA_PLACEHOLDER_LINES: [&str; 4] = ["[Image]", "[File]", "[Audio]", "[Video]"];

// =====================================================================
// 命令分类器（`CommandClassifier` 的实现）
// =====================================================================

/// 共享 engine 的命令分类器：`/new` / `/clear` / `/issue`。
///
/// ⚠️ `body` 读的是 [`mc_core::channel::message::InboundMessage::command_source_text`]：
/// adapter 若富化过 `text`，**必须**自己先写好 `command_text`，否则富化前缀会被当成命令
/// （上游 #8058 那条回归）。
#[derive(Debug, Default, Clone, Copy)]
pub struct ChannelCommandClassifier;

impl CommandClassifier for ChannelCommandClassifier {
    fn classify(&self, body: &str) -> CommandIntent {
        // `/new` 先于 `/clear`（两者语法相同，只有产品的后续动作不同）。
        if let Some(parsed) = parse_leading_command(body, NEW_CHAT_PREFIX) {
            return CommandIntent::NewChat { body: parsed };
        }
        if let Some(parsed) = parse_leading_command(body, FRESH_SESSION_PREFIX) {
            return CommandIntent::FreshSession { body: parsed };
        }
        match parse_issue_command(body) {
            Some((title, description)) => CommandIntent::Issue { title, description },
            None => CommandIntent::None,
        }
    }
}

/// `/clear` 与 `/new` 的**共享**解析（两者的入站语法**故意**完全相同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCommandKind {
    FreshSession,
    NewChat,
}

/// 一条解析出来的控制指令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlCommand {
    pub kind: ControlCommandKind,
    pub body: String,
}

/// 一个分类器解析 `/clear` 与 `/new`（与 `/issue` 的规则一致：大小写敏感、token 完整、
/// 只认第一非空行 ⇒ 同一第一行上 `/clear` 与 `/issue` 互斥）。
pub fn parse_control_command(body: &str) -> Option<ControlCommand> {
    if let Some(parsed) = parse_leading_command(body, NEW_CHAT_PREFIX) {
        return Some(ControlCommand {
            kind: ControlCommandKind::NewChat,
            body: parsed,
        });
    }
    parse_leading_command(body, FRESH_SESSION_PREFIX).map(|parsed| ControlCommand {
        kind: ControlCommandKind::FreshSession,
        body: parsed,
    })
}

/// `/clear` 的专用口（返回**剥掉指令**之后的正文）。
pub fn parse_fresh_session_command(body: &str) -> Option<String> {
    match parse_control_command(body) {
        Some(ControlCommand {
            kind: ControlCommandKind::FreshSession,
            body,
        }) => Some(body),
        _ => None,
    }
}

/// `/new` 的专用口。
pub fn parse_new_chat_command(body: &str) -> Option<String> {
    match parse_control_command(body) {
        Some(ControlCommand {
            kind: ControlCommandKind::NewChat,
            body,
        }) => Some(body),
        _ => None,
    }
}

/// `/issue <title> [\n<description>]`（前导空行容忍；`/issue` 单独出现 ⇒ 空标题）。
///
/// 空标题**不是**错误：Router 会返回一条用法提示，**绝不**从历史里推断标题。
pub fn parse_issue_command(body: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = body.split('\n').collect();
    let index = first_non_empty_line(&lines)?;
    let trimmed = lines[index].trim_start_matches([' ', '\t']);
    let rest = trimmed.strip_prefix(ISSUE_PREFIX)?;
    if !rest.starts_with([' ', '\t']) && !rest.is_empty() {
        return None;
    }
    let title = rest.trim().to_string();
    let description = if index + 1 < lines.len() {
        lines[index + 1..]
            .join("\n")
            .trim_end_matches([' ', '\t', '\n'])
            .to_string()
    } else {
        String::new()
    };
    Some((title, description))
}

/// 剥掉 `/issue` 指令行之后的正文（保留其后的版面）。
///
/// 与 `command_text` 的区别：完整正文里还有 adapter 生成的内联媒体占位
/// （issue 的初始描述要留着它们，媒体绑定器才有稳定的落点）。指令行**之前**的内容
/// **故意**排除：有些 adapter 把引用上下文富化进正文，把它抄进 issue 会改掉既有命令契约。
pub fn issue_description_from_command_body(
    body: &str,
    command_text: &str,
    fallback: &str,
) -> String {
    match issue_command_line_bounds(body, command_text) {
        Some((_, end)) => body[end..].trim().to_string(),
        None => fallback.to_string(),
    }
}

/// 在归一化正文里定位**用户自己写的**那条 `/issue` 指令行。
///
/// adapter 可能在前面富化出历史里的老 `/issue` 行 ⇒ 只匹配"第一个 token 完整的指令"不够。
/// `command_text` 是**未经富化**的命令源：先数它里面有几条相同的指令行，再在正文的用户后缀里
/// 取对应的那一条。描述里重复出现同样的指令行时依然稳定。
fn issue_command_line_bounds(body: &str, command_text: &str) -> Option<(usize, usize)> {
    let expected = first_issue_command_line(command_text)?;
    let occurrences = count_matching_lines(command_text, expected);
    let candidates = matching_line_bounds(body, expected);
    if candidates.is_empty() {
        return None;
    }
    let target = candidates.len().checked_sub(occurrences)?;
    candidates.get(target).copied()
}

fn first_issue_command_line(command_text: &str) -> Option<&str> {
    for line in command_text.split('\n') {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(ISSUE_PREFIX) {
            if rest.is_empty() || rest.starts_with([' ', '\t']) {
                return Some(trimmed);
            }
        }
        if !trimmed.is_empty() {
            return None;
        }
    }
    None
}

fn count_matching_lines(text: &str, expected: &str) -> usize {
    text.split('\n')
        .filter(|line| line.trim() == expected)
        .count()
}

/// 正文里所有"trim 之后等于 `expected`"的行的 `(start, end)` 字节区间。
fn matching_line_bounds(body: &str, expected: &str) -> Vec<(usize, usize)> {
    let mut bounds = Vec::new();
    let mut offset = 0;
    while offset <= body.len() {
        let end = match body[offset..].find('\n') {
            Some(found) => offset + found,
            None => body.len(),
        };
        if body[offset..end].trim() == expected {
            bounds.push((offset, end));
        }
        if end >= body.len() {
            break;
        }
        offset = end + 1;
    }
    bounds
}

/// `/clear` 与 `/new` 的共享匹配：只认第一非空行、token 必须完整。
fn parse_leading_command(body: &str, prefix: &str) -> Option<String> {
    let lines: Vec<&str> = body.split('\n').collect();
    let index = first_non_empty_line(&lines)?;
    let trimmed = lines[index].trim_start_matches([' ', '\t']);
    let rest = trimmed.strip_prefix(prefix)?;
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let first_line_body = rest.trim();
    if !first_line_body.is_empty() {
        parts.push(first_line_body.to_string());
    }
    if index + 1 < lines.len() {
        parts.push(lines[index + 1..].join("\n"));
    }
    Some(
        parts
            .join("\n")
            .trim_end_matches([' ', '\t', '\n'])
            .to_string(),
    )
}

fn first_non_empty_line(lines: &[&str]) -> Option<usize> {
    lines.iter().position(|line| !line.trim().is_empty())
}

// =====================================================================
// 标题
// =====================================================================

/// 渠道 Chat 的确定性标题（上游 `chattitle.Derive`）。
///
/// 首个非空行 → 去 Markdown 围栏 / 标记 / 链接 → 折叠空白 → **30 个码点**上限，
/// 超出时取前 29 个码点、去掉尾随空白、加**一个**省略号。
pub fn derive_chat_title(body: &str) -> String {
    let line = body
        .split('\n')
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let line = strip_markdown_fences(line);
    let line = strip_markdown_marks(&line);
    let line = strip_markdown_links(&line);
    let line = collapse_whitespace(&line);
    if line.is_empty() {
        return String::new();
    }
    let runes: Vec<char> = line.chars().collect();
    if runes.len() <= DETERMINISTIC_TITLE_LIMIT {
        return line;
    }
    let head: String = runes[..DETERMINISTIC_TITLE_LIMIT - 1].iter().collect();
    format!("{}…", head.trim_end())
}

/// 去掉媒体占位行（媒体首轮用它，免得标题变成 `[Image]`）。
pub fn without_media_placeholder_lines(body: &str) -> String {
    body.split('\n')
        .filter(|line| !MEDIA_PLACEHOLDER_LINES.contains(&line.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 首条消息的标题：带媒体的轮次先摘掉占位行（占位行本身不构成标题）。
pub fn derive_first_message_title(body: &str, has_media: bool) -> String {
    let body = if has_media {
        without_media_placeholder_lines(body)
    } else {
        body.to_string()
    };
    derive_chat_title(&body)
}

/// 标题的**源**：优先用户自己打的字（`command_text`），而不是（可能被富化的）正文。
///
/// 只从 `command_text` 里消费一条**已经生效**的 fresh 指令：`/clear` 的正文整体被
/// 产品消费掉了（`consumed_fresh` 为真）才剥指令；否则 `/clear …` 是普通文本，
/// 原样进标题（否则一个以 `/clear` 打头的 `/new` 正文会被悄悄吞掉）。
pub fn chat_title_source(body: &str, command_text: &str, consumed_fresh: bool) -> String {
    if !command_text.trim().is_empty() {
        if consumed_fresh {
            if let Some(current) = parse_fresh_session_command(command_text) {
                return current;
            }
        }
        return command_text.to_string();
    }
    body.to_string()
}

/// 纯媒体轮次的标题（没有附件名时用种类名兜底）。
pub fn media_type_title(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Image => "Image chat",
        MessageKind::Audio => "Audio chat",
        MessageKind::Video => "Video chat",
        _ => "File chat",
    }
}

/// Markdown 围栏 ```` ``` … ``` ```` → 单个空格（Go 的 `.` 不吃换行 ⇒ 本行内成对）。
fn strip_markdown_fences(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("```") {
        let after = &rest[start + 3..];
        match after.find("```") {
            Some(end) => {
                out.push_str(&rest[..start]);
                out.push(' ');
                rest = &after[end + 3..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// Markdown 标记字符（`#`、`*`、反引号、`>`、`~`、`_`）全删（上游 `markdownMarks`，逐字）。
fn strip_markdown_marks(line: &str) -> String {
    line.chars()
        .filter(|c| !matches!(c, '#' | '*' | '`' | '>' | '~' | '_'))
        .collect()
}

/// `[text](url)` / `![text](url)` → `text`。
fn strip_markdown_links(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    while index < chars.len() {
        let open = if chars[index] == '[' {
            Some(index)
        } else if chars[index] == '!' && index + 1 < chars.len() && chars[index + 1] == '[' {
            // `![alt](url)`：连 `!` 一起吃掉。
            Some(index)
        } else {
            None
        };
        let Some(open) = open else {
            out.push(chars[index]);
            index += 1;
            continue;
        };
        let label_start = if chars[open] == '!' {
            open + 2
        } else {
            open + 1
        };
        let Some(close_rel) = chars[label_start..].iter().position(|c| *c == ']') else {
            out.push(chars[index]);
            index += 1;
            continue;
        };
        let close = label_start + close_rel;
        let is_link = chars.get(close + 1) == Some(&'(')
            && chars
                .get(close + 2..)
                .is_some_and(|rest| rest.iter().position(|c| *c == ')').is_some());
        if !is_link {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        out.extend(chars[label_start..close].iter());
        let close_paren = close + 2 + chars[close + 2..].iter().position(|c| *c == ')').unwrap();
        index = close_paren + 1;
    }
    out
}

/// 把空白串折叠成一个空格并 trim 两端（上游 `titleSpace` 语义）。
fn collapse_whitespace(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_space = false;
    for rune in line.chars() {
        if rune.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push(' ');
        }
        in_space = false;
        out.push(rune);
    }
    out
}

// =====================================================================
// 出处（provenance）：任务输入是否来自渠道
// =====================================================================

/// 一个**已完成**的 chat 任务是否以渠道入站为输入 ⇒ 它的回复（或失败通知）属于外部平台。
///
/// 直接（web / mobile）任务可以复用渠道绑定的会话，但它们的回复留在 Multica（上游 MUL-4988）。
/// **只看 `chat_input_task_id` 判不出来**：密封的渠道任务同样拥有一个输入批次。判据是那条
/// 被拥有的批次上不可变的 `channel_ingested` 戳，并且按批次的**所有者** id 查（自动重试克隆
/// 会继承 `chat_input_task_id`，而它的消息仍挂父任务的戳）⇒ 克隆与父任务得到同一个结论。
/// `chat_input_task_id` 为空 = 密封之前的渠道任务（直接任务自 MUL-4351 起一直拥有自己的批次）
/// ⇒ 保持 #5645 的"默认投递"行为。
///
/// ⚠️ 批次那条查询（`TaskHasChannelIngestedMessages`）落在 `mc-repos` 的任务面，
/// **不属于**本片写集 ⇒ 这里是纯判据，调用方把查询结果传进来。
pub fn task_input_is_channel_ingested(
    chat_input_task_id: Option<Id>,
    batch_has_channel_ingested_messages: bool,
) -> bool {
    if chat_input_task_id.is_none() {
        return true;
    }
    batch_has_channel_ingested_messages
}

#[cfg(test)]
mod tests;
