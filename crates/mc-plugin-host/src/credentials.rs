//! 部署密钥的**消费**侧：hook 签名密钥派生 + 密钥封装的线格式（上游 `secretbox` 同形）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`internal/util/secretbox/secretbox.go` 的 `LoadKey` / `Seal` / `Open`，
//!   以及调用方 `internal/handler/plugin_surface.go:157`、`internal/service/plugin.go:46`。
//!
//! ## 密钥的**来源**不在本文件（别搞两处）
//!
//! 环境变量 `MULTICA_PLUGIN_SECRET_KEY` 的读取在 **`mc-http::state`**（`PluginSecretKey`，
//! 与 `GoogleOAuthConfig::from_env()` 同款：读 env、解析失败 = `None`、绝不 panic）。本文件
//! 只接受 `&[u8; 32]` / `&[u8]`，**不做 `std::env::var`** —— 否则「配置的入口」会有两处，
//! 测试也无法注入。原因：`mc-plugin-host` 不依赖 `mc-http`（分层），而 `mc-http` 依赖本 crate。
//!
//! ## 线格式（必须逐字对齐，否则老数据读不出来）
//!
//! - **key** = 32 字节（AES-256-GCM）；上游口径：env 是 **base64（`StdEncoding`，带填充）**，
//!   解码后长度必须**恰好 32**；空 / 非法 base64 / 长度不对 ⇒ 上游返回 error，本仓一律
//!   当「未配置」（`None`）—— **绝不**用零密钥兜底，也**不做 trim**（trim 会让上游拒绝的
//!   `" abc "` 被接受，是放宽）。
//! - **封装块** = `nonce(12) ‖ ciphertext ‖ tag(16)`，整块进 `plugin_secret.ciphertext`（BYTEA）。
//!   这是 secretbox 的排布，**不是** `mc-secrets::cipher::EncryptedPayload` 的 base64 双字段
//!   形态（两者不同形，所以本 crate 直接用 `aes-gcm`）。
//! - **AAD / 域分隔**：上游把密钥按用途域分隔（hook 签名 / storage / callback token 等）——
//!   各派生上下文要用**不同的**标签，禁止把同一把 key 既当签名密钥又当加密密钥用。
//!
//! - **本仓约定**：`zeroize` 语义要保留（key 不做 `Debug` 打印；需要 `Debug` 就手写脱敏实现）；
//!   派生用 `hmac`/`sha2` 的既有 workspace 依赖。
//! - **不做什么**：不做密钥轮换的持久化（`plugin_installation.token_rotated_at` 是 token 的，
//!   不是这把部署密钥的）。
//!
//! **状态：M6-1 已落地。**
//!
//! 行预算（门 ⑩）：预计 260 行以内（封装/解封 + 派生 + 用例）。
//!
//! # 四把**互不等价**的消费（上游逐点消息，别合并）
//!
//! 同一个 `MULTICA_PLUGIN_SECRET_KEY` 缺失时，上游四个消费点各自报**不同**的错 —— 因为
//! 它们的降级后果不同（存不了 secret ≠ 发不出 hook ≠ 面起不来）：
//!
//! | 消费点 | 本文件入口 | 上游消息 |
//! | --- | --- | --- |
//! | `plugin_secret` 封装 | [`secret_box`] | `plugin secrets are disabled: …` |
//! | `plugin_secret` 解封 | [`open_secret`] | `plugin secrets are disabled: …` |
//! | hook 签名 | [`hook_signing_secret`] | `hooks are disabled: …` |
//! | surface 启动令牌 | [`surface_launch_box`] | `plugin surface token encryption is not configured` |
//!
//! # 三把**派生**（`DoD` 要求各一条向量测试）
//!
//! 1. `plugin_secret` 封装 = **直接用**部署密钥（上游 `secretbox.New(pluginKey)`，不派生）；
//! 2. hook 签名密钥 = `hmac_sha256(key, "multica-plugin-hook-signature:v1:" + installationId)`
//!    —— 宿主必须能**重现**它才能签名，所以是 HMAC 而非单向哈希；每个安装的签名密钥**不落库**
//!    （否则库里就有一份可用的第三方密钥）；
//! 3. surface 启动盒 = `hmac_sha256(key, "multica/plugin-surface-launch/v1")` 做 AES 密钥
//!    —— 面 URL 永远解不开一条存储的 config secret；
//!    第四把（安装令牌哈希）是**无密钥**的 `sha256`，在 [`crate::token`]。

