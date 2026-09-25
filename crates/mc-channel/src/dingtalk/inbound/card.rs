//! 引用卡片的投影 + 富文本载荷的解码 + 网页 URL 扫描（上游 `inbound_card.go` 108 行）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//! - 父模块的模块文档列了三个文件的分工；本文件是其中"投影"的那一半。
//!
//! # 这份投影是**快照**的，不是模板的
//!
//! 投影的是**观察到的渲染卡片快照**（RICHTEXT 子节点序列），**不是**模板的
//! `cardParamMap`（拿模板重新渲染会得到"看起来合理但从未显示过"的文本 —— 上游为此专门写过
//! 反例用例）。`UNKNOWN` 子节点被排除在选中的正文之外：捕获到的群快照把**来源署名与布局**
//! 放在那里，我们**故意**不解码它们的序列化值。
//!
//! 已观察到的 RICHTEXT 子节点只有四种形态：`TEXT`（行内 run，不一定是段落）、
//! `LINK`（目的字符串，原样保留）、`IMAGE`（不透明 code，**不是**可下载的媒体）、
//! `UNKNOWN`（布局 / 来源数据）。**别推断别的节点形态** —— 公开的机器人消息类型表不是
//! 卡片节点的 schema。
//!
//! # `web_url_spans` 是**跨片共享件**（别各写一份）
//!
//! 上游把 URL 扫描放在 `markdown.go`（本地归 **M7-8**），但本文件的引用投影要用它
//! （"紧贴 URL 边界的一段文本不能并进 URL 的目的地"这条规则）。所以本文件把它作为
//! `pub` 件落在这里，M7-8 的 `markdown.rs` **复用它**而不是再写一份扫描器（交接写在
//! `docs/32` §19 的交接节）。
//!
//! # 与上游的三处形态差异（登记 `docs/32` §19 的 D 项）
//!
//! 1. **没有 `regex`**：上游的 `webURLStart` 是一条正则（`(?i)(?:https?://|www\.)`）。
//!    本 crate 的依赖集在 M7-0 之后冻结（`docs/60` §2.2）⇒ 三个字面量手写成"取最左匹配"
//!    的扫描器，语义逐条等价（见 [`web_url_start_at`] 的注释）；
//! 2. **JSON 表示**：上游的 `json.RawMessage` 在本仓是 [`serde_json::Value`]
//!    （`mc_core::channel::message` 的模块文档已定下这条）；
//! 3. **`IMAGE` 的 `downloadCode` 被忽略**（上游逐字）：快照里给的是不透明 code，
//!    它过不了机器人的文件下载 API ⇒ 只保留**位置**（占位符），不把它当成可下载媒体。

use serde_json::Value;

use super::{RichTextItem, IMAGE_PLACEHOLDER};

/// 卡片内容读不出来时的统一占位（上游 `renderDingTalkQuotedCard` 的 `unavailable`）。
pub const UNAVAILABLE: &str = "[quoted content unavailable]";

// =====================================================================
// 引用卡片（RICHTEXT 子节点序列）
// =====================================================================

/// 把观察到的卡片快照投影成普通正文（上游 `renderDingTalkQuotedCard`）。
///
/// 逐条规则（都是上游注释里的判据，不是风格选择）：
///
/// - 非数组 / 空数组 ⇒ [`UNAVAILABLE`]；
/// - 块不是 `RICHTEXT` 或没有 children ⇒ [`UNAVAILABLE`] 一行；
/// - 子节点不是对象、或类型不是 `TEXT`/`LINK`/`IMAGE` ⇒ [`UNAVAILABLE`] 一行
///   （`UNKNOWN` **只**跳过，不产生标记 —— 它是布局/来源数据，不是丢失的内容）；
/// - `TEXT` / `LINK` 的 `value` 必须是字符串；含 `||` 的值按
///   [`readable_quoted_text`] 的保守策略变成 [`UNAVAILABLE`]（当前输入从不被过滤）；
/// - `LINK` 的目的地原样保留，**紧贴**在相邻 run 之后时补一个换行（一个 URL 不能把
///   后面那个 run 吸进它的目的地）；
/// - `IMAGE` 变成 [`IMAGE_PLACEHOLDER`]（位置保留，code 丢弃）。
#[must_use]
pub fn render_dingtalk_quoted_card(card_content: &Value) -> String {
    let Some(blocks) = card_content.as_array() else {
        return UNAVAILABLE.to_string();
    };
    if blocks.is_empty() {
        return UNAVAILABLE.to_string();
    }
    let mut body = String::new();
    for raw in blocks {
        let Some(block) = raw.as_object() else {
            mark_missing(&mut body);
            continue;
        };
        let element_type = block.get("elementType").and_then(Value::as_str);
        let children = block.get("children").and_then(Value::as_array);
        let (Some("RICHTEXT"), Some(children)) = (element_type, children) else {
            mark_missing(&mut body);
            continue;
        };
        if children.is_empty() {
            mark_missing(&mut body);
            continue;
        }
        let mut block_started = false;
        for raw_child in children {
            let Some(node) = raw_child.as_object() else {
                mark_missing(&mut body);
                continue;
            };
            let node_type = node
                .get("elementType")
                .and_then(Value::as_str)
                .unwrap_or("");
            if node_type == "UNKNOWN" {
                continue;
            }
            if !block_started && !body.is_empty() {
                body.push_str("\n\n");
            }
            block_started = true;
            match node_type {
                "TEXT" | "LINK" => {
                    append_readable_run(&mut body, node, node_type);
                }
                "IMAGE" => {
                    if !body.is_empty() && !body.ends_with('\n') {
                        body.push('\n');
                    }
                    body.push_str(IMAGE_PLACEHOLDER);
                    body.push('\n');
                }
                _ => mark_missing(&mut body),
            }
        }
    }
    let result = body.trim();
    if result.is_empty() {
        UNAVAILABLE.to_string()
    } else {
        result.to_string()
    }
}

