//! lark 客户端**契约面与错误分类**：`ApiClient` 端口 / 未装配时的替身 / 平台错误的类别
//! （上游 `internal/integrations/lark/client.go` + `http_client.go` 的"错误分类"那一半）。
//!
//! - **写者**：M7-10（`docs/60-M7-PLAN.md` §3.3；本片是 stage 5 的第一片，**0 路由**）。
//! - **为什么端口在 lark 包内**（上游注释逐字）：`ApiClient` 是"本包需要的那一小片 Lark
//!   开放平台 HTTP 面"，**故意不取自厂商 SDK** —— 于是包里其余部分能在不拖进 Lark 传输层的
//!   前提下构建与单测，也能在不碰调用点的情况下换实现（真 SDK / 替身 / 假货）。
//!   **每个方法都按单条安装限定**：调用方已经认证过安装行并解密了 `app_secret`；
//!   客户端**从不**自己读 `lark_installation`。
//! - **本文件的边界**：**错误分类**。请求构建在 [`super::params`]、响应解码在
//!   [`super::types`]、传输核心在 [`super::http_client`]。
//!
//! # 错误分类（本片专属验收第 1 条：限流 / 凭据失效 / 网络**各一反例**）
//!
//! | [`ErrorClass`] | 判据（**逐条有上游出处**） | 调用方该做什么 |
//! | --- | --- | --- |
//! | [`ErrorClass::InvalidCredential`] | 平台码 `99_991_663`（`tenant_access_token` 既"已过期"又"凭证无效"**共用同一个码**）或 `99_991_664`（`app_access_token`） | 作废令牌缓存 + 重铸**一次**后重放（[`super::http_client::HttpApiClient`] 已内建）；再失败就是凭据真坏了 |
//! | [`ErrorClass::RateLimited`] | HTTP `429`，或平台频控码 `99_991_400` / `230_020` | 退避重试（**不**作废令牌） |
//! | [`ErrorClass::Transport`] | 链路层（DNS / 连接 / 超时 / 读体失败），或非 2xx **且没有可解析的平台码** | 交付与否**不确定** ⇒ 不重试（除 supervisor 的退避重连），也**不**改令牌 |
//! | [`ErrorClass::Refused`] | 2xx（或带码的非 2xx）里的**业务码**：请求到了 Lark、Lark 明确拒绝且**什么都没发** | 按业务处理（例如 [`is_thread_reply_unsupported`] 的那六条才允许回落到会话层发送） |
//! | [`ErrorClass::Malformed`] | 2xx 但不是认得的形状 / 入参不全 / 资源超上限 | 修代码或修入参，不重试 |
//! | [`ErrorClass::NotConfigured`] | 部署没接真客户端（[`StubApiClient`] 的哨兵） | 按"该平台未配置"处理（**不是** 5xx） |
//!
//! ⚠️ **平台错误体里的 `msg` 一律丢掉**（与 M7-8 / M7-9 的 `DingTalk` 侧同款，登记
//! `docs/32` §25 的 D 项）：本客户端的**铸令牌请求体里就是 `app_secret`**，而平台错误体正是
//! 最容易回声请求体的地方 ⇒ 错误只带 `op` / `status` / `code` 三个**我们自己或平台机器码**
//! 的字段。`http_client/tests.rs` 有一条专门用例钉住"错误路径不回显凭据"。

use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;

use super::http_client::resource::{DownloadedResource, DownloadedResourceStream};
use super::params::{
    AddReactionParams, BindingPromptParams, DeleteReactionParams, DownloadResourceParams,
    InstallationCredentials, ListMessagesParams, PatchCardParams, SendCardParams,
    SendMarkdownCardParams, SendTextParams,
};
use super::types::{BotInfo, LarkMessage};
use crate::channel::ChannelError;

// =====================================================================
// 平台码（上游 `http_client.go` 的常量，逐字）
// =====================================================================

