//! `WeCom` 契约类型：`channel_installation` 的 `WeCom` 投影 + `config` blob 的编解码
//! （上游 `internal/integrations/wecom/types.go`，**168 行**）。
//!
//! - **写者**：M7-15（`docs/60-M7-PLAN.md` §3.3 的写集表；本片的写集勘误见 `docs/32` §31 的 D1）。
//! - **上游定位**（注释逐字）：`WeCom` 智能机器人（"智能机器人" / aibot）**不是**回调式客服号，
//!   而是**客户端主动拨出**的 WebSocket 长连接（`wss://openws.work.weixin.qq.com`）。
//!   每个安装带一对 `(bot_id, secret)`；握手后 `aibot_subscribe`，此后在同一 socket 上收
//!   `aibot_msg_callback`、发 `aibot_send_msg` / `aibot_respond_msg` / `aibot_upload_media_*`。
//!   **不需要公网回调 URL**（这也是 §1.5 结论的一条实例：渠道入站不在 456 条路由表里）。
//! - **一个安装 = 一个 bot = 一条 WS**：`WeCom` 每个 bot 只允许一个活连接，第二个连接会把
//!   第一个踢掉（`disconnected_event`）⇒ 多副本不变式与 engine 的 WS 租约天然对齐。
//!
//! # 文件分工（上游 `wecom/*.go` → 本目录）
//!
//! | 上游 | 本地 | 内容 |
//! | --- | --- | --- |
//! | `types.go`（168） | **本文件** | `Installation` / `InstallationStatus` / `installConfig` 编解码 |
//! | `credentials.go`（55）+ `credential_probe.go`（205） | `credentials.rs` | 解封 + 凭据探针（含错误分类） |
//! | `installation.go`（545） | `installation.rs` | `InstallationService`（list / get / revoke / upsert） |
//! | `store.go`（79） | `store.rs` | 端口 trait + 进程内替身（PG 实现在 route 层） |
//! | `binding.go`（265） | `binding.rs` | 绑定令牌的铸 / 兑换 |
//! | `strings.go`（170）+ `language.go`（95） | `strings.rs` | 气泡文案与语言选择 |
//! | `metrics.go`（181） | `metrics.rs` | 健康信号端口 |
//! | `internal/handler/wecom_web.go`（401） | `mc-http/src/routes/channels/wecom.rs` | 4 条路由 |
//!
//! # `config` blob 的形态（**逐字对齐上游 `installConfig`**）
//!
//! ```json
//! { "app_id": "<bot_id>", "bot_id": "<bot_id>",
//!   "secret_encrypted": "<base64(secretbox 单块)>", "bot_display_name": "…" }
//! ```
//!
//! 四条别改的细节：
//!
//! 1. **`app_id == bot_id`**：`bot_id` 既是握手帧里的认证身份，也是**路由键** —— 上游把它
//!    写成 `config->>'app_id'`，为的是共用 `idx_channel_installation_type_appid`
//!    （`(channel_type, config->>'app_id')` 的**唯一**索引，`migrations/upstream/124:100`）
//!    与泛化的 `GetChannelInstallationByAppID` ⇒ 本仓照抄，**不**新造一个路由键；
//! 2. **密文列是 base64 字符串**（不是字节数组）：上游 `SecretEncrypted []byte` 过
//!    `encoding/json` 时被 Go 编成 base64(`StdEncoding`)，`124` 的迁移注释也写明
//!    `app_secret_encrypted (base64)` ⇒ 本仓写侧/读侧都走 base64（`strip_whitespace` 兜住
//!    PostgreSQL `encode(…, 'base64')` 的 MIME 折行）；
//! 3. **`bot_display_name` 可缺**（`omitempty`）：它只用于**群聊里认出自己的 @提及**
//!    （`WeCom` 的提及是**字面文本**，没有结构化 mention 列表）⇒ 空值是诚实默认值，
//!    回落成 `ws_frame` 的空白启发式；
//! 4. **`app_secret_encrypted`（lark 遗留列的写法）不是这里的键**：本文件的键叫
//!    `secret_encrypted`（上游 `types.go` 逐字），别把 lark 的键名搬过来。
//!
//! # 凭据纪律（`docs/60` §2.3 / 本片 `DoD` 第 6 条）
//!
//! - [`Installation`] **手写 `Debug`**：密文列只报 `<redacted, N bytes>`；
//! - [`ConfigError`] 每个变体只带**结构信息**（字段名 / 长度）—— 明文、密文、密钥都不进错误值；
//! - 本文件**没有**任何 `tracing::*` 插值凭据字段。

