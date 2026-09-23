//! trigger 凭据的读写与**脱敏**（`webhook_token` / `signing_secret`）。
//!
//! - **写者**：M5-3。
//! - **上游**：`signingSecretHint`15 + `redactWebhookSecrets`19（`handler/autopilot.go`）。
//! - **契约**：响应里**只出 hint**，绝不出明文；写入两条路由（`rotate-webhook-token` /
//!   `signing-secret`）是写敏感值的路由。
//! - **通道**：走 `mc-telemetry` 的 redaction（`docs/33` §12.2），不要在本文件自己 `format!` 拼接
//!   日志字段 —— 明文进日志等于泄露。
//! - **依赖**：本 crate 已声明 `mc-telemetry`；不需要 `mc-secrets`（凭据是表列，不是 keyring）。
//!
//! # 本文件与 `dto.rs` 的分工（**别复制第二份**）
//!
//! | 件 | 落点 | 为什么 |
//! | --- | --- | --- |
//! | `signing_secret_hint` / `redact_webhook_secrets` / `webhook_path_for_token` | [`crate::dto`]（M5-1 已落） | 它们是**响应形状**的一环，读面（`triggerToResponse`）与写面共用一份 |
//! | 写入期的**长度策略** + 日志纪律 | 本文件 | 只有写面用得到；读面只需要「末 4 位」 |
//!
//! 本文件**不**做任何签名验签：`github` provider 的 `X-Hub-Signature-256` 校验属 M5-5 的
//! webhook 入口，且它需要 `hmac`/`sha2` 一类密码学依赖 —— `mc-autopilot` 的 anchor 依赖表
//! （`docs/44` §5.2）**没有**它们，所以那一块不能落在这里。

use mc_telemetry::redact_log;

/// 非空 `signing_secret` 的最小长度（上游 `handler/autopilot.go:2230`）。
///
/// 上游注释给出的选值理由：16 字符足以让 SHA-256 HMAC 的暴力搜索不现实，又低到不会拒掉
/// 「签发的密钥本来就短」的 provider（Slack 的 signing secret 是 32 位十六进制，GitHub 建议 32）。
/// **单位是字节**（Go 的 `len(string)`），本地用 [`str::len`] 保持同一口径。
pub const MIN_SIGNING_SECRET_LEN: usize = 16;

/// 长度不足时的 400 文案（上游逐字，客户端按它显示表单错误）。
pub const SIGNING_SECRET_TOO_SHORT_MESSAGE: &str = "signing_secret must be at least 16 characters";

/// `signing_secret` 的写入期判负类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// 非空但短于 [`MIN_SIGNING_SECRET_LEN`]。
    #[error("signing_secret must be at least 16 characters")]
    SigningSecretTooShort,
}

/// 归一化请求体里的 `signing_secret`（上游 `strings.TrimSpace` + 长度下限的三态）。
///
/// 三态**逐字**对照上游：
///
/// | 输入 | 结果 | 落库 |
/// | --- | --- | --- |
/// | `""` / 全空白 | `Ok(None)` | `signing_secret = NULL` = **清除**（退回「只验 bearer token」） |
/// | 非空、`< 16` 字节 | `Err(SigningSecretTooShort)` | 不落库（400） |
/// | 非空、`>= 16` 字节 | `Ok(Some(trimmed))` | `signing_secret = trimmed` |
///
/// 注意落库的是 **trim 之后**的值：上游把 `TrimSpace` 的结果绑进 SQL（首尾空白不进 HMAC 计算）。
///
/// # Errors
///
/// 见上表第二行 —— HTTP 层折成 400 + [`SIGNING_SECRET_TOO_SHORT_MESSAGE`]。
pub fn normalize_signing_secret(raw: &str) -> Result<Option<&str>, CredentialError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() < MIN_SIGNING_SECRET_LEN {
        return Err(CredentialError::SigningSecretTooShort);
    }
    Ok(Some(trimmed))
}

