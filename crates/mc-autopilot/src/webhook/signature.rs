//! provider 签名校验。
//!
//! - **写者**：M5-5。
//! - **上游**：`verifyWebhookSignatureForProvider`18 + `verifyHubSignature`18。
//! - **落库列**：`webhook_delivery.signature_status ∈ {not_required,valid,invalid,missing}`。
//! - **依赖**：`hmac` / `sha2` / `base64` / `hex` **不在本 crate 的依赖表里**
//!   （§5.2 只给了 serde/uuid/chrono/chrono-tz/sqlx/thiserror/tracing）⇒ 要用就在 M5-5 的 PR 里
//!   走依赖仲裁（`docs/15` §8.4），或复用 `mc-core` 已有的实现（`mc-core` 已声明 hmac/sha2/hex）。
//!
//! # 结论：复用 `mc_core::hash`（**不加依赖**）
//!
//! `mc-core` 已有 [`mc_core::hash::hmac_sha256`] / [`mc_core::hash::hmac_sha256_verify`]，后者是
//! **常量时间**比较（逐字节 XOR 累加）⇒ 上游 `hmac.Equal` 的等价物，没有引入新依赖的必要。
//!
//! # 与上游的两处**刻意**差异（`docs/54` D5）
//!
//! 1. **大小写**：上游 `hex.DecodeString` 两种大小写都收，而 `hmac_sha256_verify` 是**逐字节比
//!    hex 字符串**（不做大小写归一）⇒ 客户端送大写 hex 会被误判 `invalid`。本地先把 hex 归一到
//!    小写再比，回到上游的「两种大小写都合法」。
//! 2. **长度/字符预检**：`hmac_sha256_verify` 只有长度相等才逐字节比，所以非 hex 的同长字符串
//!    必然不匹配 —— 行为正确，但本地仍加一道 `is_ascii_hexdigit` 预检，让「明显不是 hex」的输入
//!    走同一条 `false` 出口，不依赖实现细节。
//!
//! 前缀 `sha256=` 是**大小写敏感**的（上游 `strings.HasPrefix`），`SHA256=` 一律拒。

use mc_core::hash;

use super::provider::WebhookHeaders;
use super::SigStatus;

/// 签名头名（GitHub 兼容；`generic` provider 也用它，让 curl/Postman 能主动opt-in）。
pub const SIGNATURE_HEADER: &str = "x-hub-signature-256";

/// 签名头前缀（**大小写敏感**）。
pub const SIGNATURE_PREFIX: &str = "sha256=";

/// SHA-256 的 hex 长度（32 字节）。
const SHA256_HEX_LEN: usize = 64;

/// 上游 `verifyWebhookSignatureForProvider`：按「配没配 secret / 带没带签名头 / 验没验过」三态。
///
/// `secret` 为空 ⇒ [`SigStatus::NotRequired`]（该 trigger 选择只靠 bearer token 认证）。
/// 注意签名是**对原始 body 字节**算的，所以调用方必须传未解码的 `&[u8]`。
#[must_use]
pub fn verify_signature(secret: &str, headers: &WebhookHeaders, raw_body: &[u8]) -> SigStatus {
    if secret.is_empty() {
        return SigStatus::NotRequired;
    }
    let Some(signature) = headers.x_hub_signature_256.as_deref() else {
        return SigStatus::Missing;
    };
    if signature.is_empty() {
        return SigStatus::Missing;
    }
    if verify_hub_signature(secret, signature, raw_body) {
        SigStatus::Valid
    } else {
        SigStatus::Invalid
    }
}

/// 上游 `verifyHubSignature`：`X-Hub-Signature-256: sha256=<hex(hmac-sha256(body, secret))>`。
///
/// 常量时间比较在 `mc_core::hash` 里（本地 helper 逐字节 XOR），所以前缀错误 / 长度不符都走
/// 普通 `false`，不会因为「比到第几位才不等」泄漏信息。
#[must_use]
pub fn verify_hub_signature(secret: &str, header: &str, body: &[u8]) -> bool {
    let Some(hex_part) = header.strip_prefix(SIGNATURE_PREFIX) else {
        return false;
    };
    // ① 归一化大小写（见模块头差异 1）。
    let expected = hex_part.to_ascii_lowercase();
    // ② 预检长度与非 hex 字符（见模块头差异 2）。
    if expected.len() != SHA256_HEX_LEN || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    hash::hmac_sha256_verify(secret.as_bytes(), body, &expected)
}

/// 给测试与内部诊断用：算出某个 secret/body 的 `sha256=<hex>` 头值。
///
/// **不要**在日志里打出它的输入（secret）—— 只用于测试夹具与本地排障。
#[must_use]
pub fn hub_signature_header(secret: &str, body: &[u8]) -> String {
    format!(
        "{SIGNATURE_PREFIX}{}",
        hash::hmac_sha256(secret.as_bytes(), body)
    )
}
