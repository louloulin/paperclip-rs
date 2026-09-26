//! **每一帧的选入式记录**（上游 `internal/integrations/wecom/trace.go`，**350 行**）。
//!
//! - **写者**：M7-20（`LUM-1785` / `docs/60-M7-PLAN.md` §3.3）。
//!
//! # 上游为什么需要它
//!
//! 上游逐字：这个包里**没有别的东西记录帧**。失败会记日志，而且只记失败 —— 一个坏信封、一个非零
//! 的服务端 ack。而一次**悄悄**走错的运行在服务端**什么都不留**：本该发给某个人却发进了房间的回复、
//! 被静默丢掉的命令、发了两遍的回执。今天要核这些得有人描述他在手机上看到的东西，那既慢又有损，
//! 而且对"顺序与时刻"这一类问题根本办不到。
//!
//! `MULTICA_WECOM_TRACE=1` 时服务端记下的东西够事后核一次真机会话：哪一帧往哪个方向去、发给哪个聊、
//! 那个聊是房间还是个人、服务端回了什么。这个开关还覆盖帧自己不带的**那一件** —— 附件自己的响应说
//! 它叫什么名字（[`trace_media_headers`]），而那件事在五分钟的媒体 URL 失效之后**再也取不回来**。
//!
//! # 为什么不用 `tracing::debug!`（上游逐字，**这条别改**）
//!
//! 上游注释逐字：兄弟平台给逐帧行用的是 `slog.Debug`，而 `logger.parseLevel` 把 `LOG_LEVEL`
//! 默认成 **debug** ⇒ 在**每一个**没设 `LOG_LEVEL` 的部署里，一次 `Debug` 调用都是**开着**的。
//! **消息正文不许默认进日志**，所以这个开关必须是它自己那一个，且默认关。
//!
//! # 关着的时候记录什么：什么都不记
//!
//! 这一个开关是**全部**判据：`tracing_on()` 为假时每一个入口返回 `None`，于是没有值、也没有 emit
//! （见 [`TraceFields`]）。上游逐字：*both lines are governed by that single decision, so the log
//! can never hold an attempt whose outcome was suppressed by the switch flipping halfway through a
//! write* —— 本仓把这条落成**同一个 `OutTrace` 值**同时管两行。
//!
//! # 记录的是**具名字段**，不是帧的转储
//!
//! 上游逐字：`aibot_subscribe` 的 body 里带智能机器人的 **secret**，整体转储会把它写进日志。
//! 本文件的 [`trace_out_fields`] 因此只读具名字段，并且**从不**下探 `aibot_subscribe` 的 body。
//! 加字段之前先看清每一个 `cmd` 在那个名字下放的是什么。
//!
//! # 两条**故意**不脱敏的字符串
//!
//! 附件的 `Content-Disposition` 原样值，以及从它解出来的文件名（[`trace_media_headers`]）。
//! 上游逐字：那一行的**全部意义**就是**确切的字节** —— 脱敏器会把任何 `token=…` 形状的东西改写，
//! 而一个文件**可以**合法地叫这个名字，被脱敏的文件名谁也没法诊断。而且那两样都不是凭据：头里带的
//! 是一个名字，而**真正**带凭据的那个预签名 URL 被**故意**排除在这一行之外（也不在
//! `media_download.rs` 输出的任何一行里）。
//!
//! # 本仓的形态差异（登记 `docs/32` §38 的 D4）
//!
//! 1. **`slog` → `tracing`，且没有 `regex`**：依赖集在 M7-0 之后冻结（`docs/60` §3.1）⇒
//!    [`redact_bearer_tokens`] 是一条**手写扫描器**，逐格复刻上游那条正则的语义
//!    （`\b` 的 ASCII 词边界、四个参数名的**大小写不敏感**、值类 `[^\s&"'<>]+` 至少一个字符）。
//!    先例：`telegram/markdown.rs` 的七条正则、`dingtalk/inbound/card.rs` 的 URL 扫描。
//! 2. **字段的存在性落成哨兵**：上游按需 `append`，所以"缺席"在行里看得见。`tracing` 的字段名
//!    必须在**编译期**确定 ⇒ emit 那一侧固定渲染每一个字段，缺席写 [`TRACE_ABSENT`]（`-`）。
//!    判据没变：一个值要么在日志里，要么以一个**不可能与真实值相撞**的哨兵出现。
//! 3. **[`TraceFields`] 把"值与开关"这一半从 emit 里拆出来**：emit 需要 `&'static` 的字段名，
//!    而**判据**（记不记、记什么）是这一个值。用例因此不必装一个 subscriber 就能逐字核对字段集
//!    （先例：`dingtalk/ack.rs` 的 `on_ingested_now` 把可测的那一半抽出来）。

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use mc_core::id::Id;

