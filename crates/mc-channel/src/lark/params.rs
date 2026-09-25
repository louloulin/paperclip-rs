//! lark **请求面与参数形状**：凭据的封装 / 每个端点入参 / **请求构建** / DB 参数形状
//! （上游 `internal/integrations/lark/{params.go}` + `client.go` 的入参族 + `http_client.go`
//! 的"请求构建"那一半 + `tx.go`）。
//!
//! - **写者**：M7-10（`docs/60-M7-PLAN.md` §3.3；本片是 stage 5 的第一片，**0 路由**）。
//! - **为什么要这个文件**：上游把**一个端点一层入参**写在同一份 `client.go` 里，本仓按
//!   `docs/60` §6.3 的四文件切分把"请求构建"独立出来（另三份见 [`super::types`]、
//!   [`super::client`]、[`super::http_client`]）。
//!
//! # 凭据纪律（`docs/60` §2.3 / 本片 `DoD` 第 6 条）
//!
//! - [`AppSecret`] 承载明文 `app_secret`：**手写 `Debug`** 只输出 `<redacted>`，明文只能经
//!   [`AppSecret::expose`] 显式取出（取用处因此总是可审计的）；
//! - [`InstallationCredentials`] 同样手写 `Debug`（它的 `app_secret` 字段走上面那条），
//!   `app_id` / `tenant_key` / `region` **不是**秘密（上游的日志逐字就打印 `app_id`）；
//! - 本文件没有任何 `tracing::*`；「错误路径不回显凭据」的用例在 `http_client/tests.rs`
//!   （它跑真 HTTP、真错误路径）。
//!
//! # `tx.go` 的对齐（**本仓不造第二个事务抽象**）
//!
//! 上游 `TxStarter` 在 lark 包内**重新声明**（而不是依赖 `internal/service`），为的是
//! integrations 层不反向引用 service 层。本仓的对偶物**已经存在**：`mc_db::Db`
//! （`mc_repos::RepoWithDb::db()` 的出口，事务由 `Db::pool().begin()` 开），M7-2 已合的
//! `mc_repos::channel::session` 就是它的消费者 ⇒ 再造一个 `TxStarter` 只会与 `Db` 抢同一职责。
//! 所以这条"对齐"以**边界纪律**的形式落地：lark 的 store 面（M7-14）跨边界只传
//! [`Id`] 与本文件的参数结构，**不**把 `Db` / `sqlx` 类型递进客户端的公开面
//! （登记在 `docs/32` §25 的 D 项）。

use std::fmt;

use serde_json::{json, Value};

use mc_core::id::Id;
use mc_core::timestamp::Timestamp;

use super::types::{ChatId, OpenId, Region};

// =====================================================================
// 凭据（上游 `client.go` 的 `InstallationCredentials`）
// =====================================================================

/// **明文** `app_secret`：`Debug` 只输出 `<redacted>`，明文只能经 [`AppSecret::expose`] 取出。
///
/// ⚠️ 这是本 crate 的**第四份**同形件（`slack::config::Sensitive` /
/// `telegram::config::Sensitive` / `dingtalk::stream::AppSecret`）。收敛（提到一个共享模块）
/// 要动 M7-3/4/5/7 的**已合**文件 ⇒ 不在本片写集；登记在 `docs/32` §25 的 D 项，
/// 与 §19 / §23 的同名登记同一张收敛票。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AppSecret(String);

impl fmt::Debug for AppSecret {
    /// 手写脱敏：**任何**格式化路径都拿不到明文。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AppSecret(<redacted>)")
    }
}

impl AppSecret {
    /// 包一个明文。
    #[must_use]
    pub fn new(plaintext: impl Into<String>) -> Self {
        Self(plaintext.into())
    }