/// 追加一个 `TEXT` / `LINK` run（上游那个 case 的逐条对应）。
fn append_readable_run(body: &mut String, node: &serde_json::Map<String, Value>, node_type: &str) {
    let Some(value) = node.get("value").and_then(Value::as_str) else {
        mark_missing(body);
        return;
    };
    // 对**平台给的原始值**施加既有的保守策略；相邻节点与生成的图片占位符都保留。
    if readable_quoted_text(value) != value {
        mark_missing(body);
        return;
    }
    if node_type == "LINK" {
        if value.trim().is_empty() {
            mark_missing(body);
            return;
        }
        // 观察到的目的地原样保留，与相邻的 run 分开（link label/URL 对象不是已知形态）。
        if !body.is_empty() && !body.ends_with(char::is_whitespace) {
            body.push('\n');
        }
    }
    // URL 边界上的一段文本不能并进 URL 的目的地；行内的强调切分其余情况下照旧拼接。
    let spans = web_url_spans(body);
    if !value.is_empty() && spans.last().is_some_and(|(_, end)| *end == body.len()) {
        body.push('\n');
    }
    body.push_str(value);
}

/// 记一次"这块内容不可用"（上游 `missing()` 的逐条对应）。
///
/// 已经以 [`UNAVAILABLE`] 结尾时**不重复**追加（否则一行坏节点会堆出一串标记）。
fn mark_missing(body: &mut String) {
    if body.ends_with(UNAVAILABLE) {
        return;
    }
    if body.ends_with(UNAVAILABLE_LINE) {
        return;
    }
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(UNAVAILABLE_LINE);
}

/// [`UNAVAILABLE`] + 换行（上游拼的是 `unavailable + "\n"`）。
const UNAVAILABLE_LINE: &str = "[quoted content unavailable]\n";

/// 保守的引用文本策略（上游 `dingTalkReadableQuotedText`）。
///
/// 公开示例里出现过 `||` 当分隔符，但那**不**证明长度 / 字母表 / 版本 / 尾部字段数 —— 所以
/// 含这个歧义分隔符的选中文本一律判为不可用（**当前输入永不被过滤**）。代价是"引用里含
/// `||` 的合法代码 / 散文也读不出来"，这条取舍在上游的 PR #8061 评审里被接受。
///
/// 只对**平台给的文本值**施加；不要拿它过滤已经渲染好的引用块（那会丢掉生成的图片标记
/// 与它们的媒体位次）。
#[must_use]
pub fn readable_quoted_text(value: &str) -> String {
    if value.contains("||") {
        UNAVAILABLE.to_string()
    } else {
        value.to_string()
    }
}

// =====================================================================
// 富文本载荷解码
// =====================================================================

/// `msgtype=richText` 的 `content` 载荷（上游 `richTextContent`）。
///
/// `None` = **解码失败**（非对象 / `richText` 不是数组）；`Some(vec![])` = 字段缺席或为空
/// —— 两者的处置在上游是同一支（`len(rc.RichText) == 0` ⇒ 不可用占位），调用方两条都查。
#[must_use]
pub fn rich_text_content(content: &Value) -> Option<Vec<RichTextItem>> {
    let object = content.as_object()?;
    match object.get("richText") {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(value) => value.as_array().map(|array| {
            array
                .iter()
                .map(|item| RichTextItem::from_value(item, false))
                .collect()
        }),
    }
}