use super::media_download::MediaHeaders;
use super::ws_frame::{AibotMsgCallback, FrameEnvelope};

// =====================================================================
// 开关
// =====================================================================

/// 打开逐帧记录的部署环境变量（上游逐字的那一个名字）。
pub const TRACE_ENV: &str = "MULTICA_WECOM_TRACE";

/// 开关（上游 `tracing`，一个包级原子）。**默认关**。
///
/// 它必须是原子而不是普通 `static mut`：启动时写一次、每一帧读一次，`-race` 会把这两侧对上。
static TRACING: AtomicBool = AtomicBool::new(false);

/// 开或关，并返回**它设成了什么**（上游 `SetTrace`：好让调用方把那件事记一条日志）。
///
/// 上游由服务端接线用 `MULTICA_WECOM_TRACE` 调用它；本仓同款，见 [`set_trace_from_env`]。
pub fn set_trace(on: bool) -> bool {
    TRACING.store(on, Ordering::SeqCst);
    on
}

/// 此刻在不在记录（上游 `tracingOn`）。
#[must_use]
pub fn tracing_on() -> bool {
    TRACING.load(Ordering::SeqCst)
}

/// 从一个**原始配置字符串**固定开关，并返回设成了什么（上游接线的 `== "1"`）。
///
/// **`trim` 之后精确等于 `"1"`**：认不出的值（含空串、`"true"`、带引号的值）都判**关**。
/// 理由与 `strings.rs::set_deployment_locale` 同款 —— 环境变量里的一个笔误不该悄悄把一个
/// **记录消息正文**的开关打开。上游那句 `MULTICA_WECOM_TRACE=1` 就是唯一被承诺过的形态。
#[must_use]
pub fn parse_trace_switch(raw: &str) -> bool {
    raw.trim() == "1"
}

/// 从进程环境读一次并设上（宿主接线的唯一入口）。
///
/// 返回设成了什么，好让调用方照上游那句"returns what it set so the caller can log it"记一条。
pub fn set_trace_from_env() -> bool {
    set_trace(parse_trace_switch(
        &std::env::var(TRACE_ENV).unwrap_or_default(),
    ))
}

// =====================================================================
// 上限
// =====================================================================

/// 一条消息正文里有多少 runes 能进日志（上游 `tracePreviewRunes`，**逐字** 120）。
///
/// 够分辨两条文案、也够看出它是什么语言；短到一份 transcript **没法**从日志里重建出来。
pub const TRACE_PREVIEW_RUNES: usize = 120;

/// 附件的 `Content-Disposition`（以及从它解出来的名字）在路上的上限（上游
/// `traceHeaderRunes`，**逐字** 2048）。
///
/// 上游逐字：它是**远端字符串的失控闸**，不是预览上限 —— 这正是它**不是**
/// [`TRACE_PREVIEW_RUNES`] 的理由。120 会毁掉那一行：`attachment; filename=""` 本身就占 23 个
/// rune，一个百分号转义的 CJK 字符是 9 个，所以十一个中文字的名字就已经越界；而**非 ASCII 的名字
/// 正是这一行存在的理由**（一个全是转义的名字最可能出错）。比丢掉尾巴更糟：切点会落在转义**中间**
/// （`…%E7%89%8…`），而写了一半的转义**没法**解回它代表的那个字符 ⇒ 被截断的那一行连它要回答的
/// 问题都答不了。
///
/// 2048 是照"它合法能有多大"定的，不是凑的整数：POSIX 文件系统把名字停在 255 **字节**，全转义会
/// 变成 765，而一个带着 `filename=` 与 `filename*=` **两种**参数形式的头约 1570 个 rune。
pub const TRACE_HEADER_RUNES: usize = 2048;