use std::fmt;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use mc_core::id::Id;
use rand::RngCore;
use sha2::Sha256;

/// 部署密钥的字节数（上游 `secretbox.KeySize`，AES-256）。
pub const DEPLOYMENT_KEY_SIZE: usize = 32;

/// GCM nonce 的字节数（上游 `cipher.NewGCM` 的默认 nonce 长）。
pub const GCM_NONCE_SIZE: usize = 12;

/// GCM tag 的字节数（封装块的最小尾巴）。
pub const GCM_TAG_SIZE: usize = 16;

/// 安装级签名密钥的 wire 前缀（上游 `HookSigningSecret`）。
pub const HOOK_SIGNING_SECRET_PREFIX: &str = "whsec_";

/// 签名版本前缀（上游 `hookSignatureVersion`，请求头里写成 `v1=<hex>`）。
pub const HOOK_SIGNATURE_VERSION: &str = "v1";

/// 时间戳请求头名（上游 `plugin_hook.go:321`）。
pub const HOOK_TIMESTAMP_HEADER: &str = "X-Multica-Timestamp";

/// 签名请求头名（上游 `plugin_hook.go:322`）。
pub const HOOK_SIGNATURE_HEADER: &str = "X-Multica-Signature";

/// 安装标识请求头名（上游 `plugin_hook.go:323`）。
pub const HOOK_INSTALLATION_HEADER: &str = "X-Multica-Plugin-Installation";

/// 时间戳容差（上游 `hookTimestampTolerance = 5 * time.Minute`）。
pub const HOOK_TIMESTAMP_TOLERANCE_SECS: u64 = 300;

/// hook 签名的域分隔标签（上游 `plugin_hook.go:461`，**带尾部冒号**）。
const HOOK_SIGNING_LABEL: &[u8] = b"multica-plugin-hook-signature:v1:";

/// surface 启动盒的域分隔标签（上游 `plugin_surface.go:58`，**无尾部冒号**）。
const SURFACE_LAUNCH_LABEL: &[u8] = b"multica/plugin-surface-launch/v1";

type HmacSha256 = Hmac<Sha256>;

/// 缺密钥 / 封装块损坏 / 签名不符。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// `plugin_secret` 面：未配置部署密钥（上游 `plugin.go:587`）。
    #[error("plugin secrets are disabled: MULTICA_PLUGIN_SECRET_KEY is not configured")]
    PluginSecretsDisabled,
    /// hook 面：未配置部署密钥（上游 `plugin_hook.go:454`）。
    #[error("hooks are disabled: MULTICA_PLUGIN_SECRET_KEY is not configured")]
    HooksDisabled,
    /// surface 面：未配置部署密钥（上游 `plugin_surface.go:113`）。
    #[error("plugin surface token encryption is not configured")]
    SurfaceTokensDisabled,
    /// 密钥长度不是 32（上游 `secretbox.ErrInvalidKey`）。
    #[error("secretbox: key must be 32 bytes")]
    InvalidKey,
    /// 密文短于 nonce + tag（上游 `secretbox.ErrCiphertextTooShort`）。
    #[error("secretbox: ciphertext too short")]
    CiphertextTooShort,
    /// GCM 认证失败 —— 被改过，或换过密钥。
    #[error("secretbox: ciphertext failed authentication")]
    Authentication,
    /// 取 nonce 失败（上游 wrap `rand.Read`）。
    #[error("secretbox: read nonce")]
    Nonce,
    /// 令牌不是合法的 base64url（上游 `"invalid plugin surface token encoding"`）。
    #[error("plugin token is not valid base64url")]
    TokenEncoding,
    /// 签名密钥不是 hex（上游 `"signing secret is not valid hex"`）。
    #[error("signing secret is not valid hex")]
    SignatureSecretFormat,
    /// 时间戳不是整数（上游 `"timestamp is not an integer"`）。
    #[error("timestamp is not an integer")]
    SignatureTimestampFormat,
    /// 时间戳超出窗口（上游 `"timestamp is outside the accepted window"`）。
    #[error("timestamp is outside the accepted window")]
    SignatureTimestampWindow,
    /// 签名不匹配（上游 `"signature does not match"`）。
    #[error("signature does not match")]
    SignatureMismatch,
}

