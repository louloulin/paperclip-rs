//! lark 的**存储值形状与 config 边界**
//! （上游 `internal/integrations/lark/store.go`，306 行）。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - **上游定位**：`ChannelStore`（`channel_store.go`）与包内其余部分之间的**唯一** JSONB
//!   边界 + 扁平行投影。上游注释逐字：*the rest of the package keeps working with flat
//!   domain structs whose fields mirror the retired `db.Lark*` rows one-for-one, so the call
//!   sites are a mechanical rename rather than a reshape.*
//!
//! # 🔴 两套表并存（R-M7-5）：本文件是唯一把这件事讲清楚的地方
//!
//! 上游 `f41fae6b08fb` 的 `channel_store.go` 自己写着 *"This store reads and writes only
//! `channel_*`, never `lark_*`; queries/lark.sql is deleted. The physical `lark_*` tables are
//! retained one release for rollout/rollback safety (see migration 124's ROLLOUT note) and
//! dropped by a later cleanup migration."* —— 也就是说**上游运行时走泛化 `channel_*`**，
//! `lark_*` 只是（上游那一侧）的退役面。
//!
//! 但本仓的 `crates/mc-repos/src/channel/installation.rs`（M7-1，**W**）把这件事定成了另一条
//! 硬约束：*lark 的安装行必须读/写**遗留** `lark_installation`（上游同时在用两套表，`docs/60`
//! §6.4）；**不得**把 `lark_*` 并进 `channel_*`*，并据此落了 `find_lark_by_app_id` /
//! `get_lark` / `revoke_lark` / `LarkChatSessionBindingRepo` / `LarkInboundDedupRepo` /
//! `LarkInboundAuditRepo`。M7-12 的 [`super::resolvers`] 也建在这批遗留仓储上。
//!
//! ⇒ 本片**服从合并树**（那是编译单元与 e2e 唯一能跑起来的事实），并把两族的**边界**这一件事
//! 落在一个地方：
//!
//! | 行 | 上游（`f41fae6b08fb`） | 本仓合并树 | 本片的桥 |
//! | --- | --- | --- | --- |
//! | 安装行 | `channel_installation`（`channel_type='feishu'`） | **遗留** `lark_installation` | [`super::channel_store`] |
//! | 用户绑定 | `channel_user_binding` | **遗留** `lark_user_binding` | 同上 |
//! | 会话绑定 | `channel_chat_session_binding` | **两族都在**（入站写泛化，遗留面本片补写） | 同上 |
//! | 任务投递 | `channel_task_delivery` | 只有泛化 | 同上 |
//! | 出站卡片 | `channel_outbound_card_message` | 只有泛化 | 同上 |
//! | 丢弃审计 | `channel_inbound_audit` | **遗留** `lark_inbound_audit` | [`super::audit`] |
//!
//! **两族的密文形态不同，且不通用**：泛化行的 `config.app_secret_encrypted` 是 **base64 串**
//! （SQL 回填会带 MIME 换行），遗留行的 `app_secret_encrypted` 是 **`BYTEA` 裸密文**。所以
//! [`decode_secret`] 只服务前者、[`super::resolvers::LarkInstallation`] 直接持后者 —— 一个
//! "把两族合并成一张表"的改动会在这一行静默丢密文。这就是"不得强行合并"的**可测**含义。
//!
//! # 与 `feishu_channel/credentials.rs` 的一处重复（**登记**，见 `docs/32` §32.1 的 D2）
//!
//! 上游只有一份 `decodeSecret`（在本文件）。M7-12 在
//! `feishu_channel/credentials.rs` 里落了一份**私有**的同款（它的写集里没有本文件），本片按
//! 上游文件名把边界那一份落在**这里**。两份共用同一个判据（先剥 ASCII 空白再 base64 解码）与
//! 同一个错误分类（只报长度）⇒ 行为一致；但**这是一处重复**，M7-21 若要把它们并成一份，本
//! 文件是存留点。