/// "你出示的凭据被拒了"的租户令牌码。
///
/// 它**同时**覆盖两半 —— "`tenant_access_token` 已过期"与"凭证无效"共用这一个码 —— 也是上游
/// #7611 那只"卡住的缓存"撞上的码。
pub const CODE_TENANT_TOKEN_INVALID: i32 = 99_991_663;

/// `app_access_token` 的对应码。
///
/// 本客户端**从不**铸 `app_access_token`；它留在这一类是**故意**的：它无歧义地表示"你出示的
/// 凭据被拒"，而万一 Lark 真拿它回一次我们以租户令牌认证的调用，重铸就是唯一的恢复路径。
/// 判错只花一次有界的刷新 + 一次重放；漏掉它则要再养一只卡住的缓存。
pub const CODE_APP_TOKEN_INVALID: i32 = 99_991_664;

/// 开放平台的**通用**频控码（请求触发频率限制）。
pub const CODE_FREQUENCY_LIMIT: i32 = 99_991_400;

/// IM 消息发送端点的频控码（上游 `threadReplyUnsupportedCodes` 的注释逐字点名它是
/// **限流**而不是"这条消息不能收线程回复"）。
pub const CODE_IM_RATE_LIMIT: i32 = 230_020;

/// "机器人给不了这个人发私信"（对方在机器人的可见范围之外）。
///
/// 保留**群级**回复路径可见，用户因此仍能拿到指引（上游口径）。
pub const CODE_NO_AVAILABILITY: i32 = 230_013;

/// HTTP 429（限流）。
pub const HTTP_TOO_MANY_REQUESTS: u16 = 429;

/// 只有这些**业务码**才允许把线程回复回落成会话层发送（上游 `threadReplyUnsupportedCodes`，逐条）。
///
/// 判据是"这条触发消息 / 话题**确实**收不了线程回复，且**什么都没发**，而同会话的普通发送不受
/// 影响"。**故意排除**：限流（[`CODE_IM_RATE_LIMIT`]）、"消息正在发送中"（`230049`，交付与否
/// 不明确）、权限/内容错（会话层也会失败）、以及一切传输 / 5xx / 超时 —— 那些保持失败，
/// 于是我们**永不**重复回复、也**永不**把只该在线程里的回复泄进主群聊。
pub const THREAD_REPLY_UNSUPPORTED_CODES: [i32; 6] = [
    230_011, // 触发消息已被撤回
    230_019, // 话题不存在
    230_050, // 触发消息对操作者不可见
    230_071, // 该群不支持话题内回复
    230_072, // 合并消息不支持话题内回复
    230_111, // 不能回复自毁消息
];

// =====================================================================
// 错误
// =====================================================================

/// 错误**类别**（稳定、可上报；判据见模块文档的表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorClass {
    /// 部署没接真客户端（[`StubApiClient`] 的哨兵）。
    NotConfigured,
    /// 凭据失效 ⇒ 作废缓存 + 重铸一次后重放。
    InvalidCredential,
    /// 限流 ⇒ 退避重试。
    RateLimited,
    /// 链路层 / 交付是否发生**不确定**。
    Transport,
    /// 平台明确拒绝且什么都没发。
    Refused,
    /// 形状不对 / 入参不全 / 资源超上限。
    Malformed,
}

impl ErrorClass {
    /// 稳定字串（看板聚合 / 日志字段用；与 `ChannelError::code` 的命名风格一致）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::InvalidCredential => "invalid_credential",
            Self::RateLimited => "rate_limited",
            Self::Transport => "transport",
            Self::Refused => "refused",
            Self::Malformed => "malformed",
        }
    }
}