impl CredentialError {
    /// 稳定错误码（route 层映射 JSON 错误体时用）。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PluginSecretsDisabled | Self::HooksDisabled | Self::SurfaceTokensDisabled => {
                "plugin_credentials_unavailable"
            }
            Self::SignatureMismatch
            | Self::SignatureSecretFormat
            | Self::SignatureTimestampFormat
            | Self::SignatureTimestampWindow => "plugin_signature_invalid",
            Self::InvalidKey
            | Self::CiphertextTooShort
            | Self::Authentication
            | Self::Nonce
            | Self::TokenEncoding => "plugin_secret_invalid",
        }
    }
}

/// `MULTICA_PLUGIN_SECRET_KEY` 解码后的 32 字节部署密钥。
///
/// **不进 `Debug`**：手写实现只打印一个占位串，免得密钥经日志/tracing 漏出去。
#[derive(Clone, PartialEq, Eq)]
pub struct DeploymentKey([u8; DEPLOYMENT_KEY_SIZE]);

impl DeploymentKey {
    /// 从**已解码**的字节构造；长度不是 32 时返回 `None`（上游把长度错也当「未配置」）。
    ///
    /// 长度判定收在构造点，是**更严**的等价：上游要到用的时候才报
    /// `"…must decode to 32 bytes"`，本仓让这种密钥根本构造不出来。
    #[must_use]
    pub fn new(bytes: impl AsRef<[u8]>) -> Option<Self> {
        let bytes = bytes.as_ref();
        if bytes.len() != DEPLOYMENT_KEY_SIZE {
            return None;
        }
        let mut key = [0u8; DEPLOYMENT_KEY_SIZE];
        key.copy_from_slice(bytes);
        Some(Self(key))
    }

    /// 从 base64（`StdEncoding`，**带填充、不 trim**）构造 —— 与 `secretbox.LoadKey` 同形。
    ///
    /// 空串 / 非法 base64 / 长度不是 32 一律 `None`。
    #[must_use]
    pub fn from_base64(raw: &str) -> Option<Self> {
        let decoded = base64::engine::general_purpose::STANDARD.decode(raw).ok()?;
        Self::new(decoded)
    }

    /// 原始字节（只在派生/建盒处用）。
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; DEPLOYMENT_KEY_SIZE] {
        &self.0
    }
}

impl fmt::Debug for DeploymentKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DeploymentKey(<redacted>)")
    }
}

/// AES-256-GCM 封装盒（上游 `secretbox.Box`）。
///
/// 构造一次、进程内复用（上游注释：每请求重建会白白重推 AES 轮密钥）。
#[derive(Clone)]
pub struct SecretBox {
    cipher: Aes256Gcm,
}

impl fmt::Debug for SecretBox {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBox(<redacted>)")
    }
}

impl SecretBox {
    /// 用一把 32 字节密钥建盒。
    ///
    /// # Errors
    ///
    /// [`CredentialError::InvalidKey`]：长度不是 32（本仓的 `DeploymentKey` 与派生结果都
    /// 保证是 32，故这条只在被直接喂字节时出现）。
    pub fn new(key: &[u8]) -> Result<Self, CredentialError> {
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CredentialError::InvalidKey)?;
        Ok(Self { cipher })
    }

    /// 封一条明文：`nonce(12) ‖ ciphertext ‖ tag(16)`。
    ///
    /// nonce 每次随机 —— **同一明文两次封装的输出不同**，不要拿密文当索引。
    ///
    /// # Errors
    ///
    /// [`CredentialError::Nonce`]（熵源失败）/ [`CredentialError::Authentication`]（GCM 拒绝）。
    pub fn seal(&self, plaintext: &[u8], rng: &mut impl RngCore) -> Result<Vec<u8>, CredentialError> {
        let mut nonce = [0u8; GCM_NONCE_SIZE];
        rng.try_fill_bytes(&mut nonce)
            .map_err(|_| CredentialError::Nonce)?;
        let sealed = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .map_err(|_| CredentialError::Authentication)?;
        let mut out = Vec::with_capacity(GCM_NONCE_SIZE + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// 解一条封装块（`nonce ‖ ciphertext ‖ tag`）。
    ///
    /// # Errors
    ///
    /// [`CredentialError::CiphertextTooShort`] / [`CredentialError::Authentication`]。
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, CredentialError> {
        if sealed.len() < GCM_NONCE_SIZE + GCM_TAG_SIZE {
            return Err(CredentialError::CiphertextTooShort);
        }
        let (nonce, ciphertext) = sealed.split_at(GCM_NONCE_SIZE);
        self.cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| CredentialError::Authentication)
    }
}