use chrono::{DateTime, Utc};
use mc_core::id::Id;
use mc_repos::channel::binding::LarkBindingTokenRow;
use mc_repos::channel::dedup::InboundDedupRow;
use mc_repos::channel::delivery::ChannelTaskDeliveryRow;
use mc_repos::channel::inbound_audit::LarkInboundAuditRow;
use mc_repos::channel::outbound::ChannelOutboundCardMessageRow;
use mc_repos::channel::session::LarkChatSessionBindingRow;
use serde_json::{json, Value as Json};

use super::types::{ChatId, ChatType};
use crate::engine::resolvers::DropReason;

// =====================================================================
// 密文边界（上游 `decodeSecret` + `stripWhitespace`）
// =====================================================================

/// base64 密文列解不开（**只报长度**，绝不回显密文 —— 凭据纪律第 2 条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("lark: app_secret_encrypted is not valid base64 ({length} bytes)")]
pub struct SecretNotBase64 {
    /// 原始（未剥离空白之前的）串长。
    pub length: usize,
}

/// 剥掉 MIME base64 包装引入的 ASCII 空白（上游 `stripWhitespace`，**逐字**）。
///
/// 上游注释逐字：SQL 回填走的是 PostgreSQL 的 `encode(...,'base64')`，它对每 76 个字符
/// 折行，而一个封装后的 ~72 字节密文就超过这个宽度；Go 侧 `encodeInstallConfig` 写的是
/// **不带折行**的 base64。⇒ 读侧剥离空白让两个来源可互换。
///
/// 无空白时**原样返回**（上游的快速路径：`strings.ContainsAny` 不命中就不分配新串）。
#[must_use]
pub fn strip_whitespace(raw: &str) -> String {
    if !raw.contains(['\n', '\r', ' ', '\t']) {
        return raw.to_string();
    }
    raw.chars()
        .filter(|c| !matches!(c, '\n' | '\r' | ' ' | '\t'))
        .collect()
}

/// base64 解码一行泛化层的密文列（上游 `decodeSecret`）。
///
/// 空串 ⇒ 空 `Vec`（上游逐字：*an installation mid-registration before the secret is
/// sealed* ⇒ 不是错误，是"还没封"）。
///
/// # Errors
///
/// 不是合法 base64 ⇒ [`SecretNotBase64`]（**只带长度**）。
pub fn decode_secret(encoded: &str) -> Result<Vec<u8>, SecretNotBase64> {
    use base64::Engine as _;

    if encoded.is_empty() {
        return Ok(Vec::new());
    }
    base64::engine::general_purpose::STANDARD
        .decode(strip_whitespace(encoded).as_bytes())
        .map_err(|_| SecretNotBase64 {
            length: encoded.len(),
        })
}

// =====================================================================
// 绑定行的 `config` 边界（上游 `larkBindingConfig`）
// =====================================================================

/// 会话绑定行 `config` 列的 lark 形状（上游 `larkBindingConfig`）。
///
/// 它只有一个字段，因为 `channel_chat_id` 在**话题隔离**下是复合键（上游形态
/// `chat_id:话题 id`），真实 chat id 必须另存一处才能出站寻址。上游把这两个概念写在一起：
/// 键里有冒号 **且** `config.chat_id != 键` ⇒ 这一行是话题隔离行。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingConfig {
    /// 真实的 Lark 会话 id（`oc_…`）。
    pub chat_id: String,
}

impl BindingConfig {
    /// 从一个**未解码**的 `config` 值读出真实 chat id。
    ///
    /// 非对象 / 缺键 / 键不是字符串 / 空串：一律 `None`（上游 `json.Unmarshal` 失败与
    /// `ChatID == ""` 走的是同一条降级路径）。**永不**上抛 —— 这是纯读路径的解读，
    /// 坏 config 不该让入站瘫痪。
    #[must_use]
    pub fn chat_id_from(raw: &Json) -> Option<String> {
        let value = raw.get("chat_id")?.as_str()?;
        if value.is_empty() {
            return None;
        }
        Some(value.to_string())
    }

