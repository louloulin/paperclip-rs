//! 成员写的文本不许变成**机器人签名**的 Markdown（上游
//! `internal/integrations/wecom/markdown.go`，**456 行**）。
//!
//! - **写者**：M7-19（`LUM-1784` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：本文件被收件箱卡片（`inbox_message.go`）与 `/issue`
//!   确认（`replier.go`）**共用** —— 两者都把别人的话拼进一条以机器人名义发出的消息。
//!
//! # 两段闸，一个入口
//!
//! [`break_member_links`] 是**唯一**入口，它按顺序上两道闸：
//!
//! | 段 | 挡住的构造 | 上游函数 |
//! | --- | --- | --- |
//! | 行内邻接 | `[点这里](http://evil.example)` | `breakLinkAdjacency`（复用 M7-1 的 [`crate::message::break_markdown_link_adjacency`]） |
//! | 引用定义 | `[重置密码]: https://evil.example` + 下一行 `[重置密码]` | `breakLinkReferenceDefinitions`（**本文件**，约两百行） |
//!
//! 两段都是**纯插入一个空格**，所以两个顺序等价、合起来幂等：谁也不会造出另一个要找的形态。
//! 一个入口而不是两个，理由逐字来自上游：**一个调用点不可能只取一段、忘掉另一段**。
//!
//! # 本片接的是 M7-17 的交接 H2（`docs/32` §34.4）
//!
//! M7-17 先落了 [`crate::wecom::replier::MemberLinks`] 端口 + 只覆盖**行内邻接**的现成实现
//! [`crate::wecom::replier::AdjacencyBreaker`]，并把「引用定义那一段」明确交接给本片
//! （`docs/32` §34 的 D6 + R3）。本文件交付：
//!
//! 1. 两段都覆盖的纯函数 [`break_member_links`]；
//! 2. 端口的生产实现 [`MemberLinkBreaker`]（`replier.rs` 用 `with_member_links` 挂上它，
//!    于是 R3 那条"降级"消失：`/issue` 确认里的标题不再被整个省掉）。
//!
//! # 为什么"分离"而不是"转义"（上游逐字，别改回去）
//!
//! 反斜杠转义在这个渲染器上**不可用**：对活租户实测，`\[Bug\]` 回来是一个斜体的衬线 "Bug"
//! 且方括号消失（那是行内数学块的渲染方式），而**不渲染 Markdown** 的会话列表预览里反斜杠
//! 原样可见。所以本文件**绝不**吐出一个反斜杠。
//!
//! # 上游刻意放过的（照抄，不是遗漏）
//!
//! - **相对目标**：`[文件]: report.pdf`、`[页面]: /inbox`、`[Bug]: 登录失败` 仍然构成引用定义
//!   —— 但指向客户端**自己的**基址，指不到攻击者的服务器。"只能指向别的主机的目标才破"就是
//!   这条规则的线；
//! - `[foo]:(https://evil.example)`：`CommonMark` 把括号留在 href 里，不是任何人会导航去的 URL
//!   （前导 `(` 仍会被跳过，因为跳过它零成本）。
//!
//! # 哪里会误伤，以及为什么误伤是安全的那一侧
//!
//! 本实现把块容器只建模成 `>` 的**个数**，所以一个 `CommonMark` 会折进上一段、或读成缩进代码块的
//! `[x]: https://…` 行上它也会触发；也不检查规范那条"目标之后不许有别的东西"，于是
//! `[x]: https://evil.example 请点击` 也会被破。每一个的代价都是**一行本来就带 URL 的行上多一个
//! 空格**；而漏判的代价是机器人名字底下一条**能点的链接**。近似朝这一侧倒，是故意的。

use crate::message::break_markdown_link_adjacency;
use crate::wecom::replier::MemberLinks;

/// 上游 `breakMemberLinks`：把成员写的文本塞进机器人签名的消息之前**唯一**要跑的入口。
///
/// 两段闸都上（见模块文档的表）；两道都是纯插入，所以调用顺序不重要、且整体幂等。
#[must_use]
pub fn break_member_links(text: &str) -> String {
    break_link_reference_definitions(&break_link_adjacency(text))
}