/// 消费点 1/4：`plugin_secret` 的**封装**盒（**直接用**部署密钥，不派生）。
///
/// # Errors
///
/// [`CredentialError::PluginSecretsDisabled`]。
pub fn secret_box(key: Option<&DeploymentKey>) -> Result<SecretBox, CredentialError> {
    let key = key.ok_or(CredentialError::PluginSecretsDisabled)?;
    SecretBox::new(key.as_bytes())
}

/// 消费点 2/4：`plugin_secret` 的**解封**（与封装同一个盒，但独立入口 ⇒ 降级可逐点测）。
///
/// # Errors
///
/// [`CredentialError::PluginSecretsDisabled`]，以及 [`SecretBox::open`] 的错。
pub fn open_secret(key: Option<&DeploymentKey>, sealed: &[u8]) -> Result<Vec<u8>, CredentialError> {
    secret_box(key)?.open(sealed)
}

/// 消费点 3/4：某个安装的 hook 签名密钥（32 字节，**不落库**）。
///
/// `hmac_sha256(key, "multica-plugin-hook-signature:v1:" + installationId)`。
/// 用 HMAC 而非单向哈希是**故意的**：宿主必须能重现这个值才能签名；而安装令牌是反方向
/// （插件产生、宿主只验证）⇒ 那边只存哈希。同一把部署密钥，两个方向。
///
/// # Errors
///
/// [`CredentialError::HooksDisabled`]。
pub fn hook_signing_key(
    key: Option<&DeploymentKey>,
    installation_id: Id,
) -> Result<[u8; 32], CredentialError> {
    let key = key.ok_or(CredentialError::HooksDisabled)?;
    let mut mac = hmac_new(key.as_bytes())?;
    mac.update(HOOK_SIGNING_LABEL);
    mac.update(installation_id.as_string().as_bytes());
    Ok(mac.finalize().into_bytes().into())
}

/// 消费点 3/4（作者形态）：`whsec_` + 签名密钥的 hex —— 装好后配到作者自己的服务器上。
///
/// # Errors
///
/// [`CredentialError::HooksDisabled`]。
pub fn hook_signing_secret(
    key: Option<&DeploymentKey>,
    installation_id: Id,
) -> Result<String, CredentialError> {
    let derived = hook_signing_key(key, installation_id)?;
    Ok(format!(
        "{HOOK_SIGNING_SECRET_PREFIX}{}",
        hex::encode(derived)
    ))
}

/// 宿主侧签名：`hex(hmac_sha256(签名密钥, timestamp + "." + body))`。
///
/// 用 `.` 连接是**故意的**（上游注释）：只签 body 会让抓到的请求可永久重放；直接拼接又
/// 会让构造的时间戳 + body 交换字节。
///
/// # Errors
///
/// [`CredentialError::HooksDisabled`]。
pub fn sign_hook_payload(
    key: Option<&DeploymentKey>,
    installation_id: Id,
    timestamp: &str,
    body: &[u8],
) -> Result<String, CredentialError> {
    let derived = hook_signing_key(key, installation_id)?;
    Ok(hex::encode(hmac_hex(&derived, timestamp, body)))
}