/// 字段缺席时的哨兵（见模块文档的形态差异 2）。
///
/// `-` 不可能是一个 chat id / cmd / `req_id` 的合法取值，也不会与任何一个 `errcode` 相撞。
pub const TRACE_ABSENT: &str = "-";

/// 一次写失败的**阶段**名（上游 `traceStageDeadline` / `traceStageWrite`）：一个被拒的帧要能与
/// "连写截止时刻都没设上"的那种分开。
pub const TRACE_STAGE_DEADLINE: &str = "set_write_deadline";

/// 见 [`TRACE_STAGE_DEADLINE`]。
pub const TRACE_STAGE_WRITE: &str = "write_message";

/// 每一个 trace 点的记录名（上游 `log.Info("wecom trace", …)` 的那**一个**字符串）。
///
/// 下面五个 emit 函数里的字面量就是它 —— `tracing` 的宏要求消息是**字面量**，所以它们不能引用
/// 这个常量，由用例把两者连起来。
pub const TRACE_MESSAGE: &str = "wecom trace";

// =====================================================================
// 截断与脱敏
// =====================================================================

/// 一条消息正文的有界预览，**带**裸 token 脱敏（上游 `tracePreview`）。
///
/// 脱敏**不是可选的**。上游逐字：`sendBindingPrompt` 拼出
/// `👋 请先绑定你的 Multica 账号，才能与我对话：\n` + appURL + `/wecom/bind?token=` + 一个 43 字符的
/// token，而在一个正常的 `MULTICA_APP_URL` 下那个 token 的最后一个字符落在第 107–112 个 rune ——
/// **在**上限之内。没有这一层，为一次调试会话打开记录就等于把**活的绑定凭据**整条写进日志。
/// 一个绑定 token 是 bearer 凭据（绑定页在打开时以当前登录者身份兑换它）⇒ 能读日志的人可以在用户
/// 点自己的链接之前，把那个发送者的 `WeCom` 身份绑到自己的 Multica 账号上。
#[must_use]
pub fn trace_preview(text: &str) -> String {
    trace_bound(&redact_bearer_tokens(text), TRACE_PREVIEW_RUNES)
}

/// [`trace_preview`] 的**后半**单独拿出来：单行化 + 一个上限，**不带**脱敏。
///
/// 上限是参数，因为两个调用方为**不同理由**界不同的东西 —— 消息预览是**故意短**
/// （[`TRACE_PREVIEW_RUNES`]），附件头只是让一个远端字符串不能失控（[`TRACE_HEADER_RUNES`]）。
/// 换行与回车都变成空格，所以**一帧就是一行**；切点落在 rune 边界上。
#[must_use]
pub fn trace_bound(text: &str, limit: usize) -> String {
    let mut out = String::with_capacity(text.len().min(limit * 4));
    let mut cut = false;
    for (index, character) in text.chars().enumerate() {
        if index == limit {
            cut = true;
            break;
        }
        out.push(if character == '\n' || character == '\r' {
            ' '
        } else {
            character
        });
    }
    if cut {
        out.push('…');
    }
    out
}

/// 被隐藏的 token 值替换成的固定标记（上游 `${1}[redacted]` 里的那一半）。
pub const REDACTED_MARKER: &str = "[redacted]";

/// 值得隐藏的查询参数名（上游那条正则的四个分支，**顺序即交替顺序**）。
///
/// 它**故意宽**：`access_token` 或一个 `code` 在日志里并不比一个绑定 token 更安全。
const TOKEN_PARAM_NAMES: [&str; 4] = ["binding_token", "access_token", "token", "code"];

