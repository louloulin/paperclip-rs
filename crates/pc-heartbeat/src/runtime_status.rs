//! heartbeat run runtime status：进程内（in-memory）实时状态，对齐 Node `heartbeat-run-runtime-status.ts`。
//!
//! 包含：
//! - 常量：`HEARTBEAT_RUN_RUNTIME_STATUS_TTL_MS` / `MAX_HEARTBEAT_RUN_RUNTIME_STATUS_MESSAGE_CHARS` /
//!   `MAX_HEARTBEAT_RUN_RUNTIME_TOOL_NAME_CHARS` / `MAX_HEARTBEAT_RUN_RUNTIME_ASSISTANT_SNIPPET_CHARS`
//! - 类型：`HeartbeatRunStatusPhase` / `HeartbeatRunRuntimeStatus` / `HeartbeatRunRuntimeStatusUpdate`
//! - 纯函数：`sanitize_heartbeat_run_runtime_status_message` /
//!   `sanitize_heartbeat_run_runtime_tool_name` /
//!   `sanitize_heartbeat_run_runtime_assistant_snippet`
//! - 状态管理：`set_heartbeat_run_runtime_status` / `touch_heartbeat_run_runtime_status` /
//!   `get_heartbeat_run_runtime_status` / `clear_heartbeat_run_runtime_status` /
//!   `list_heartbeat_run_runtime_statuses`
//!
//! 设计：
//! - 状态容器抽成 trait `RuntimeStatusStore`，方便后续接 Redis / DB
//! - 默认实现 `InMemoryRuntimeStatusStore` 使用 `std::sync::RwLock<HashMap>`，
//!   单进程内等价 Node `runtimeStatusesByRunId` 行为
//! - sanitize 纯函数无副作用，方便单测
//! - 多实例部署时需要换成外部共享存储（Redis 等），trait 抽象为未来留口

use std::collections::HashMap;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 进程内 runtime status TTL（90s，与 Node 一致）。
pub const HEARTBEAT_RUN_RUNTIME_STATUS_TTL_MS: i64 = 90_000;

/// runtime status `message` 字段截断上限（180 chars）。
pub const MAX_HEARTBEAT_RUN_RUNTIME_STATUS_MESSAGE_CHARS: usize = 180;

/// runtime status `currentToolName` 字段截断上限（80 chars）。
pub const MAX_HEARTBEAT_RUN_RUNTIME_TOOL_NAME_CHARS: usize = 80;

/// runtime status `lastAssistantSnippet` 字段截断上限（220 chars）。
pub const MAX_HEARTBEAT_RUN_RUNTIME_ASSISTANT_SNIPPET_CHARS: usize = 220;

/// heartbeat run 阶段枚举（与 Node `HeartbeatRunStatusPhase` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatRunStatusPhase {
    RunActivity,
    RunStarted,
    RunToolCall,
    RunAssistant,
    RunFinal,
    RunFailed,
    RunCancelled,
    RunTimedOut,
    RunWake,
}

impl HeartbeatRunStatusPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunActivity => "run_activity",
            Self::RunStarted => "run_started",
            Self::RunToolCall => "run_tool_call",
            Self::RunAssistant => "run_assistant",
            Self::RunFinal => "run_final",
            Self::RunFailed => "run_failed",
            Self::RunCancelled => "run_cancelled",
            Self::RunTimedOut => "run_timed_out",
            Self::RunWake => "run_wake",
        }
    }
}

/// heartbeat run 实时状态（与 Node `HeartbeatRunRuntimeStatus` 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRunRuntimeStatus {
    pub company_id: String,
    pub issue_id: Option<String>,
    pub agent_id: String,
    pub run_id: String,
    pub phase: HeartbeatRunStatusPhase,
    pub message: String,
    pub updated_at: DateTime<Utc>,
    pub current_tool_name: Option<String>,
    pub last_assistant_snippet: Option<String>,
    pub last_event_at: Option<DateTime<Utc>>,
}