    /// 取出明文（**唯一**出口：调用点因此总是显式可见的）。
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 是否为空（空串 = 未配置）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// 每次调用都要带的**按安装**传输上下文（上游 `InstallationCredentials`）。
///
/// 上游逐字的设计理由：把凭据**每次显式传入**（而不是给每条安装造一个客户端实例），
/// 让生命周期简单到没有（hub 解密一次 `app_secret`，之后每次出站调用都复用这个结构）。
///
/// **明文 `app_secret` 只在一次调用在飞期间存在于本结构里**；调用方**不得**记日志、不得持久化。
#[derive(Clone)]
pub struct InstallationCredentials {
    /// 应用的 `app_id`（`cli_…`）。**不是**秘密，上游日志逐字打印它。
    pub app_id: String,
    /// 应用的 `app_secret`（**秘密**；`Debug` 走 [`AppSecret`]，输出 `<redacted>`）。
    pub app_secret: AppSecret,
    /// 租户键；`None` = 那一行没写（`params.go` 的 `TenantKey pgtype.Text`）。
    pub tenant_key: Option<String>,
    /// 该安装所在的云：**每次调用**按它解析主机（[`Region::open_platform_base_url`]）。
    /// 默认飞书（历史行的取值）。
    pub region: Region,
}

impl fmt::Debug for InstallationCredentials {
    /// 手写脱敏：`app_secret` 走 [`AppSecret`] 的 `Debug`；其余三个字段显式列出（诊断需要）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallationCredentials")
            .field("app_id", &self.app_id)
            .field("app_secret", &self.app_secret)
            .field("tenant_key", &self.tenant_key)
            .field("region", &self.region)
            .finish()
    }
}

impl InstallationCredentials {
    /// 装配（`tenant_key` 缺失、region 取默认飞书）。
    #[must_use]
    pub fn new(app_id: impl Into<String>, app_secret: AppSecret) -> Self {
        Self {
            app_id: app_id.into(),
            app_secret,
            tenant_key: None,
            region: Region::default(),
        }
    }

    /// 补 `tenant_key`。
    #[must_use]
    pub fn with_tenant_key(mut self, tenant_key: impl Into<String>) -> Self {
        self.tenant_key = Some(tenant_key.into());
        self
    }

    /// 指定云。
    #[must_use]
    pub fn with_region(mut self, region: Region) -> Self {
        self.region = region;
        self
    }

    /// `app_id` / `app_secret` 是不是都齐（上游每个方法开头的 `missing app_id` 判据）。
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.app_id.is_empty() && !self.app_secret.is_empty()
    }
}

// =====================================================================
// wire 路径（上游 `http_client.go` 的逐字常量）
// =====================================================================

/// 自建应用的 `tenant_access_token` 端点（上游 `/open-apis/auth/v3/tenant_access_token/internal`）。
///
/// ⚠️ **`/internal` 不是笔误**：市场版 / 多租户应用走 `/tenant_access_token/v3` 且请求体形状
/// 不同，而本仓的 `PersonalAgent` 是**每 workspace 自建**的应用 ⇒ 停在 `/internal`（上游注释逐字）。
pub const TENANT_ACCESS_TOKEN_PATH: &str = "/open-apis/auth/v3/tenant_access_token/internal";

/// IM v1 消息集合（`POST` 发消息 / `GET` 列消息）。
pub const MESSAGES_PATH: &str = "/open-apis/im/v1/messages";

/// `/bot/v3/info`（安装期取 Bot 身份）。
pub const BOT_INFO_PATH: &str = "/open-apis/bot/v3/info";

/// 通讯录单用户查询（补 `union_id`）。
pub const CONTACT_USERS_PATH: &str = "/open-apis/contact/v3/users";

/// 通讯录批量查询（把 `open_id` 解析成显示名）。
pub const CONTACT_USERS_BATCH_PATH: &str = "/open-apis/contact/v3/users/batch";

/// 路径段转义（上游 `url.PathEscape` 的等价物）。
///
/// 本 crate 的依赖面冻结（无 `url` 直依赖）⇒ 自带一份**最小**实现：只放行 RFC 3986 的
/// unreserved（`A-Z a-z 0-9 - . _ ~`），其余字节按 `%XX` 转义。**逐字节**处理 UTF-8，
/// 于是非 ASCII 的 id 也不会漏出去（`url.PathEscape` 的同一语义）。
#[must_use]
pub fn escape_path_segment(raw: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if unreserved {
            out.push(char::from(*byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0F)]));
        }
    }
    out
}

