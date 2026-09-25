//! lark 安装行的**配置解码**与 `app_secret` **解密**
//! （上游 `internal/integrations/lark/store.go` 的 `feishuInstallConfig` / `installationFromRow` /
//! `decodeSecret` + `installation.go` 的 `InstallationService.DecryptAppSecret`）。
//!
//! - **写者**：M7-12（`docs/60-M7-PLAN.md` §3.3 的写集勘误见 `docs/32` §29 —— 本文件是
//!   `feishu_channel.rs` 的子模块，对应 `docs/32` §28.3 的 **D3**：「解密 `app_secret` 需要
//!   解密器与安装行，那是 M7-12 的 `feishu_channel.rs` / M7-14 的安装面」）。
//! - **两条安装路径的形状不同，不通用**（`docs/60` §6.4 的两套表）：
//!
//! | 表 | 密文形态 | 谁读 |
//! | --- | --- | --- |
//! | `channel_installation.config`（JSONB） | `app_secret_encrypted` 是 **base64 字符串** | 工厂（[`LarkInstallConfig`]） |
//! | `lark_installation.app_secret_encrypted`（`BYTEA`） | **裸密文字节** | 解析器 / 媒体面（[`super::super::resolvers::LarkInstallation`]） |
//!
//!   两条路径的**共同出口**是 [`installation_credentials_for`]：它只认「密文字节 + 解密器」，
//!   于是密文形态的差异被挡在本文件里。
//!
//! # 凭据纪律（`docs/60` §2.3 的四条判据，逐条落在这里）
//!
//! 1. **手写 `Debug`**：[`LarkInstallConfig`] / [`Decrypter`] / [`super::super::params::InstallationCredentials`]
//!    都不派生；密文列只报 `<redacted>` / `<empty>`（运维要知道"配没配"，不需要值）；
//! 2. [`ConfigError`] 的**每个变体只带结构信息**（字段名 / 长度 / 来源），**绝不**带明文、
//!    密文或密钥字节 —— 这是「错误路径不回显凭据」的结构性保证；
//! 3. 解密只经 `mc_secrets::secretbox`（`nonce(12) ‖ ct ‖ tag` 的单块字节形态）；
//! 4. 本文件**零** `tracing::*` 调用。
//!
//! # 未接线 = 失败关闭（与 slack 的同一纪律）
//!
//! [`Decrypter::login_plaintext`] 是**测试便利**，不是生产降级：拿它跑生产会把密文当
//! `app_secret` 用。所以 [`Decrypter::fail_closed`] 存在 —— 未经显式注入的装配路径用它，
//! **宁可拒装配也不假装连上**（`docs/32` §29 的接线一节）。

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use mc_secrets::secretbox::SecretBox;
use serde::{Deserialize, Serialize};

use super::super::params::{AppSecret, InstallationCredentials};
use super::super::resolvers::LarkInstallation;
use super::super::types::{OpenId, Region};

/// 解密失败的分类（上游 `secretbox.Box.Open` 的错误）。
///
/// 变体**不带载荷**：既不回显密文也不回显明文。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecryptError {
    /// `secretbox` 认证失败 / 长度不对。
    #[error("lark: stored app_secret failed authentication")]
    Authentication,
    /// 这条装配路径**没有**接入解密器（[`Decrypter::fail_closed`]）：拒装配。
    #[error("lark: no credential decrypter is wired for this build path")]
    NotWired,
    /// 解出来的字节不是 UTF-8（`app_secret` 必须是文本）。
    #[error("lark: decrypted app_secret is not valid UTF-8")]
    NotUtf8,
}

/// 解密函数的裸形态（别名是为了让 `clippy::type_complexity` 满意）。
pub type DecryptFn = dyn Fn(&[u8]) -> Result<Vec<u8>, DecryptError> + Send + Sync;