/// 接收方校验（上游 `VerifyHookSignature`，**同一份**实现，免得只有一方能实现的方案没法评审）。
///
/// `presented` 允许带 `v1=` 前缀；比较是**常数时间**的（逐字节比较会泄露猜对了几位）。
///
/// # Errors
///
/// [`CredentialError::SignatureSecretFormat`] / [`SignatureTimestampFormat`] /
/// [`SignatureTimestampWindow`] / [`SignatureMismatch`]。
pub fn verify_hook_signature(
    secret_hex: &str,
    timestamp: &str,
    body: &[u8],
    presented: &str,
    now_unix: u64,
) -> Result<(), CredentialError> {
    let key = hex::decode(
        secret_hex
            .strip_prefix(HOOK_SIGNING_SECRET_PREFIX)
            .unwrap_or(secret_hex),
    )
    .map_err(|_| CredentialError::SignatureSecretFormat)?;
    let seconds = timestamp
        .parse::<i64>()
        .map_err(|_| CredentialError::SignatureTimestampFormat)?;
    let now = i64::try_from(now_unix).unwrap_or(i64::MAX);
    let drift = now.saturating_sub(seconds).unsigned_abs();
    if drift > HOOK_TIMESTAMP_TOLERANCE_SECS {
        return Err(CredentialError::SignatureTimestampWindow);
    }
    let mut mac = hmac_new(&key)?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    let presented = presented
        .strip_prefix(&format!("{HOOK_SIGNATURE_VERSION}="))
        .unwrap_or(presented);
    let presented = hex::decode(presented).map_err(|_| CredentialError::SignatureMismatch)?;
    mac.verify_slice(&presented)
        .map_err(|_| CredentialError::SignatureMismatch)
}

/// 消费点 4/4：surface 启动令牌盒 —— 用**派生**密钥，面 URL 永远解不开存储的 config secret。
///
/// # Errors
///
/// [`CredentialError::SurfaceTokensDisabled`]。
pub fn surface_launch_box(key: Option<&DeploymentKey>) -> Result<SecretBox, CredentialError> {
    let key = key.ok_or(CredentialError::SurfaceTokensDisabled)?;
    SecretBox::new(&derive(key, SURFACE_LAUNCH_LABEL))
}

/// 把一段载荷封成 URL 安全的令牌：`base64url_nopad(seal(payload))`（上游 `mintPluginSurfaceToken`）。
///
/// claims 的类型与校验归 M6-7；本函数只管**信封**。
///
/// # Errors
///
/// [`SecretBox::seal`]。
pub fn seal_to_token(
    boxed: &SecretBox,
    payload: &[u8],
    rng: &mut impl RngCore,
) -> Result<String, CredentialError> {
    let sealed = boxed.seal(payload, rng)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sealed))
}

/// 反向：`open(base64url_nopad_decode(token))`（上游 `openPluginSurfaceToken` 的前两步）。
///
/// # Errors
///
/// [`CredentialError::TokenEncoding`]，以及 [`SecretBox::open`] 的错。
pub fn open_token(boxed: &SecretBox, token: &str) -> Result<Vec<u8>, CredentialError> {
    let sealed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| CredentialError::TokenEncoding)?;
    boxed.open(&sealed)
}

/// HMAC 的构造点：`KeyInit` 与 `Mac` 都提供 `new_from_slice`，必须写明走哪一个。
fn hmac_new(key: &[u8]) -> Result<HmacSha256, CredentialError> {
    <HmacSha256 as Mac>::new_from_slice(key).map_err(|_| CredentialError::InvalidKey)
}

/// `hmac_sha256(key, ...)` 的 32 字节结果。
fn hmac_hex(key: &[u8], timestamp: &str, body: &[u8]) -> [u8; 32] {
    let mut mac = hmac_new(key).expect("hmac accepts any key length");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.finalize().into_bytes().into()
}