/// `GET|PATCH /open-apis/im/v1/messages/{message_id}`。
#[must_use]
pub fn message_path(message_id: &str) -> String {
    format!("{MESSAGES_PATH}/{}", escape_path_segment(message_id))
}

/// `POST /open-apis/im/v1/messages/{message_id}/reply`（话题内回复端点）。
#[must_use]
pub fn reply_path(message_id: &str) -> String {
    format!("{}/reply", message_path(message_id))
}

/// `GET /open-apis/im/v1/messages/{message_id}/resources/{file_key}`。
#[must_use]
pub fn resource_path(message_id: &str, file_key: &str) -> String {
    format!(
        "{}/resources/{}",
        message_path(message_id),
        escape_path_segment(file_key)
    )
}

/// `POST /open-apis/im/v1/messages/{message_id}/reactions`。
#[must_use]
pub fn reactions_path(message_id: &str) -> String {
    format!("{}/reactions", message_path(message_id))
}

/// `DELETE /open-apis/im/v1/messages/{message_id}/reactions/{reaction_id}`。
#[must_use]
pub fn reaction_path(message_id: &str, reaction_id: &str) -> String {
    format!(
        "{}/{}",
        reactions_path(message_id),
        escape_path_segment(reaction_id)
    )
}

/// `GET /open-apis/contact/v3/users/{open_id}`。
#[must_use]
pub fn contact_user_path(open_id: &str) -> String {
    format!("{CONTACT_USERS_PATH}/{}", escape_path_segment(open_id))
}

// =====================================================================
// 入参（上游 `client.go` 的 `*Params` 族）
// =====================================================================

/// 出站消息的**线程目标**（上游 `ReplyTarget`）。
///
/// `message_id` 非空时传输层走 Lark 的 **回复端点**（`POST …/{id}/reply`），消息因此落进
/// 原消息所在的话题；`in_thread` 映射到 `reply_in_thread` 标志。**零值语义**（空
/// `message_id`）= "在会话层发送" —— 历史行为，不在乎线程的调用方就不设置它。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplyTarget {
    /// 被回复的消息 id；空 = 不线程化。
    pub message_id: String,
    /// 回复是否留在该消息的话题里（`reply_in_thread`）。
    pub in_thread: bool,
}

impl ReplyTarget {
    /// 是否该走回复端点：回复需要父 `message_id`，没有就回落到会话层发送。
    #[must_use]
    pub fn is_set(&self) -> bool {
        !self.message_id.is_empty()
    }
}

/// 发一张新互动卡（上游 `SendCardParams`）。
#[derive(Debug, Clone)]
pub struct SendCardParams {
    /// 该安装的凭据。
    pub credentials: InstallationCredentials,
    /// 目标会话。
    pub chat_id: ChatId,
    /// 原样的 Lark 互动卡 JSON 体（**不透明透传**：卡片模板可以独立演进，而不必拽着本接口走）。
    pub card_json: String,
    /// 线程目标（见 [`ReplyTarget`]）。
    pub reply_target: ReplyTarget,
}

/// 改一张已发出的卡片（上游 `PatchCardParams`）。Lark 的 patch 端点**整卡替换**，
/// 调用方（patcher）每次都要渲染**完整**的卡片。
#[derive(Debug, Clone)]
pub struct PatchCardParams {
    /// 该安装的凭据。
    pub credentials: InstallationCredentials,
    /// Lark 侧的卡片消息 id。
    pub card_message_id: String,
    /// 整卡 JSON 体。
    pub card_json: String,
}

/// 发一条纯文本消息（上游 `SendTextParams`）。`text` 原样交给 Lark，
/// `{"text": "…"}` 的信封由传输层编码。
#[derive(Debug, Clone)]
pub struct SendTextParams {
    /// 该安装的凭据。
    pub credentials: InstallationCredentials,
    /// 目标会话。
    pub chat_id: ChatId,
    /// 正文。
    pub text: String,
    /// 线程目标。
    pub reply_target: ReplyTarget,
}

