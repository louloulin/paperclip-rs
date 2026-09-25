//! GitHub 入站 webhook 的验签与事件分类入口 —— 上游 `HandleGitHubWebhook`
//! （`github.go:1056`）+ `verifyWebhookSignature`（`github.go:1096`）。
//!
//! # 凭据与路由口径（`docs/61` §1.5 / §2.5）
//!
//! `POST /api/webhooks/github` 是**公开块**路由（不挂会话 middleware），凭据是
//! `GITHUB_WEBHOOK_SECRET` + `X-Hub-Signature-256`（HMAC-SHA256）。**缺密钥 ⇒ 404
//! `not found`**（上游第一条：宁可整体拒收，也不把未配置的部署当成「所有签名都有效」）；
//! **验签失败 ⇒ 401 `invalid signature`**（**不是** 404：路由必须在，鉴权在 handler 内）。
//!
//! 两个失败分支的状态码由**调用方**（HTTP handler）决定：本文件只提供「密钥空 ⇒ 必失败」
//! 与「签名是否有效」两个判据，不持有 `GITHUB_WEBHOOK_SECRET` 的读取口（那是
//! `mc_http::state` 的 `github_keys`，`docs/61` §2.4 的唯一出口纪律）。
//!
//! # 幂等
//!
//! 同一 webhook 重投 2 次只插 1 行 PR（M8-4 的 `DoD`，`docs/61` §6.5）：幂等由
//! `github_pull_request` 的 `(workspace_id, repo_owner, repo_name, pr_number)` 唯一键
//! 与 `issue_pull_request` 的 `ON CONFLICT` upsert 承担，本文件不额外去重。
//!
//! # 为什么不用 `mc_vcs::signature`
//!
//! anchor 的文件头注释提议「实现可直接复用 `mc_vcs::signature::verify_hmac_sha256_hex`」，
//! 但本 crate 的依赖边里**没有** `mc-vcs`（`Cargo.toml` 逐字：`mc-core` / `mc-errors` /
//! `mc-repos` / `mc-telemetry` + 一组既有三方包）。`hmac` / `sha2` / `hex` 都是直连边，
//! 所以这里用它们逐字复刻那个函数的三条语义（见下）。

use hmac::Mac;
use http::HeaderMap;

use crate::payload::GithubEventKind;

/// `X-Hub-Signature-256` 头名。
pub const SIGNATURE_HEADER: &str = "X-Hub-Signature-256";

/// `X-GitHub-Event` 头名。
pub const EVENT_HEADER: &str = "X-GitHub-Event";

/// 校验 `X-Hub-Signature-256`。
///
/// 逐字对齐上游 `verifyWebhookSignature` 的三条语义：
///
/// 1. 头值必须以 `sha256=` 打头（大小写**敏感**：上游 `strings.HasPrefix`）；
/// 2. 去掉前缀后必须是**合法十六进制**，否则 `false`（**不是** panic —— 上游 `hex.DecodeString`
///    的错误就是返回 `false`）；
/// 3. 比较是**常量时间**（上游 `hmac.Equal`；这里用 `hmac::Mac::verify_slice`）。
///
/// 额外一条本仓契约（anchor 钉的）：`secret` **空** ⇒ 必失败，**不得**短路成 `true`
/// （空密钥的 HMAC 是「谁都能算」的，把它当有效就是未配置部署被当成全通）。
pub fn verify_webhook_signature(secret: &str, headers: &HeaderMap, body: &[u8]) -> bool {
    if secret.is_empty() {
        return false;
    }
    let Some(header) = headers.get(SIGNATURE_HEADER) else {
        return false;
    };
    let Ok(header) = header.to_str() else {
        return false;
    };
    let Some(hex_digest) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_digest) else {
        return false;
    };
    let Ok(mut mac) = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()) else {
        // 上游对任意长度的密钥都能建 HMAC；`new_from_slice` 的失败分支在本算法下不可达
        // （`Hmac<Sha256>` 接受任意长度密钥）⇒ 保守失败而不是 unwrap。
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

/// 计算签名头（**测试与替身**用；生产只校验不生成）。
///
/// 与 [`verify_webhook_signature`] 是同一份 HMAC，所以「真实帧」的构造不需要第二套实现
/// （`docs/61` §4.2 的替身纪律第 ② 条：帧要逐字段可比）。
pub fn sign_webhook_body(secret: &str, body: &[u8]) -> String {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .expect("Hmac<Sha256> accepts any key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// 从请求头分类事件（`X-GitHub-Event` 缺席 ⇒ [`GithubEventKind::Other`]）。
pub fn event_kind_for_request(headers: &HeaderMap) -> GithubEventKind {
    headers
        .get(EVENT_HEADER)
        .and_then(|value| value.to_str().ok())
        .map_or(GithubEventKind::Other, GithubEventKind::classify)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(signature: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(SIGNATURE_HEADER, signature.parse().expect("header value"));
        headers
    }

    #[test]
    fn round_trip_with_a_real_hmac_sha256_frame() {
        let secret = "s3cr3t";
        let body = br#"{"action":"opened"}"#;
        let signature = sign_webhook_body(secret, body);
        assert!(signature.starts_with("sha256="));
        assert!(verify_webhook_signature(secret, &headers(&signature), body));
        // 换密钥 / 换 body 都不成立。
        assert!(!verify_webhook_signature(
            "other",
            &headers(&signature),
            body
        ));
        assert!(!verify_webhook_signature(
            secret,
            &headers(&signature),
            br#"{"action":"closed"}"#
        ));
    }

    #[test]
    fn empty_secret_never_validates() {
        let body = b"{}";
        // 用空密钥算出来的签名，在 secret 为空的部署里必须**不**被接受。
        let signature = sign_webhook_body("", body);
        assert!(!verify_webhook_signature("", &headers(&signature), body));
    }

    #[test]
    fn malformed_headers_return_false_instead_of_panicking() {
        let body = b"{}";
        // 头缺席。
        assert!(!verify_webhook_signature("s", &HeaderMap::new(), body));
        // 前缀不对（大小写敏感 + 缺前缀）。
        assert!(!verify_webhook_signature(
            "s",
            &headers("SHA256=abcd"),
            body
        ));
        assert!(!verify_webhook_signature("s", &headers("sha1=abcd"), body));
        // 非法十六进制 / 奇数长度 / 空。
        assert!(!verify_webhook_signature("s", &headers("sha256=zz"), body));
        assert!(!verify_webhook_signature("s", &headers("sha256=abc"), body));
        assert!(!verify_webhook_signature("s", &headers("sha256="), body));
        // 长度正确的十六进制但不是本体的签名。
        assert!(!verify_webhook_signature(
            "s",
            &headers(&format!("sha256={}", "00".repeat(32))),
            body
        ));
    }

    #[test]
    fn event_kind_reads_the_header() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            event_kind_for_request(&headers),
            GithubEventKind::Other,
            "缺头 ⇒ Other（确认后忽略）"
        );
        headers.insert(EVENT_HEADER, "pull_request".parse().expect("value"));
        assert_eq!(
            event_kind_for_request(&headers),
            GithubEventKind::PullRequest
        );
        headers.insert(EVENT_HEADER, "ping".parse().expect("value"));
        assert_eq!(event_kind_for_request(&headers), GithubEventKind::Ping);
    }
}
