//! **入站归一化**：一条 aibot 回调怎么变成跨平台的 [`InboundMessage`]
//! （上游 `internal/integrations/wecom/ws_frame.go` 的后半段 + `channelMessageFromCallback`，
//! 合计约 **640 行**）。
//!
//! - **写者**：M7-19（`LUM-1784` / `docs/60-M7-PLAN.md` §3.3）。
//! - **为什么在这里而不是 `ws_frame.rs`**：M7-16 的 `ws_frame.rs` 只交"帧编解码"，并在它的模块
//!   文档里把这一段**逐条点名**交给 M7-19（`docs/32` §33 的 D2）。上游自己也在
//!   `inbox_message.go` 里放归一化信封，两者本就同面。
//!
//! # 本文件回答的一个问题
//!
//! **这一帧里，agent 该读到什么？** 四个不同的答案，每一个都有它的理由（上游逐字）：
//!
//! | 字段 | 来源 | 为什么不是别的 |
//! | --- | --- | --- |
//! | [`AibotMsgCallback::own_text`] | `text` / 语音转写 / `[Image]` 占位 / 混排各段按序 | 语音由 `WeCom` 自己识别，**不下载**；附件要有个东西先站在那里 |
//! | [`AibotMsgCallback::own_command_source`] | 只取**发件人自己写的字** | `[Image]` 开头会让 `/issue` 解析器看到占位符而不是指令 |
//! | [`AibotMsgCallback::quoted_context`] | 被引用那条消息，渲染成引用块 | 没有它，"这个怎么处理"对 agent 是一个空问题 |
//! | [`channel_msg_type`] | 归一化枚举 | `mixed` 映射成 text（它**就是**文本，跑完 `own_text` 之后） |
//!
//! # 群聊里的"被 @ 到"是字面文本
//!
//! `WeCom` 只在被叫到时才把群消息转给机器人，所以**收到的每一条群消息都算被叫到**；而 `@`
//! 是正文里的字面文本、没有结构化的 mention 列表 ⇒ [`strip_leading_mentions`] 按**形状**剥掉
//! 开头的 `@某某`（**只在群聊**：单聊里那个 `@` 是发件人在说别人）。

use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, Source};
use serde::{Deserialize, Serialize};

use crate::engine::commands::{parse_control_command, parse_issue_command, ControlCommand};
use crate::wecom::markdown::quote_lines;
use crate::wecom::types::CHANNEL_TYPE;
use crate::wecom::ws_frame::{AibotMsgCallback, MediaBody, MixedItem, QuotedMessage, TextBody};

/// 附件在**存下来的正文**里的占位标记（上游 `mediaPlaceholder`）。
///
/// 三个字面量与 lark / dingtalk **逐字节一致**（`lark/content_flatten.go` 的
/// `[Image]` / `[File]` / `[Video]`，`dingtalk/inbound.rs` 的同名常量）：agent 用**同一个**
/// prompt 读所有渠道，一个 wecom 专属的写法只会给它多一件要学的东西。
#[must_use]
pub fn media_placeholder(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Image => "[Image]",
        MessageKind::Video => "[Video]",
        _ => "[File]",
    }
}

/// 引用块的标签（上游 `quotePrefix`）。
///
/// 它**在** markdown 引用块**里面**而不是替掉它：引用可能有好几行，而只有引用块能让后面那些行
/// 挂在它下面。写法与媒体占位符一致，这样 agent 只遇到一套词表。
pub const QUOTE_PREFIX: &str = "[Quote]";

/// 引用块的上限（上游 `maxQuotedRunes`）。数 **rune** 而不是字节：引用的文本通常是中文，按字节
/// 算会少切掉将近三分之二的字，而且可能把一个字劈成两半。
pub const MAX_QUOTED_RUNES: usize = 500;

/// 读不懂的消息类型要回的那一句话（上游 `unsupportedMsgTypeReceipt`）。
///
/// 它从前说的是"我目前只能处理文字消息"—— 在照片、文件、视频与图文混排都开始路由之后就不再
/// 成立：一个刚看着机器人回答了一张截图、然后被告知"只能处理文字"的人，读到的不是"这一类不支持"，
/// 而是"这个机器人坏了"。
pub const UNSUPPORTED_MSG_TYPE_RECEIPT: &str = "抱歉，我暂时无法处理这类消息。";