    /// 编码成本仓的 `config` 列形态（`{"chat_id": …}`；空 chat id ⇒ `{}`）。
    ///
    /// 上游 `jsonb_strip_nulls` 的同款结果：**空键不写**，于是 upsert 的
    /// `config || excluded.config` 合并不必区分"没写"与"写成空"。
    #[must_use]
    pub fn encode(chat_id: &str) -> Json {
        if chat_id.is_empty() {
            return json!({});
        }
        json!({ "chat_id": chat_id })
    }
}

// =====================================================================
// 会话绑定：出站面要的那几列（上游 `ChatSessionBinding`）
// =====================================================================

/// 出站回复要的会话绑定视图（上游 `ChatSessionBinding`）。
///
/// 上游注释逐字的两条要点，本仓逐条保留：
///
/// - `config` 携带**真实 chat id**（当 `channel_chat_id` 是复合话题键时），否则 `{}`；
/// - `last_sender_id` 是触发那条消息的**平台原生 id**（`open_id`），也是出站回复要 `@` 的
///   账号。它**按任务冻结**在 `channel_task_delivery` 上，而**不是**从 Multica 成员反查 ——
///   一个成员在同一安装下可以持有多个 `open_id`，按成员反查可能 `@` 错人。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSessionBinding {
    pub id: Id,
    /// 对应的 Multica `chat_session`。
    ///
    /// `None` = 这一路读的是**投递行**（`channel_task_delivery` 上没有这一列），所以只有回查
    /// 遗留绑定表才拿得到。上游 `processEvent` 正是这个形态 —— 它从事件里拿 `chat_session_id`，
    /// 不从投递行拿。**不**用一个随机 id 假装有值。
    pub chat_session_id: Option<Id>,
    pub installation_id: Id,
    /// 存库的 `channel_chat_id`（话题隔离下是复合键）。
    pub channel_chat_id: String,
    pub chat_type: ChatType,
    /// 原样的 `config` 列（`{}` = 无附加路由）。
    pub config: Json,
    pub last_message_id: Option<String>,
    pub last_thread_id: Option<String>,
    /// 触发消息的发送者平台 id（见上）。
    pub last_sender_id: Option<String>,
}

impl ChatSessionBinding {
    /// 从一行遗留会话绑定投影（**没有**投递行 ⇒ 回复目标与发件人都缺）。
    #[must_use]
    pub fn from_legacy_row(row: &LarkChatSessionBindingRow) -> Self {
        Self {
            id: row.id(),
            chat_session_id: Some(row.chat_session_id()),
            installation_id: Id(row.installation_id),
            channel_chat_id: row.lark_chat_id.clone(),
            chat_type: ChatType::from_str_opt(&row.lark_chat_type).unwrap_or(ChatType::P2p),
            config: Json::Null,
            last_message_id: row.last_lark_message_id.clone(),
            last_thread_id: row.last_lark_thread_id.clone(),
            last_sender_id: None,
        }
    }

    /// 用一行遗留绑定行补上 `chat_session_id`（投递行没有这一列）。
    ///
    /// 投递行给的是"**这次**触发的消息"，绑定行给的是会话身份 —— 两者互补，所以
    /// [`super::channel_store`] 的 `binding_for_task` 是"先投递行、再绑定行补一格"。
    #[must_use]
    pub fn with_legacy_row(mut self, row: &LarkChatSessionBindingRow) -> Self {
        self.chat_session_id = Some(row.chat_session_id());
        self
    }