/// 行内邻接那一段（上游 `breakLinkAdjacency`）：`](` → `] (`。
///
/// 一个链接只在 `]` 与 `(` **紧邻**时才成形（`CommonMark` 要求链接文本**紧接** `(`；替真正解析器
/// 的那些朴素重写器也要求），图片语法 `![x](u)` 需要同样的邻接 ⇒ 一条规则全覆盖。不含 `](` 的
/// 文本**逐字节**原样返回 —— 常见的 `[Bug] 登录失败` 标题因此一字不动。
///
/// 插入的空格**不可能**出现在行首（它前面永远有个同一行的 `]`），所以不会开出缩进代码块、
/// 也不会变成行尾硬换行。
#[must_use]
pub fn break_link_adjacency(text: &str) -> String {
    break_markdown_link_adjacency(text)
}

/// 引用定义那一段（上游 `breakLinkReferenceDefinitions`）：让成员写不出一个机器人卡片随后会
/// **解析**的链接。
///
/// 上游对活租户实测的形态：评论正文里
///
/// ```text
/// [重置密码]: https://evil.example
/// [重置密码]
/// ```
///
/// 回来时第一行被当成定义**吞掉**、第二行渲染成一条指向 `evil.example` 的蓝色下划线链接。
/// 里面没有任何邻接，所以行内那一段原样放过整个攻击。杀掉定义就同时杀掉快捷式 `[label]`、
/// 折叠式 `[label][]` 与完整式 `[text][label]` —— 三者查同一张表，而没有定义时一个都解析不了。
///
/// # 规则（三条同时成立才断）
///
/// 1. 该行 `[` 之前**只有块脚手架** —— 缩进、`>` 引用标记、列表符号。必要而非装饰：引用块或
///    列表项里的定义**仍然**填充整篇文档的引用表，并在块外解析；
/// 2. 方括号构成一个 `CommonMark` 链接标签：第一个未转义的 `]` 关闭它，里面没有未转义的 `[`，
///    至少一个非空白字符，最多 999 个（标签可以跨行且仍然解析，所以扫描也跨行）；
/// 3. 冒号之后**像一个链接目标** —— 见 [`looks_like_link_destination`]。
///
/// 第 1 与第 3 条是同一个问题的两半，由同一个答案保持诚实：[`container_prefix_before`] 报出
/// 托着标签的块容器，[`looks_like_link_destination`] 拿到**同一份** container prefix 才能在目标
/// 落在下一行时回退进那些容器。只教一半认得某个容器，正是让一条定义活下来的原因。
///
/// # 断点落在哪
///
/// 落在 `]` 与 `:` **之间**。`CommonMark` 要求冒号紧接标签，所以一个空格就结束这条定义。
/// 它不能落进标签里（引用标签按空白折叠 + 大小写折叠后匹配，`[ 重置密码]` 仍然匹配
/// `[重置密码]`，定义照样活）；不能是反斜杠（理由见模块文档）；也不能落在 `[` 之前
/// （没有任何字符能把一行推出块位置而不自己显示成内容、或在四个空格之后开一个代码块）。
///
/// 与行内那一段一样，每次出现**多一个 rune** ⇒ 调用方若在预算一个长度上限，必须**先**调
/// 本函数**再**量长度；而它的输出里不含还能再次触发的 `]:`，所以第二遍是 no-op。
#[must_use]
pub fn break_link_reference_definitions(text: &str) -> String {
    if !text.contains("]:") {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut prev = 0;
    let mut index = 0;
    while index < text.len() {
        if text.as_bytes()[index] != b'[' {
            index += 1;
            continue;
        }
        let Some(container) = container_prefix_before(text, index) else {
            index += 1;
            continue;
        };
        let Some(end) = link_label_end(text, index) else {
            index += 1;
            continue;
        };
        // 标签里没有未转义的 `[`，所以里面不可能再开一个 —— 两种情况都从这个标签之后接着扫。
        index = end;
        if text.as_bytes().get(end + 1) != Some(&b':') {
            index += 1;
            continue;
        }
        if !looks_like_link_destination(&text[end + 2..], container) {
            index += 1;
            continue;
        }
        out.push_str(&text[prev..=end]);
        out.push(' ');
        prev = end + 1;
        index += 1;
    }
    if prev == 0 {
        return text.to_owned();
    }
    out.push_str(&text[prev..]);
    out
}

/// 一行块脚手架开出来的东西：这一行所在的**引用块嵌套深度**。
///
/// 这是规则拥有的**唯一**一种容器概念，而规则的两半都从它出发 —— 决定标签前面能有什么的那一半
/// 产出它，扫过冒号的那一半重放它。谁也没法被教会一个对方不认识的容器，这正是它作为一个**值**
/// 而不是一个 `bool` 存在的全部理由。
///
/// 只数 `>` 标记。缩进与列表符号也是脚手架，但它们不需要重放：`CommonMark` 用普通缩进延续一个
/// 列表项，而目标扫描本来就把缩进当空白跨过去。引用块不同 —— 它**每一行**都重复自己的标记，
/// 于是落在下一行的目标到达时带着 `> `，一个不预期这个标记的扫描会把它读成目标本身。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContainerPrefix {
    /// `>` 标记的个数。
    pub quotes: usize,
}

