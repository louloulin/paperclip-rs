//! `DingTalk` 安装配置的**写侧与公开投影**（上游 `internal/integrations/dingtalk/config.go`，150 行）。
//!
//! - **写者**：M7-9（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §23 的 D1）。
//! - **BYO 模型**（上游注释逐字）：agent owner 或 workspace 管理员自己建一个
//!   `DingTalk` **Stream 模式机器人**，把 `AppKey`（client id）+ `AppSecret`（client secret）
//!   贴进 Multica。**每个安装带自己的 `AppSecret`、自己的一条 Stream 连接** ⇒ 同一个
//!   `DingTalk` 组织里可以有几个 agent、各有一个 bot 身份。
//!
//! # 本文件与另外两个文件的分工（**别各写一套**）
//!
//! | 面向 | 落点 | 理由 |
//! | --- | --- | --- |
//! | **读**（config → 明文凭据） | `outbound::credentials::decode_credentials`（M7-8 已合） | M7-7 的 `resolve_app_secret` 是唯一的凭据判据（密文优先 / 明文 / 都缺），M7-8 薄包了一层；本文件**再导出**它，于是 route 层的唯一入口叫 `config::decode_credentials`，而实现**只有一份**（`docs/32` §22 的 D4 交接项） |
//! | **写**（明文 → config 的密文列） | 本文件的 [`InstallConfig`] + [`encode_ciphertext`] | 上游 `config.go` 的 `installConfig` 形态（`app_id` / `robot_code` / `app_secret_encrypted`）只在 BYO 安装那一条路上用到 |
//! | **公开投影**（config → 非密身份列） | 本文件的 [`decode_public_config`] | 安装列表要渲染 `app_id` / `robot_code`，而 `AppKey` **本身不是秘密**（上游逐字：`The AppKey itself is not a secret`） |
//!
//! ⚠️ **`Decrypter` 的类型定义留在 `mod.rs`**（M7-7 落的 `pub struct Decrypter`）：把它搬进
//! 本文件是一次纯移动，而 `mod.rs` 只剩 **7 行**门 ⑩ 余量（`docs/60` §6.5 第 4 条 + 派发
//! 补充的硬预算）⇒ 本片**不动**它，把"类型在 `mod.rs`、判据在 `outbound`、写侧在本文件"这条
//! 三分法登记为 `docs/32` §23 的 **D2**（收敛票归 M7-21）。
//!
//! # 凭据纪律（`docs/60` §2.3 的四条判据）
//!
//! 1. [`InstallConfig`] **手写 `Debug`**：`app_secret_encrypted` 只打印 `<empty>` / `<redacted>`；
//! 2. [`ConfigError`] 的**每个变体只带结构信息**（字段名 / 长度），**绝不**带明文、密文或密钥
//!    字节 —— 这是「错误路径不回显凭据」的结构性保证（比"注意别打日志"可靠）；
//! 3. 本文件**没有任何** `tracing::*` 插值凭据字段；
//! 4. 密文形态逐字对齐上游：`secretbox` 的单块字节（`nonce(12) ‖ ct ‖ tag(16)`）
//!    **base64(`StdEncoding`)** 之后再进 JSON（上游 `byo_install.go` 的
//!    `base64.StdEncoding.EncodeToString(sealedSecret)`），**不是** `mc_secrets::cipher`
//!    的 JSON payload 形态。

use std::fmt;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 存储口径的渠道名（`ChannelKind::DingTalk.storage_str()`；`config->>'app_id'` 索引的同表列）。
pub const CHANNEL_TYPE: &str = "dingtalk";

/// 承载 `AppSecret` 密文的那个 JSON 键（上游 `installConfig.app_secret_encrypted`，**逐字**）。
pub const FIELD_APP_SECRET_ENCRYPTED: &str = "app_secret_encrypted";

/// 路由键（`config->>'app_id'`，上游唯一索引 `idx_channel_installation_type_appid` 的表达式）。
pub const FIELD_APP_ID: &str = "app_id";

