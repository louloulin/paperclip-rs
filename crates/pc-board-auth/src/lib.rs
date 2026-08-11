#![forbid(unsafe_code)]
//! `pc-board-auth` —— board auth service（对应 Node `services/board-auth.ts`）。
//!
//! 设计目标：
//!
//! - 1:1 对齐 Node 语义：常量、token 格式、status 推导、approval 流程。
//! - 通过 `BoardAuthService` 暴露所有公开 API；内部使用 pc-repos 的
//!   `BoardKeyRepo`、`ChallengeRepo`、`InstanceUserRoleRepo`、
//!   `CompanyRepo`、`CompanyMemberRepo`、`UserProfileRepo`。
//! - Token 哈希：SHA-256 hex（Node `createHash("sha256")`）；用 `subtle::ConstantTimeEq`
//!   风格的等长比较防止 timing attack（我们用 Rust 自带的 `subtle` crate 等价物，
//!   这里直接比较字节切片，先比长度再用固定时间常量比较）。
//!
//! 公共 API：
//!
//! - 常量 [`BOARD_API_KEY_TTL_MS`] / [`CLI_AUTH_CHALLENGE_TTL_MS`]
//! - 工具 [`hash_bearer_token`] / [`token_hashes_match`]
//! - 工厂 [`create_board_api_token`] / [`create_cli_auth_secret`]
//! - 时间工具 [`board_api_key_expires_at`] / [`cli_auth_challenge_expires_at`]
//! - 状态推导 [`challenge_status_for_row`]
//! - 服务 [`BoardAuthService`] + 工厂 [`board_auth_service`]

pub mod chat;
pub mod service;
pub mod types;

pub use service::{board_auth_service, BoardAuthService, Clock, SystemClock};
pub use types::{
    BoardAccess, BoardApiKeyCreated, BoardApiKeyListItem, BoardAuthServiceError,
    BoardAuthServiceResult, BoardMembership, BoardUserSummary, ChallengeStatus,
    CliAuthChallengeCreated, CliAuthChallengeDescription, CliAuthChallengeRow,
    CliAuthChallengeStatus, CliRequestedAccess,
};

/// Board API key 的 TTL（毫秒）—— 与 Node `BOARD_API_KEY_TTL_MS` 一致。
pub const BOARD_API_KEY_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// CLI auth challenge 的 TTL（毫秒）—— 与 Node `CLI_AUTH_CHALLENGE_TTL_MS` 一致。
pub const CLI_AUTH_CHALLENGE_TTL_MS: i64 = 10 * 60 * 1000;

/// SHA-256 hex hash of a bearer token. 与 Node `createHash("sha256").update(token).digest("hex")` 1:1 对齐。
pub fn hash_bearer_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let bytes = hasher.finalize();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

/// 比较两个 hex-encoded token hash 是否相等（固定时间）。与 Node `tokenHashesMatch` 1:1 对齐。
///
/// 先比较长度（避免不必要的 hex decode），再用恒定时间逐字节比较 hex 字符。
pub fn token_hashes_match(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let lb = left.as_bytes();
    let rb = right.as_bytes();
    let mut diff: u8 = 0;
    for i in 0..lb.len() {
        diff |= lb[i] ^ rb[i];
    }
    diff == 0
}

/// 生成 board API token 明文（仅创建时返回一次）。格式：`pcp_board_<48-hex-chars>`。
pub fn create_board_api_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 24];
    // 用 `getrandom` 的 std 接口（在 wasm 上也可用）；这里用 `rand` 的兼容性 trait。
    // 为了纯函数化与可测性，我们用 `uuid::Uuid::new_v4()` 的字节扩展是 *不够* 安全的，
    // 所以用 `rand::thread_rng().fill_bytes`。但要避免引入 rand 依赖，我们改用
    // 一个简单的 thread-local PRNG（XorShift）作为 fallback，或者直接使用
    // `getrandom::getrandom`。这里引入 rand 以保持与 Node `randomBytes(24)` 等价。
    let mut rng = rand::thread_rng();
    rng.fill_bytes(&mut buf);
    format!("pcp_board_{}", hex_encode(&buf))
}

/// 生成 CLI auth secret 明文。格式：`pcp_cli_auth_<48-hex-chars>`。
pub fn create_cli_auth_secret() -> String {
    let mut buf = [0u8; 24];
    let mut rng = rand::thread_rng();
    rand::RngCore::fill_bytes(&mut rng, &mut buf);
    format!("pcp_cli_auth_{}", hex_encode(&buf))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

/// 计算 board api key 的过期时间（默认 30 天后）。
pub fn board_api_key_expires_at(now_ms: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now_ms + BOARD_API_KEY_TTL_MS)
        .unwrap_or_else(chrono::Utc::now)
}

/// 计算 CLI auth challenge 的过期时间（默认 10 分钟后）。
pub fn cli_auth_challenge_expires_at(now_ms: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now_ms + CLI_AUTH_CHALLENGE_TTL_MS)
        .unwrap_or_else(chrono::Utc::now)
}

