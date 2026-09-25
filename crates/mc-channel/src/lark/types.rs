//! lark **类型层**：标识符 / `region` / 丢弃原因 / 领域值对象 / **响应解码**
//! （上游 `internal/integrations/lark/{types.go,ids.go,chat.go}` + `http_client.go` 的
//! 响应解码那一半）。
//!
//! - **写者**：M7-10（`docs/60-M7-PLAN.md` §3.3；本片是 stage 5 的第一片，只做客户端与类型，
//!   **0 路由**）。
//! - **本文件的边界**（`docs/60` §6.3 逼出来的四文件切分）：本文件持有
//!   **响应解码** —— wire DTO（`LarkRestMessageItem` 一族）与其归一化、平台信封的三种形状、
//!   以及资源下载的响应体。**请求构建**在 [`super::params`]、**错误分类**在
//!   [`super::client`]、**传输核心**在 [`super::http_client`]。
//!
//! # 上游 `types.go` 的两处**不重复定义**（本地已有领域类型）
//!
//! | 上游 | 本仓 | 为什么不在本文件重建 |
//! | --- | --- | --- |
//! | `ChatType`（`p2p` / `group`） | [`ChatType`]（重导出 `mc_core::channel::message`） | 它是**跨渠道**领域类型（`channel_chat_session_binding.chat_type` 的 `CHECK` 与 lark 的 `lark_chat_session_binding.lark_chat_type` 取值逐字相同）⇒ 两份定义必然漂移 |
//! | `InstallationStatus`（`active` / `revoked`） | [`InstallationStatus`]（重导出 `mc_core::channel::installation`） | 同上（`lark_installation.status` 与 `channel_installation.status` 是同一组取值） |
//!
//! 两个重导出让后续片有一个入口（`use crate::lark::types::{ChatType, InstallationStatus}`），
//! 取值与上游**逐字**相同（有用例钉住）。
//!
//! # `region` 与 `union_id` 的形态（本片专属验收的第 3 条）
//!
//! - **`region`**：上游把它从"部署级 env"改成"**每安装**一列"（`lark_installation.region`，
//!   迁移 `116`），因为一个 Multica 部署同时服务飞书（大陆）与 Lark（国际）两个云。
//!   [`Region::open_platform_base_url`] 是**唯一**的 region → host 映射；
//!   [`Region::or_default`] 对应上游 `RegionOrDefault`：空值 / 未知值一律回落**飞书**，
//!   于是坏数据永远不会解析出空 host（也不会写出违反 `CHECK` 的值）。
//! - **`union_id`**：`/open-apis/bot/v3/info` **不返回**它 ⇒ 上游再打一次通讯录端点补查，
//!   且补查失败是**软失败**（安装仍可用，p2p 场景不受影响）。因此传输层的形态是
//!   [`BotInfo::union_id`] 的**空串 = 未解析**；而落库形态（`params.go` 的
//!   `BotUnionID pgtype.Text`）是**可空** ⇒ 见 [`super::params::SetInstallationBotUnionId`]。
//!   两种形态**故意不同**：传输层"空串"带一行警告，存储层"`NULL`"带一次补偿查询。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 本文件**没有任何**凭据字段（凭据类型 [`super::params::AppSecret`] /
//! [`super::params::InstallationCredentials`] 在 `params.rs`），也**没有**任何 `tracing::*`。

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use mc_core::channel::installation::InstallationStatus;
pub use mc_core::channel::message::ChatType;

use super::client::ApiError;

// =====================================================================
// 常量（上游 `types.go` / `http_client.go` 的 host 定义）
// =====================================================================

/// 飞书（大陆）开放平台主机（上游 `types.go` 的 `defaultLarkBaseURL`，定义在
/// `http_client.go`）。它同时是 WS 长连接引导（`POST /callback/ws/endpoint`）的主机。
pub const DEFAULT_LARK_BASE_URL: &str = "https://open.feishu.cn";

/// Lark（国际）开放平台主机（上游 `larkInternationalOpenBaseURL`）。
pub const LARK_INTERNATIONAL_OPEN_BASE_URL: &str = "https://open.larksuite.com";