/// 把 agent 回复作为 **schema 2.0 的 markdown 卡**发出（上游 `SendMarkdownCardParams`）。
///
/// 为什么要 2.0 而不是旧 schema 的 `div` + `lark_md`：后者的 markdown 方言窄得多 ——
/// 没有围栏代码块（语法高亮）、没有表格、没有标题字号；2.0 的 `markdown` 标签更接近 GFM。
#[derive(Debug, Clone)]
pub struct SendMarkdownCardParams {
    /// 该安装的凭据。
    pub credentials: InstallationCredentials,
    /// 目标会话。
    pub chat_id: ChatId,
    /// 正文（GFM 风格：`**粗**`、代码块、标题、有序/无序列表、链接、表格、引用、分割线）。
    pub markdown: String,
    /// 聊天列表 / 桌面通知里的单行预览；空 = 让 Lark 从正文自己推导。
    pub summary: String,
    /// 线程目标。
    pub reply_target: ReplyTarget,
}

/// 成员绑定提示卡（上游 `BindingPromptParams`）：单一 CTA（打开绑定链接）。
#[derive(Debug, Clone)]
pub struct BindingPromptParams {
    /// 该安装的凭据。
    pub credentials: InstallationCredentials,
    /// 收件人的按安装 `open_id`（**直接发给这个人**，不是发到会话）。
    pub open_id: OpenId,
    /// 用户点击的绝对 URL。令牌由**调用方**嵌进 URL；传输层**永远看不到**它。
    pub bind_url: String,
}

/// 给消息加一个表情表态（上游 `AddReactionParams`）。标准用途是"打字中"指示。
#[derive(Debug, Clone)]
pub struct AddReactionParams {
    /// 该安装的凭据。
    pub credentials: InstallationCredentials,
    /// 目标消息 id。
    pub message_id: String,
    /// 表情类型（例如 `Typing`）。
    pub emoji_type: String,
}

/// 撤掉一个已加的表态（上游 `DeleteReactionParams`）。
#[derive(Debug, Clone)]
pub struct DeleteReactionParams {
    /// 该安装的凭据。
    pub credentials: InstallationCredentials,
    /// 目标消息 id。
    pub message_id: String,
    /// 加表态时 Lark 返回的 `reaction_id`。
    pub reaction_id: String,
}

/// 取一个会话里**有界、最近**的一段消息（上游 `ListMessagesParams`）。
///
/// 只暴露富上下文装配器今天需要的字段；`start_time` 与 `page_token` **故意不暴露**
/// （等真的有调用方需要再加）。
#[derive(Debug, Clone, Default)]
pub struct ListMessagesParams {
    /// 目标会话。
    pub chat_id: ChatId,
    /// 非空时把窗口收窄到**一个 Lark 话题**（`container_id_type=thread` +
    /// `container_id=<thread_id>`），于是话题里的 `@` 提及**永远看不到**同会话的兄弟话题
    /// （上游 #5835）。Lark 的 thread 容器**不接受** `end_time` ⇒ 这条路径上
    /// `end_time` 被忽略，调用方在客户端侧锚定窗口。空 = 会话级容器。
    pub thread_id: String,
    /// 取最近的多少条；传输层把它夹到 Lark 的合法区间 `1..=50`。
    pub page_size: usize,
    /// `> 0` 时把窗口上界钉在这个 Unix 时间戳（**秒**，Lark 的 `end_time` 是秒粒度）。
    /// 富上下文装配器把它设成**触发消息**的时间，于是预取锚在 `@` 发生的那一刻，
    /// 而不是"取的时候最新"。`thread_id` 非空时忽略。
    pub end_time: i64,
}

/// 下载一条消息挂的资源（上游 `DownloadResourceParams`）。
#[derive(Debug, Clone, Default)]
pub struct DownloadResourceParams {
    /// 载有资源的消息 id。
    pub message_id: String,
    /// 资源的 `file_key` / `image_key`。
    pub file_key: String,
    /// 开放平台的资源类别：`image`（对应 `image_key`）或 `file`（`file_key` 系的视频/文件/音频）。
    pub resource_type: String,
}

