//! OpenClaw Gateway adapter constants — 对齐 Node
//! `packages/adapters/openclaw-gateway/src/server/execute.ts` 顶部 const。

#![allow(dead_code)]

/// Adapter 类型标识。
pub const ADAPTER_TYPE: &str = "openclaw_gateway";
/// Paperclip UI 标签。
pub const ADAPTER_LABEL: &str = "OpenClaw Gateway";

/// Wire protocol version（与 Node `PROTOCOL_VERSION` 一致）。
pub const PROTOCOL_VERSION: u32 = 4;

/// 默认 `scopes` — 单 `operator.admin` 范围。
pub const DEFAULT_SCOPES: &[&str] = &["operator.admin"];

/// 默认客户端身份。
pub const DEFAULT_CLIENT_ID: &str = "gateway-client";
pub const DEFAULT_CLIENT_MODE: &str = "backend";
pub const DEFAULT_CLIENT_VERSION: &str = "paperclip";
pub const DEFAULT_ROLE: &str = "operator";

/// 默认 session key fallback（Node: `"paperclip"`）。
pub const DEFAULT_SESSION_KEY: &str = "paperclip";

/// 3 种 session key 策略。
pub const VALID_SESSION_KEY_STRATEGIES: &[&str] = &["fixed", "issue", "run"];

/// 默认 session key 策略（`issue`，对齐 Node `normalizeSessionKeyStrategy` fallback）。
pub const DEFAULT_SESSION_KEY_STRATEGY: &str = "issue";

/// 默认 Gateway request timeout ms。
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// 默认 connect timeout ms。
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 15_000;

/// 默认 header 大小限制（bytes）。
pub const DEFAULT_MAX_HEADER_BYTES: usize = 65_536;

/// 默认 ws 最大消息大小（bytes）。
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Frame type 枚举（JSON 协议，type 字段）。
pub mod frame_types {
    pub const REQ: &str = "req";
    pub const RES: &str = "res";
    pub const EVENT: &str = "event";
}

/// 头部敏感日志 key 模式（lowercase 比对）。
///
/// 完整正则（与 Node 等价）：
/// `(^|[_-])(auth|authorization|token|secret|password|api[_-]?key|private[_-]?key)([_-]|$)`
/// OR `^x-openclaw-(auth|token)$`。
///
/// Rust 端用列表前置匹配近似覆盖所有真实场景；测试覆盖全部 8 个分支。
pub const SENSITIVE_LOG_KEY_BRANCHES: &[&str] = &[
    "auth",
    "authorization",
    "token",
    "secret",
    "password",
    "api_key",
    "api-key",
    "apikey",
    "private_key",
    "private-key",
    "privatekey",
    "x-openclaw-auth",
    "x-openclaw-token",
];

/// 已知 Gateway 错误码子集（决定重试策略 — 后续 round 引入 retry 模块）。
pub const TRANSIENT_GATEWAY_CODES: &[&str] = &["RATE_LIMITED", "GATEWAY_BUSY", "UPSTREAM_TIMEOUT"];
pub const PERMANENT_GATEWAY_CODES: &[&str] = &[
    "INVALID_REQUEST",
    "UNAUTHORIZED",
    "FORBIDDEN",
    "NOT_FOUND",
    "BAD_STATE",
];