impl fmt::Display for ErrorClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// lark HTTP 面的一次失败。
///
/// 变体只带 `op`（**我们自己**给的静态操作名）、HTTP 状态码、平台机器码与上限数字 ——
/// **不带**平台 `msg`、不带 URL、不带请求体、不带响应体（见模块文档的凭据一段）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// 部署没接真客户端（[`StubApiClient`] 的哨兵；上游 `ErrAPIClientNotConfigured`）。
    #[error("lark: API client not configured")]
    NotConfigured,
    /// 链路层失败（DNS / 连接 / 超时 / 响应体读不出来）。
    #[error("lark: {op}: transport failed")]
    Transport {
        /// 操作名。
        op: &'static str,
    },
    /// 非 2xx 且**没有**可解析的平台信封。
    #[error("lark: {op}: http {status}")]
    Http {
        /// 操作名。
        op: &'static str,
        /// HTTP 状态码。
        status: u16,
    },
    /// 平台给了错误码：2xx 的业务拒绝，或带码的非 2xx（Lark 把凭据失效表达成 HTTP 400 +
    /// `{"code":99991663}`，**不是** 2xx 信封）。
    #[error("lark: {op}: refused (code={code})")]
    Refused {
        /// 操作名。
        op: &'static str,
        /// 带码时的 HTTP 状态码；2xx 业务拒绝是 `None`。
        status: Option<u16>,
        /// 平台机器码。
        code: i32,
    },
    /// 2xx 但不是我们认得的形状。
    #[error("lark: {op}: malformed response")]
    Malformed {
        /// 操作名。
        op: &'static str,
    },
    /// 入参不全 —— **发之前**就拒（上游每个方法开头的 `missing chat_id` 一族）。
    #[error("lark: {op}: {reason}")]
    InvalidRequest {
        /// 操作名。
        op: &'static str,
        /// 缺什么 / 哪里不对。
        reason: &'static str,
    },
    /// 资源超过传输层的上限（上游 `maxMessageResourceBytes`）。
    #[error("lark: {op}: resource exceeds {cap} bytes")]
    ResourceTooLarge {
        /// 操作名。
        op: &'static str,
        /// 上限（字节）。
        cap: usize,
    },
}

impl ApiError {
    /// 操作名（静态常量或字符串字面量；**不含**任何请求内容）。
    #[must_use]
    pub fn op(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Transport { op }
            | Self::Http { op, .. }
            | Self::Refused { op, .. }
            | Self::Malformed { op }
            | Self::InvalidRequest { op, .. }
            | Self::ResourceTooLarge { op, .. } => op,
        }
    }

    /// 平台机器码；没有码（链路失败 / 纯状态码 / 形状错）时是 `None`。
    #[must_use]
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Refused { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// 错误类别（判据见模块文档的表）。
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::NotConfigured => ErrorClass::NotConfigured,
            Self::InvalidRequest { .. }
            | Self::Malformed { .. }
            | Self::ResourceTooLarge { .. } => ErrorClass::Malformed,
            Self::Transport { .. } => ErrorClass::Transport,
            Self::Http { status, .. } => {
                if *status == HTTP_TOO_MANY_REQUESTS {
                    ErrorClass::RateLimited
                } else {
                    ErrorClass::Transport
                }
            }
            Self::Refused { code, .. } => {
                if is_token_error(*code) {
                    ErrorClass::InvalidCredential
                } else if is_rate_limit_code(*code) {
                    ErrorClass::RateLimited
                } else {
                    ErrorClass::Refused
                }
            }
        }
    }

    /// 映射成本 crate 的渠道错误（`send` / `connect` 的返回面）。
    ///
    /// **凭据失效**落 [`ChannelError::Auth`]（凭据面）—— 不许被 supervisor 当成可重试的传输
    /// 失败；其余一律 [`ChannelError::Transport`]（与 M7-8 / M7-9 的 `DingTalk` 侧同款）。
    /// 更细的分流（限流退避、业务拒绝）走 [`ApiError::class`]，调用方自己判。
    #[must_use]
    pub fn into_channel_error(self) -> ChannelError {
        match self.class() {
            ErrorClass::InvalidCredential => ChannelError::Auth {
                message: "lark: tenant access token is invalid".to_string(),
            },
            _ => ChannelError::Transport {
                message: self.to_string(),
            },
        }
    }
}

/// 这个平台码是不是"你出示的凭据被拒了"（上游 `isTokenError`）。
#[must_use]
pub fn is_token_error(code: i32) -> bool {
    code == CODE_TENANT_TOKEN_INVALID || code == CODE_APP_TOKEN_INVALID
}