/// 把任何 token 形状的查询参数的值换成一个固定标记，同时留住足够让这一行仍值得记的东西
/// （上游 `redactBearerTokens`）。
///
/// 它**按查询参数**匹配，不按绑定的路径匹配：上游逐字，本函数是包级的、看不见配置的
/// `BindingPath` ⇒ 任何 URL 里的 `token=` 都是值得藏起来的形状。
///
/// # 没有 `regex`（见模块文档的形态差异 1）
///
/// 逐格复刻上游那条 `(?i)\b((?:binding_token|access_token|token|code)=)([^\s&"'<>]+)`：
///
/// - **`\b` 是 ASCII 词边界** ⇒ `my_token=…` 与 `x_code=…` **不**匹配（`_` 是词字符，
///   它前面没有边界），而 `?token=…` 与 `binding_token=…` 匹配；
/// - **参数名大小写不敏感**（`(?i)`），替换值取**原文**（Go 的 `${1}` 保留匹配到的原文）；
/// - **值类至少一个字符** ⇒ `token=&x` 不匹配（`&` 在停止集里），原样留着；
/// - **停止集**是 `[\s&"'<>]`（Go 的 `\s` = 空格 / `\t` / `\n` / `\f` / `\r`）。
#[must_use]
pub fn redact_bearer_tokens(input: &str) -> String {
    if !input.contains('=') {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut index = 0usize;
    while index < input.len() {
        if let Some((name_end, value_end)) = match_token_parameter(input, index) {
            // 参数名与 `=` **原样**复制（大小写敏感地），只有值被换掉。
            out.push_str(&input[index..index + name_end]);
            out.push_str(REDACTED_MARKER);
            index += value_end;
            continue;
        }
        // 往前**一个字符**（不是一字节）：截断预算按 rune 算，扫描也必须 UTF-8 安全。
        let character = input[index..].chars().next().unwrap_or('\u{fffd}');
        out.push(character);
        index += character.len_utf8();
    }
    out
}

/// 在 `index` 处匹配一个 token 参数，返回 `(参数名 + "=" 的结束位置, 值的结束位置)`。
fn match_token_parameter(input: &str, index: usize) -> Option<(usize, usize)> {
    if !is_word_boundary(input, index) {
        return None;
    }
    let rest = &input[index..];
    for name in TOKEN_PARAM_NAMES {
        let name_len = name.len();
        let Some(head) = rest.get(..name_len) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(name) {
            continue;
        }
        if rest.as_bytes().get(name_len) != Some(&b'=') {
            continue;
        }
        let value_start = name_len + 1;
        let mut value_len = 0usize;
        for byte in &rest.as_bytes()[value_start..] {
            if is_value_stop(*byte) {
                break;
            }
            value_len += 1;
        }
        if value_len == 0 {
            // `[^…]+` 至少一个字符 ⇒ 空值不匹配、原样留着。
            continue;
        }
        return Some((value_start, value_start + value_len));
    }
    None
}

/// ASCII 词边界（Go `\b` 的那一半）：前一个字符与当前位置上的字符**一个是一个不是**。
fn is_word_boundary(input: &str, index: usize) -> bool {
    let before = input[..index].chars().next_back().is_some_and(is_word_char);
    let at = input[index..].chars().next().is_some_and(is_word_char);
    before != at
}

fn is_word_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// 值的停止集 `[^\s&"'<>]`。
fn is_value_stop(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b'\x0c' | b'\r' | b'&' | b'"' | b'\'' | b'<' | b'>'
    )
}

// =====================================================================
// 字段集
// =====================================================================

/// 一条 trace 记录的**字段集**（上游 `slog` 的 `[]any` attrs）。
///
/// 它是**值与开关的唯一来源**：emit 那一侧只负责把同一个键名交给 `tracing`（字段名要求编译期
/// 常量），值一律经 [`TraceFields::get`] 读出来。于是"记不记"与"记什么"这两件事在**一个**地方。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceFields {
    entries: Vec<(&'static str, String)>,
}