// =====================================================================
// 一种可下载类型
// =====================================================================

/// 这条回调上的一件可下载附件（上游 `InboundMedia`）。
///
/// `url` 是五分钟有效的预签名地址（**不用** access token），`aeskey` 是按 url 现铸的单次密钥 ⇒
/// 两者**都不进表、不进日志**（`Debug` 手写脱敏）。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundMedia {
    /// 归一化的媒体类型（附件行按它打标签）。
    pub kind: MediaKind,
    /// 预签名地址。
    pub url: String,
    /// 解它的密钥。
    pub aeskey: String,
}

/// [`InboundMedia::kind`] 的 wire 取值（`"image"` / `"file"` / `"video"`；音频走转写、不下载）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    File,
    Video,
}

impl MediaKind {
    /// 归一化后的消息种类。
    #[must_use]
    pub fn message_kind(self) -> MessageKind {
        match self {
            Self::Image => MessageKind::Image,
            Self::File => MessageKind::File,
            Self::Video => MessageKind::Video,
        }
    }
}

impl std::fmt::Debug for InboundMedia {
    /// 手写脱敏（`docs/60` §2.3）：地址能直接取回密文、密钥能解开它，两者都不进 `{:?}`。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |value: &str| {
            if value.is_empty() {
                "<empty>"
            } else {
                "<redacted>"
            }
        };
        formatter
            .debug_struct("InboundMedia")
            .field("kind", &self.kind)
            .field("url", &redact(&self.url))
            .field("aeskey", &redact(&self.aeskey))
            .finish()
    }
}

/// 归一化某一种类型 + 它的三个 body ⇒ 那个 body 与它的种类；`None` = 这一类**不下载**。
///
/// 上游 `mediaFor`。
#[must_use]
pub fn media_for(
    msg_type: &str,
    image: &MediaBody,
    file: &MediaBody,
    video: &MediaBody,
) -> Option<(MediaBody, MediaKind)> {
    match msg_type.to_ascii_lowercase().as_str() {
        "image" => Some((image.clone(), MediaKind::Image)),
        "file" => Some((file.clone(), MediaKind::File)),
        "video" => Some((video.clone(), MediaKind::Video)),
        _ => None,
    }
}

// =====================================================================
// 一帧里 agent 该读到什么
// =====================================================================

impl AibotMsgCallback {
    /// 这条回调上要下载的媒体，按用户发出的顺序（上游 `attachments`）。
    ///
    /// 没有 url 的 body 被跳过：没有什么可取，而把它带下去只会给一个**永远不会存在**的对象
    /// 造一条意图账本行。
    #[must_use]
    pub fn attachments(&self) -> Vec<InboundMedia> {
        let mut out = Vec::new();
        let add = |body: &MediaBody, kind: MediaKind, out: &mut Vec<InboundMedia>| {
            if body.url.trim().is_empty() {
                return;
            }
            out.push(InboundMedia {
                kind,
                url: body.url.clone(),
                aeskey: body.aeskey.clone(),
            });
        };
        if let Some((body, kind)) = media_for(&self.msgtype, &self.image, &self.file, &self.video) {
            add(&body, kind, &mut out);
            return out;
        }
        if !self.msgtype.eq_ignore_ascii_case("mixed") {
            return Vec::new();
        }
        for item in &self.mixed.msg_item {
            if let Some((body, kind)) =
                media_for(&item.msgtype, &item.image, &item.file, &item.video)
            {
                add(&body, kind, &mut out);
            }
        }
        out
    }