/// 这个平台码是不是频控。
#[must_use]
pub fn is_rate_limit_code(code: i32) -> bool {
    code == CODE_FREQUENCY_LIMIT || code == CODE_IM_RATE_LIMIT
}

/// 从错误体里**尽力**取出平台码（上游 `parseLarkErrorBody`）。
///
/// 完全不是 JSON 的体（代理的 HTML 错误页、空的 502）⇒ `None` —— 没有码可分类，这类失败因此
/// 留在"纯传输"路径上。
#[must_use]
pub fn parse_lark_error_body(raw: &[u8]) -> Option<i32> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        code: i32,
    }
    serde_json::from_slice::<Envelope>(raw)
        .ok()
        .map(|env| env.code)
}

/// 这一条错误是否能安全地回落成会话层发送（上游 `isThreadReplyUnsupported`）。
///
/// 只有 [`THREAD_REPLY_UNSUPPORTED_CODES`] 里的码为真。没有平台码的（链路失败 / 超时 /
/// 代理的 HTML 502）一律为假 ⇒ **交付不明确的失败永不触发回落**。
#[must_use]
pub fn is_thread_reply_unsupported(error: &ApiError) -> bool {
    error
        .code()
        .is_some_and(|code| THREAD_REPLY_UNSUPPORTED_CODES.contains(&code))
}

// =====================================================================
// 端口（上游 `client.go` 的 `APIClient` + `TokenCacheInvalidator`）
// =====================================================================

/// 本包需要的那一小片 Lark 开放平台 HTTP 面（上游 `APIClient`）。
///
/// 每个方法都**按单条安装限定**：调用方已经认证过安装行、解密过 `app_secret`；
/// 实现**从不**自己读 `lark_installation`。
///
/// 实现方：生产是 [`super::http_client::HttpApiClient`]，未装配时是 [`StubApiClient`]。
#[async_trait]
pub trait ApiClient: Send + Sync {
    /// 这个客户端能不能真的够到 Lark。
    ///
    /// 它是"出站 HTTP 已接线"的信号：替身返回 `false`；真客户端一旦构造出来就返回 `true`。
    /// 路由层据此决定要不要把需要跟 Lark 通话的安装 / 管理界面亮出来。
    fn is_configured(&self) -> bool;

    /// 往一个会话发一张新互动卡，返回 Lark 的 `message_id`（patcher 拿它做后续 patch 的靶子）。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝。
    async fn send_interactive_card(&self, params: SendCardParams) -> Result<String, ApiError>;

    /// 替换一张已发出的卡片的正文。**限流决策归调用方**，本方法只做网络调用。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝。
    async fn patch_interactive_card(&self, params: PatchCardParams) -> Result<(), ApiError>;

    /// 往一个会话发一条纯文本消息（无 markdown 语法的短句走这条）。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝。
    async fn send_text_message(&self, params: SendTextParams) -> Result<String, ApiError>;

    /// 把 agent 回复作为 **schema 2.0 的 markdown 卡**发出（正文含 markdown 时走这条）。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝。
    async fn send_markdown_card(&self, params: SendMarkdownCardParams) -> Result<String, ApiError>;

    /// 发"你需要先绑定"的专用出站（绑卡模板留在实现里，调用点因此不必知道卡片 schema）。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝。
    async fn send_binding_prompt_card(&self, params: BindingPromptParams) -> Result<(), ApiError>;

    /// 取 Bot 的按安装 `open_id`（以及尽力而为的 `union_id`）。**注册服务是唯一调用方**。
    ///
    /// # Errors
    ///
    /// 凭据不全 / 链路失败 / 平台拒绝 / 响应缺 `open_id`。
    async fn get_bot_info(&self, credentials: InstallationCredentials)
        -> Result<BotInfo, ApiError>;