// =====================================================================
// 请求构建（上游 `http_client.go` 的 `outboundMessageRequest` + 绑定卡模板）
// =====================================================================

/// 三个发送端点共用的 `(path, body)`（上游 `outboundMessageRequest`）。
///
/// [`ReplyTarget::is_set`] 为真时走 **回复端点**（消息落进原消息的话题，`reply_in_thread`
/// 带上目标的 `in_thread`）；否则走会话级发送端点（`receive_id_type=chat_id`，历史行为）。
/// `body` 是 `Value`（不是 `map[string]string`）因为 `reply_in_thread` 是**布尔**。
#[must_use]
pub fn outbound_message_request(
    chat_id: &ChatId,
    msg_type: &str,
    content: &str,
    target: &ReplyTarget,
) -> (String, Value) {
    if target.is_set() {
        let body = json!({
            "msg_type": msg_type,
            "content": content,
            "reply_in_thread": target.in_thread,
        });
        return (reply_path(&target.message_id), body);
    }
    let body = json!({
        "receive_id": chat_id.as_str(),
        "msg_type": msg_type,
        "content": content,
    });
    (format!("{MESSAGES_PATH}?receive_id_type=chat_id"), body)
}

/// 富上下文预取的 `(path, 是否按会话容器)`（上游 `ListChatMessages` 的查询构建）。
///
/// `thread_id` 非空 ⇒ `container_id_type=thread`（且**不**带 `end_time`）；
/// 否则 `container_id_type=chat`（`end_time > 0` 才带）。`page_size` 在这里**夹紧**到
/// Lark 的单页上限（超过就静默取上限，而不是让 Lark 回 400）。
#[must_use]
pub fn list_messages_request(params: &ListMessagesParams) -> String {
    let size = params.page_size.clamp(1, MAX_LIST_MESSAGES_PAGE_SIZE);
    let mut query = String::from("?");
    if params.thread_id.is_empty() {
        query.push_str("container_id_type=chat&container_id=");
        query.push_str(&escape_path_segment(params.chat_id.as_str()));
        if params.end_time > 0 {
            query.push_str("&end_time=");
            query.push_str(&params.end_time.to_string());
        }
    } else {
        query.push_str("container_id_type=thread&container_id=");
        query.push_str(&escape_path_segment(&params.thread_id));
    }
    query.push_str("&sort_type=ByCreateTimeDesc&page_size=");
    query.push_str(&size.to_string());
    query.push_str("&user_id_type=open_id");
    format!("{MESSAGES_PATH}{query}")
}

/// Lark 对 IM 消息列表**单页**的硬上限（上游 `larkListMessagesMaxPageSize`）。
pub const MAX_LIST_MESSAGES_PAGE_SIZE: usize = 50;

/// Lark 对 `contact/v3/users/batch` **单次** `user_ids` 的硬上限
/// （上游 `larkBatchGetUsersMaxIDs`；超出部分**丢弃**而不是报错）。
pub const MAX_BATCH_GET_USERS_IDS: usize = 50;

/// `contact/v3/users/batch` 的查询串（`user_id_type=open_id` + 重复的 `user_ids`）；
/// 超过 [`MAX_BATCH_GET_USERS_IDS`] 的入参按上游口径**截断**，空串一律跳过。
#[must_use]
pub fn batch_get_users_query(open_ids: &[String]) -> String {
    let mut query = String::from("?user_id_type=open_id");
    for open_id in open_ids.iter().take(MAX_BATCH_GET_USERS_IDS) {
        if open_id.is_empty() {
            continue;
        }
        query.push_str("&user_ids=");
        query.push_str(&escape_path_segment(open_id));
    }
    query
}