use std::fmt;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use mc_core::channel::{ChannelKind, InstallationStatus};
use mc_core::id::Id;
use mc_repos::channel::installation::ChannelInstallationRow;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 存储口径的渠道名（`ChannelKind::WeCom.storage_str()`；`channel_type` 列的字面量）。
pub const CHANNEL_TYPE: &str = "wecom";

/// 本 adapter 的平台判别式。
pub const KIND: ChannelKind = ChannelKind::WeCom;

/// 路由键（`config->>'app_id'`；唯一索引 `idx_channel_installation_type_appid` 的表达式）。
pub const FIELD_APP_ID: &str = "app_id";

/// 机器人标识（上游 `installConfig.BotID`；值**等于** [`FIELD_APP_ID`]）。
pub const FIELD_BOT_ID: &str = "bot_id";

/// 长连接密钥的密文列（base64 的 `secretbox` 单块；上游 `installConfig.SecretEncrypted`）。
pub const FIELD_SECRET_ENCRYPTED: &str = "secret_encrypted";

/// 机器人在会话里的显示名（可选；只用于识别自己的 @提及）。
pub const FIELD_BOT_DISPLAY_NAME: &str = "bot_display_name";

// =====================================================================
// 错误（**不带**任何凭据字节）
// =====================================================================

/// `config` blob 的编解码失败。
///
/// 每个变体只带**结构信息**：字段名、长度、原因。密文与明文都不进错误值 ⇒
/// 「错误路径回显凭据」在类型层面就不成立（`docs/60` §2.3 第 3 条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// `config` 列是 `NULL` / 空对象（上游 `installationFromRow` 的零值分支）。
    #[error("wecom: empty installation config")]
    Empty,
    /// `config` 列不是 JSON 对象。
    #[error("wecom: installation config is not a JSON object")]
    NotAnObject,
    /// 配置里缺路由键（上游 `encodeInstallConfig`：`wecom: bot_id is required`）。
    #[error("wecom: bot_id is required")]
    MissingBotId,
    /// 密文列不是合法 base64（**只报字段名与长度**）。
    #[error("wecom: {field} is not valid base64 (len={len})")]
    Ciphertext { field: &'static str, len: usize },
    /// 字段类型不对（只报字段名）。
    #[error("wecom: {field} has an unexpected type")]
    FieldType { field: &'static str },
}

/// 把一条安装行解成 [`Installation`] 时的失败（`config` 坏了 / 缺路由键）。
pub type InstallationDecodeError = ConfigError;

// =====================================================================
// config blob（`channel_installation.config` 的 WeCom 形态）
// =====================================================================

/// `channel_installation.config` 的 `WeCom` 形状（上游 `installConfig`，**四个键**）。
///
/// `app_id` 与 `bot_id` 同时存在是**上游有意的冗余**：前者是索引/查询用的路由键，
/// 后者是读侧的业务字段。写侧由 [`Installation::encode_config`] 保证两者一致。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallConfig {
    /// 路由键（`config->>'app_id'`；值**必须**等于 `bot_id`）。
    #[serde(default)]
    pub app_id: String,
    /// 智能机器人的标识（`WeCom` 管理后台在创建机器人时分配）。
    #[serde(default)]
    pub bot_id: String,
    /// 长连接密钥的 base64(`secretbox` 单块)密文。
    #[serde(default)]
    pub secret_encrypted: String,
    /// 群聊里认出自己 @提及用的显示名（可选）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bot_display_name: String,
}