impl TraceFields {
    /// 空字段集。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一个字段（上游 `append(attrs, …)`；后写的同名字段**不覆盖**，见
    /// [`TraceFields::get`] 的"先到先得"）。
    pub fn push(&mut self, key: &'static str, value: impl Into<String>) {
        self.entries.push((key, value.into()));
    }

    /// 追加一个只在该值存在时才出现的字段（上游那种 `if ok { append }`）。
    pub fn push_if_some(&mut self, key: &'static str, value: Option<impl Into<String>>) {
        if let Some(value) = value {
            self.push(key, value);
        }
    }

    /// 一个字段的值；**这个字段不在**时报 [`TRACE_ABSENT`]。
    ///
    /// 缺席与写了空串在这里**故意不区分成同一件事**：一个"记下来的时候它确实为空"的字段
    /// （比如一个没有发的 `Content-Disposition`）与一个"帧上根本没有这一格"的字段是两条不同的
    /// 事实，而上游按需 `append` 时它们的区别就是"这个 attr 在不在"。判据只有一条：**值要么在，
    /// 要么以一个不可能与真实值相撞的哨兵出现。**
    #[must_use]
    pub fn get(&self, key: &str) -> &str {
        self.entries
            .iter()
            .find(|(name, _)| *name == key)
            .map_or(TRACE_ABSENT, |(_, value)| value.as_str())
    }

    /// 这个字段在不在（**含**写了空串的那种）。
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.entries.iter().any(|(name, _)| *name == key)
    }

    /// 字段对数（诊断与用例用）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空吗。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 逐对遍历（顺序即追加顺序）。
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.entries
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
    }

    /// 渲染成 `key=value key=value …`（**只为用例与人工核对**：真日志由 `tracing` 出）。
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (index, (name, value)) in self.entries.iter().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            out.push_str(name);
            out.push('=');
            out.push_str(value);
        }
        out
    }
}

impl fmt::Display for TraceFields {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

// =====================================================================
// 出站：一次尝试 + 一次结局（同一个 seq）
// =====================================================================

/// 一次出站写**在抽取字段与逐字记录之间**保留下来的东西（上游 `outTrace`）。
///
/// 一个 `None` 表示这一帧**不被记录**；两行（尝试与结局）都由**同一个**决定管着，见模块文档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutTrace {
    fields: TraceFields,
    cmd: String,
    req_id: String,
}

impl OutTrace {
    /// 这一帧的 `cmd`（空 = 帧上没有）。
    #[must_use]
    pub fn cmd(&self) -> &str {
        &self.cmd
    }

    /// 这一帧回显的 `req_id`（空 = 帧上没有；一个 `pong` 回显服务端的 `req_id`，它可能为空）。
    #[must_use]
    pub fn req_id(&self) -> &str {
        &self.req_id
    }

    /// 抽取出来的字段（**不含** `seq` 与 `dir`：那两样由 emit 那一刻给）。
    #[must_use]
    pub fn fields(&self) -> &TraceFields {
        &self.fields
    }
}