/// 解密密文的函数值（上游 `InstallationService.DecryptAppSecret` 的那个 `box`）。
///
/// 生产装配注入 [`mc_secrets::secretbox`] 的实现（[`Decrypter::secret_box`]）；
/// 用例注入身份解密器（[`Decrypter::login_plaintext`]，上游 `box == nil` 的等价物）。
#[derive(Clone)]
pub struct Decrypter {
    inner: Arc<DecryptFn>,
    label: &'static str,
}

impl fmt::Debug for Decrypter {
    /// 手写脱敏：函数值不可打印，只打印**类别**（生产排查要能区分"接的是哪个解密器"）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Decrypter")
            .field(&self.label)
            .finish()
    }
}

impl Decrypter {
    /// 身份解密器：把存下来的字节当明文（上游 `box == nil` 的语义，**用例便利**）。
    #[must_use]
    pub fn login_plaintext() -> Self {
        Self {
            inner: Arc::new(|plaintext: &[u8]| Ok(plaintext.to_vec())),
            label: "plaintext(test)",
        }
    }

    /// `secretbox` 解密器（生产形态）。
    #[must_use]
    pub fn secret_box(boxed: SecretBox) -> Self {
        Self {
            inner: Arc::new(move |ciphertext: &[u8]| {
                boxed
                    .open(ciphertext)
                    .map_err(|_| DecryptError::Authentication)
            }),
            label: "secretbox",
        }
    }

    /// 未接线：任何非空密文都**拒绝**。
    #[must_use]
    pub fn fail_closed() -> Self {
        Self {
            inner: Arc::new(|_| Err(DecryptError::NotWired)),
            label: "fail-closed(not-wired)",
        }
    }

    /// 自定义解密器（用例与将来的密钥轮换用）。
    pub fn custom<F>(label: &'static str, f: F) -> Self
    where
        F: Fn(&[u8]) -> Result<Vec<u8>, DecryptError> + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(f),
            label,
        }
    }

    /// 解一块密文（唯一出口）。
    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, DecryptError> {
        (self.inner)(ciphertext)
    }
}

// =====================================================================
// `channel_installation.config` 的 lark 形状
// =====================================================================

/// `channel_installation.config` 的 lark 形状（上游 `feishuInstallConfig`）。
///
/// `app_secret_encrypted` 是 **base64 的 `secretbox` 密文**（`omitempty` 对应迁移的
/// `jsonb_strip_nulls`）；**手写 `Debug`** 只报这一列的存在性。
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct LarkInstallConfig {
    /// 平台的 `app_id`（`cli_…`）。**不是**秘密，路由键就是它。
    #[serde(default)]
    pub app_id: String,
    /// base64 的密文；空串 = 这条配置**没有**密文（工厂据此拒装配）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app_secret_encrypted: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tenant_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bot_open_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bot_union_id: String,
    /// `feishu` / `lark`；空值回落飞书（[`Region::or_default`]）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub region: String,
}

impl fmt::Debug for LarkInstallConfig {
    /// 手写脱敏：只打印**哪个**密文列非空，绝不打印它的值。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LarkInstallConfig")
            .field("app_id", &self.app_id)
            .field(
                "app_secret_encrypted",
                &redaction_of(&self.app_secret_encrypted),
            )
            .field("tenant_key", &self.tenant_key)
            .field("bot_open_id", &self.bot_open_id)
            .field("bot_union_id", &redaction_of(self.bot_union_id.as_str()))
            .field("region", &self.region)
            .finish()
    }
}

/// 某一列的脱敏说明（`<redacted>` / `<empty>`）。
fn redaction_of(value: &str) -> &'static str {
    if value.is_empty() {
        "<empty>"
    } else {
        "<redacted>"
    }
}