/// 绑定提示卡的 JSON 体（上游 `bindingPromptTemplate`）。
///
/// 单 CTA 指向兑换 URL；其余是应用内语气的中文文案。**留在这里**（而不是渲染器里）：
/// 绑定卡是一次性的，状态卡是原地 patch 的 —— 生命周期不同，模板要能各自演进。
///
/// 本仓用 `serde_json::Value` 直接构造（上游 `map[string]any` 的等价物）⇒ **没有**错误路径
/// （上游那个返回 `error` 的签名是 `json.Marshal` 的形状，在本仓不可能失败）。
#[must_use]
pub fn binding_prompt_card(bind_url: &str) -> String {
    let card = json!({
        "config": { "wide_screen_mode": true },
        "header": {
            "template": "blue",
            "title": { "tag": "plain_text", "content": "Multica" },
        },
        "elements": [
            {
                "tag": "div",
                "text": {
                    "tag": "lark_md",
                    "content": "你还没有绑定 Multica 账户。点击下方按钮完成绑定后即可使用此 Agent。",
                },
            },
            {
                "tag": "action",
                "actions": [
                    {
                        "tag": "button",
                        "text": { "tag": "plain_text", "content": "去绑定" },
                        "type": "primary",
                        "url": bind_url,
                    },
                ],
            },
        ],
    });
    card.to_string()
}

// =====================================================================
// DB 参数形状（上游 `params.go` 的 17 个结构）
// =====================================================================

/// 把**按安装 + 按 workspace**限定的一次安装查找传给 store。
///
/// 上游 `params.go` 的 `pgtype.UUID` 在本仓就是 [`Id`]；`pgtype.Text` 是 `Option<String>`
/// （`None` = SQL `NULL`）；`pgtype.Timestamptz` 是 [`Timestamp`]。**逐字段对齐，别顺手增删**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetInstallationInWorkspaceParams {
    /// `lark_installation.id`。
    pub id: Id,
    /// 该安装所属的 workspace。
    pub workspace_id: Id,
}

/// 一次安装 / 重装的扁平字段（上游 `UpsertInstallationParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertInstallationParams {
    /// 所属 workspace。
    pub workspace_id: Id,
    /// 挂在这个安装上的 agent。
    pub agent_id: Id,
    /// 应用的 `app_id`（`cli_…`）。
    pub app_id: String,
    /// `app_secret` 的**密文**（`nonce ‖ ct ‖ tag` 的单块字节；明文**绝不**入库）。
    pub app_secret_encrypted: Vec<u8>,
    /// Bot 的按安装 `open_id`。
    pub bot_open_id: String,
    /// 安装发起人。
    pub installer_user_id: Id,
    /// 租户键（可空）。
    pub tenant_key: Option<String>,
    /// Bot 的 `union_id`（可空；未解析时是 `None` —— 见 [`SetInstallationBotUnionIdParams`]）。
    pub bot_union_id: Option<String>,
    /// `lark_installation.region` 的字面量（`feishu` / `lark`）。
    pub region: String,
}

/// 翻一条安装的 `status`（上游 `SetInstallationStatusParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetInstallationStatusParams {
    /// `lark_installation.id`。
    pub id: Id,
    /// `active` / `revoked` 的字面量。
    pub status: String,
}

/// 补记 Bot 的 `union_id`（上游 `SetInstallationBotUnionIDParams`；补查路径专用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetInstallationBotUnionIdParams {
    /// `lark_installation.id`。
    pub id: Id,
    /// `None` = SQL `NULL`（**不是**空串：列可空，空串会与"未解析"混起来）。
    pub bot_union_id: Option<String>,
}

/// 抢占一条安装的 WS 租约（上游 `AcquireWSLeaseParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquireWsLeaseParams {
    /// 新租约令牌。
    pub new_token: Option<String>,
    /// 新租约到期时刻。
    pub new_expires_at: Option<Timestamp>,
    /// `lark_installation.id`。
    pub id: Id,
}

/// 释放一条**自己还持有**的 WS 租约（上游 `ReleaseWSLeaseParams`；带当前令牌做围栏）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseWsLeaseParams {
    /// `lark_installation.id`。
    pub id: Id,
    /// 调用方认为自己在持有的令牌。
    pub current_token: Option<String>,
}

/// 按**渠道原生**用户 id 查绑定（上游 `GetUserBindingByOpenIDParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetUserBindingByOpenIdParams {
    /// 安装 id。
    pub installation_id: Id,
    /// 渠道侧的用户 id（lark 就是 `open_id`）。
    pub channel_user_id: String,
}