    /// 这条回调的 **agent 可读正文**，以及它到底有没有正文（上游 `ownText`）。
    ///
    /// - 纯文本：正文本身；
    /// - 语音：`WeCom` 识别出来的**转写**，那是发件人自己的话、不需要下载；
    /// - 照片 / 文件 / 视频：一个方括号占位符 —— 字节稍后从那条脱离的媒体路径来，而消息在此
    ///   之间必须说点什么（**下载永远不成功时活下来的也正是这个占位符**）；
    /// - 图文混排：各段按**用户编排的顺序**渲染，于是"看这个"仍然读在它说的那张图上面。
    ///
    /// 识别在背景噪音或半秒的按键上会回来是空的，而一个空正文会作为**什么都没有的一轮**到达
    /// agent ⇒ 空转写答 `false`、走那条"回执"路径，与一张位置卡或 `WeCom` 明年新加的一种类型
    /// 完全同款。
    #[must_use]
    pub fn own_text(&self) -> (String, bool) {
        match self.msgtype.to_ascii_lowercase().as_str() {
            "text" => (self.text.content.clone(), true),
            "voice" => {
                let transcript = self.voice.content.trim().to_string();
                let usable = !transcript.is_empty();
                (transcript, usable)
            }
            "image" | "file" | "video" => {
                let Some((body, kind)) =
                    media_for(&self.msgtype, &self.image, &self.file, &self.video)
                else {
                    return (String::new(), false);
                };
                if body.url.trim().is_empty() {
                    return (String::new(), false);
                }
                (media_placeholder(kind.message_kind()).to_string(), true)
            }
            "mixed" => {
                let runs: Vec<String> = self
                    .mixed
                    .msg_item
                    .iter()
                    .map(MixedItem::render)
                    .filter(|run| !run.is_empty())
                    .collect();
                if runs.is_empty() {
                    return (String::new(), false);
                }
                (runs.join("\n"), true)
            }
            _ => (String::new(), false),
        }
    }

    /// 发件人在回复的那条消息，渲染成**引用块**（上游 `quotedContext`）。
    ///
    /// 没有它一条回复就答不了："这个怎么处理"引用一条告警，在聊里是一个完整的问题，对 agent
    /// 却是一个空问题 —— 它只看到那几个字、看不到它们指着什么。
    ///
    /// 它**故意**不进 [`AibotMsgCallback::own_command_source`]：命令解析器只读第一个非空行，而
    /// 引用的那一行不是发件人在这里敲的；把它前缀上，会让"引用别人写的 `/issue …`"凭空建一个
    /// 谁也没要的 issue。
    #[must_use]
    pub fn quoted_context(&self) -> String {
        let mut rendered = self.quote.render().trim().to_string();
        if rendered.is_empty() {
            return String::new();
        }
        // 一份被引用的文档否则会变成正文本身。发件人引用它是为了**指**它，不是为了重发它，
        // 而承载他问题的是他自己的话 —— 那些话在引用块**后面**，不该被它顶出 agent 的视野。
        if rendered.chars().count() > MAX_QUOTED_RUNES {
            let head: String = rendered.chars().take(MAX_QUOTED_RUNES).collect();
            rendered = format!("{}…", head.trim_end_matches([' ', '\t', '\n']));
        }
        let prefixed = format!("{QUOTE_PREFIX} {rendered}");
        quote_lines(&prefixed)
    }

    /// 斜杠命令解析器要读的东西：**发件人自己的话**，没有本 adapter 写的任何字
    /// （上游 `ownCommandSource`）。
    ///
    /// 它**不是** [`AibotMsgCallback::own_text`]：
    ///
    /// - `own_text` 在附件的位置插入 `[Image]` / `[File]` / `[Video]`，因为存下来的正文必须显示
    ///   "有东西被附上了、在哪儿"；而 `/issue` 解析器**只看第一个非空行** ⇒ 一个人先截一张图、
    ///   再打 `/issue 登录坏了`（自然的顺序，也是 `WeCom` 的输入框鼓励的顺序）会得到一个以
    ///   `[Image]` 开头的正文，解析器看到的是占位符而不是指令，于是没有 issue 被建、也没有任何
    ///   地方告诉他为什么。反过来先打指令再截图就正常 —— 这不是一个用户可以指望自己知道的区别。
    /// - 一条**单独**的照片 / 文件 / 视频答空串：它的整个正文就是一个占位符，里面没有字可以解析，
    ///   而把一个发件人从没打过的字符串递给解析器，正是这条函数要移除的那个缺陷。
    #[must_use]
    pub fn own_command_source(&self) -> String {
        match self.msgtype.to_ascii_lowercase().as_str() {
            "text" => self.text.content.clone(),
            "voice" => self.voice.content.trim().to_string(),
            "mixed" => {
                let runs: Vec<String> = self
                    .mixed
                    .msg_item
                    .iter()
                    .map(MixedItem::words)
                    .filter(|run| !run.is_empty())
                    .collect();
                runs.join("\n")
            }
            _ => String::new(),
        }
    }
}