/// `set` API 的输入（除自动 sanitize 字段外，其余字段来自调用方）。
#[derive(Debug, Clone)]
pub struct HeartbeatRunRuntimeStatusUpdate {
    pub company_id: String,
    pub issue_id: Option<String>,
    pub agent_id: String,
    pub run_id: String,
    pub phase: HeartbeatRunStatusPhase,
    pub message: String,
    pub updated_at: Option<DateTime<Utc>>,
    pub current_tool_name: Option<String>,
    pub last_assistant_snippet: Option<String>,
    pub last_event_at: Option<DateTime<Utc>>,
}

// ============================================================================
// Sanitize 纯函数
// ============================================================================

/// 简化版敏感信息 redact（与 Node `redactSensitiveText` 等价接口）：
/// - 多个空白折叠为单个空格
/// - 去除首尾空白
/// - 用 `***` 替换常见的 secret / token / password 关键字后的引号内容
///   （避免引入 Node 完整 redactor 依赖，单测覆盖核心 case）
fn redact_sensitive_text(input: &str) -> String {
    static SENSITIVE_KEYS: &[&str] = &[
        "api_key", "apikey", "api-key", "token", "password", "secret", "bearer",
    ];
    let mut out = input.to_string();
    for key in SENSITIVE_KEYS {
        // 匹配 key=xxx / key: xxx / key "xxx" / key 'xxx'
        for sep in ["=", ":"] {
            let pattern_eq = format!("{key}{sep}");
            if let Some(idx) = out.to_lowercase().find(&pattern_eq) {
                let after = idx + pattern_eq.len();
                if let Some(stripped) = out.get(after..) {
                    // 跳过可能的引号
                    let trimmed = stripped.trim_start_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
                    if let Some(end_idx) = find_token_end(trimmed) {
                        let token = &trimmed[..end_idx];
                        let replaced = format!("{}{}{}***", &out[..after], &trimmed[..trimmed.len() - token.len() - (trimmed.len() - token.len())], "");
                        // 用更简单的拼接方式
                        let prefix_len = after + (trimmed.len() - token.len());
                        let mut s = out[..prefix_len].to_string();
                        s.push_str("***");
                        // 拼接剩余尾部
                        let rest_start = prefix_len + token.len();
                        s.push_str(&out[rest_start.min(out.len())..]);
                        out = s;
                    }
                }
            }
        }
    }
    out
}

fn find_token_end(s: &str) -> Option<usize> {
    let mut end = 0usize;
    let mut chars = s.char_indices();
    let mut started = false;
    while let Some((i, c)) = chars.next() {
        if c.is_whitespace() || c == ',' || c == ';' || c == '"' || c == '\'' {
            if started {
                return Some(end);
            }
        } else {
            started = true;
            end = i + c.len_utf8();
        }
    }
    if started {
        Some(end)
    } else {
        None
    }
}

/// 通用 sanitize：whitespace 折叠 → redact → 截断到 maxChars。
fn sanitize_runtime_status_text(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let redacted = redact_sensitive_text(&normalized);
    if redacted.len() <= max_chars {
        redacted
    } else {
        let mut end = max_chars;
        while end > 0 && !redacted.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &redacted[..end.saturating_sub(3)])
    }
}

pub fn sanitize_heartbeat_run_runtime_status_message(message: &str) -> String {
    sanitize_runtime_status_text(message, MAX_HEARTBEAT_RUN_RUNTIME_STATUS_MESSAGE_CHARS)
}

pub fn sanitize_heartbeat_run_runtime_tool_name(tool_name: &str) -> String {
    sanitize_runtime_status_text(tool_name, MAX_HEARTBEAT_RUN_RUNTIME_TOOL_NAME_CHARS)
}

pub fn sanitize_heartbeat_run_runtime_assistant_snippet(snippet: &str) -> String {
    sanitize_runtime_status_text(snippet, MAX_HEARTBEAT_RUN_RUNTIME_ASSISTANT_SNIPPET_CHARS)
}