impl fmt::Debug for InstallConfig {
    /// 手写脱敏（凭据纪律第 1 条）：密文列只报告**有没有**。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallConfig")
            .field("app_id", &self.app_id)
            .field("bot_id", &self.bot_id)
            .field(
                FIELD_SECRET_ENCRYPTED,
                &redaction_of(&self.secret_encrypted),
            )
            .field("bot_display_name", &self.bot_display_name)
            .finish()
    }
}

impl InstallConfig {
    /// 从 `config` 列解出（未知键忽略；`secret_encrypted` 保持 base64 形态）。
    ///
    /// # Errors
    ///
    /// `null` ⇒ [`ConfigError::Empty`]；非对象 ⇒ [`ConfigError::NotAnObject`]；
    /// 键类型不对 ⇒ [`ConfigError::FieldType`]。
    pub fn from_value(raw: &Value) -> Result<Self, ConfigError> {
        if raw.is_null() {
            return Err(ConfigError::Empty);
        }
        if !raw.is_object() {
            return Err(ConfigError::NotAnObject);
        }
        Ok(Self {
            app_id: optional_string(raw, FIELD_APP_ID)?.unwrap_or_default(),
            bot_id: optional_string(raw, FIELD_BOT_ID)?.unwrap_or_default(),
            secret_encrypted: optional_string(raw, FIELD_SECRET_ENCRYPTED)?.unwrap_or_default(),
            bot_display_name: optional_string(raw, FIELD_BOT_DISPLAY_NAME)?.unwrap_or_default(),
        })
    }

    /// 转成要写进 `config` 列的 JSON。
    ///
    /// # Errors
    ///
    /// 序列化失败（不可能：四个 `String`）。
    pub fn to_config_value(&self) -> Result<Value, ConfigError> {
        serde_json::to_value(self).map_err(|_| ConfigError::NotAnObject)
    }

    /// 解出密文字节（base64 → `secretbox` 单块）。
    ///
    /// # Errors
    ///
    /// 非法 base64 ⇒ [`ConfigError::Ciphertext`]（只带长度）。
    pub fn secret_bytes(&self) -> Result<Vec<u8>, ConfigError> {
        decode_ciphertext(&self.secret_encrypted)
    }
}

/// 读一个可选字符串键。缺失（或 JSON `null`）⇒ `Ok(None)`；非字符串 ⇒ `Err`。
fn optional_string(raw: &Value, field: &'static str) -> Result<Option<String>, ConfigError> {
    match raw.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ConfigError::FieldType { field }),
    }
}

// =====================================================================
// 安装行（领域投影）
// =====================================================================

/// `channel_installation` 的一行，按 `WeCom` 语义解码（上游 `Installation`）。
///
/// - `secret_encrypted` 是 `secretbox` 的单块密文（`nonce(12) ‖ ct ‖ tag(16)`），
///   **永不明文**；需要明文的地方只经
///   [`crate::wecom::credentials::CredentialsResolver`]；
/// - [`Installation::config`] 保留原始 blob（写回 / 诊断用），三个时间戳保留给管理列表。
#[derive(Clone, PartialEq, Eq)]
pub struct Installation {
    pub id: Id,
    pub workspace_id: Id,
    pub agent_id: Id,
    pub installer_user_id: Id,
    pub status: InstallationStatus,

    /// 智能机器人标识。既是 `aibot_subscribe` 帧里的认证身份，也是路由键
    /// （落库时写成 `config->>'app_id'`）。
    pub bot_id: String,

    /// 封好的长连接密钥（**不是**回调模式机器人的 token / `EncodingAESKey` —— 那套不用）。
    pub secret_encrypted: Vec<u8>,