impl MixedItem {
    /// 这一段**发件人打的或说的**那部分（上游 `mixedItem.words`）。附件贡献空串 ——
    /// 这就是它与 [`MixedItem::render`] 的区别。
    #[must_use]
    pub fn words(&self) -> String {
        match self.msgtype.to_ascii_lowercase().as_str() {
            "text" => self.text.content.trim().to_string(),
            // `WeCom` 在自己那侧跑语音识别、只投结果 ⇒ 一段语音就是一句"恰好是说出来的"话：
            // 不下载、不要密钥。它是发件人自己的话，所以说出来的 `/issue 登录坏了` 与打出来的一样
            // 是一条命令。
            "voice" => self.voice.content.trim().to_string(),
            _ => String::new(),
        }
    }

    /// 这一段为消息正文贡献的那一行（上游 `mixedItem.render`）。认不出的一段贡献**空串**，
    /// 而不是一个乱跑的占位符。
    #[must_use]
    pub fn render(&self) -> String {
        let words = self.words();
        if !words.is_empty() {
            return words;
        }
        match media_for(&self.msgtype, &self.image, &self.file, &self.video) {
            Some((body, kind)) if !body.url.trim().is_empty() => {
                media_placeholder(kind.message_kind()).to_string()
            }
            _ => String::new(),
        }
    }
}

impl QuotedMessage {
    /// 把被引用的消息渲染成它贡献的那几行（上游 `quotedMessage.render`）。
    ///
    /// 认不出的类型贡献空串 —— 与一段混排完全同款。
    #[must_use]
    pub fn render(&self) -> String {
        if !self.msgtype.eq_ignore_ascii_case("mixed") {
            return render_quoted_leaf(
                &self.msgtype,
                (&self.text, &self.voice),
                (&self.image, &self.file, &self.video),
            );
        }
        let runs: Vec<String> = self
            .mixed
            .msg_item
            .iter()
            .map(MixedItem::render)
            .filter(|run| !run.is_empty())
            .collect();
        runs.join("\n")
    }
}

/// 非混排的引用：`MixedItem` 的平铺字段（上游 `quotedMessage` 的 `embedded mixedItem`）。
fn render_quoted_leaf(
    msg_type: &str,
    text_and_voice: (&TextBody, &TextBody),
    media: (&MediaBody, &MediaBody, &MediaBody),
) -> String {
    let (text, voice) = text_and_voice;
    let (image, file, video) = media;
    match msg_type.to_ascii_lowercase().as_str() {
        "text" => text.content.trim().to_string(),
        "voice" => voice.content.trim().to_string(),
        _ => match media_for(msg_type, image, file, video) {
            Some((body, kind)) if !body.url.trim().is_empty() => {
                media_placeholder(kind.message_kind()).to_string()
            }
            _ => String::new(),
        },
    }
}

// =====================================================================
// 归一化信封（进 `InboundMessage::raw`）
// =====================================================================

/// `WeCom` 这一侧的扁平信封，由读循环从一条解出来的回调构造、以 JSON 塞进
/// [`InboundMessage::raw`]，好让 [`crate::wecom::resolvers`] 够得着跨平台信封不带的那些平台字段
/// （`bot_id` / `req_id`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeComInboundMessage {
    /// 这条事件投给哪个智能机器人。**它就是安装解析器用的那个路由键。**
    pub bot_id: String,

    /// `WeCom` 的每条消息标识，两阶段去重用它。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub msg_id: String,

    /// 原始的 wecom 类型串（`"text"` / `"image"` / `"event"`…）。媒体 / 未知类型经
    /// [`channel_msg_type`] 往返；原始串留在这里供审计。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub msg_type: String,

    /// 腾讯内部的会话判别式（`"single"` 1:1 / `"group"` 群聊）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub chat_type: String,

    /// 消息来自哪里 —— 单聊是 userid、群聊是 chatid（出站与会话绑定的路由身份）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub chat_id: String,

    /// 打字那个人的 userid。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sender_user_id: String,

    /// 人可读的正文（`own_text` 的产物）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,

    /// 服务端发来这一帧时的 `req_id`。留着它，是为了将来 `aibot_respond_msg`（5 秒窗）能把它
    /// 回显回去；迭代 1 无条件用 `aibot_send_msg`，还不需要它。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub req_id: String,

    /// 要取的附件，按用户发出的顺序。它是 `MediaResolver` 的输入，且**只**在
    /// [`InboundMessage::raw`] 里旅行 —— engine 在内存里传给下一环、**从不**持久化：url 五分钟
    /// 就失效、密钥是一次性的，两者都不该进表或进日志行。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<InboundMedia>,
}