/// 本片写凭据路由的**唯一**日志通道：先过 `mc-telemetry` 的 redaction 再交给 `tracing`。
///
/// ⚠️ 这一层是**兜底**，不是免责：`Redactor::redact_str` 只认 `key=value` / `"key": "value"` /
/// `key: value` 三种形状（`crates/mc-telemetry/src/redact.rs:78`），**裸值不会被抹**。
/// 所以「不要把凭据值放进这一行」仍是调用方的责任 —— 本函数的用途是把「拼进日志的文本」
/// 统一收口到一个可审计的点上，并让 [`contains_credential`] 的断言有意义。
#[must_use]
pub fn redact_log_line(input: &str) -> String {
    redact_log(input)
}

/// 「这段文本里是否出现了这份凭据」—— 响应体 / 日志捕获的**共用判据**（DoD 的凭据断言）。
///
/// `credential` 为空时恒 `false`：`str::contains("")` 为真，空值下的朴素 `contains` 会把
/// 「本轮没有凭据」误判成泄露。写测试与断言时一律走这个函数，别自己写 `contains`。
#[must_use]
pub fn contains_credential(text: &str, credential: &str) -> bool {
    !credential.is_empty() && text.contains(credential)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_blank_clear_the_secret() {
        assert_eq!(normalize_signing_secret(""), Ok(None));
        assert_eq!(normalize_signing_secret("   \t\n "), Ok(None));
    }

    #[test]
    fn short_secret_is_rejected_with_the_upstream_message() {
        let err = normalize_signing_secret("too-short").expect_err("15 bytes");
        assert_eq!(err, CredentialError::SigningSecretTooShort);
        assert_eq!(err.to_string(), SIGNING_SECRET_TOO_SHORT_MESSAGE);
        // 边界：正好 16 字节通过，15 字节不通过。
        assert_eq!(
            normalize_signing_secret(&"x".repeat(15)),
            Err(CredentialError::SigningSecretTooShort)
        );
        let exactly = "x".repeat(MIN_SIGNING_SECRET_LEN);
        assert_eq!(normalize_signing_secret(&exactly), Ok(Some(exactly.as_str())));
    }

    #[test]
    fn length_is_measured_in_bytes_like_go() {
        // 上游 `len(string)` 是**字节**数（Go 的惯用口径）。6 个汉字 = 18 字节 ⇒ 通过。
        // 这条钉住「别顺手换成 `chars().count()`」—— 那会让同一份请求在两边行为不同。
        let cjk = "秘密秘密秘密";
        assert_eq!(cjk.chars().count(), 6);
        assert_eq!(normalize_signing_secret(cjk), Ok(Some(cjk)));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_before_storing() {
        let raw = "  0123456789abcdef  ";
        assert_eq!(normalize_signing_secret(raw), Ok(Some("0123456789abcdef")));
    }

    #[test]
    fn contains_credential_ignores_the_empty_case() {
        // 空凭据下 `"x".contains("")` 为真 ⇒ 朴素 contains 会误报泄露。
        assert!(!contains_credential("anything", ""));
        assert!(contains_credential("token=awt_abc", "awt_abc"));
        assert!(!contains_credential("token=[REDACTED]", "awt_abc"));
    }

    #[test]
    fn log_redaction_masks_keyed_shapes_but_not_bare_values() {
        // 兜底能力：`key=value` / `"key": "value"` 形状会被抹掉。
        assert!(!redact_log_line("webhook_token=awt_secret_value").contains("awt_secret_value"));
        assert!(redact_log_line("webhook_token=awt_secret_value").contains("[REDACTED]"));
        assert!(
            !redact_log_line(r#"{"signing_secret": "abcdef0123456789"}"#)
                .contains("abcdef0123456789")
        );
        // 边界（**故意钉住的已知限制**）：裸值不会被抹 ⇒ 日志里绝不能出现凭据值本身。
        assert!(redact_log_line("rotated awt_secret_value").contains("awt_secret_value"));
    }
}