/// 域分隔派生：`hmac_sha256(deployment_key, label)`。
fn derive(key: &DeploymentKey, label: &[u8]) -> [u8; 32] {
    let mut mac = hmac_new(key.as_bytes()).expect("hmac accepts any key length");
    mac.update(label);
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定密钥：`0x00..0x1f`（base64 见 `deployment_key_accepts_only_exact_base64`）。
    fn key() -> DeploymentKey {
        DeploymentKey::new((0u8..32).collect::<Vec<u8>>()).expect("32 bytes")
    }

    /// 固定安装 id。
    fn installation() -> Id {
        Id::parse("11111111-2222-3333-4444-555555555555").expect("uuid")
    }

    /// 可预测熵源。
    struct FixedRng(u8);

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            u32::from(self.0) * 0x0101_0101
        }
        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0u8; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            dest.fill(self.0);
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    /// 顺序熵源：第 n 个字节填 n（对齐 python 向量里的 `nonce = 0x00..0x0b`）。
    struct SequentialRng(u8);

    impl RngCore for SequentialRng {
        fn next_u32(&mut self) -> u32 {
            let mut bytes = [0u8; 4];
            self.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        }
        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0u8; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for byte in dest.iter_mut() {
                *byte = self.0;
                self.0 = self.0.wrapping_add(1);
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    #[test]
    fn deployment_key_accepts_only_exact_base64() {
        assert_eq!(
            DeploymentKey::from_base64("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="),
            Some(key())
        );
        // 空 / 非法 base64 / 长度不对 ⇒ 未配置（绝不用零密钥兜底）。
        assert_eq!(DeploymentKey::from_base64(""), None);
        assert_eq!(DeploymentKey::from_base64("not base64!!"), None);
        assert_eq!(DeploymentKey::from_base64("AAEC"), None);
        // 不做 trim：上游拒绝的 `" <b64> "` 不能被接受。
        assert_eq!(
            DeploymentKey::from_base64(" AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8= "),
            None
        );
        // Debug 必须脱敏。
        assert_eq!(format!("{:?}", key()), "DeploymentKey(<redacted>)");
        assert!(!format!("{:?}", key()).contains("AAEC"));
    }

    #[test]
    fn secret_box_round_trips_a_python_vector() {
        // 交叉实现向量（python `cryptography` AESGCM，key = 0x00..0x1f，nonce = 0x00..0x0b,
        // 明文 `hello plugin secret`）：blob = nonce ‖ ct ‖ tag。
        let blob = hex::decode(concat!(
            "000102030405060708090a0b",
            "2f67ba77aac5b277f826fee5919a1d0ef1b3f31527501d53a42fae6d12f41cb5449f0b"
        ))
        .expect("hex");
        let boxed = secret_box(Some(&key())).expect("box");
        assert_eq!(
            boxed.open(&blob).expect("open"),
            b"hello plugin secret".to_vec()
        );
        // 反向：nonce = 0x00..0x0b（顺序熵源）⇒ 逐字节等于向量。
        assert_eq!(
            boxed
                .seal(b"hello plugin secret", &mut SequentialRng(0))
                .unwrap(),
            blob
        );
    }

    #[test]
    fn secret_box_rejects_tampering_and_short_blobs() {
        let boxed = secret_box(Some(&key())).expect("box");
        let mut blob = boxed.seal(b"x", &mut FixedRng(7)).expect("seal");
        assert_eq!(blob.len(), GCM_NONCE_SIZE + 1 + GCM_TAG_SIZE);
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert_eq!(boxed.open(&blob), Err(CredentialError::Authentication));
        assert_eq!(
            boxed.open(&[0u8; GCM_NONCE_SIZE + GCM_TAG_SIZE - 1]),
            Err(CredentialError::CiphertextTooShort)
        );
        // nonce 随机：同一明文两次封装不同。
        let first = boxed.seal(b"same", &mut FixedRng(1)).unwrap();
        let second = boxed.seal(b"same", &mut FixedRng(2)).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn hook_signing_key_matches_upstream_hmac() {
        // python: hmac_sha256(key, b"multica-plugin-hook-signature:v1:" + installationId)
        assert_eq!(
            hex::encode(hook_signing_key(Some(&key()), installation()).unwrap()),
            "c546e0ba85cd275ae78732bc3e42e3785f113859c0393a1775ed18534ddf636c"
        );
        assert_eq!(
            hook_signing_secret(Some(&key()), installation()).unwrap(),
            "whsec_c546e0ba85cd275ae78732bc3e42e3785f113859c0393a1775ed18534ddf636c"
        );
    }

    #[test]
    fn hook_signature_round_trips_and_rejects_replay() {
        let secret = hook_signing_secret(Some(&key()), installation()).unwrap();
        let body = br#"{"a":1}"#;
        let signature = sign_hook_payload(Some(&key()), installation(), "1700000000", body).unwrap();
        // python 向量。
        assert_eq!(
            signature,
            "873c6ba06ad0a611567c056b99fb2186e7f71764a9c93afea986301a62af684c"
        );
        assert_eq!(
            verify_hook_signature(&secret, "1700000000", body, &signature, 1_700_000_100),
            Ok(())
        );
        // `v1=` 前缀在接收侧被剥掉；`whsec_` 前缀在密钥侧被剥掉。
        assert_eq!(
            verify_hook_signature(
                &secret,
                "1700000000",
                body,
                &format!("v1={signature}"),
                1_700_000_100
            ),
            Ok(())
        );
        assert_eq!(
            verify_hook_signature(&secret, "1700000000", body, "deadbeef", 1_700_000_100),
            Err(CredentialError::SignatureMismatch)
        );
        // 换 body ⇒ 不匹配（这就是签名存在的意义）。
        assert_eq!(
            verify_hook_signature(&secret, "1700000000", b"{}", &signature, 1_700_000_100),
            Err(CredentialError::SignatureMismatch)
        );
        // 重放：时间戳超窗。
        assert_eq!(
            verify_hook_signature(&secret, "1700000000", body, &signature, 1_700_000_400),
            Err(CredentialError::SignatureTimestampWindow)
        );
        // 时间戳不是整数。
        assert_eq!(
            verify_hook_signature(&secret, "yesterday", body, &signature, 1_700_000_100),
            Err(CredentialError::SignatureTimestampFormat)
        );
        // 密钥不是 hex。
        assert_eq!(
            verify_hook_signature("zz", "1700000000", body, &signature, 1_700_000_100),
            Err(CredentialError::SignatureSecretFormat)
        );
    }

    #[test]
    fn surface_launch_box_uses_a_derived_key() {
        // python: AesGcm(hmac_sha256(key, b"multica/plugin-surface-launch/v1")) 的固定向量。
        let blob = hex::decode(concat!(
            "000102030405060708090a0b",
            "be89dcb0d2ec0d6e2226ff2d8991c24b0f54619b258ccbf907579c2c45a1"
        ))
        .expect("hex");
        let boxed = surface_launch_box(Some(&key())).expect("box");
        assert_eq!(boxed.open(&blob).expect("open"), b"surface-claims".to_vec());
        // 派生密钥 ≠ 部署密钥：面盒解不开用部署密钥封的块，反之亦然。
        let direct = secret_box(Some(&key())).expect("box");
        assert_eq!(boxed.open(&direct.seal(b"x", &mut FixedRng(3)).unwrap()), Err(CredentialError::Authentication));
        assert_eq!(direct.open(&blob), Err(CredentialError::Authentication));
    }

    #[test]
    fn surface_token_envelope_is_base64url_without_padding() {
        let boxed = surface_launch_box(Some(&key())).expect("box");
        let token = seal_to_token(&boxed, br#"{"k":"v"}"#, &mut FixedRng(9)).expect("seal");
        assert!(!token.contains('=') && !token.contains('+') && !token.contains('/'));
        assert_eq!(open_token(&boxed, &token).expect("open"), br#"{"k":"v"}"#.to_vec());
        assert_eq!(
            open_token(&boxed, "not!base64url!"),
            Err(CredentialError::TokenEncoding)
        );
        // 被截断的令牌 = 认证失败，不是 panic。
        assert!(matches!(
            open_token(&boxed, &token[..token.len() - 4]),
            Err(CredentialError::Authentication | CredentialError::TokenEncoding)
        ));
    }

    #[test]
    fn missing_deployment_key_degrades_each_consumer_explicitly() {
        // DoD：缺密钥 ⇒ 4 个消费点各自显式失败，且**消息不同**（降级后果不同）。
        assert_eq!(
            secret_box(None).unwrap_err(),
            CredentialError::PluginSecretsDisabled
        );
        assert_eq!(
            open_secret(None, &[0u8; 32]).unwrap_err(),
            CredentialError::PluginSecretsDisabled
        );
        assert_eq!(
            hook_signing_secret(None, installation()).unwrap_err(),
            CredentialError::HooksDisabled
        );
        assert_eq!(
            surface_launch_box(None).unwrap_err(),
            CredentialError::SurfaceTokensDisabled
        );
        assert_eq!(
            CredentialError::PluginSecretsDisabled.to_string(),
            "plugin secrets are disabled: MULTICA_PLUGIN_SECRET_KEY is not configured"
        );
        assert_eq!(
            CredentialError::HooksDisabled.to_string(),
            "hooks are disabled: MULTICA_PLUGIN_SECRET_KEY is not configured"
        );
        assert_eq!(
            CredentialError::SurfaceTokensDisabled.to_string(),
            "plugin surface token encryption is not configured"
        );
        // 缺密钥的两个入口都是「不可用」码，签名不符是「无效」码。
        assert_eq!(
            CredentialError::HooksDisabled.code(),
            "plugin_credentials_unavailable"
        );
        assert_eq!(
            CredentialError::SignatureMismatch.code(),
            "plugin_signature_invalid"
        );
    }
}