    /// 按 id 取一条消息。
    ///
    /// Lark **永远**回数组（`data.items[]`）：普通消息恰好一项；`merge_forward` 消息里第一项是
    /// 转发哨兵、其余是被打包的子消息（各自按 `upper_message_id` 链回父）。
    /// **两项都保留**：富上下文装配器对引用回复用 `items[0]`、对转发记录用 `items[1..]`。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝（撤回或越权都落这里）。
    async fn get_message(
        &self,
        credentials: InstallationCredentials,
        message_id: &str,
    ) -> Result<Vec<LarkMessage>, ApiError>;

    /// 列一个会话里最近的一段消息（群上下文预取）。
    ///
    /// 只取**单页**（`page_size <= 50`）：分页被故意不暴露，好让入站 ACK 路径的 HTTP 扇出
    /// 保持一次往返。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝。
    async fn list_chat_messages(
        &self,
        credentials: InstallationCredentials,
        params: ListMessagesParams,
    ) -> Result<Vec<LarkMessage>, ApiError>;

    /// 下载一条消息挂的**一个**二进制资源（缓冲式，带上限）。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝 / 超过上限。
    async fn download_message_resource(
        &self,
        credentials: InstallationCredentials,
        params: DownloadResourceParams,
    ) -> Result<DownloadedResource, ApiError>;

    /// 同一资源下载的**流式**版本（上游 `httpAPIClient.DownloadMessageResourceStream`，
    /// 只在包内可见的那个额外方法）。
    ///
    /// ⚠️ **与上游的一处形态差异**（登记 `docs/32` §25 的 D 项）：上游把它留在**具体类型**上
    /// （接口只暴露缓冲版），本仓把它放进 `trait` —— 因为媒体面（M7-12）必须隔着
    /// `dyn ApiClient` 用它，而 `http_client.rs` 是**单写者**文件、后续片不得再改。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝。
    async fn download_message_resource_stream(
        &self,
        credentials: InstallationCredentials,
        params: DownloadResourceParams,
    ) -> Result<DownloadedResourceStream, ApiError>;

    /// 把一组用户 `open_id` 解析成显示名。
    ///
    /// 只包含 API **真的**返回的 `id → name`：通讯录范围受限或未知 id 只会让映射变小
    /// （`code == 0` 但项更少），**不会**报错 ⇒ 富上下文装配器降级成位置标签。
    /// 超过单次上限的 id 被客户端丢弃。
    ///
    /// # Errors
    ///
    /// 链路失败 / 平台拒绝。
    async fn batch_get_users(
        &self,
        credentials: InstallationCredentials,
        open_ids: Vec<String>,
    ) -> Result<HashMap<String, String>, ApiError>;

    /// 给消息加一个表情表态（标准用途是"打字中"指示），返回 `reaction_id` 供后续撤掉。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝。
    async fn add_message_reaction(&self, params: AddReactionParams) -> Result<String, ApiError>;

    /// 撤掉一个已加的表态（打字指示生命周期的清理那一半）。
    ///
    /// # Errors
    ///
    /// 入参不全 / 链路失败 / 平台拒绝。
    async fn delete_message_reaction(&self, params: DeleteReactionParams) -> Result<(), ApiError>;
}

/// 会**进程内缓存** `tenant_access_token` 的 [`ApiClient`] 实现，因此凭据轮换能叫它忘掉手里那份。
///
/// 上游注释逐字：轮换否则是**不可见的** —— 重新注册 Bot 会在**同一个 `app_id`** 下发一把新的
/// `app_secret`，Lark 随即吊销用旧密钥铸出的**每一个**令牌，而缓存键（`app_id`）**没变**。
///
/// 它**故意不从** [`ApiClient`] 继承（不是一个方法）：只有真的 HTTP 客户端手里有缓存，
/// 替身 / 假货无物可忘。调用方类型断言后跳过。
pub trait TokenCacheInvalidator {
    /// 忘掉 `app_id` 的那份缓存令牌。
    fn invalidate_token_cache(&self, app_id: &str);
}

// =====================================================================
// 未装配时的替身（上游 `stubAPIClient`）
// =====================================================================