    /// 用一条**按任务冻结**的投递行补上回复目标与发件人（上游 `processEvent` 的组装）。
    ///
    /// 上游从 `channel_task_delivery` 一次读出 `(binding_id, installation_id,
    /// channel_chat_id, chat_type, config, channel_message_id, channel_thread_id,
    /// channel_sender_id)`，据此**就地**造一个 `ChatSessionBinding`（它不回查绑定表）——
    /// 因为投递行上的 `last_message_id` 才是"**这次**触发的消息"，而绑定表上的是"最近一次"。
    /// 本函数照落这个形态：投递行**覆盖**绑定表的游标。
    #[must_use]
    pub fn with_delivery(mut self, delivery: &ChannelTaskDeliveryRow) -> Self {
        self.installation_id = Id(delivery.installation_id);
        self.channel_chat_id.clone_from(&delivery.channel_chat_id);
        self.chat_type = ChatType::from_str_opt(&delivery.chat_type).unwrap_or(self.chat_type);
        self.config = delivery.config.clone();
        self.last_message_id
            .clone_from(&delivery.channel_message_id);
        self.last_thread_id.clone_from(&delivery.channel_thread_id);
        self.last_sender_id = channel_sender_id(&delivery.config);
        self
    }

    /// 直接由投递行造（上游 `processEvent` 不读绑定表的那个形态）。
    #[must_use]
    pub fn from_delivery_row(delivery: &ChannelTaskDeliveryRow) -> Self {
        Self {
            id: Id(delivery.binding_id),
            // 投递行没有 `chat_session_id` 这一列 ⇒ 留给调用方回查绑定表补（见 `with_legacy_row`）。
            chat_session_id: None,
            installation_id: Id(delivery.installation_id),
            channel_chat_id: delivery.channel_chat_id.clone(),
            chat_type: ChatType::from_str_opt(&delivery.chat_type).unwrap_or(ChatType::P2p),
            config: delivery.config.clone(),
            last_message_id: delivery.channel_message_id.clone(),
            last_thread_id: delivery.channel_thread_id.clone(),
            last_sender_id: channel_sender_id(&delivery.config),
        }
    }

    /// 真实 Lark 会话 id（上游 `outboundChatID`）：话题隔离行读 `config`，否则就是键本身。
    #[must_use]
    pub fn outbound_chat_id(&self) -> String {
        BindingConfig::chat_id_from(&self.config).unwrap_or_else(|| self.channel_chat_id.clone())
    }

    /// 这一行是不是**一个话题**而不是整个会话（上游 `isTopicIsolated`）。
    ///
    /// 上游注释逐字：只有话题会话会写 `config`（键 `"chat:话题"`、值 `{"chat_id": 真实}`），
    /// 所以"解得出 `chat_id`"**就是**隔离标记。话题之前的行带 `{}`、普通会话没有 config，
    /// 两者都读成"未隔离"。
    #[must_use]
    pub fn is_topic_isolated(&self) -> bool {
        BindingConfig::chat_id_from(&self.config)
            .is_some_and(|chat_id| chat_id != self.channel_chat_id)
    }
}

/// 从投递行的 `config` 读触发消息的发送者平台 id。
///
/// 上游把 `ChannelSenderID` 当**独立列**放在投递行上（`channel_task_delivery.channel_sender_id`）；
/// 本仓的 `channel_task_delivery` 行（`mc-repos/src/channel/delivery.rs`，M7-1 的形态）**没有**
/// 这一列，于是它落在同一个 `config` JSONB 里、键名逐字沿用 `sender_id`。
/// ⇒ 读法与 [`BindingConfig::chat_id_from`] 同款：缺键 / 坏形状一律 `None`（**不**上抛）。
#[must_use]
pub fn channel_sender_id(config: &Json) -> Option<String> {
    let value = config.get("sender_id")?.as_str()?;
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

/// 把发送者平台 id 写进投递行的 `config`（[`channel_sender_id`] 的写侧）。
///
/// 保留已有的 `chat_id`（两件事共用一个 blob，覆盖式写会把另一半抹掉）。
#[must_use]
pub fn with_channel_sender_id(mut config: Json, sender_id: &str) -> Json {
    if sender_id.is_empty() {
        return config;
    }
    if let Some(map) = config.as_object_mut() {
        map.insert("sender_id".to_string(), json!(sender_id));
        return config;
    }
    json!({ "sender_id": sender_id })
}

// =====================================================================
// 出站卡片行（上游 `OutboundCardMessage`）
// =====================================================================

/// 出站卡片状态（上游 `lark_outbound_card_message.status` 的 `CardStatus`）。
///
/// 上游注释逐字：*Kept as a typed alias so callers can't pass arbitrary strings into the
/// status column.* 本仓照落成 `enum`，于是**类型层面**就没有"随便传一个字串"这条路。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CardStatus {
    /// 已发出、还没 patch 过。
    #[default]
    Pending,
    /// 正在原地 patch。
    Streaming,
    /// 收口（终态，不该再 patch）。
    Final,
    /// 失败收口（终态）。
    Error,
}