/// 抽取一帧**在被写上 wire 之前**值得记的东西（上游 `traceOutFields`）。
///
/// 上游逐字：`wsSender.write` 在**拿写者互斥量之前**调用它 —— 这是**贵**的那一半（一次正则脱敏 +
/// 一次逐 rune 截断），而它**一寸都不需要**与 socket 串行化。只有 emit 需要，那是
/// [`trace_out_attempt`] 的事。
///
/// 它读**具名字段**而不是转储帧：`aibot_subscribe` 的 body 带智能机器人的 secret，整体转储会把它
/// 写进日志。
///
/// ⚠️ **上游这一格只读 `markdown.content`**（照抄）：一条流帧的正文在 `stream.content` 下，
/// 所以 [`trace_out_fields`] **不记它**。这不是本仓的取舍 —— 上游就是这样，而"一个片顺手补全"
/// 会让两个部署的日志形状漂开。本片把它记在案（`docs/32` §38 的 R2），并在用例里逐字钉住。
#[must_use]
pub fn trace_out_fields(frame: &serde_json::Value) -> Option<OutTrace> {
    if !tracing_on() {
        return None;
    }
    let mut trace = OutTrace {
        fields: TraceFields::new(),
        cmd: String::new(),
        req_id: String::new(),
    };
    if let Some(cmd) = frame.get("cmd").and_then(serde_json::Value::as_str) {
        cmd.clone_into(&mut trace.cmd);
        trace.fields.push("cmd", cmd);
    }
    if let Some(req_id) = frame
        .get("headers")
        .and_then(|headers| headers.get("req_id"))
        .and_then(serde_json::Value::as_str)
    {
        if !req_id.is_empty() {
            req_id.clone_into(&mut trace.req_id);
            trace.fields.push("req_id", req_id);
        }
    }
    let Some(body) = frame.get("body") else {
        return Some(trace);
    };
    // 🔴 **不下探 `aibot_subscribe` 的 body**：它在 `secret` 下面带着明文密钥。
    trace.fields.push_if_some(
        "chatid",
        body.get("chatid")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    );
    trace.fields.push_if_some(
        "chat_type",
        body.get("chat_type")
            .and_then(serde_json::Value::as_i64)
            .map(|value| value.to_string()),
    );
    trace.fields.push_if_some(
        "msgtype",
        body.get("msgtype")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    );
    if let Some(content) = body
        .get("markdown")
        .and_then(|markdown| markdown.get("content"))
        .and_then(serde_json::Value::as_str)
    {
        trace
            .fields
            .push("len", content.chars().count().to_string());
        trace.fields.push("text", trace_preview(content));
    }
    Some(trace)
}

/// 一次尝试的字段：`dir=out`、`seq`，然后是抽取出来的那些（上游 `traceOutAttempt`）。
#[must_use]
pub fn out_attempt_fields(seq: u64, trace: &OutTrace) -> TraceFields {
    let mut fields = TraceFields::new();
    fields.push("dir", "out");
    fields.push("seq", seq.to_string());
    for (name, value) in trace.fields.iter() {
        fields.push(name, value);
    }
    fields
}

/// 一次结局的字段：`dir=out.done`、同一个 `seq`、`cmd` / `req_id`，以及 `ok` 与（失败时的）
/// `stage` / `error`（上游 `traceOutResult`）。
///
/// 错误文本过 [`trace_preview`]：那是 **socket 说的话**，不是我们的，所以它像这里每一条别的消息
/// 字符串一样被界住并脱敏。
#[must_use]
pub fn out_result_fields(
    seq: u64,
    trace: &OutTrace,
    stage: &str,
    error: Option<&str>,
) -> TraceFields {
    let mut fields = TraceFields::new();
    fields.push("dir", "out.done");
    fields.push("seq", seq.to_string());
    fields.push_if_some("cmd", Some(trace.cmd.clone()).filter(|cmd| !cmd.is_empty()));
    fields.push_if_some(
        "req_id",
        Some(trace.req_id.clone()).filter(|req_id| !req_id.is_empty()),
    );
    match error {
        None => fields.push("ok", "true"),
        Some(message) => {
            fields.push("ok", "false");
            fields.push("stage", stage);
            fields.push("error", trace_preview(message));
        }
    }
    fields
}

// =====================================================================
// 入站
// =====================================================================

/// 一帧从 `WeCom` 到来的字段，**包括服务端对我们发出去的东西的判决** —— 一个 `errcode` 正是在这
/// 里把一次静默失败变成看得见的（上游 `traceIn`）。
///
/// 上游逐字：`dispatchFrame` 只在匿名 ack 那一种情况下对非零 `errcode` 记一条 warn，所以没有这一条，
/// 一个被拒的 `aibot_send_msg` 就是**唯一**根本不出现的拒绝。
#[must_use]
pub fn in_fields(envelope: &FrameEnvelope) -> Option<TraceFields> {
    if !tracing_on() {
        return None;
    }
    let mut fields = TraceFields::new();
    fields.push("dir", "in");
    fields.push("cmd", envelope.cmd.clone());
    fields.push("req_id", envelope.headers.req_id.clone());
    fields.push("errcode", envelope.errcode.to_string());
    fields.push("errmsg", trace_preview(&envelope.error_message));
    Some(fields)
}