/// 没有注册生产客户端时的**默认** [`ApiClient`]（上游 `stubAPIClient`）。
///
/// 它对**每一个**传输调用都回 [`ApiError::NotConfigured`]，于是配置错了的部署会**响亮地**
/// 失败，而不是悄悄丢卡片或丢扫码注册的响应。
///
/// 上游注释逐字：我们**故意不做**"静默成功"—— 一个返回空 `message_id` 的替身会让入站分派器
/// 记下指向虚空的一堆 `lark_outbound_card_message` 行。
#[derive(Debug, Clone, Copy, Default)]
pub struct StubApiClient;

impl StubApiClient {
    /// 构造（无状态）。
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// 每个方法的共同收尾：打一行警告（**只带操作名与非凭据标识**）并回哨兵错误。
    fn refuse(op: &'static str, detail: impl fmt::Display) -> ApiError {
        tracing::warn!(op, detail = %detail, "lark stub client: transport call refused");
        ApiError::NotConfigured
    }
}

#[async_trait]
impl ApiClient for StubApiClient {
    fn is_configured(&self) -> bool {
        false
    }

    async fn send_interactive_card(&self, params: SendCardParams) -> Result<String, ApiError> {
        Err(Self::refuse("send_interactive_card", params.chat_id))
    }

    async fn patch_interactive_card(&self, params: PatchCardParams) -> Result<(), ApiError> {
        Err(Self::refuse(
            "patch_interactive_card",
            params.card_message_id,
        ))
    }

    async fn send_text_message(&self, params: SendTextParams) -> Result<String, ApiError> {
        Err(Self::refuse("send_text_message", params.chat_id))
    }

    async fn send_markdown_card(&self, params: SendMarkdownCardParams) -> Result<String, ApiError> {
        Err(Self::refuse("send_markdown_card", params.chat_id))
    }

    async fn send_binding_prompt_card(&self, params: BindingPromptParams) -> Result<(), ApiError> {
        Err(Self::refuse("send_binding_prompt_card", params.open_id))
    }

    async fn get_bot_info(
        &self,
        _credentials: InstallationCredentials,
    ) -> Result<BotInfo, ApiError> {
        // ⚠️ 这里**不打印 `app_id`**：凭据纪律优先于上游的日志字段，而这条警告的诊断价值全在
        // "谁在调未装配的客户端"。
        Err(Self::refuse("get_bot_info", "an installation"))
    }

    async fn get_message(
        &self,
        _credentials: InstallationCredentials,
        message_id: &str,
    ) -> Result<Vec<LarkMessage>, ApiError> {
        Err(Self::refuse("get_message", message_id))
    }

    async fn list_chat_messages(
        &self,
        _credentials: InstallationCredentials,
        params: ListMessagesParams,
    ) -> Result<Vec<LarkMessage>, ApiError> {
        Err(Self::refuse("list_chat_messages", params.chat_id))
    }

    async fn download_message_resource(
        &self,
        _credentials: InstallationCredentials,
        params: DownloadResourceParams,
    ) -> Result<DownloadedResource, ApiError> {
        Err(Self::refuse("download_message_resource", params.message_id))
    }

    async fn download_message_resource_stream(
        &self,
        _credentials: InstallationCredentials,
        params: DownloadResourceParams,
    ) -> Result<DownloadedResourceStream, ApiError> {
        Err(Self::refuse(
            "download_message_resource_stream",
            params.message_id,
        ))
    }

    async fn batch_get_users(
        &self,
        _credentials: InstallationCredentials,
        open_ids: Vec<String>,
    ) -> Result<HashMap<String, String>, ApiError> {
        Err(Self::refuse("batch_get_users", open_ids.len()))
    }

    async fn add_message_reaction(&self, params: AddReactionParams) -> Result<String, ApiError> {
        Err(Self::refuse("add_message_reaction", params.message_id))
    }

    async fn delete_message_reaction(&self, params: DeleteReactionParams) -> Result<(), ApiError> {
        Err(Self::refuse("delete_message_reaction", params.message_id))
    }
}

#[cfg(test)]
mod tests;