/// 建一条成员绑定（上游 `CreateUserBindingParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateUserBindingParams {
    /// 所属 workspace。
    pub workspace_id: Id,
    /// Multica 用户 id。
    pub multica_user_id: Id,
    /// 安装 id。
    pub installation_id: Id,
    /// 渠道侧的用户 id。
    pub channel_user_id: String,
    /// `union_id`（可空）。
    pub union_id: Option<String>,
}

/// 按会话 id 查绑定（上游 `GetChatSessionBindingParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetChatSessionBindingParams {
    /// 安装 id。
    pub installation_id: Id,
    /// 渠道侧的会话 id。
    pub channel_chat_id: String,
}

/// 记录最新的入站触发消息与线程，供出站 patcher 定位回复（上游 `UpdateChatSessionBindingReplyTargetParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateChatSessionBindingReplyTargetParams {
    /// Multica 的 `chat_session.id`。
    pub chat_session_id: Id,
    /// 最新的触发消息 id（可空）。
    pub last_message_id: Option<String>,
    /// 最新的线程 id（可空）。
    pub last_thread_id: Option<String>,
}

/// 两阶段幂等：**认领**一条入站消息的 dedup 行（上游 `ClaimInboundDedupParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimInboundDedupParams {
    /// 安装 id。
    pub installation_id: Id,
    /// 平台的消息 id。
    pub message_id: String,
}

/// 把已认领的消息标记为已处理（**带围栏令牌**；上游 `MarkInboundDedupProcessedParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkInboundDedupProcessedParams {
    /// 安装 id。
    pub installation_id: Id,
    /// 平台的消息 id。
    pub message_id: String,
    /// 认领时拿到的围栏令牌。
    pub claim_token: Id,
}

/// 处理失败时释放认领（**带围栏令牌**；上游 `ReleaseInboundDedupParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInboundDedupParams {
    /// 安装 id。
    pub installation_id: Id,
    /// 平台的消息 id。
    pub message_id: String,
    /// 认领时拿到的围栏令牌。
    pub claim_token: Id,
}

/// 写一条**不含正文**的丢弃审计（上游 `RecordInboundDropParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordInboundDropParams {
    /// 平台事件类型。
    pub event_type: String,
    /// 丢弃原因的字面量（见 [`super::types::DropReason::as_str`]）。
    pub drop_reason: String,
    /// 安装 id（无安装的事件可以是 `Id::nil()` —— 上游的"可能无效的 UUID"）。
    pub installation_id: Id,
    /// 渠道会话 id（可空）。
    pub channel_chat_id: Option<String>,
    /// 平台事件 id（可空）。
    pub channel_event_id: Option<String>,
    /// 平台消息 id（可空）。
    pub channel_message_id: Option<String>,
}

/// 铸一枚**短期、单次**的成员绑定令牌（上游 `CreateBindingTokenParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateBindingTokenParams {
    /// 令牌的**哈希**（明文令牌**不入库**）。
    pub token_hash: String,
    /// 所属 workspace。
    pub workspace_id: Id,
    /// 安装 id。
    pub installation_id: Id,
    /// 渠道侧的用户 id。
    pub channel_user_id: String,
    /// 到期时刻（存储层的 `CHECK` 把上限钉在 [`super::types::BINDING_TOKEN_TTL`]）。
    pub expires_at: Timestamp,
}

/// 记一条出站卡片（上游 `CreateOutboundCardMessageParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOutboundCardMessageParams {
    /// Multica 的 `chat_session.id`。
    pub chat_session_id: Id,
    /// 渠道侧的会话 id。
    pub channel_chat_id: String,
    /// 平台侧的卡片消息 id。
    pub channel_card_message_id: String,
    /// 该卡片的业务状态。
    pub status: String,
    /// 关联的 task id。
    pub task_id: Id,
}

/// 翻一条出站卡片的状态（上游 `UpdateOutboundCardStatusParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutboundCardStatusParams {
    /// 卡片行 id。
    pub id: Id,
    /// 新状态。
    pub status: String,
}

#[cfg(test)]
mod tests;