impl WeComInboundMessage {
    /// 装进 [`InboundMessage::raw`] 的 JSON 值。
    ///
    /// 序列化一个全是 `String` / `Vec` 的结构不会失败；真失败时给一个空对象而不是 panic
    /// （读循环上的一次 panic 会杀掉整条连接）。
    #[must_use]
    pub fn to_raw_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

// =====================================================================
// 控制指令的正文重排
// =====================================================================

/// 上游 `normalizeWeComControlLayout`：把 `/clear` 或 `/new` 从 agent 可读的正文里**摘掉**，
/// 同时把媒体占位符留在它们在混排里原本的位置上。
///
/// `CommandText` 仍然是发件人写的、没有占位符的那一份，于是 Router 独自承担两个指令之间的语义
/// 差别。
///
/// `has_other_content` 说这条消息除了指令之外还带着东西 —— 一个附件、一条引用、或者两者都有。
/// 一个什么都没带的指令是**共享的 pending 哨兵**，原样留着让 Router 去认；而一个带着内容来的
/// 指令是一轮真回合，把指令留在正文里会把它当 prompt 文本存下去。
///
/// 返回 `(可见正文, 认出来的指令)`；第二个是 `None` = 什么都没重排（正文原样）。
#[must_use]
pub fn normalize_wecom_control_layout(
    callback: &AibotMsgCallback,
    visible: &str,
    command: &str,
    chat_type: ChatType,
    bot_display_name: &str,
    has_other_content: bool,
) -> (String, Option<ControlCommand>) {
    let Some(control) = parse_control_command(command) else {
        return (visible.to_string(), None);
    };
    if control.body.is_empty() && !has_other_content {
        return (visible.to_string(), None);
    }

    let normalize_words = |words: &str| -> String {
        if chat_type == ChatType::Group {
            strip_leading_mentions(words, bot_display_name)
        } else {
            words.trim().to_string()
        }
    };

    match callback.msgtype.to_ascii_lowercase().as_str() {
        "text" => {
            let Some(item_control) =
                parse_control_command(&normalize_words(&callback.text.content))
            else {
                return (visible.to_string(), None);
            };
            if item_control.kind != control.kind {
                return (visible.to_string(), None);
            }
            (item_control.body, Some(control))
        }
        "voice" => {
            let Some(item_control) =
                parse_control_command(&normalize_words(&callback.voice.content))
            else {
                return (visible.to_string(), None);
            };
            if item_control.kind != control.kind {
                return (visible.to_string(), None);
            }
            (item_control.body, Some(control))
        }
        "mixed" => {
            let mut runs: Vec<String> = Vec::new();
            let mut consumed = false;
            for item in &callback.mixed.msg_item {
                let mut rendered = item.render();
                if !consumed {
                    let words = item.words();
                    if !words.is_empty() {
                        if let Some(item_control) = parse_control_command(&normalize_words(&words))
                        {
                            if item_control.kind == control.kind {
                                rendered = item_control.body;
                                consumed = true;
                            }
                        }
                    }
                }
                if !rendered.is_empty() {
                    runs.push(rendered);
                }
            }
            if consumed {
                (runs.join("\n"), Some(control))
            } else {
                (visible.to_string(), None)
            }
        }
        _ => (visible.to_string(), None),
    }
}

/// 上游 `stripLeadingMentions`：剥掉一条消息**开头**的 `@提及` —— 在群聊里，那是发件人在叫机器人。
///
/// `WeCom` 把它们放在文本里、不发 mention 列表，所以除了**形状**没有东西可以拿来匹配：
/// 最前面的一个 `@`，到下一个空格为止。
///
/// **只在群聊**（调用方按 `chatType` 把关）。单聊里没人需要叫机器人，所以那里开头同一个 `@` 是
/// 发件人自己句子里一个同事的名字，删掉就是改写他说的话。
///
/// **只剥开头。** 句子更深处的一个名字是发件人在说**别人** ——"@Andrew 问一下 @李雷 昨天那件事"
/// 是一条指令点名一个同事 —— 剥掉它同样是悄悄改写他说的话。
///
/// 它主要喂命令分类。对一个认出来的 `/clear` 或 `/new`，
/// [`normalize_wecom_control_layout`] 在**重建** agent 可见的混排正文时也上同一道清理，于是机器人
/// 的提及与被消费掉的指令都不会作为 prompt 文本被存下去。
///
/// slack 用正则过它的 mention token（`slack/inbound.rs` 的 `clean_text`）；飞书拿到的已经是
/// 平台洗干净的指令正文。**wecom 曾是唯一一个把原始文本直接递下去的 adapter。**
#[must_use]
pub fn strip_leading_mentions(text: &str, bot_name: &str) -> String {
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start();
        if !trimmed.starts_with('@') {
            return trimmed.to_string();
        }
        // **自己的名字优先，整名匹配。** 显示名可能含空格 —— "Multica Bot" 就是最明显的那个 ——
        // 而在第一个空格处切会留下 "Bot /clear 重新分析"，那不是一条命令，于是那个群里每一条
        // 斜杠命令仍然会被丢掉。
        //
        // 名字不是猜的：它来自安装配置（机器人被连上时设的），因为回调里没有结构化的 mention 列表
        // 可以读。缺席时跑下面那条启发式 —— 对一个单词的名字是对的，也是每个安装在有
        // 人填那个字段之前的状态。
        if !bot_name.is_empty() && trimmed[1..].starts_with(bot_name) {
            rest = &trimmed[1 + bot_name.len()..];
            continue;
        }
        let Some(index) = trimmed.find(char::is_whitespace) else {
            // 整条消息就是一个提及、别的什么都没有。没有命令也没有话 —— 原样留着，让"空正文"由
            // 调用方决定，而不是在这里造出来。
            return trimmed.to_string();
        };
        rest = &trimmed[index..];
    }
}