impl CardStatus {
    /// 列里的字面量。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Streaming => "streaming",
            Self::Final => "final",
            Self::Error => "error",
        }
    }

    /// 解回枚举；未知取值 `None`（**不**回落：库里出现第四种状态是数据问题，不该被静默吞掉）。
    #[must_use]
    pub fn from_str_opt(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "streaming" => Some(Self::Streaming),
            "final" => Some(Self::Final),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// 终态：`final` / `error`（上游 `OutboundCardMessage.isTerminal` 同义）。
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Final | Self::Error)
    }
}

impl std::fmt::Display for CardStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 一行出站卡片（上游 `OutboundCardMessage`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundCardMessage {
    pub id: Id,
    pub chat_session_id: Id,
    pub task_id: Option<Id>,
    pub channel_chat_id: String,
    /// Lark 侧的卡片消息 id（patch 的靶子）。
    pub channel_card_message_id: String,
    /// 原样的状态列字串（未知状态**不**被吞掉，见 [`CardStatus::from_str_opt`]）。
    pub status: String,
    pub last_patched_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl OutboundCardMessage {
    /// 收口判定（库侧口径，未知状态按"未收口"处理 —— 宁可多 patch 一次，也不静默跳过）。
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        CardStatus::from_str_opt(&self.status).is_some_and(CardStatus::is_terminal)
    }
}

impl From<&ChannelOutboundCardMessageRow> for OutboundCardMessage {
    fn from(row: &ChannelOutboundCardMessageRow) -> Self {
        Self {
            id: row.id(),
            chat_session_id: Id(row.chat_session_id),
            task_id: row.task_id.map(Id),
            channel_chat_id: row.channel_chat_id.clone(),
            channel_card_message_id: row.channel_card_message_id.clone(),
            status: row.status.clone(),
            last_patched_at: row.last_patched_at,
            created_at: row.created_at,
        }
    }
}

/// 建一行卡片记录的入参（上游 `CreateOutboundCardMessageParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutboundCard {
    pub chat_session_id: Id,
    pub task_id: Id,
    pub channel_chat_id: String,
    pub channel_card_message_id: String,
    pub status: CardStatus,
}

// =====================================================================
// 用户绑定 / 绑定令牌 / 去重 / 审计的扁平投影
// =====================================================================

/// 一条遗留用户绑定的扁平视图（上游 `UserBinding`）。
///
/// `channel_user_id` 是 lark 的 `open_id`；`union_id`（次级身份）在遗留表上是**独立列**
/// （泛化层把它折进 `config`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserBinding {
    pub id: Id,
    pub workspace_id: Id,
    pub multica_user_id: Id,
    pub installation_id: Id,
    pub channel_user_id: String,
    pub union_id: Option<String>,
    pub bound_at: DateTime<Utc>,
}

impl From<&mc_repos::channel::binding::LarkUserBindingRow> for UserBinding {
    fn from(row: &mc_repos::channel::binding::LarkUserBindingRow) -> Self {
        Self {
            id: Id(row.id),
            workspace_id: Id(row.workspace_id),
            multica_user_id: Id(row.multica_user_id),
            installation_id: Id(row.installation_id),
            channel_user_id: row.lark_open_id.clone(),
            union_id: row.union_id.clone(),
            bound_at: row.bound_at,
        }
    }
}