/// 上游 `containerPrefixBefore`：报出托着 `text[index]` 那个 `[` 的容器，以及它那一行从行首到
/// `index` 之间**是不是**全为块脚手架（缩进、`>`、列表符号）。出现任何别的东西（一个词、一个
/// `**`）就意味着这个 `[` 在一段散文里，而散文里开不出定义。
///
/// 它只往回走到第一个不可能是脚手架的字节，所以散文中间的一个 `[` 由读**一个**字节就判定，
/// 一篇满是 `[` 的正文保持线性。
///
/// 返回 `None` = 这个 `[` 不在块位置（上游的 `ok == false`）。
#[must_use]
pub fn container_prefix_before(text: &str, index: usize) -> Option<ContainerPrefix> {
    debug_assert!(index < text.len());
    let bytes = text.as_bytes();
    let mut start = index;
    while start > 0 && is_block_scaffold_byte(bytes[start - 1]) {
        start -= 1;
    }
    if start > 0 && bytes[start - 1] != b'\n' {
        return None;
    }
    parse_container_prefix(&text[start..index])
}

/// 上游 `parseContainerPrefix`：把 `prefix` 读成一整段块脚手架，报出它开出来的容器。
/// 里面有任何不是脚手架的东西 ⇒ `None`。
#[must_use]
pub fn parse_container_prefix(prefix: &str) -> Option<ContainerPrefix> {
    let bytes = prefix.as_bytes();
    let mut quotes = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' => i += 1,
            b'>' => {
                quotes += 1;
                i += 1;
            }
            b'-' | b'+' | b'*' if i + 1 < bytes.len() && matches!(bytes[i + 1], b' ' | b'\t') => {
                i += 2;
            }
            _ => {
                // 有序列表标记：至多 9 位数字，然后 `.` 或 `)`，然后一个空格。
                let mut digits = 0;
                while i + digits < bytes.len() && digits < 9 && bytes[i + digits].is_ascii_digit() {
                    digits += 1;
                }
                if digits == 0
                    || i + digits + 1 >= bytes.len()
                    || !matches!(bytes[i + digits], b'.' | b')')
                    || !matches!(bytes[i + digits + 1], b' ' | b'\t')
                {
                    return None;
                }
                i += digits + 2;
            }
        }
    }
    Some(ContainerPrefix { quotes })
}

/// 上游 `skipContinuationPrefix`：在一条延续行的开头跨过 `prefix` 里那些容器的标记，返回这一行
/// 内容的起点。它是 [`container_prefix_before`] 的重放那一半。
///
/// 它**至多**吃掉 `prefix.quotes` 个标记，绝不多吃。更少是允许的 —— 引用块接受惰性延续行
/// （`> [x]:` 后面跟一行**裸的** `https://…` 仍然定义引用）。更多则根本不是这条定义的延续
/// —— 比它所在的块更深的一行开的是一个**新**块 —— 所以扫描停下，让多出来的 `>` 自己说话，
/// 也就是"它不是目标"。
#[must_use]
pub fn skip_continuation_prefix(text: &str, index: usize, prefix: ContainerPrefix) -> usize {
    let bytes = text.as_bytes();
    let mut i = skip_spaces_tabs(bytes, index);
    for _ in 0..prefix.quotes {
        if i >= bytes.len() || bytes[i] != b'>' {
            return i;
        }
        i = skip_spaces_tabs(bytes, i + 1);
    }
    i
}

/// 脚手架前缀能取用的字符集 —— 是 [`parse_container_prefix`] 精确解析的那组标记的**超集**。
/// 它只决定往回走多远；这一段到底是不是脚手架由 `parse_container_prefix` 回答。
fn is_block_scaffold_byte(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'>' | b'-' | b'+' | b'*' | b'.' | b')' | b'0'..=b'9'
    )
}