/// 上游 `isIssueCommand`：问 engine **自己的**解析器，而不是镜像它。
///
/// 那个镜像是漂过的：它用 `strings.TrimSpace` 裁（会裁掉每一个 Unicode 空格，含中文全角输入法
/// 打出的 U+3000 表意空格），而 `engine.ParseIssueCommand` 只裁 `" \t"`。于是一条以 U+3000 开头的
/// 单聊行在这里读成命令、在那里读成散文：`SkipAgentRun` 被置上所以没有 agent 跑，而解析器拒了
/// 所以没有 issue 被建 —— 发件人什么都没收到，也没有任何地方说明为什么。
///
/// **一个解析器的镜像就是一个解析器。** 委托出去只在一条本来就要做 I/O 的路径上多一次分配，
/// 却消掉了整整一类问题。
#[must_use]
pub fn is_issue_command(body: &str) -> bool {
    parse_issue_command(body).is_some()
}

/// 上游 `channelMsgType`：把原始 aibot `msg_type` 映射到归一化枚举。
#[must_use]
pub fn channel_msg_type(wecom_type: &str) -> MessageKind {
    // 图文混排（`mixed`）与 `text` **同一格**：跑完 `own_text` 之后这条消息**就是**文本
    // （各段按编排顺序、每个附件以它的占位符站在那里），而附件另行以 `MediaRef` 旅行 —— 与
    // lark 的 `post` 一个形状换了个名字。
    //
    // 这一格从前是 `Unknown`，而那时的注释对**当时**的代码是对的：`dispatch_frame` 只路由
    // "以字到达"的那几种，所以一条混排消息根本到不了归一化，把它叫 Text 会声称一件没有发生的
    // 路由。**这个改动让那句声称成真**，两者必须一起落地。
    match wecom_type.to_ascii_lowercase().as_str() {
        "text" | "mixed" => MessageKind::Text,
        "image" => MessageKind::Image,
        "file" => MessageKind::File,
        "voice" | "audio" => MessageKind::Audio,
        "video" => MessageKind::Video,
        // 这个 adapter 根本读不懂的一种。`dispatch_frame` 会回一句"读不懂"的回执并停下，
        // 所以归一化**永远不会**为它跑。
        _ => MessageKind::Unknown,
    }
}

// =====================================================================
// 回调 ⇒ 跨平台信封
// =====================================================================