/// 成员绑定令牌的寿命上限（上游 `BindingTokenTTL`）。
///
/// 存储层的 `CHECK`（`channel_binding_token.expires_at <= created_at + INTERVAL '15 minutes'`）
/// 把同一个界限钉在 DB 上 ⇒ 配置错的调用方或手写的 SQL 都超不过它。**两个值要同步改**。
///
/// （`from_mins` 而不是 `15 * 60` 秒：clippy 的 `duration_suboptimal_units` 要求更大单位 ——
/// 本 crate 不声明 MSRV，工具链是仓根的 `rust-toolchain.toml` 的 `stable`。）
pub const BINDING_TOKEN_TTL: Duration = Duration::from_mins(15);

// =====================================================================
// region（上游 `types.go` 的 `Region` + `OpenPlatformBaseURL` + `RegionOrDefault`）
// =====================================================================

/// 安装行所在的开放平台云。
///
/// 飞书（大陆，`open.feishu.cn` / `accounts.feishu.cn`）与 Lark（国际，
/// `open.larksuite.com` / `accounts.larksuite.com`）是**两个云、两套主机**；一个 Multica
/// 部署靠本值**每安装**解析主机，而不是靠一把部署级 env。与
/// `lark_installation.region` 的 `CHECK`（迁移 `116`）逐字对齐 —— 两个口径**必须同步**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    /// 飞书（大陆）。**默认值**：所有 region 之前落地的历史行都是这一档。
    #[default]
    Feishu,
    /// Lark（国际）。
    Lark,
}

impl Region {
    /// 列里的字面量（`CHECK` 的两个取值）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Feishu => "feishu",
            Self::Lark => "lark",
        }
    }

    /// 该云的开放平台主机 —— **REST API 与 WS 引导共用同一个 host**。
    ///
    /// 未设置或未知 region **回落飞书**（上游 `OpenPlatformBaseURL` 的判据逐字：
    /// 只认 `lark`，其余一律飞书）⇒ 一个坏的 region 字串永远不会解析出空 host。
    #[must_use]
    pub fn open_platform_base_url(self) -> &'static str {
        match self {
            Self::Lark => LARK_INTERNATIONAL_OPEN_BASE_URL,
            Self::Feishu => DEFAULT_LARK_BASE_URL,
        }
    }

    /// 把**存库的** region 字串（`lark_installation.region` 列）归一化成本枚举，
    /// 空值 / 未知值回落飞书（上游 `RegionOrDefault`）。
    ///
    /// 公开导出（上游注释逐字：路由层的 WS 凭据提供者从**原始行**补水时会用到）。
    #[must_use]
    pub fn or_default(raw: &str) -> Self {
        match raw {
            "lark" => Self::Lark,
            _ => Self::Feishu,
        }
    }

    /// 解回枚举；未知取值返回 `None`。
    ///
    /// ⚠️ 与 [`Region::or_default`] **不是一个函数**：本函数保留"这一行是坏的"这一事实
    /// （给校验/迁移用），`or_default` 是**运行路径**上的回落。别互相替代。
    #[must_use]
    pub fn from_str_opt(raw: &str) -> Option<Self> {
        match raw {
            "feishu" => Some(Self::Feishu),
            "lark" => Some(Self::Lark),
            _ => None,
        }
    }
}

// =====================================================================
// 标识符（上游 `types.go` 的三个 string alias）
// =====================================================================

/// 一个 Lark 用户的**按安装**标识（上游 `OpenID`）。
///
/// 同一个真人在**不同安装**里 `open_id` 不同；跨安装的身份归并要 `union_id`（见
/// [`BotInfo`]）。用强类型而不是裸 `String`，是为了让调用方**不可能**把 Multica 的
/// 用户 UUID 当成 Lark 的 `open_id` 递进来。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpenId(String);