/// 上游 `linkLabelEnd`：返回从 `text[open]` 开始的链接标签那个闭合 `]` 的字节偏移。
///
/// 跟随 `CommonMark`：第一个未转义的 `]` 关闭它；出现未转义的 `[` 就取消资格；至少要有**一个**
/// 非空白字符、至多 999 个；可以跨行结束。
///
/// 上限数的是 **rune**（上游 `utf8.DecodeRuneInString` 的语义）：一个 999 个汉字的标签是 2997
/// 个字节，按字节数会在第三个字附近就判超限，而一个 999 rune 的标签**是**合法标签。
///
/// 返回 `None` = `text[open]` 这里不是一个标签（含"没闭合"）。
#[must_use]
pub fn link_label_end(text: &str, open: usize) -> Option<usize> {
    const MAX_LABEL_RUNES: usize = 999;
    let bytes = text.as_bytes();
    let mut runes = 0_usize;
    let mut has_content = false;
    let mut i = open + 1;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b']' {
            return has_content.then_some(i);
        }
        if byte == b'[' {
            return None;
        }
        runes += 1;
        if runes > MAX_LABEL_RUNES {
            return None;
        }
        if byte.is_ascii() {
            // 反斜杠把它后面那个字符转义掉，所以 `\]` 不关标签、`\[` 也不取消资格。
            if byte == b'\\' && i + 1 < bytes.len() {
                i += 1 + next_char_len(text, i + 1);
                runes += 1;
                has_content = true;
                continue;
            }
            if !matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                has_content = true;
            }
            i += 1;
            continue;
        }
        // 非 ASCII：整块跳过一个 rune（它不可能是 `[` / `]` / `\`，也不可能是 ASCII 空白）。
        let size = next_char_len(text, i);
        if size == 0 {
            // 非法 UTF-8 的孤立续字节：当作一个普通字符吃掉，不 panic。
            i += 1;
            has_content = true;
            continue;
        }
        has_content = true;
        i += size;
    }
    None
}

/// `text[index]` 处那个字符占的字节数；`index` 越界或不是字符起点 ⇒ 0。
///
/// 用 `decode_utf8` 而不是 `text[index..].chars().next()`：后者对非字符边界**panic**，而这里
/// 的 `index` 来自一次 `\\` 之后的偏移，喂进来一段坏字节时不该把读循环带下水。
fn next_char_len(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return 0;
    }
    std::str::from_utf8(&text.as_bytes()[index..])
        .ok()
        .and_then(|rest| rest.chars().next())
        .map_or(0, char::len_utf8)
}

/// 上游 `looksLikeLinkDestination`：`rest`（`[label]:` 之后的全部）是否开出一个**能把读者送到
/// 另一个主机**的链接目标。就是这条判据让 `[Bug]: 登录失败` 保持完整。
///
/// 一个目标合格，当它带 scheme（`https:`，也包括 `javascript:` / `data:`）、或者是 scheme 相对的
/// （`//host/path`）、或者用了能把这两种拼出来的转义机制。
///
/// `prefix` 是定义自己那一行所在的容器，做成参数而不是在这里重新发现，理由写在
/// [`ContainerPrefix`] 上：目标在下一行时，它到达时带着同一批标记，而一个越过冒号却不带
/// 这些标记的扫描会把标记读成目标，从而放过一条**会解析**的定义。
#[must_use]
pub fn looks_like_link_destination(rest: &str, prefix: ContainerPrefix) -> bool {
    let bytes = rest.as_bytes();
    let mut i = skip_spaces_tabs(bytes, 0);
    // CommonMark 允许冒号与目标之间有至多一个换行，所以定义可以跨两行。第二个换行结束这个块、
    // 也就没有定义 —— 把换行留在原地让它自然落空即可，因为一个换行永远不是一个目标的开始。
    if i < bytes.len() && bytes[i] == b'\r' {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\n' {
        i = skip_continuation_prefix(rest, i + 1, prefix);
    }
    if i < bytes.len() && matches!(bytes[i], b'<' | b'(') {
        // `<…>` 是规范里的尖括号目标形态，而它可能带前导空格（客户端会把它们从 URL 上剥掉）。
        i = skip_spaces_tabs(bytes, i + 1);
    }
    let dest = &rest[i..];
    let dest = match dest.find([' ', '\t', '\r', '\n']) {
        Some(end) => &dest[..end],
        None => dest,
    };
    if dest.is_empty() {
        return false;
    }
    if dest.starts_with("//") || has_uri_scheme(dest) {
        return true;
    }
    // 目标里认得反斜杠转义与字符引用，而 `https\://evil.example`、`\/\/evil.example` 与
    // `&#x68;ttps://evil.example` 三者都解析成一条能用的跨主机 URL（对 CommonMark 解析器逐个验过）。
    // 与其解码它们，不如把**用到了这套机制**本身当作目标。
    //
    // 是机制，不是字符。孤零零一个 `\` 或 `&` 什么也拼不出来：`R&D`、`\d+`、`docs\setup`
    // 与 `foo&bar` 都是相对目标 —— 本函数承诺原样保留它们 —— 而它们不含任何解析器会动的转义。
    has_backslash_escape(dest) || has_character_reference(dest)
}