/// 一行绑定令牌的扁平视图（上游 `BindingTokenRow`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingTokenRow {
    pub token_hash: String,
    pub workspace_id: Id,
    pub installation_id: Id,
    /// 兑换后要绑定的 `open_id`。
    pub channel_user_id: String,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<&LarkBindingTokenRow> for BindingTokenRow {
    fn from(row: &LarkBindingTokenRow) -> Self {
        Self {
            token_hash: row.token_hash.clone(),
            workspace_id: Id(row.workspace_id),
            installation_id: Id(row.installation_id),
            channel_user_id: row.lark_open_id.clone(),
            expires_at: row.expires_at,
            consumed_at: row.consumed_at,
            created_at: row.created_at,
        }
    }
}

/// 一行入站去重的扁平视图（上游 `InboundMessageDedup`）。
///
/// 上游注释逐字：*Every field is a flat column (no JSON), so this mirrors the channel row
/// 1:1.* —— 两族（遗留 / 泛化）的列清单**逐字相同**（`mc-repos` 的 `DEDUP_COLUMNS` 注释）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundMessageDedup {
    pub installation_id: Id,
    pub message_id: String,
    pub received_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    pub claim_token: Id,
}

impl From<&InboundDedupRow> for InboundMessageDedup {
    fn from(row: &InboundDedupRow) -> Self {
        Self {
            installation_id: row.installation_id(),
            message_id: row.message_id.clone(),
            received_at: row.received_at,
            processed_at: row.processed_at,
            claim_token: row.claim_token(),
        }
    }
}

/// 一行丢弃审计的扁平视图（上游 `audit.go` 写进去的那些列的读侧）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundAuditRow {
    pub id: Id,
    pub installation_id: Option<Id>,
    pub channel_chat_id: Option<String>,
    pub event_type: String,
    pub channel_event_id: Option<String>,
    pub channel_message_id: Option<String>,
    pub drop_reason: String,
    pub received_at: DateTime<Utc>,
}

impl From<&LarkInboundAuditRow> for InboundAuditRow {
    fn from(row: &LarkInboundAuditRow) -> Self {
        Self {
            id: row.id(),
            installation_id: row.installation_id.map(Id),
            channel_chat_id: row.lark_chat_id.clone(),
            event_type: row.event_type.clone(),
            channel_event_id: row.lark_event_id.clone(),
            channel_message_id: row.lark_message_id.clone(),
            drop_reason: row.drop_reason.clone(),
            received_at: row.received_at,
        }
    }
}

// =====================================================================
// 纯工具
// =====================================================================

/// 非空串 ⇒ 有值；空串 ⇒ `None`（上游 `textIfNonEmpty`）。
///
/// 上游注释逐字：*Avoids storing literal empty strings in the audit table, which would mask
/// the difference between "the event lacked this field" and "the field was deliberately
/// empty".*
#[must_use]
pub fn text_if_non_empty(raw: &str) -> Option<String> {
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

/// 丢弃原因 → 审计列的字面量（上游 `string(p.Reason)` 在 lark 侧的等价物）。
///
/// 单独一个函数（而不是 `reason.as_str()` 直接用）是为了让"审计列写什么"只有一处：lark 的
/// [`DropReason`]（`super::types`）与 engine 的词表**取值逐字相同**，但它们是两个类型。
#[must_use]
pub fn drop_reason_str(reason: DropReason) -> &'static str {
    reason.as_str()
}

/// 会话类型 → 存库字面量（`lark_chat_session_binding.lark_chat_type` 的 `CHECK` 取值）。
#[must_use]
pub fn chat_type_str(chat_type: ChatType) -> &'static str {
    chat_type.as_str()
}

/// 一条会话绑定对应的 Lark 会话 id 的类型化读口（出站寻址）。
#[must_use]
pub fn outbound_chat_id_of(binding: &ChatSessionBinding) -> ChatId {
    ChatId::new(binding.outbound_chat_id())
}

#[cfg(test)]
mod tests;