/// 一条**已解码**的用户消息的字段：这个 adapter 相信它是谁发的、落在哪个聊 —— 而那两个字段正是会
/// 被搞混的那一对（房间的 id 进了一个字段、人的 id 进了另一个）。
///
/// [`in_fields`] 看不出这件事：那一刻回调的 body 还是**原始 JSON**。
#[must_use]
pub fn inbound_fields(callback: &AibotMsgCallback, text: &str) -> Option<TraceFields> {
    if !tracing_on() {
        return None;
    }
    let mut fields = TraceFields::new();
    fields.push("dir", "in.msg");
    fields.push("msg_id", callback.msgid.clone());
    fields.push("chatid", callback.chatid.clone());
    fields.push("chat_type", callback.chattype.clone());
    fields.push("sender", callback.from.userid.clone());
    fields.push("msgtype", callback.msgtype.clone());
    fields.push("len", text.chars().count().to_string());
    fields.push("text", trace_preview(text));
    Some(fields)
}

/// 一次附件的响应关于它自己说了什么：`Content-Disposition` **到达时的原样值**，以及本包从它解出来
/// 的名字（上游 `traceMediaHeaders`）。
///
/// 那两样并排就是"文件名看起来不对"的**全部**诊断，而且它们**事后无法恢复** —— 它们来自的 URL 只
/// 活五分钟，所以明天被质疑名字的附件永远没法重新取回来核对。
///
/// 两个值都单行化并按 [`TRACE_HEADER_RUNES`] 截断，而且**都不**过 [`redact_bearer_tokens`]：
/// 见模块文档那两条故意的例外。
///
/// 它在头**缺席**时**也发**，记一个空值：一条记录都没有的运行会让读的人分不清"服务端没发
/// `Content-Disposition`"与"开关关着"。
///
/// # 多出来的那一格：`installation_id`
///
/// 上游那一行**没有**它（`traceMediaHeaders(log, msgID, index, h)`）。本仓加它是因为**媒体路径的
/// 其余每一行都带着它**（`media_ingest.rs` 交错的两条路径），而这一行是**唯一无法事后恢复**的那
/// 一条 ⇒ 去掉它会让"哪个 bot 的附件名不对"变成猜谜。交付这一格的是
/// [`crate::wecom::media_ingest`] 的收敛调用点（M7-18 的 §35.4 **H3**），登记为 `docs/32` §38 的 D12。
#[must_use]
pub fn media_headers_fields(
    msg_id: &str,
    index: usize,
    headers: &MediaHeaders,
    installation_id: Option<Id>,
) -> Option<TraceFields> {
    if !tracing_on() {
        return None;
    }
    let mut fields = TraceFields::new();
    fields.push("dir", "in.media");
    fields.push_if_some("installation_id", installation_id.map(|id| id.to_string()));
    fields.push("msg_id", msg_id);
    fields.push("index", index.to_string());
    fields.push(
        "content_disposition",
        trace_bound(&headers.disposition, TRACE_HEADER_RUNES),
    );
    fields.push(
        "filename",
        trace_bound(&headers.filename, TRACE_HEADER_RUNES),
    );
    Some(fields)
}

// =====================================================================
// emit（`tracing` 的字段名必须编译期确定 ⇒ 这里是唯一的常量面）
// =====================================================================
//
// 每个 emit 函数都**只**通过上面那几个 `*_fields` 取字段：值与开关的那一半只有一个来源，而下面
// 这些 `fields.get("…")` 里的键名是它在**这一侧**的镜像（`tracing` 的宏要求编译期常量名）。
// 用例断言的是 `*_fields` 那一半，所以"记不记、记什么"不会被这一侧改写。