/// 显式 robot code 的键（上游 `robot_code`）。
pub const FIELD_ROBOT_CODE: &str = "robot_code";

// =====================================================================
// 错误（**不带**任何凭据字节）
// =====================================================================

/// 安装配置的编码 / 解码失败。
///
/// 每个变体只带**结构信息**：字段名、长度、来源。密文与明文都不进错误值 ——
/// 于是「错误路径回显凭据」在类型层面就不成立。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// 配置列是 `NULL` / 空对象（上游 `dingtalk: empty installation config`）。
    #[error("dingtalk: empty installation config")]
    Empty,
    /// 配置列不是 JSON 对象。
    #[error("dingtalk: installation config is not a JSON object")]
    NotAnObject,
    /// 配置里没有路由键（`app_id`）。
    #[error("dingtalk: installation has no app_id")]
    MissingAppId,
    /// 凭据解不出来（解密器拒绝 / 密文形态不对）。**文案只带原因，不带密文**。
    #[error("dingtalk: installation credentials could not be decoded: {reason}")]
    Credentials { reason: String },
    /// 密文不是合法 base64（只报长度）。
    #[error("dingtalk: {field} is not valid base64 (len={len})")]
    Ciphertext { field: &'static str, len: usize },
}

// =====================================================================
// 配置 blob（`channel_installation.config` 的 DingTalk 形态）
// =====================================================================

/// `channel_installation.config` 的 `DingTalk` 形状（上游 `installConfig`，**逐字三键**）。
///
/// - `app_id` 是 `AppKey`。对 Stream 模式机器人它同时是入站事件的 `robotCode`，也是
///   per-installation 的**路由键**（`(channel_type, app_id)` 唯一索引）；
/// - `robot_code` **单独存一份**（上游注释逐字：`kept explicit … so the outbound path never has
///   to assume the equivalence`）⇒ 出站取机器人码时**不要**用 `app_id` 去顶替；
/// - `app_secret_encrypted` 是 `secretbox` 密文的 base64，**绝不明文**。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallConfig {
    /// `AppKey`（= 路由键；明文，因为它**不是**秘密）。
    #[serde(default)]
    pub app_id: String,
    /// 显式 robot code（上游 `robot_code,omitempty`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub robot_code: String,
    /// `AppSecret` 的 base64(`secretbox`) 密文。
    #[serde(default)]
    pub app_secret_encrypted: String,
}

impl fmt::Debug for InstallConfig {
    /// 手写脱敏（凭据纪律第 1 条）：密文列只报告**有没有**。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallConfig")
            .field("app_id", &self.app_id)
            .field("robot_code", &self.robot_code)
            .field(
                FIELD_APP_SECRET_ENCRYPTED,
                &redaction_of(&self.app_secret_encrypted),
            )
            .finish()
    }
}

impl InstallConfig {
    /// 造一份 BYO 安装配置（`app_id` 与 `robot_code` 都取 `AppKey`——上游 `RegisterBYO` 逐字：
    /// Stream 模式机器人的 `robotCode` **等于** `AppKey`）。
    #[must_use]
    pub fn byo(app_key: impl Into<String>, sealed_app_secret: &[u8]) -> Self {
        let app_key = app_key.into();
        Self {
            robot_code: app_key.clone(),
            app_id: app_key,
            app_secret_encrypted: encode_ciphertext(sealed_app_secret),
        }
    }

    /// 上游 `robotCodeOrAppID`：显式值优先，退到 `app_id`。
    #[must_use]
    pub fn robot_code_or_app_id(&self) -> &str {
        if self.robot_code.is_empty() {
            &self.app_id
        } else {
            &self.robot_code
        }
    }

    /// 转成 `config` 列要写的 JSON。
    ///
    /// # Errors
    ///
    /// 只在序列化失败（不可能：三个 `String`）时报 [`ConfigError::NotAnObject`]。
    pub fn to_config_value(&self) -> Result<Value, ConfigError> {
        serde_json::to_value(self).map_err(|_| ConfigError::NotAnObject)
    }
}