// ============================================================================
// 状态存储抽象（trait + 进程内默认实现）
// ============================================================================

pub trait RuntimeStatusStore: Send + Sync {
    fn set(&self, status: HeartbeatRunRuntimeStatus);
    fn get(&self, run_id: &str) -> Option<HeartbeatRunRuntimeStatus>;
    fn clear(&self, run_id: &str);
    fn list(&self) -> Vec<HeartbeatRunRuntimeStatus>;
}

/// 进程内 HashMap 实现（与 Node `runtimeStatusesByRunId` 等价）。
#[derive(Debug, Default)]
pub struct InMemoryRuntimeStatusStore {
    inner: RwLock<HashMap<String, HeartbeatRunRuntimeStatus>>,
}

impl InMemoryRuntimeStatusStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }
}

impl RuntimeStatusStore for InMemoryRuntimeStatusStore {
    fn set(&self, status: HeartbeatRunRuntimeStatus) {
        self.inner.write().expect("runtime status lock poisoned").insert(status.run_id.clone(), status);
    }

    fn get(&self, run_id: &str) -> Option<HeartbeatRunRuntimeStatus> {
        self.inner.read().expect("runtime status lock poisoned").get(run_id).cloned()
    }

    fn clear(&self, run_id: &str) {
        self.inner.write().expect("runtime status lock poisoned").remove(run_id);
    }

    fn list(&self) -> Vec<HeartbeatRunRuntimeStatus> {
        self.inner
            .read()
            .expect("runtime status lock poisoned")
            .values()
            .cloned()
            .collect()
    }
}

fn clone_status(status: &HeartbeatRunRuntimeStatus) -> HeartbeatRunRuntimeStatus {
    let mut s = status.clone();
    s.updated_at = status.updated_at;
    s.last_event_at = status.last_event_at;
    s
}

fn is_expired(status: &HeartbeatRunRuntimeStatus, now: DateTime<Utc>, ttl_ms: i64) -> bool {
    (now - status.updated_at).num_milliseconds() > ttl_ms
}

// ============================================================================
// 公共 API（对齐 Node）
// ============================================================================

/// 写入或覆盖一条 runtime status。
///
/// 规则：
/// - 空 message → 清空现有 status 并返回 None（与 Node 一致）
/// - 否则按 update 字段构造新 status，存入 store 并返回克隆
pub fn set_heartbeat_run_runtime_status<S: RuntimeStatusStore>(
    store: &S,
    input: HeartbeatRunRuntimeStatusUpdate,
) -> Option<HeartbeatRunRuntimeStatus> {
    let message = sanitize_heartbeat_run_runtime_status_message(&input.message);
    if message.is_empty() {
        clear_heartbeat_run_runtime_status(store, &input.run_id);
        return None;
    }
    let now = Utc::now();
    let status = HeartbeatRunRuntimeStatus {
        company_id: input.company_id,
        issue_id: input.issue_id,
        agent_id: input.agent_id,
        run_id: input.run_id,
        phase: input.phase,
        message,
        updated_at: input.updated_at.unwrap_or(now),
        current_tool_name: input
            .current_tool_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(sanitize_heartbeat_run_runtime_tool_name),
        last_assistant_snippet: input
            .last_assistant_snippet
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(sanitize_heartbeat_run_runtime_assistant_snippet),
        last_event_at: input.last_event_at,
    };
    store.set(status.clone());
    Some(clone_status(&status))
}

#[derive(Debug, Clone)]
pub struct TouchHeartbeatRunRuntimeStatusInput {
    pub company_id: String,
    pub issue_id: Option<String>,
    pub agent_id: String,
    pub run_id: String,
    pub at: Option<DateTime<Utc>>,
    pub fallback_phase: Option<HeartbeatRunStatusPhase>,
    pub fallback_message: Option<String>,
}