/// 记录一次尝试（上游 `traceOutAttempt`）。
///
/// 上游逐字：`wsSender.write` 在**写者互斥量之下**调用它，而那正是并发发送者（ping 循环、agent
/// 回复、收件箱推送）变得**有序**的那一点。所以这些行的顺序**由构造**就是这些帧到达
/// `WriteMessage` 的顺序，不是靠关联推出来的。
///
/// ⚠️ **`None` 就是那个开关**（上游 `if t == nil { return }`）：尝试与结局两行由**同一个**
/// `OutTrace` 值管着，所以日志里不可能留下一个其结局被"写到一半翻转的开关"吞掉的尝试。
pub fn trace_out_attempt(seq: u64, trace: Option<&OutTrace>) {
    let Some(trace) = trace else {
        return;
    };
    let fields = out_attempt_fields(seq, trace);
    tracing::info!(
        dir = %fields.get("dir"),
        seq,
        cmd = %fields.get("cmd"),
        req_id = %fields.get("req_id"),
        chatid = %fields.get("chatid"),
        chat_type = %fields.get("chat_type"),
        msgtype = %fields.get("msgtype"),
        len = %fields.get("len"),
        text = %fields.get("text"),
        "wecom trace"
    );
}

/// 记录那个背着同一个 `seq` 的尝试变成了什么（上游 `traceOutResult`）。
///
/// 错误文本过 [`trace_preview`]：那是 **socket 说的话**，不是我们的，所以它像这里每一条别的消息
/// 字符串一样被界住并脱敏。
pub fn trace_out_result(seq: u64, trace: Option<&OutTrace>, stage: &str, error: Option<&str>) {
    let Some(trace) = trace else {
        return;
    };
    let fields = out_result_fields(seq, trace, stage, error);
    tracing::info!(
        dir = %fields.get("dir"),
        seq,
        cmd = %fields.get("cmd"),
        req_id = %fields.get("req_id"),
        ok = %fields.get("ok"),
        stage = %fields.get("stage"),
        error = %fields.get("error"),
        "wecom trace"
    );
}

/// 记录一帧入站（上游 `traceIn`）。
pub fn trace_in(envelope: &FrameEnvelope) {
    let Some(fields) = in_fields(envelope) else {
        return;
    };
    tracing::info!(
        dir = %fields.get("dir"),
        cmd = %fields.get("cmd"),
        req_id = %fields.get("req_id"),
        errcode = %fields.get("errcode"),
        errmsg = %fields.get("errmsg"),
        "wecom trace"
    );
}

/// 记录一条已解码的用户消息（上游 `traceInbound`）。
pub fn trace_inbound(callback: &AibotMsgCallback, text: &str) {
    let Some(fields) = inbound_fields(callback, text) else {
        return;
    };
    tracing::info!(
        dir = %fields.get("dir"),
        msg_id = %fields.get("msg_id"),
        chatid = %fields.get("chatid"),
        chat_type = %fields.get("chat_type"),
        sender = %fields.get("sender"),
        msgtype = %fields.get("msgtype"),
        len = %fields.get("len"),
        text = %fields.get("text"),
        "wecom trace"
    );
}

/// 记录一次附件的头（上游 `traceMediaHeaders`）。
///
/// 这是**唯一**一处附件头进日志的地方：`media_ingest.rs` 的 M7-18 版本曾经自己记一条（开关与截断
/// 都没有），M7-18 的 H3 把收敛留给本片 ⇒ 现在那一处只是**调用**这里（`docs/32` §38 的 D12）。
pub fn trace_media_headers(
    msg_id: &str,
    index: usize,
    headers: &MediaHeaders,
    installation_id: Option<Id>,
) {
    let Some(fields) = media_headers_fields(msg_id, index, headers, installation_id) else {
        return;
    };
    tracing::info!(
        dir = %fields.get("dir"),
        installation_id = %fields.get("installation_id"),
        msg_id = %fields.get("msg_id"),
        index = %fields.get("index"),
        content_disposition = %fields.get("content_disposition"),
        filename = %fields.get("filename"),
        "wecom trace"
    );
}

#[cfg(test)]
mod tests;