/// 上游 `channelMessageFromCallback`：把一条 wecom 侧的 `aibot_msg_callback` 转成
/// engine 消费的跨平台 [`InboundMessage`]。
///
/// 路由身份：
///
/// - `single` → `ChatType::P2p`，`chat_id` = userid，`sender_id` = userid
/// - `group`  → `ChatType::Group`，`chat_id` = chatid，`sender_id` = `from.userid`
///
/// 在群里 @ 机器人与一条裸的群消息在 wire 上**分不出来** —— `WeCom` 只在被叫到时才转给机器人，
/// 所以收到的每一条群消息都算被叫到。
///
/// `text` 是调用方已经用 [`AibotMsgCallback::own_text`] 解出来的 agent 可读正文。它作为参数传进来
/// 而不是在这里重算，是因为调用方必须**先**知道这条消息是否可路由。指令源是**另一个**字符串，
/// 在这里从 `callback` 导出 —— 见 [`AibotMsgCallback::own_command_source`] 为什么两者不能是同一个。
#[must_use]
pub fn channel_message_from_callback(
    bot_id: &str,
    bot_display_name: &str,
    callback: &AibotMsgCallback,
    text: &str,
    req_id: &str,
) -> InboundMessage {
    let chat_type = if callback.chattype.eq_ignore_ascii_case("group") {
        ChatType::Group
    } else {
        ChatType::P2p
    };
    let sender_id = callback.from.userid.clone();
    let chat_id = if chat_type == ChatType::P2p && callback.chatid.is_empty() {
        // 有些形态只给群聊设 `chatid`；退到发件人。
        sender_id.clone()
    } else {
        callback.chatid.clone()
    };

    // 指令源是**发件人自己的话**（`own_command_source`，不是解出来的正文），所以一条第一段是截图的
    // 图文混排，仍然把它的 "/issue …" 留在解析器读的那第一行上。
    //
    // 群里那个 @ 提及**就是**叫到机器人的方式，所以它紧贴着后面打的字到达 —— "@Andrew /clear"
    // 是一个人在要一条新会话，而不是恰好含某个词的散文 —— 于是开头的称呼要摘掉。
    //
    // **只在群聊。** 单聊里没人需要叫机器人，所以开头的一个 "@" 是发件人在**谈论**的某个同事：
    // "@李雷 /issue 帮我问问他"是一个问题，剥掉那个名字会把它变成一条谁也没要的 issue，并且
    // （经下面的 `skip_agent_run`）连回答都没有。
    let mut command = callback.own_command_source();
    if chat_type == ChatType::Group {
        command = strip_leading_mentions(&command, bot_display_name);
    }
    let media = callback.attachments();
    // 引用在这里也算"有内容"，理由与媒体一样：它是下面那些"只有指令"的形态**不是**空 pending
    // 哨兵的原因。渲染在更下面 —— 这里只需要知道它存在。
    let quoted = callback.quoted_context();
    let (normalized_text, control) = normalize_wecom_control_layout(
        callback,
        text,
        &command,
        chat_type,
        bot_display_name,
        !media.is_empty() || !quoted.is_empty(),
    );
    let mut text = if control.is_some() {
        normalized_text
    } else {
        text.to_string()
    };

    // 被引用的消息放在**最后**，于是上面的一切 —— 控制布局重写、以及它可能交回来的指令源 ——
    // 读到的仍然是发件人真正编排的正文。长出来的只有存下来、agent 可见的那个 text。
    let own_body = text.clone();
    if !quoted.is_empty() {
        text = if text.is_empty() {
            quoted.clone()
        } else {
            format!("{quoted}\n\n{text}")
        };
    }

    // 一条仍然带着内容的裸 `/clear`（媒体、引用、或两者）是一轮真回合，不是那条共享的 pending
    // 哨兵，所以它不许带着指令到达 Router 的指令源。下面的 `force_fresh` 驮着已经被消费掉的指令；
    // 指令源变成**剩下**的正文，而剩下的一律不是命令（引用以 "> " 开头、占位符以 "[" 开头），
    // 所以下游没有任何东西会重新解析它。
    //
    // 这一步跑在引用被前缀**之后**、而不是之前：留在上面会让 Router 拿到一个空的 CommandText，
    // 而它会从 Text 里填 —— 绕一圈回到同一处，却只在这条引用恰好不解析成命令时才成立。
    if let Some(control) = &control {
        if control.kind == crate::engine::commands::ControlCommandKind::FreshSession
            && control.body.is_empty()
        {
            command.clone_from(&text);
        }
    }

    // 一个富化过的 adapter 欠 Router 一个指令源（上游 `router.go:200-208`）。
    // 对一条单独的照片 / 文件 / 视频，`own_command_source` 故意答空串 —— 占位符不是发件人打的字
    // —— 但一旦引用被前缀上，Router 那个"空的 CommandText 就填 Text"的兜底会把**已经富化过**的
    // Text 赋进去，于是那条引用变成了 Chat 标题（#8058 的形态）。
    //
    // 所以交出**富化之前**的那个正文：仍然没有发件人没打过的字，而占位符会在下游被
    // `derive_first_message_title` 再丢一次，标题于是落回"同一张截图不带引用进来时"它走的那条
    // 媒体路径上。
    if command.is_empty() && !quoted.is_empty() {
        command = own_body;
    }

    let skip_agent_run = is_issue_command(&command);

    let force_fresh = control.as_ref().is_some_and(|control| {
        control.kind == crate::engine::commands::ControlCommandKind::FreshSession
    });

    let raw = WeComInboundMessage {
        bot_id: bot_id.to_string(),
        msg_id: callback.msgid.clone(),
        msg_type: callback.msgtype.clone(),
        chat_type: callback.chattype.clone(),
        chat_id: chat_id.clone(),
        sender_user_id: sender_id.clone(),
        content: text.clone(),
        req_id: req_id.to_string(),
        media,
    }
    .to_raw_value();

    InboundMessage {
        event_id: callback.msgid.clone(),
        message_id: callback.msgid.clone(),
        kind: channel_msg_type(&callback.msgtype),
        text,
        addressed_to_bot: true,
        // 发件人自己的话，群里的称呼已摘掉、媒体占位符已排除。命令分类是**共享**的
        // （`channel/message.rs`）并在它是空的时候退到 Text —— 而 Text 在群里以那个提及开头、
        // 在任何截图先到时以 "[Image]" 开头，所以在那条兜底上每一条这样的斜杠命令都被读成了普通
        // 散文。lark 从它的 command body 设这个字段、slack 从它洗干净的文本设 —— **wecom 曾是
        // 唯一一个把这个字段留空的 adapter。**
        command_text: command,
        // 引用是发件人**通过回复选中**的上下文，而 `has_selected_context` 指的正是这件事：即使一条
        // 控制指令自己没有正文，它也是输入。没有它，一条引用后面跟着的裸指令对 Router 是一条空消息
        // —— 什么都不持久化、也不回答任何人。
        has_selected_context: !quoted.is_empty(),
        media_refs: Vec::new(),
        reply_to: None,
        force_fresh,
        // `WeCom` 里一条**纯** `/issue` 不该触发 agent —— engine 已经建了 issue，出站回复器已经
        // 发了"✅ 已创建 #N"。让 agent 看到 "/issue foo" 只会再产出一条"我不认识这个斜杠命令"的
        // 回复，把对话搞乱。wecom 在这一条上是**独一份**的：slack / lark 保留历史上"让 agent 也
        // 看到 /issue 并回应"的行为。
        //
        // 读的是**同一个**源，engine 将会解析的那个 —— 所以群里的 `/issue` 与单聊的表现一致，
        // 而不是既建了 issue 又去问 agent。它必须是同一个源：读原始文本会让人一条单聊的
        // "@李雷 /issue …" 建了 issue 还保持沉默（上面那条 strip 之所以把关正是这个原因）；
        // 读解出来的正文则会让"先截图再 /issue"这条跳过 agent，而解析器又拒了那行占位符 ——
        // 发件人既没有 issue 也没有回答。
        skip_agent_run,
        source: Source {
            channel_type: crate::wecom::types::KIND,
            chat_id,
            chat_type,
            sender_id,
            // aibot 的 userid 是**每条 (bot, user) 匿名稳定**的标识，与真实企业 userid / email
            // 没有任何关系 ⇒ 没有跨安装的稳定身份可以填。
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        raw,
    }
}

/// 存库口径的渠道名（`channel_type` 列的字面量）—— 诊断与审计行用。
#[must_use]
pub fn storage_channel_type() -> &'static str {
    CHANNEL_TYPE
}

#[cfg(test)]
mod tests;
