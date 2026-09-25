//! 安装凭据的**出站解码**（上游 `config.go` 的 `decodeCredentials`）。
//!
//! - **写者**：M7-8（`docs/32` §22 的 D1；门 ⑩ 的切分）。
//! - **三条分支复用 M7-7 的判据**（密文优先 / 明文 / 都缺 ⇒ 错误）：`mod.rs` 的
//!   `StreamInstallConfig` / `resolve_app_secret` 是**唯一**一份实现；`Decrypter` 的正式
//!   收敛归 M7-9 的 `config.rs`。

use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use crate::dingtalk::outbound::DingTalkApiError;
use crate::dingtalk::stream::AppSecret;

// =====================================================================
// 凭据
// =====================================================================

/// 一个安装的 `DingTalk` 出站凭据（上游 `credentials`）。
#[derive(Clone)]
pub struct Credentials {
    /// `AppKey`（路由键，也是令牌缓存的键）。
    pub app_key: String,
    /// 明文 `AppSecret`（手写脱敏）。
    pub app_secret: AppSecret,
    /// 机器人码（上游 `robotCodeOrAppID`）。
    pub robot_code: String,
}

impl fmt::Debug for Credentials {
    /// 手写脱敏（`docs/60` §2.3 第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("app_key", &self.app_key)
            .field("app_secret", &self.app_secret)
            .field("robot_code", &self.robot_code)
            .finish()
    }
}

/// 安装配置里的出站字段（上游 `installConfig` 的**收窄**形态：本片直接关心的三个键）。
///
/// ⚠️ 真正解凭据走的是 `mod.rs` 的 [`crate::dingtalk::StreamInstallConfig`]（它含密文列）；
/// 本结构只用于诊断 / 用例，以及 [`OutboundConfig::robot_code_or_app_id`] 这条纯函数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OutboundConfig {
    /// `app_id`（= `AppKey`）。
    #[serde(default)]
    pub app_id: String,
    /// 显式 robot code；空则退到 `app_id`。
    #[serde(default)]
    pub robot_code: String,
    /// 明文 `AppSecret`（**只**给本地 / 用例；生产形态走密文列，收敛归 M7-9 的 `config.rs`）。
    #[serde(default)]
    pub app_secret: String,
}

impl OutboundConfig {
    /// 上游 `robotCodeOrAppID`。
    #[must_use]
    pub fn robot_code_or_app_id(&self) -> String {
        if self.robot_code.is_empty() {
            self.app_id.clone()
        } else {
            self.robot_code.clone()
        }
    }
}

/// 从 `channel_installation.config` 解出**出站**凭据（上游 `decodeCredentials`）。
///
/// 三条分支（密文优先 / 明文 / 都缺 ⇒ 错误）**复用** `mod.rs` 的 `resolve_app_secret`
/// （M7-7 落的同一条判据，上游 `config.go` 里也只有那一份）⇒ 出站与连接面不会各解一套。
/// `Decrypter` 的正式收敛归 M7-9 的 `config.rs`（登记 `docs/32` §22）。
///
/// # Errors
///
/// config 不是对象 / `app_id` 为空 / 凭据解不出来（文案**不含**密文与明文）。
pub fn decode_credentials(
    config: &Value,
    decrypt: &crate::dingtalk::Decrypter,
) -> Result<Credentials, DingTalkApiError> {
    let cfg: crate::dingtalk::StreamInstallConfig = serde_json::from_value(config.clone())
        .map_err(|_| DingTalkApiError::InvalidTarget {
            reason: "installation config is not an object",
        })?;
    if cfg.app_id.is_empty() {
        return Err(DingTalkApiError::InvalidTarget {
            reason: "installation has no app_id",
        });
    }
    let app_secret = crate::dingtalk::resolve_app_secret(&cfg, decrypt).map_err(|_| {
        DingTalkApiError::InvalidTarget {
            reason: "installation credentials could not be decoded",
        }
    })?;
    let robot_code = cfg.robot_code_or_app_id().to_string();
    Ok(Credentials {
        app_key: cfg.app_id,
        app_secret,
        robot_code,
    })
}