/// 给定 challenge 行与当前时间（毫秒），推导状态。
///
/// 与 Node `challengeStatusForRow` 1:1 对齐：
/// - `cancelled_at` 非空 → `Cancelled`
/// - `expires_at <= now` → `Expired`
/// - `approved_at && board_api_key_id` 都非空 → `Approved`
/// - 否则 `Pending`
pub fn challenge_status_for_row(row: &CliAuthChallengeRow, now_ms: i64) -> ChallengeStatus {
    if row.cancelled_at.is_some() {
        return ChallengeStatus::Cancelled;
    }
    if row.expires_at.timestamp_millis() <= now_ms {
        return ChallengeStatus::Expired;
    }
    if row.approved_at.is_some() && row.board_api_key_id.is_some() {
        return ChallengeStatus::Approved;
    }
    ChallengeStatus::Pending
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r687_board_api_key_ttl_constant() {
        assert_eq!(BOARD_API_KEY_TTL_MS, 30 * 24 * 60 * 60 * 1000);
    }

    #[test]
    fn r687_cli_challenge_ttl_constant() {
        assert_eq!(CLI_AUTH_CHALLENGE_TTL_MS, 10 * 60 * 1000);
    }

    #[test]
    fn r687_hash_bearer_token_known_vector() {
        // SHA-256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        let h = hash_bearer_token("hello");
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn r687_hash_bearer_token_distinct_inputs() {
        let a = hash_bearer_token("a");
        let b = hash_bearer_token("b");
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(b.len(), 64);
    }

    #[test]
    fn r687_token_hashes_match_identical() {
        let a = hash_bearer_token("token-1");
        let b = hash_bearer_token("token-1");
        assert!(token_hashes_match(&a, &b));
    }

    #[test]
    fn r687_token_hashes_match_different_lengths() {
        let a = "abcdef";
        let b = "abcdef0";
        assert!(!token_hashes_match(a, b));
    }

    #[test]
    fn r687_token_hashes_match_one_char_diff() {
        let a = "abcdef";
        let b = "abcdeF";
        assert!(!token_hashes_match(a, b));
    }

    #[test]
    fn r687_create_board_api_token_prefix_and_length() {
        let t = create_board_api_token();
        assert!(t.starts_with("pcp_board_"));
        assert_eq!(t.len(), "pcp_board_".len() + 48);
    }

    #[test]
    fn r687_create_cli_auth_secret_prefix_and_length() {
        let t = create_cli_auth_secret();
        assert!(t.starts_with("pcp_cli_auth_"));
        assert_eq!(t.len(), "pcp_cli_auth_".len() + 48);
    }

    #[test]
    fn r687_tokens_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            seen.insert(create_board_api_token());
            seen.insert(create_cli_auth_secret());
        }
        assert_eq!(seen.len(), 100);
    }

    #[test]
    fn r687_board_api_key_expires_at_offset() {
        let now = 1_700_000_000_000_i64;
        let exp = board_api_key_expires_at(now);
        assert_eq!(exp.timestamp_millis(), now + BOARD_API_KEY_TTL_MS);
    }

    #[test]
    fn r687_cli_challenge_expires_at_offset() {
        let now = 1_700_000_000_000_i64;
        let exp = cli_auth_challenge_expires_at(now);
        assert_eq!(exp.timestamp_millis(), now + CLI_AUTH_CHALLENGE_TTL_MS);
    }

    fn fake_row(
        cancelled: bool,
        approved_at: Option<i64>,
        expires_at: i64,
        board_key_id: Option<uuid::Uuid>,
    ) -> CliAuthChallengeRow {
        CliAuthChallengeRow {
            id: uuid::Uuid::new_v4(),
            secret_hash: "x".into(),
            command: "c".into(),
            client_name: None,
            requested_access: "board".into(),
            requested_company_id: None,
            pending_key_hash: "y".into(),
            pending_key_name: "z".into(),
            approved_by_user_id: if approved_at.is_some() {
                Some("u".into())
            } else {
                None
            },
            approved_at: approved_at.map(|ms| {
                chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).unwrap()
            }),
            cancelled_at: if cancelled {
                Some(chrono::Utc::now())
            } else {
                None
            },
            expires_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(expires_at)
                .unwrap(),
            created_at: chrono::Utc::now(),
            board_api_key_id: board_key_id,
        }
    }

    #[test]
    fn r687_challenge_status_cancelled_wins() {
        let r = fake_row(true, None, 1_000_000_000_000, None);
        assert_eq!(
            challenge_status_for_row(&r, 1_000_000_000_000),
            ChallengeStatus::Cancelled
        );
    }

    #[test]
    fn r687_challenge_status_expired() {
        // expires_at = 1000, now = 2000 → expired
        let r = fake_row(false, None, 1_000, None);
        assert_eq!(challenge_status_for_row(&r, 2_000), ChallengeStatus::Expired);
    }

    #[test]
    fn r687_challenge_status_approved_requires_both() {
        // 只有 approved_at 没有 board_api_key_id → 仍为 Pending
        let r = fake_row(false, Some(500), 1_000, None);
        assert_eq!(challenge_status_for_row(&r, 0), ChallengeStatus::Pending);
        // 两者都有 → Approved
        let r2 = fake_row(false, Some(500), 1_000, Some(uuid::Uuid::new_v4()));
        assert_eq!(challenge_status_for_row(&r2, 0), ChallengeStatus::Approved);
    }

    #[test]
    fn r687_challenge_status_pending() {
        let r = fake_row(false, None, 1_000_000, None);
        assert_eq!(
            challenge_status_for_row(&r, 0),
            ChallengeStatus::Pending
        );
    }

    #[test]
    fn r687_challenge_status_expired_boundary() {
        // expires_at == now → expired (Node 用 `<=`)
        let r = fake_row(false, None, 1_000, None);
        assert_eq!(challenge_status_for_row(&r, 1_000), ChallengeStatus::Expired);
    }
}
