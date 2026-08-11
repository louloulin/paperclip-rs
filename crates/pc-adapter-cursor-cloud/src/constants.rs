//! Cursor Cloud adapter constants（对齐 Node
//! `packages/adapters/cursor-cloud/src/server/index.ts` 与 UI/schema）。
//!
//! 与本地 CLI adapter 不同：Cursor Cloud 走的是 `@cursor/sdk` HTTP API，
//! 所以本 adapter 暴露的是云端 session/agent/run 抽象而非本地 spawn 行为。

#![allow(dead_code)]

/// Adapter 类型标识（Paperclip `adapterType` 字段使用）。
pub const ADAPTER_TYPE: &str = "cursor_cloud";

/// Paperclip UI 标签。
pub const ADAPTER_LABEL: &str = "Cursor Cloud";

/// Provider 名（用于 `AdapterExecutionResult.provider`）。
pub const PROVIDER: &str = "cursor";

/// Biller 名。
pub const BILLER: &str = "cursor";

/// Billing type（永远 `api`，因为是云端订阅/billing）。
pub const BILLING_TYPE: &str = "api";

/// Cursor Cloud runtime env types（合法值集合）。
///
/// 取自 Node `normalizeEnvType` — 非 `pool` / `machine` 一律视为 `cloud`。
pub const RUNTIME_ENV_TYPES: &[&str] = &["cloud", "pool", "machine"];

/// 默认 runtime env type（**`cloud`** 是 SDK 的最小可行选项）。
pub const DEFAULT_RUNTIME_ENV_TYPE: &str = "cloud";

/// 默认 `workOnCurrentBranch` — 关闭避免误 push。
pub const DEFAULT_WORK_ON_CURRENT_BRANCH: bool = false;

/// 默认 `autoCreatePR` — 关闭，让上游决定。
pub const DEFAULT_AUTO_CREATE_PR: bool = false;

/// 默认 `skipReviewerRequest` — 关闭。
pub const DEFAULT_SKIP_REVIEWER_REQUEST: bool = false;

/// 需要从 config 里读取以驱动的必填字段（缺失时 `execute` 抛 clearable error）。
pub const REQUIRED_CONFIG_FIELDS: &[&str] = &["CURSOR_API_KEY", "repoUrl"];

/// `[env]` 配置 map 的 key（前缀 = paperclip env）。
pub const PAPERCLIP_ENV_PREFIX: &str = "PAPERCLIP_";

/// 强制清除的 key：它必须来自环境变量或 secret bindings，不允许用户写在 config 里。
pub const FORBIDDEN_CONFIG_KEYS: &[&str] = &["PAPERCLIP_API_KEY"];

/// Cloud agent 名称模板（用于 `AgentOptions.name`）。
pub const CLOUD_AGENT_NAME_PREFIX: &str = "Paperclip ";

/// 默认 request timeout (秒) — Cursor Cloud 长时间运行（5 分钟典型）。
pub const DEFAULT_REQUEST_TIMEOUT_SEC: u64 = 1800;