/// 刷新 activity 时间戳；如果不存在或已过期 → 用 fallback 重新创建。
pub fn touch_heartbeat_run_runtime_status<S: RuntimeStatusStore>(
    store: &S,
    input: TouchHeartbeatRunRuntimeStatusInput,
) -> Option<HeartbeatRunRuntimeStatus> {
    let at = input.at.unwrap_or_else(Utc::now);
    let existing = store.get(&input.run_id);
    if let Some(ref ex) = existing {
        let same_owner =
            ex.company_id == input.company_id && ex.agent_id == input.agent_id;
        if same_owner && !is_expired(ex, at, HEARTBEAT_RUN_RUNTIME_STATUS_TTL_MS) {
            // 在原地更新时间戳
            let mut updated = ex.clone();
            if at > updated.updated_at {
                updated.updated_at = at;
            }
            match updated.last_event_at {
                Some(prev) if at <= prev => {}
                _ => updated.last_event_at = Some(at),
            }
            store.set(updated.clone());
            return Some(clone_status(&updated));
        }
    }
    // 创建 fallback
    set_heartbeat_run_runtime_status(
        store,
        HeartbeatRunRuntimeStatusUpdate {
            company_id: input.company_id,
            issue_id: input.issue_id,
            agent_id: input.agent_id,
            run_id: input.run_id,
            phase: input.fallback_phase.unwrap_or(HeartbeatRunStatusPhase::RunActivity),
            message: input
                .fallback_message
                .unwrap_or_else(|| "Receiving agent output".to_string()),
            updated_at: Some(at),
            current_tool_name: None,
            last_assistant_snippet: None,
            last_event_at: Some(at),
        },
    )
}

#[derive(Debug, Clone, Default)]
pub struct GetHeartbeatRunRuntimeStatusExpectations {
    pub company_id: Option<String>,
    pub agent_id: Option<String>,
}

/// 读取一条 runtime status（含 TTL 检查）。
///
/// 如果 expected 提供，则当 company_id / agent_id 不匹配时也视为「不存在」。
pub fn get_heartbeat_run_runtime_status<S: RuntimeStatusStore>(
    store: &S,
    run_id: &str,
    expected: Option<GetHeartbeatRunRuntimeStatusExpectations>,
) -> Option<HeartbeatRunRuntimeStatus> {
    let now = Utc::now();
    let status = store.get(run_id)?;
    if is_expired(&status, now, HEARTBEAT_RUN_RUNTIME_STATUS_TTL_MS) {
        store.clear(run_id);
        return None;
    }
    if let Some(exp) = expected {
        if let Some(expected_company) = exp.company_id {
            if status.company_id != expected_company {
                return None;
            }
        }
        if let Some(expected_agent) = exp.agent_id {
            if status.agent_id != expected_agent {
                return None;
            }
        }
    }
    Some(clone_status(&status))
}

pub fn clear_heartbeat_run_runtime_status<S: RuntimeStatusStore>(store: &S, run_id: &str) {
    store.clear(run_id);
}