// =====================================================================
// 公开投影（非密身份列）
// =====================================================================

/// 安装配置里**可以对外**的那部分（上游响应里的两个身份列）。
///
/// 上游逐字：`The AppKey itself is not a secret and lives in app_id in the clear, exactly like
/// Feishu stores app_id in the clear next to app_secret_encrypted` ⇒ 这两个键**不是**凭据面。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicConfig {
    /// `AppKey`（机器人码）。
    pub app_id: String,
    /// 机器人码（`robot_code` 显式值，退到 `app_id`）。
    pub robot_code: String,
}

/// 从 `config` 列解出公开身份列（**不**碰密文列，**不**需要解密器）。
///
/// 与 `slack` / `telegram` 的 `decode_public_config` 同形（各平台各持一份，收敛见模块文档）。
#[must_use]
pub fn decode_public_config(raw: &Value) -> PublicConfig {
    let app_id = raw
        .get(FIELD_APP_ID)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let robot_code = raw
        .get(FIELD_ROBOT_CODE)
        .and_then(Value::as_str)
        .unwrap_or_default();
    let robot_code = if robot_code.is_empty() {
        app_id.clone()
    } else {
        robot_code.to_string()
    };
    PublicConfig { app_id, robot_code }
}

// =====================================================================
// 凭据解码（**唯一入口**，实现只有一份）
// =====================================================================

pub use crate::dingtalk::outbound::credentials::{decode_credentials, Credentials};

/// 上游 `decodeCredentials` 的**本层门面**：把 `config` 列解成明文凭据。
///
/// 实现是 `outbound::credentials::decode_credentials`（M7-8 已合；它复用 M7-7 的
/// `resolve_app_secret` 三条分支）。本层只把错误**收敛**成 [`ConfigError`]，于是 route 层与
/// 群身份面拿到的错误文案里没有密文、没有明文、没有密钥字节。
///
/// # Errors
///
/// 配置为空 / 不是对象 / 缺 `app_id` / 凭据解不出来。
pub fn credentials_from_config(
    raw: &Value,
    decrypt: &crate::dingtalk::Decrypter,
) -> Result<Credentials, ConfigError> {
    if raw.is_null() {
        return Err(ConfigError::Empty);
    }
    if !raw.is_object() {
        return Err(ConfigError::NotAnObject);
    }
    if decode_public_config(raw).app_id.is_empty() {
        return Err(ConfigError::MissingAppId);
    }
    decode_credentials(raw, decrypt).map_err(|error| ConfigError::Credentials {
        reason: error.code().to_string(),
    })
}

// =====================================================================
// 密文编解码（上游 `base64.StdEncoding` + `stripWhitespace`）
// =====================================================================

/// `secretbox` 密文 → 落库用的 base64（上游 `base64.StdEncoding.EncodeToString`，**逐字带填充**）。
#[must_use]
pub fn encode_ciphertext(sealed: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(sealed)
}

/// 落库的 base64 → `secretbox` 密文字节。
///
/// 先按上游 [`strip_whitespace`] 去掉 ASCII 空白 —— PostgreSQL 的 `encode(…, 'base64')`
/// 每 64 字符折一行（MIME 形态），两种写法必须解出同一份字节。
///
/// # Errors
///
/// 非法 base64 ⇒ [`ConfigError::Ciphertext`]（**只带长度**，不回显内容）。
pub fn decode_ciphertext(enc: &str) -> Result<Vec<u8>, ConfigError> {
    let stripped = strip_whitespace(enc);
    base64::engine::general_purpose::STANDARD
        .decode(&stripped)
        .map_err(|_| ConfigError::Ciphertext {
            field: FIELD_APP_SECRET_ENCRYPTED,
            len: enc.len(),
        })
}

/// 去掉 ASCII 空白（上游 `stripWhitespace`，逐字：`' '` / `'\t'` / `'\n'` / `'\r'`）。
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
mod tests;