    /// 机器人在会话里的显示名（可选；空 ⇒ `ws_frame` 的空白启发式）。
    pub bot_display_name: String,

    /// 原始 `config` blob（写回与诊断用；**含密文**）。
    pub config: Value,

    pub installed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for Installation {
    /// 手写脱敏（凭据纪律第 1 条）：密文只报告长度，`config` 整个不打印。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Installation")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("installer_user_id", &self.installer_user_id)
            .field("status", &self.status)
            .field("bot_id", &self.bot_id)
            .field(
                FIELD_SECRET_ENCRYPTED,
                &format!("<redacted, {} bytes>", self.secret_encrypted.len()),
            )
            .field("bot_display_name", &self.bot_display_name)
            .field("config", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Installation {
    /// `channel_type` 的**存储**取值（写库/比对用这一份，别用 `kind.as_str()`）。
    #[must_use]
    pub fn channel_type(&self) -> &'static str {
        KIND.storage_str()
    }

    /// 是否还能承载长连接（`status = 'active'`）。
    ///
    /// 上游逐字：`revoked` 行被路由的安装解析器跳过（`Active=false` ⇒ 带审计地丢弃事件）。
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status.is_live()
    }

    /// 从 `channel_installation` 行解码（上游 `installationFromRow`）。
    ///
    /// 调用方已经按 `channel_type = 'wecom'` 收窄过查询，所以这里**不**复查渠道类型。
    ///
    /// # Errors
    ///
    /// `config` 不是对象 / 缺 `bot_id` / 密文不是合法 base64。
    pub fn from_row(row: &ChannelInstallationRow) -> Result<Self, InstallationDecodeError> {
        let config = InstallConfig::from_value(&row.config)?;
        // 上游用 `config.BotID`（不是 `app_id`）当业务字段；两者写侧一致，读侧以 `bot_id`
        // 为准，缺它时退到路由键 —— 这样一条老行（只有 `app_id`）仍然认得出机器人。
        let bot_id = if config.bot_id.is_empty() {
            config.app_id.clone()
        } else {
            config.bot_id.clone()
        };
        if bot_id.is_empty() {
            return Err(ConfigError::MissingBotId);
        }
        Ok(Self {
            id: row.id(),
            workspace_id: row.workspace_id(),
            agent_id: Id(row.agent_id),
            installer_user_id: Id(row.installer_user_id),
            status: InstallationStatus::from_str_opt(&row.status)
                .unwrap_or(InstallationStatus::Revoked),
            bot_id,
            secret_encrypted: config.secret_bytes()?,
            bot_display_name: config.bot_display_name,
            config: row.config.clone(),
            installed_at: row.installed_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }

    /// 造一份要写进 `config` 列的 JSON（上游 `encodeInstallConfig`）。
    ///
    /// # Errors
    ///
    /// `bot_id` 为空 ⇒ [`ConfigError::MissingBotId`]（上游逐字：`wecom: bot_id is required`）。
    pub fn encode_config(&self) -> Result<Value, ConfigError> {
        if self.bot_id.is_empty() {
            return Err(ConfigError::MissingBotId);
        }
        InstallConfig {
            // 路由键与业务字段**同时**写：前者供唯一索引与泛化查询，后者供读侧。
            app_id: self.bot_id.clone(),
            bot_id: self.bot_id.clone(),
            secret_encrypted: encode_ciphertext(&self.secret_encrypted),
            bot_display_name: self.bot_display_name.clone(),
        }
        .to_config_value()
    }

    /// 明文凭据的**非密**投影（列表响应里的身份列）。
    #[must_use]
    pub fn public_bot_id(&self) -> &str {
        &self.bot_id
    }
}

// =====================================================================
// 密文编解码（上游 Go 的 `[]byte` ↔ base64 语义）
// =====================================================================

/// `secretbox` 密文 → 落库用的 base64（`StdEncoding`，**带填充**；上游 `encoding/json` 对
/// `[]byte` 的默认编码就是它）。
#[must_use]
pub fn encode_ciphertext(sealed: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(sealed)
}

/// 落库的 base64 → `secretbox` 密文字节。
///
/// 先按 [`strip_whitespace`] 去掉 ASCII 空白 —— PostgreSQL 的 `encode(…, 'base64')`
/// 每 64 字符折一行（`124` 的迁移语句就是这么写 lark 的），两种写法必须解出同一份字节。
///
/// # Errors
///
/// 非法 base64 ⇒ [`ConfigError::Ciphertext`]（**只带长度**，不回显内容）。
pub fn decode_ciphertext(encoded: &str) -> Result<Vec<u8>, ConfigError> {
    let stripped = strip_whitespace(encoded);
    base64::engine::general_purpose::STANDARD
        .decode(&stripped)
        .map_err(|_| ConfigError::Ciphertext {
            field: FIELD_SECRET_ENCRYPTED,
            len: encoded.len(),
        })
}

/// 去掉 ASCII 空白（`' '` / `'\t'` / `'\n'` / `'\r'`）。
#[must_use]
pub fn strip_whitespace(value: &str) -> String {
    value
        .chars()
        .filter(|c| !matches!(c, ' ' | '\t' | '\n' | '\r'))
        .collect()
}

/// 只报告"这个凭据字段**有没有**"，绝不打印它的值。
fn redaction_of(value: &str) -> &'static str {
    if value.is_empty() {
        "<empty>"
    } else {
        "<redacted>"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// 上游 `wecom/installation_test.go` 的 `bot_id` 形状 + 一条固定密文。
    const SEALED: [u8; 30] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
        26, 27, 28, 29, 30,
    ];

    fn installation() -> Installation {
        Installation {
            id: Id::new(),
            workspace_id: Id::new(),
            agent_id: Id::new(),
            installer_user_id: Id::new(),
            status: InstallationStatus::Active,
            bot_id: "bot_5f1c9a".to_string(),
            secret_encrypted: SEALED.to_vec(),
            bot_display_name: "Multica Bot".to_string(),
            config: Value::Null,
            installed_at: Utc::now(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// 上游 `encodeInstallConfig` 的四个键，且 `app_id == bot_id`（路由键与业务字段同源）。
    #[test]
    fn encode_config_writes_the_upstream_four_keys() {
        let encoded = installation().encode_config().expect("encode");
        assert_eq!(encoded["app_id"], Value::String("bot_5f1c9a".into()));
        assert_eq!(encoded["bot_id"], Value::String("bot_5f1c9a".into()));
        assert_eq!(
            encoded["bot_display_name"],
            Value::String("Multica Bot".into())
        );
        // 密文列是 **base64 字符串**（Go 的 `[]byte` 语义），不是数字数组。
        assert_eq!(
            encoded[FIELD_SECRET_ENCRYPTED],
            Value::String(encode_ciphertext(&SEALED))
        );
    }

    /// 往返闭合：`encode` → `from_row` 解回同一份字节与同一份显示名。
    #[test]
    fn config_round_trips_through_a_row() {
        let original = installation();
        let config = original.encode_config().expect("encode");
        let row = row_with(config);
        let decoded = Installation::from_row(&row).expect("decode");
        assert_eq!(decoded.bot_id, original.bot_id);
        assert_eq!(decoded.secret_encrypted, original.secret_encrypted);
        assert_eq!(decoded.bot_display_name, original.bot_display_name);
        assert_eq!(decoded.status, InstallationStatus::Active);
        assert_eq!(decoded.channel_type(), "wecom");
        assert!(decoded.is_active());
        // `app_id` 单独出现（没有 `bot_id`）的老行仍然解得出来。
        let legacy = row_with(serde_json::json!({
            "app_id": "bot_legacy",
            FIELD_SECRET_ENCRYPTED: encode_ciphertext(&SEALED),
        }));
        assert_eq!(
            Installation::from_row(&legacy).expect("legacy").bot_id,
            "bot_legacy"
        );
    }

    /// 缺路由键 / 坏 `config` / 坏 base64 ⇒ 各自的变体，且**都不带载荷**。
    #[test]
    fn decode_failures_are_structural_only() {
        assert_eq!(
            Installation::from_row(&row_with(Value::Null)),
            Err(ConfigError::Empty)
        );
        assert_eq!(
            Installation::from_row(&row_with(serde_json::json!([1, 2]))),
            Err(ConfigError::NotAnObject)
        );
        assert_eq!(
            Installation::from_row(&row_with(serde_json::json!({ "bot_display_name": "x" }))),
            Err(ConfigError::MissingBotId)
        );
        assert_eq!(
            optional_string(&serde_json::json!({ "bot_id": 7 }), FIELD_BOT_ID),
            Err(ConfigError::FieldType {
                field: FIELD_BOT_ID
            })
        );
        let bad = row_with(serde_json::json!({
            "bot_id": "b",
            FIELD_SECRET_ENCRYPTED: "CIPHERTEXT-DO-NOT-LOG !!",
        }));
        let error = Installation::from_row(&bad).unwrap_err();
        assert_eq!(
            error,
            ConfigError::Ciphertext {
                field: FIELD_SECRET_ENCRYPTED,
                len: 24
            }
        );
        let rendered = format!("{error:?}{error}");
        assert!(!rendered.contains("CIPHERTEXT"), "{rendered}");
    }

    /// 空 `bot_id` 不许编码（上游 `encodeInstallConfig` 的硬前置）。
    #[test]
    fn encoding_refuses_a_missing_bot_id() {
        let mut blank = installation();
        blank.bot_id = String::new();
        assert_eq!(blank.encode_config(), Err(ConfigError::MissingBotId));
    }

    /// 空白容忍：PostgreSQL `encode(…, 'base64')` 的 MIME 折行与 `StdEncoding` 解出同一份字节。
    #[test]
    fn whitespace_in_the_ciphertext_is_tolerated() {
        let plain = encode_ciphertext(&SEALED);
        let mime = format!("{}\n{}", &plain[..20], &plain[20..]);
        assert_eq!(decode_ciphertext(&mime).expect("mime"), SEALED.to_vec());
        assert_eq!(decode_ciphertext("").expect("empty"), Vec::<u8>::new());
        assert_eq!(strip_whitespace("a b\tc\nd\re"), "abcde");
    }

    /// 凭据纪律：`Debug` 不回显密文字节、base64 形式，也不回显 `config`。
    #[test]
    fn debug_never_echoes_ciphertext() {
        let inst = installation();
        let rendered = format!("{inst:?}");
        assert!(rendered.contains("<redacted, 30 bytes>"), "{rendered}");
        assert!(
            !rendered.contains(&encode_ciphertext(&SEALED)),
            "{rendered}"
        );
        assert!(!rendered.contains("CIPHERTEXT"), "{rendered}");
        let config =
            InstallConfig::from_value(&inst.encode_config().expect("encode")).expect("cfg");
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains(&encode_ciphertext(&SEALED)),
            "{rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains(&hex::encode(SEALED)), "{rendered}");
    }

    /// 造一行 `channel_installation`（`mc-repos` 的行结构；没有外键 ⇒ 不需要先建 workspace）。
    fn row_with(config: Value) -> ChannelInstallationRow {
        let now = Utc::now();
        ChannelInstallationRow {
            id: uuid::Uuid::new_v4(),
            workspace_id: uuid::Uuid::new_v4(),
            agent_id: uuid::Uuid::new_v4(),
            channel_type: CHANNEL_TYPE.to_string(),
            config,
            status: "active".to_string(),
            ws_lease_token: None,
            ws_lease_expires_at: None,
            installer_user_id: uuid::Uuid::new_v4(),
            installed_at: now,
            created_at: now,
            updated_at: now,
        }
    }
}