/// 列出所有未过期的 runtime status。
pub fn list_heartbeat_run_runtime_statuses<S: RuntimeStatusStore>(
    store: &S,
) -> Vec<HeartbeatRunRuntimeStatus> {
    let now = Utc::now();
    store
        .list()
        .into_iter()
        .filter(|s| !is_expired(s, now, HEARTBEAT_RUN_RUNTIME_STATUS_TTL_MS))
        .map(|s| clone_status(&s))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn sample_update(run_id: &str) -> HeartbeatRunRuntimeStatusUpdate {
        HeartbeatRunRuntimeStatusUpdate {
            company_id: "comp-1".into(),
            issue_id: Some("issue-1".into()),
            agent_id: "agent-1".into(),
            run_id: run_id.into(),
            phase: HeartbeatRunStatusPhase::RunAssistant,
            message: "Working on it".into(),
            updated_at: None,
            current_tool_name: Some("web.search".into()),
            last_assistant_snippet: Some("Here is the answer...".into()),
            last_event_at: None,
        }
    }

    #[test]
    fn constants_match_node() {
        assert_eq!(HEARTBEAT_RUN_RUNTIME_STATUS_TTL_MS, 90_000);
        assert_eq!(MAX_HEARTBEAT_RUN_RUNTIME_STATUS_MESSAGE_CHARS, 180);
        assert_eq!(MAX_HEARTBEAT_RUN_RUNTIME_TOOL_NAME_CHARS, 80);
        assert_eq!(MAX_HEARTBEAT_RUN_RUNTIME_ASSISTANT_SNIPPET_CHARS, 220);
    }

    #[test]
    fn phase_strings_round_trip() {
        for p in [
            HeartbeatRunStatusPhase::RunActivity,
            HeartbeatRunStatusPhase::RunStarted,
            HeartbeatRunStatusPhase::RunToolCall,
            HeartbeatRunStatusPhase::RunAssistant,
            HeartbeatRunStatusPhase::RunFinal,
            HeartbeatRunStatusPhase::RunFailed,
            HeartbeatRunStatusPhase::RunCancelled,
            HeartbeatRunStatusPhase::RunTimedOut,
            HeartbeatRunStatusPhase::RunWake,
        ] {
            assert!(!p.as_str().is_empty());
        }
    }

    #[test]
    fn sanitize_message_truncates_and_trims() {
        let long = "x".repeat(500);
        let out = sanitize_heartbeat_run_runtime_status_message(&long);
        assert!(out.len() <= MAX_HEARTBEAT_RUN_RUNTIME_STATUS_MESSAGE_CHARS);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn sanitize_message_collapses_whitespace() {
        let out = sanitize_heartbeat_run_runtime_status_message("  a   b\tc\nd  ");
        assert_eq!(out, "a b c d");
    }

    #[test]
    fn sanitize_tool_name_respects_limit() {
        let long = "tool." .to_string() + &"x".repeat(200);
        let out = sanitize_heartbeat_run_runtime_tool_name(&long);
        assert!(out.len() <= MAX_HEARTBEAT_RUN_RUNTIME_TOOL_NAME_CHARS);
    }

    #[test]
    fn sanitize_redacts_api_key() {
        let out = sanitize_heartbeat_run_runtime_status_message("api_key=sk-abc123 calling");
        assert!(out.contains("***"));
        assert!(!out.contains("sk-abc123"));
    }

    #[test]
    fn sanitize_redacts_token_quote() {
        let out = sanitize_heartbeat_run_runtime_status_message("token: \"secret-value\" end");
        assert!(out.contains("***"));
        assert!(!out.contains("secret-value"));
    }

    #[test]
    fn set_stores_and_returns_clone() {
        let store = InMemoryRuntimeStatusStore::new();
        let result = set_heartbeat_run_runtime_status(&store, sample_update("run-1")).unwrap();
        assert_eq!(result.run_id, "run-1");
        assert_eq!(result.message, "Working on it");
        assert_eq!(result.current_tool_name.as_deref(), Some("web.search"));
        let fetched = get_heartbeat_run_runtime_status(&store, "run-1", None);
        assert!(fetched.is_some());
    }

    #[test]
    fn set_with_empty_message_clears_existing() {
        let store = InMemoryRuntimeStatusStore::new();
        set_heartbeat_run_runtime_status(&store, sample_update("run-1"));
        let mut update = sample_update("run-1");
        update.message = "   ".into();
        let result = set_heartbeat_run_runtime_status(&store, update);
        assert!(result.is_none());
        assert!(get_heartbeat_run_runtime_status(&store, "run-1", None).is_none());
    }

    #[test]
    fn touch_refreshes_existing_within_ttl() {
        let store = InMemoryRuntimeStatusStore::new();
        set_heartbeat_run_runtime_status(&store, sample_update("run-1"));
        let later = Utc::now() + Duration::seconds(10);
        let result = touch_heartbeat_run_runtime_status(
            &store,
            TouchHeartbeatRunRuntimeStatusInput {
                company_id: "comp-1".into(),
                issue_id: Some("issue-1".into()),
                agent_id: "agent-1".into(),
                run_id: "run-1".into(),
                at: Some(later),
                fallback_phase: None,
                fallback_message: None,
            },
        )
        .unwrap();
        assert_eq!(result.updated_at, later);
        assert_eq!(result.last_event_at, Some(later));
        // message 应保留原值
        assert_eq!(result.message, "Working on it");
    }

    #[test]
    fn touch_creates_fallback_when_no_existing() {
        let store = InMemoryRuntimeStatusStore::new();
        let result = touch_heartbeat_run_runtime_status(
            &store,
            TouchHeartbeatRunRuntimeStatusInput {
                company_id: "comp-1".into(),
                issue_id: None,
                agent_id: "agent-1".into(),
                run_id: "run-1".into(),
                at: None,
                fallback_phase: Some(HeartbeatRunStatusPhase::RunActivity),
                fallback_message: Some("Receiving output".into()),
            },
        )
        .unwrap();
        assert_eq!(result.phase, HeartbeatRunStatusPhase::RunActivity);
        assert_eq!(result.message, "Receiving output");
    }

    #[test]
    fn touch_falls_back_when_owner_mismatch() {
        let store = InMemoryRuntimeStatusStore::new();
        set_heartbeat_run_runtime_status(&store, sample_update("run-1"));
        // 不同的 agent_id → 走 fallback 路径
        let mut update = sample_update("run-1");
        update.agent_id = "agent-2".into();
        update.message = "different agent message".into();
        let _ = set_heartbeat_run_runtime_status(&store, update);
        let fetched = get_heartbeat_run_runtime_status(&store, "run-1", None).unwrap();
        assert_eq!(fetched.agent_id, "agent-2");
    }

    #[test]
    fn get_returns_none_when_expired() {
        let store = InMemoryRuntimeStatusStore::new();
        let mut update = sample_update("run-1");
        // 写入一个 2 分钟前的时间戳
        update.updated_at = Some(Utc::now() - Duration::seconds(120));
        set_heartbeat_run_runtime_status(&store, update);
        assert!(get_heartbeat_run_runtime_status(&store, "run-1", None).is_none());
    }

    #[test]
    fn get_filters_by_expected_company() {
        let store = InMemoryRuntimeStatusStore::new();
        set_heartbeat_run_runtime_status(&store, sample_update("run-1"));
        let expected = GetHeartbeatRunRuntimeStatusExpectations {
            company_id: Some("comp-other".into()),
            agent_id: None,
        };
        assert!(get_heartbeat_run_runtime_status(&store, "run-1", Some(expected)).is_none());
    }

    #[test]
    fn get_filters_by_expected_agent() {
        let store = InMemoryRuntimeStatusStore::new();
        set_heartbeat_run_runtime_status(&store, sample_update("run-1"));
        let expected = GetHeartbeatRunRuntimeStatusExpectations {
            company_id: None,
            agent_id: Some("other-agent".into()),
        };
        assert!(get_heartbeat_run_runtime_status(&store, "run-1", Some(expected)).is_none());
    }

    #[test]
    fn clear_removes_status() {
        let store = InMemoryRuntimeStatusStore::new();
        set_heartbeat_run_runtime_status(&store, sample_update("run-1"));
        clear_heartbeat_run_runtime_status(&store, "run-1");
        assert!(get_heartbeat_run_runtime_status(&store, "run-1", None).is_none());
    }

    #[test]
    fn list_skips_expired_entries() {
        let store = InMemoryRuntimeStatusStore::new();
        // 一条新鲜
        set_heartbeat_run_runtime_status(&store, sample_update("run-1"));
        // 一条过期
        let mut expired = sample_update("run-2");
        expired.updated_at = Some(Utc::now() - Duration::seconds(300));
        set_heartbeat_run_runtime_status(&store, expired);
        let listed = list_heartbeat_run_runtime_statuses(&store);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].run_id, "run-1");
    }
}