/// 上游 `hasBackslashEscape`：`s` 里是否用到了 `CommonMark` 反斜杠转义 —— 一个 `\` 后面跟 ASCII
/// 标点，那是反斜杠**唯一**有意义的位置。在 `\d+` 与 `docs\setup` 里它保持字面文本。
#[must_use]
pub fn has_backslash_escape(text: &str) -> bool {
    let bytes = text.as_bytes();
    (0..bytes.len().saturating_sub(1)).any(|i| bytes[i] == b'\\' && is_ascii_punct(bytes[i + 1]))
}

/// `CommonMark` 的 ASCII 标点集 —— 每个既不是字母、也不是数字、也不是空格的 ASCII 字符。
fn is_ascii_punct(byte: u8) -> bool {
    matches!(byte, b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{'..=b'~')
}

/// 上游 `hasCharacterReference`：`s` 里是否有一个字符引用 —— `&`、一个名字或以 `#` 开头的数字
/// 体、再一个 `;`。`R&D` 与 `foo&bar` 没有 `;` 来关闭一个引用，所以里面什么都不会被解码。
///
/// 不与 HTML5 的名字表核对：一个未知名字解码成空，而破掉它的代价是一个空格 —— 本文件其余部分
/// 本来就跑在这一侧。
#[must_use]
pub fn has_character_reference(text: &str) -> bool {
    let bytes = text.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte != b'&' {
            continue;
        }
        for (offset, inner) in bytes[i + 1..].iter().enumerate() {
            if *inner == b';' {
                if offset > 0 {
                    return true;
                }
                break;
            }
            if !(inner.is_ascii_alphanumeric() || *inner == b'#') {
                break;
            }
        }
    }
    false
}

/// 上游 `hasURIScheme`：`s` 是否以一个 URI scheme 加 `:` 开头 —— 一个字母，然后字母、数字、
/// `+`、`-` 或 `.`（RFC 3986）。
#[must_use]
pub fn has_uri_scheme(text: &str) -> bool {
    for (i, byte) in text.bytes().enumerate() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b'0'..=b'9' | b'+' | b'-' | b'.' if i > 0 => {}
            b':' => return i > 0,
            _ => return false,
        }
    }
    false
}

fn skip_spaces_tabs(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }
    index
}

/// 上游 `strings.Split(rendered, "\n")` + 逐行加 `"> "` 的渲染器（`quotedContext` 的那一半，
/// 但**不含**首行 `[Quote]` 前缀 —— 那个前缀是入站归一化的词表，见 [`super::wecom_channel`]）。
///
/// 放在这里而不是入站文件里，是因为它的唯一目的就是**别让成员文本变成 Markdown**：把渲染结果
/// 变成引用块，正是让引用内容不进正文语法位置的手段。
#[must_use]
pub fn quote_lines(rendered: &str) -> String {
    let mut out = String::with_capacity(rendered.len() + 8);
    for (index, line) in rendered.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str("> ");
        out.push_str(line);
    }
    out
}

/// 端口的生产实现（M7-17 的 [`MemberLinks`] 端口 + 本片的两段闸）。
///
/// 宿主用 `WeComOutboundReplier::with_member_links(Arc::new(MemberLinkBreaker))` 挂上它，
/// `/issue` 确认里的标题就不再被整个省掉（`docs/32` §34 的 D6 / R3）。
#[derive(Debug, Clone, Copy, Default)]
pub struct MemberLinkBreaker;

impl MemberLinks for MemberLinkBreaker {
    fn break_links(&self, text: &str) -> String {
        break_member_links(text)
    }
}

#[cfg(test)]
mod tests;