impl OpenId {
    /// 包一个明文标识。
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取字串（wire / SQL 的绑定处）。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 是否为空 —— 空 `open_id` 是"没解析出来"，调用方必须把它当错误而不是当用户。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for OpenId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 一个 Lark 会话（p2p 或群）的标识（上游 `ChatID`）。
///
/// 一个 `ChatID` 通过 `lark_chat_session_binding` 映射到一个 Multica `chat_session`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChatId(String);

impl ChatId {
    /// 包一个明文标识。
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取字串（wire / SQL 的绑定处）。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for ChatId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

// =====================================================================
// 丢弃原因（上游 `types.go` 的 `DropReason`；落点是 `lark_inbound_audit`）
// =====================================================================

/// 入站流水线写进 `lark_inbound_audit.drop_reason` 的类别（上游 `DropReason`）。
///
/// 列是开放的 `TEXT`（加新原因不需要迁移），但调用方**应当**复用这些常量，否则看板与查询
/// 会各自漂移。所有 `drop_reason` 的行**都不带消息体**（上游 MUL-2671 §4.7 的丢弃审计口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// 发送者的 `open_id` 在这条安装下**没有** `lark_user_binding` 行。
    /// Bot 回绑定卡；消息本身**不落库**。
    UnboundUser,
    /// 发送者解析出了 Multica 用户，但该用户**不是**这条安装 workspace 的成员。
    /// Bot 回"不在本 workspace"的提示；消息本身**不落库**。
    NonWorkspaceMember,
    /// 消息来自群聊，但既没有 `@` Bot、也不是对 Bot 卡片的回复。
    /// 群聊只摄取**明确指向 Bot**的消息。
    NotAddressedInGroup,
    /// `message_id` 已经在 `lark_inbound_message_dedup` 里。
    /// WS 重连会重放事件；这是幂等路径。
    Duplicate,
    /// `installation.status = 'revoked'`。WS 那时**应当**已经断了；
    /// 本档兜住拆除期间落地的在飞事件。
    RevokedInstallation,
    /// 载荷没过 schema 校验（缺必需字段、`event_type` 与这条 hook 不符…）。
    InvalidEvent,
}

impl DropReason {
    /// 列里的字面量（`drop_reason` 的取值）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnboundUser => "unbound_user",
            Self::NonWorkspaceMember => "non_workspace_member",
            Self::NotAddressedInGroup => "not_addressed_in_group",
            Self::Duplicate => "duplicate",
            Self::RevokedInstallation => "revoked_installation",
            Self::InvalidEvent => "invalid_event",
        }
    }
}

// =====================================================================
// 领域值对象（上游 `client.go` 里被本片带过来的那几个）
// =====================================================================

/// Bot 的身份：按安装的 `open_id` 与（可选的）跨应用稳定 `union_id`（上游 `BotInfo`）。
///
/// 两个标识都落在 `lark_installation` 上：
///
/// - `open_id` 是"这个应用里"的 Lark 标识 —— `/bot/v3/info` 返回它，**出站**发送路径用它
///   寻址用户；
/// - `union_id` 是**租户内跨应用**稳定的标识 —— 多 Bot 群里两个 WS 视角给出的 `open_id`
///   结构上是**相反**的，只有 `union_id` 一致（见上游 MUL-2671 的群 `@` 分类）。解码器拿
///   入站 `mentions[].id` 去比 `union_id`，才能在多 Bot 群里让**对的那个** supervisor 处理事件。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BotInfo {
    /// Bot 的按安装 `open_id`。
    pub open_id: OpenId,
    /// Bot 的稳定 `union_id`；**空串 = 未解析**（通讯录端点被拒 / 上游没给这个字段）。
    ///
    /// 空串**不是**错误：安装仍可用于 p2p，解码器回落到（结构上有缺陷的）`open_id` 匹配路径，
    /// 直到运维把范围配好。上游为此专门打一行警告。
    pub union_id: String,
}

impl BotInfo {
    /// `union_id` 是否已解析（空串 = 未解析，见字段文档）。
    #[must_use]
    pub fn has_union_id(&self) -> bool {
        !self.union_id.is_empty()
    }
}

/// 归一化后的 IM 消息项：富上下文装配器真正需要的那个切片（上游 `LarkMessage`）。
///
/// `content` 是**原样透传**的（Lark 双重编码的、按 `msg_type` 而定的 JSON 字符串）——
/// 解释它的责任在 flattener（M7-12 的 `content_flatten.rs`），**不在**传输客户端。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LarkMessage {
    /// Lark 的 `message_id`。
    pub message_id: String,
    /// Lark 的 `msg_type`：`text` / `post` / `image` / `merge_forward` / …
    pub message_type: String,
    /// 原样的 `body.content`（一个 JSON **编码过的字符串**）。
    pub content: String,
    /// `sender.id`（用户是 `open_id`，应用是 `app_id`）。
    pub sender_id: String,
    /// `sender.sender_type`：`user` / `app` / `anonymous` / …
    pub sender_type: String,
    /// 纪元**毫秒**、Lark 原样返回的**字串**。
    pub create_time: String,
    /// `parent_id`（对某条消息的回复）。
    pub parent_id: String,
    /// `root_id`（话题根）。
    pub root_id: String,
    /// Lark 话题（`thread_id`）；话题外的消息为空串。
    pub thread_id: String,
    /// 合并转发里子消息挂靠的 `upper_message_id`。
    pub upper_message_id: String,
    /// 该消息是否已撤回/删除。
    pub deleted: bool,
    /// `mentions[]`（REST 形状，见 [`LarkMessageMention`]）。
    pub mentions: Vec<LarkMessageMention>,
}