impl LarkInstallConfig {
    /// 解出一条**凭据用**的安装投影（上游 `installationFromRow`）。
    ///
    /// 身份字段（workspace / agent / installer / status）在配置 blob 里**没有** ⇒ 由调用方
    /// 从泛化行补齐（[`LarkInstallConfig::into_installation`] 接受它们）。
    ///
    /// # Errors
    ///
    /// `app_secret_encrypted` 不是合法 base64 ⇒ [`ConfigError::SecretNotBase64`]（**只报长度**）。
    pub fn into_installation(
        self,
        id: mc_core::id::Id,
        workspace_id: mc_core::id::Id,
        agent_id: mc_core::id::Id,
        installer_user_id: mc_core::id::Id,
        status: impl Into<String>,
    ) -> Result<LarkInstallation, ConfigError> {
        Ok(LarkInstallation {
            id,
            workspace_id,
            agent_id,
            app_id: self.app_id,
            app_secret_encrypted: decode_secret(&self.app_secret_encrypted)?,
            tenant_key: non_empty(self.tenant_key),
            bot_open_id: OpenId::new(self.bot_open_id),
            bot_union_id: non_empty(self.bot_union_id),
            region: Region::or_default(&self.region),
            installer_user_id,
            status: status.into(),
        })
    }
}

/// 空串 → `None`（上游 `textOrNull`）。
fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

// =====================================================================
// 配置解码 / 解密
// =====================================================================

/// 配置解码 / 解密失败（上游 `decodeSecret` / `installationFromRow` 的那批 `fmt.Errorf`）。
///
/// **每个变体只带字段名或长度**（凭据纪律第 2 条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// 密文列不是合法 base64（只报原始长度）。
    #[error("lark: app_secret_encrypted is not valid base64 ({length} bytes)")]
    SecretNotBase64 { length: usize },
    /// 解密失败（转成 [`DecryptError`]，**不带**任何密文/明文字节）。
    #[error("lark: decrypt app_secret: {source}")]
    Decrypt {
        #[source]
        source: DecryptError,
    },
    /// 安装行没有 `app_id` ⇒ 工厂拒装配（配置不全不该交出半成品）。
    #[error("lark: installation config has no app_id")]
    MissingAppId,
    /// 安装行没有密文列 ⇒ 工厂拒装配。
    #[error("lark: installation config has no app_secret_encrypted")]
    MissingSecret,
}

/// 把 base64 密文列解成裸字节（上游 `decodeSecret` + `stripWhitespace`）。
///
/// ⚠️ 先剥掉 ASCII 空白：SQL 回填可能带上 MIME 换行包装的 base64（上游注释逐字）。
fn decode_secret(encoded: &str) -> Result<Vec<u8>, ConfigError> {
    if encoded.is_empty() {
        return Ok(Vec::new());
    }
    let stripped: String = encoded
        .chars()
        .filter(|c| !matches!(c, '\n' | '\r' | ' ' | '\t'))
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(stripped.as_bytes())
        .map_err(|_| ConfigError::SecretNotBase64 {
            length: encoded.len(),
        })
}

/// 解一条安装行的**明文**凭据（上游 `installationCredentialsFor`）。
///
/// 上游注释逐字的设计理由：明文 `app_secret` **只在一次调用在飞期间存在**于返回的结构里；
/// 调用方**不得**记日志、不得持久化（[`InstallationCredentials`] 的 `Debug` 已经脱敏）。
///
/// # Errors
///
/// 密文解不开 ⇒ [`ConfigError::Decrypt`]（**不回显**密文）。
pub fn installation_credentials_for(
    installation: &LarkInstallation,
    decrypter: &Decrypter,
) -> Result<InstallationCredentials, ConfigError> {
    let plain = decrypter
        .decrypt(&installation.app_secret_encrypted)
        .map_err(|source| ConfigError::Decrypt { source })?;
    let secret = String::from_utf8(plain).map_err(|_| ConfigError::Decrypt {
        source: DecryptError::NotUtf8,
    })?;
    let mut credentials =
        InstallationCredentials::new(&installation.app_id, AppSecret::new(secret))
            .with_region(installation.region);
    if let Some(tenant_key) = installation.tenant_key.as_deref() {
        credentials = credentials.with_tenant_key(tenant_key);
    }
    Ok(credentials)
}

#[cfg(test)]
mod tests;