/// 一组有序的富文本项（上游 `unmarshal` 一个 `[]json.RawMessage` 的那一段）。
///
/// 非数组 ⇒ 空集（与上游 `json.Unmarshal` 进 `[]json.RawMessage` 失败同款 —— 调用方那边
/// 同样落到"不可用"）。
#[must_use]
pub fn rich_text_items(value: Option<&Value>, quoted: bool) -> Vec<RichTextItem> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .map(|item| RichTextItem::from_value(item, quoted))
        .collect()
}

/// `msgtype=picture` 的 `content` 载荷（上游 `pictureContent`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PictureContent {
    pub download_code: String,
    pub picture_download_code: String,
}

/// 解码 `msgtype=picture` 的载荷；`None` = 解码失败（非对象 / 某个码不是字符串）。
///
/// 真实的回调可能只带两个码中的一个（`downloadCode` 或 `pictureDownloadCode`），两者都经
/// `messageFiles/download` 解析 ⇒ 缺一个不算失败，两个都缺由调用方判（`ref == ""`）。
#[must_use]
pub fn picture_content(content: &Value) -> Option<PictureContent> {
    let object = content.as_object()?;
    Some(PictureContent {
        download_code: string_code(object.get("downloadCode"))?,
        picture_download_code: string_code(object.get("pictureDownloadCode"))?,
    })
}

/// 读一个字符串码：缺席 / `null` ⇒ 空串（Go 把 `null` 解进 `string` 是零值且不报错）；
/// 其它类型 ⇒ `None`（Go 报错 ⇒ 整个载荷解码失败）。
fn string_code(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => Some(String::new()),
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => None,
    }
}

// =====================================================================
// 网页 URL 扫描（上游 `markdown.go` 的 `webURLStart` + `webURLSpans`）
// =====================================================================

/// 找 `body[start..]` 里最靠左的网页 URL 起点，返回 `(匹配起, 内容起)`。
///
/// 上游是一条正则 `(?i)(?:https?://|www\.)`：最左匹配，且同一位置上的备选按模式顺序。
/// 三个字面量手写扫描（见模块文档差异 1）：`to_ascii_lowercase` 只改 ASCII 字母、**逐字节等长**
/// ⇒ 在它上面取的字节下标对原串同样成立。
fn web_url_start_at(body_lower: &str, from: usize) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    // 顺序 = "同一位置时更长的匹配优先"（`https://` 在 `http://` 之上，与正则的贪婪 `s?` 一致）。
    for (needle, length) in [("https://", 8usize), ("http://", 7), ("www.", 4)] {
        if let Some(position) = body_lower[from..].find(needle) {
            let start = from + position;
            if best.is_none_or(|(current, _)| start < current) {
                best = Some((start, start + length));
            }
        }
    }
    best
}

/// 正文里所有网页 URL 的**源跨度**（上游 `webURLSpans`）。
///
/// 这是**源跨度**，不是"解析后重新序列化"的 URL：原始拼写、编码、query、fragment 都要保留
/// （`DingTalk` 会自动把引用文本里的 URL 变成链接）。
///
/// 终止规则（逐条照上游）：空白 / 控制字符 / `<` `>` `"` `\` / 反引号 一律终止；
/// `(` `[` 入栈并要求配对的
/// `)` `]` 闭合，**不配对**的闭合括号就地终止（所以
/// `https://example.com/a_(b)?x=a_b+c&y=2#part_2` 是**一个**完整跨度）。
#[must_use]
pub fn web_url_spans(body: &str) -> Vec<(usize, usize)> {
    let lower = body.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut start = 0usize;
    while start < body.len() {
        let Some((match_start, content_start)) = web_url_start_at(&lower, start) else {
            break;
        };
        let mut end = body.len();
        let mut closing: Vec<char> = Vec::new();
        for (offset, character) in body[content_start..].char_indices() {
            if character.is_whitespace()
                || character.is_control()
                || matches!(character, '<' | '>' | '"' | '\\' | '`')
            {
                end = content_start + offset;
                break;
            }
            match character {
                '(' => closing.push(')'),
                '[' => closing.push(']'),
                ')' | ']' => {
                    if closing.last() != Some(&character) {
                        end = content_start + offset;
                        break;
                    }
                    closing.pop();
                }
                _ => {}
            }
        }
        spans.push((match_start, end));
        start = end;
    }
    spans
}

#[cfg(test)]
mod tests;