/// `mentions[]` 的一项（上游 `LarkMessageMention`）。
///
/// ⚠️ **与 WS 收到事件的提及形状不同**：这里 `id` 是裸 `open_id` 字串，不是
/// `{open_id, union_id, user_id}` 那个嵌套对象（后者是 M7-11 的帧解码面）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LarkMessageMention {
    /// 正文里的占位键，例如 `@_user_1`。
    pub key: String,
    /// 被提及者的 `open_id`。
    pub id: String,
    /// 显示名（**可能为空**）。
    pub name: String,
}

// =====================================================================
// 响应解码（上游 `http_client.go` 的 `larkRESTMessageItem` + 各端点信封）
// =====================================================================

/// IM v1 的**消息项** wire 形状（上游 `larkRESTMessageItem`，私有）。
///
/// 与 WS 收到的事件有两处不同：字段叫 `msg_type`（不是 `message_type`），且
/// `sender.id` / `mentions[].id` 是**扁平字串**（不是嵌套 id 对象）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct LarkRestMessageItem {
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub root_id: String,
    #[serde(default)]
    pub parent_id: String,
    #[serde(default)]
    pub thread_id: String,
    #[serde(default)]
    pub upper_message_id: String,
    #[serde(default)]
    pub msg_type: String,
    #[serde(default)]
    pub create_time: String,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub sender: RestSender,
    #[serde(default)]
    pub body: RestBody,
    #[serde(default)]
    pub mentions: Vec<RestMention>,
}

/// `sender` 子对象。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RestSender {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub sender_type: String,
}

/// `body` 子对象。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RestBody {
    #[serde(default)]
    pub content: String,
}

/// `mentions[]` 的一项。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RestMention {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

impl LarkRestMessageItem {
    /// 归一化成 [`LarkMessage`]。
    pub(crate) fn normalize(self) -> LarkMessage {
        LarkMessage {
            message_id: self.message_id,
            message_type: self.msg_type,
            content: self.body.content,
            sender_id: self.sender.id,
            sender_type: self.sender.sender_type,
            create_time: self.create_time,
            parent_id: self.parent_id,
            root_id: self.root_id,
            thread_id: self.thread_id,
            upper_message_id: self.upper_message_id,
            deleted: self.deleted,
            mentions: self
                .mentions
                .into_iter()
                .map(|mention| LarkMessageMention {
                    key: mention.key,
                    id: mention.id,
                    name: mention.name,
                })
                .collect(),
        }
    }
}

/// `{"data": {"items": [...]}}` —— `get` / `list` 两条读消息端点共用的信封。
///
/// 信封里的 `code` **不在这里声明**：它在 [`super::http_client`] 里被**统一**检查（2xx 里的非零
/// 业务码也要触发作废 + 重放一次，与上游把这段放在每个方法里相比，更不容易漏）。因此本文件
/// 的信封只描述 `data` 的形状。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct MessageItemsEnvelope {
    #[serde(default)]
    pub data: Option<MessageItemsData>,
}

/// `data.items[]`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct MessageItemsData {
    #[serde(default)]
    pub items: Vec<LarkRestMessageItem>,
}

impl MessageItemsEnvelope {
    /// 归一化成 [`LarkMessage`] 列表；**缺 `data` 与空数组都合法**（上游只判 `code`）。
    pub(crate) fn messages(self) -> Vec<LarkMessage> {
        self.data
            .map(|data| {
                data.items
                    .into_iter()
                    .map(LarkRestMessageItem::normalize)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// `{"data": {"message_id": "om_…"}}` —— 三个发送端点共用的信封。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct MessageIdEnvelope {
    #[serde(default)]
    pub data: Option<MessageIdData>,
}

/// `data.message_id`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct MessageIdData {
    #[serde(default)]
    pub message_id: String,
}

impl MessageIdEnvelope {
    /// 取 `message_id`；**空 `message_id` 算形状错**（上游把 `code != 0 || MessageID == ""`
    /// 并成一件事——替身回一个空 id 会让上层记下指向虚空的卡片行）。
    pub(crate) fn message_id(self, op: &'static str) -> Result<String, ApiError> {
        match self.data.map(|data| data.message_id) {
            Some(message_id) if !message_id.is_empty() => Ok(message_id),
            _ => Err(ApiError::Malformed { op }),
        }
    }
}

/// `{"data": {"reaction_id": "…"}}` —— 加表态端点用的信封。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ReactionIdEnvelope {
    #[serde(default)]
    pub data: Option<ReactionIdData>,
}

/// `data.reaction_id`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ReactionIdData {
    #[serde(default)]
    pub reaction_id: String,
}

impl ReactionIdEnvelope {
    /// 取 `reaction_id`；空算形状错（理由同 [`MessageIdEnvelope::message_id`]）。
    pub(crate) fn reaction_id(self, op: &'static str) -> Result<String, ApiError> {
        match self.data.map(|data| data.reaction_id) {
            Some(reaction_id) if !reaction_id.is_empty() => Ok(reaction_id),
            _ => Err(ApiError::Malformed { op }),
        }
    }
}

/// `{"tenant_access_token": "…", "expire": 7200}`（铸令牌端点，**顶层**字段，无 `data`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TenantTokenResponse {
    #[serde(default)]
    pub tenant_access_token: String,
    /// 平台给的寿命（秒）——上游 `expire`；缺字段/零值由传输层兜底。
    #[serde(default)]
    pub expire: i64,
}

/// `{"bot": {"open_id": "…"}}` —— `/bot/v3/info`。
///
/// ⚠️ **`bot` 在顶层**、不在 `data` 下（上游 `botResp` 结构逐字）。其余字段（显示名 / 头像 /
/// `ip_white_list`…）**故意不声明**（上游口径：下游需要时按 `bot_open_id` 现取，冻结进表只会制造漂移面）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct BotInfoEnvelope {
    #[serde(default)]
    pub bot: Option<RestBot>,
}

/// `/bot/v3/info` 的 `bot` 子对象。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RestBot {
    #[serde(default)]
    pub open_id: String,
}

impl BotInfoEnvelope {
    /// 取 Bot 的 `open_id`；缺 `bot` 或空 `open_id` 都是形状错（上游 `response missing open_id`）。
    pub(crate) fn open_id(self, op: &'static str) -> Result<OpenId, ApiError> {
        match self.bot.map(|bot| bot.open_id) {
            Some(open_id) if !open_id.is_empty() => Ok(OpenId::new(open_id)),
            _ => Err(ApiError::Malformed { op }),
        }
    }
}

/// `{"data": {"user": {"union_id": …}}}` —— 通讯录单用户查询。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UnionIdEnvelope {
    #[serde(default)]
    pub data: Option<UnionIdData>,
}

/// `data.user`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UnionIdData {
    #[serde(default)]
    pub user: Option<RestUser>,
}

/// 通讯录用户的 `union_id` 切片（其余字段不声明，理由同 [`RestBot`]）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RestUser {
    #[serde(default)]
    pub union_id: String,
}

impl UnionIdEnvelope {
    /// 取 `union_id`。
    ///
    /// **空串 + `Ok` 是合法结果**：应用的通讯录范围受限时 Lark 会回 `code = 0` 而**不带**
    /// `union_id` ⇒ 调用方记一行警告并继续（与上游软失败口径逐字）。
    #[must_use]
    pub(crate) fn union_id(self) -> String {
        self.data
            .and_then(|data| data.user)
            .map(|user| user.union_id)
            .unwrap_or_default()
    }
}

/// `{"data": {"items": [{"open_id":…, "name":…}]}}` —— `contact/v3/users/batch`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UserBatchEnvelope {
    #[serde(default)]
    pub data: Option<UserBatchData>,
}

/// `data.items[]`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UserBatchData {
    #[serde(default)]
    pub items: Vec<RestNamedUser>,
}

/// 批量查用户的一项（**只认 `open_id` + `name` 两个字段**：上游口径是"API 没返回的 id 就
/// 不在映射里"，所以缺名/缺 id 的项被直接丢掉）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RestNamedUser {
    #[serde(default)]
    pub open_id: String,
    #[serde(default)]
    pub name: String,
}

impl UserBatchEnvelope {
    /// 归一化成 `open_id → name`（缺名/缺 id 的项丢掉，与上游同款）。
    pub(crate) fn names(self) -> std::collections::HashMap<String, String> {
        self.data
            .map(|data| data.items)
            .unwrap_or_default()
            .into_iter()
            .filter(|user| !user.open_id.is_empty() && !user.name.is_empty())
            .map(|user| (user.open_id, user.name))
            .collect()
    }
}

#[cfg(test)]
mod tests;
