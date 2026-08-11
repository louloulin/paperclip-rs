//! Cursor Cloud session codec — 把 `runtime.sessionParams` (JSON)
//! 与内部 `CursorCloudSession` 结构互转。
//!
//! 对齐 Node `packages/adapters/cursor-cloud/src/server/session.ts`：
//! - deserialize: 三种 key 别名（cursorAgentId / agentId / sessionId），
//!   缺一即丢弃
//! - serialize: 跳过空字段，避免无意义字段写入 session
//! - getDisplayId: cursorAgentId 字符串化
//!
//! 严格无副作用（pure functions）。

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Cursor Cloud runtime env 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEnvType {
    Cloud,
    Pool,
    Machine,
}

impl Default for RuntimeEnvType {
    fn default() -> Self {
        RuntimeEnvType::Cloud
    }
}

impl RuntimeEnvType {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeEnvType::Cloud => "cloud",
            RuntimeEnvType::Pool => "pool",
            RuntimeEnvType::Machine => "machine",
        }
    }

    /// 归一化（对齐 Node `normalizeEnvType`）：trim + lowercase，
    /// 只有 `pool` / `machine` 接受，其他全部回落到 `cloud`。
    pub fn from_loose(raw: &str) -> Self {
        let normalized = raw.trim().to_lowercase();
        match normalized.as_str() {
            "pool" => RuntimeEnvType::Pool,
            "machine" => RuntimeEnvType::Machine,
            _ => RuntimeEnvType::Cloud,
        }
    }

    /// 从任意 `serde_json::Value` 读取（trim/upper-aware）。
    pub fn from_value(v: Option<&Value>) -> Self {
        v.and_then(|x| x.as_str())
            .map(Self::from_loose)
            .unwrap_or_default()
    }
}

/// Cursor Cloud repo 引用（与 SDK `AgentOptions.cloud.repos[]` 元素对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorCloudRepo {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub starting_ref: Option<String>,
    #[serde(rename = "prUrl", skip_serializing_if = "Option::is_none", default)]
    pub pr_url: Option<String>,
}

/// Cursor Cloud session (持久化 `runtime.sessionParams` 结构)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorCloudSession {
    pub cursor_agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub latest_run_id: Option<String>,
    pub runtime: &'static str, // always "cloud"
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub env_type: Option<RuntimeEnvType>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub env_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub repos: Vec<CursorCloudRepo>,
}

impl CursorCloudSession {
    /// 构造最小 session（仅有 cursorAgentId 与 runtime）。
    pub fn new_minimal(cursor_agent_id: impl Into<String>) -> Self {
        Self {
            cursor_agent_id: cursor_agent_id.into(),
            latest_run_id: None,
            runtime: "cloud",
            env_type: None,
            env_name: None,
            repos: Vec::new(),
        }
    }
}

// ─── Pure helpers ────────────────────────────────────────────────────

fn read_trimmed_string(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn read_repos(v: Option<&Value>) -> Vec<CursorCloudRepo> {
    let arr = match v.and_then(|x| x.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|entry| {
            let obj = entry.as_object()?;
            let url = read_trimmed_string(obj.get("url"))?;
            let starting_ref = read_trimmed_string(obj.get("startingRef"))
                .or_else(|| read_trimmed_string(obj.get("starting_ref")));
            let pr_url = read_trimmed_string(obj.get("prUrl"))
                .or_else(|| read_trimmed_string(obj.get("pr_url")));
            Some(CursorCloudRepo {
                url,
                starting_ref,
                pr_url,
            })
        })
        .collect()
}

/// Deserialize `runtime.sessionParams` → `CursorCloudSession`。
///
/// 三种 id 别名：`cursorAgentId` > `agentId` > `sessionId`，命中其一即取。
/// `latestRunId` > `runId`。
/// `runtime` 默认为 `"cloud"`。`envType` 经 `RuntimeEnvType::from_loose` 归一化。
pub fn deserialize_session(value: &Value) -> Option<CursorCloudSession> {
    let obj = value.as_object()?;
    let cursor_agent_id = read_trimmed_string(obj.get("cursorAgentId"))
        .or_else(|| read_trimmed_string(obj.get("agentId")))
        .or_else(|| read_trimmed_string(obj.get("sessionId")))?;
    let latest_run_id = read_trimmed_string(obj.get("latestRunId"))
        .or_else(|| read_trimmed_string(obj.get("runId")));
    let env_type = obj
        .get("envType")
        .or_else(|| obj.get("env_type"))
        .map(|_| RuntimeEnvType::from_value(obj.get("envType")));
    let env_name = read_trimmed_string(obj.get("envName"))
        .or_else(|| read_trimmed_string(obj.get("env_name")));
    let repos = read_repos(obj.get("repos"));
    Some(CursorCloudSession {
        cursor_agent_id,
        latest_run_id,
        runtime: "cloud",
        env_type,
        env_name,
        repos,
    })
}

/// Serialize `CursorCloudSession` → `runtime.sessionParams`（JSON Value）。
///
/// 与 Node `normalize` 一致：所有空字段跳过，避免无意义写入。
pub fn serialize_session(session: &CursorCloudSession) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("cursorAgentId".into(), json!(session.cursor_agent_id));
    if let Some(rid) = &session.latest_run_id {
        obj.insert("latestRunId".into(), json!(rid));
    }
    obj.insert("runtime".into(), json!(session.runtime));
    if let Some(et) = session.env_type {
        obj.insert("envType".into(), json!(et.as_str()));
    }
    if let Some(name) = &session.env_name {
        obj.insert("envName".into(), json!(name));
    }
    if !session.repos.is_empty() {
        let arr: Vec<Value> = session
            .repos
            .iter()
            .map(|r| {
                let mut o = serde_json::Map::new();
                o.insert("url".into(), json!(r.url));
                if let Some(sr) = &r.starting_ref {
                    o.insert("startingRef".into(), json!(sr));
                }
                if let Some(pr) = &r.pr_url {
                    o.insert("prUrl".into(), json!(pr));
                }
                Value::Object(o)
            })
            .collect();
        obj.insert("repos".into(), Value::Array(arr));
    }
    Value::Object(obj)
}

/// 显示用 ID（Paperclip `sessionDisplayId` 字段）。
pub fn display_id(session: &CursorCloudSession) -> String {
    session.cursor_agent_id.clone()
}

/// `sessionMatches` — 判断两个 session 是否“指向同一运行上下文”。
///
/// 对齐 Node `sessionMatches`：
/// - session 必须存在
/// - envType 必须相等（缺失视为 cloud）
/// - envName 必须相等（双向 null 也算）
/// - repo 列表必须 1:1 等价（url / startingRef / prUrl 三元组）
pub fn session_matches(
    session: Option<&CursorCloudSession>,
    target_env_type: RuntimeEnvType,
    target_env_name: Option<&str>,
    target_repos: &[CursorCloudRepo],
) -> bool {
    let session = match session {
        Some(s) => s,
        None => return false,
    };
    let session_env_type = session.env_type.unwrap_or_default();
    if session_env_type != target_env_type {
        return false;
    }
    let session_env_name = session.env_name.as_deref();
    if session_env_name != target_env_name {
        return false;
    }
    if session.repos.len() != target_repos.len() {
        return false;
    }
    session
        .repos
        .iter()
        .zip(target_repos.iter())
        .all(|(a, b)| a.url == b.url && a.starting_ref == b.starting_ref && a.pr_url == b.pr_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_env_type_from_loose_normalizes_unknown_to_cloud() {
        assert_eq!(RuntimeEnvType::from_loose("POOL"), RuntimeEnvType::Pool);
        assert_eq!(
            RuntimeEnvType::from_loose("Machine"),
            RuntimeEnvType::Machine
        );
        assert_eq!(
            RuntimeEnvType::from_loose("  cLoUd  "),
            RuntimeEnvType::Cloud
        );
        assert_eq!(RuntimeEnvType::from_loose(""), RuntimeEnvType::Cloud);
        assert_eq!(RuntimeEnvType::from_loose("garbage"), RuntimeEnvType::Cloud);
    }

    #[test]
    fn deserialize_session_prefers_cursor_agent_id_alias() {
        let v = json!({"cursorAgentId": "cu-123"});
        let s = deserialize_session(&v).unwrap();
        assert_eq!(s.cursor_agent_id, "cu-123");
        assert_eq!(s.runtime, "cloud");
    }

    #[test]
    fn deserialize_session_falls_back_to_session_id() {
        let v = json!({"sessionId": "  cu-fallback  "});
        let s = deserialize_session(&v).unwrap();
        assert_eq!(s.cursor_agent_id, "cu-fallback");
    }

    #[test]
    fn deserialize_session_returns_none_without_any_id() {
        let v = json!({"envType": "pool"});
        assert!(deserialize_session(&v).is_none());
    }

    #[test]
    fn deserialize_session_captures_latest_run_id() {
        let v = json!({"cursorAgentId": "cu-1", "runId": "r-9"});
        let s = deserialize_session(&v).unwrap();
        assert_eq!(s.latest_run_id.as_deref(), Some("r-9"));
    }

    #[test]
    fn deserialize_session_normalizes_env_type() {
        let v = json!({"cursorAgentId": "cu-1", "envType": "Pool"});
        let s = deserialize_session(&v).unwrap();
        assert_eq!(s.env_type, Some(RuntimeEnvType::Pool));
    }

    #[test]
    fn deserialize_session_reads_repos_with_alternate_keys() {
        let v = json!({
            "cursorAgentId": "cu-1",
            "repos": [
                {"url": "https://github.com/foo/bar", "startingRef": "main", "prUrl": "https://github.com/foo/bar/pull/1"},
                {"url": "  https://github.com/x/y  "}
            ]
        });
        let s = deserialize_session(&v).unwrap();
        assert_eq!(s.repos.len(), 2);
        assert_eq!(s.repos[0].url, "https://github.com/foo/bar");
        assert_eq!(s.repos[0].starting_ref.as_deref(), Some("main"));
        assert_eq!(
            s.repos[0].pr_url.as_deref(),
            Some("https://github.com/foo/bar/pull/1")
        );
        assert_eq!(s.repos[1].url, "https://github.com/x/y");
        assert!(s.repos[1].starting_ref.is_none());
    }

    #[test]
    fn deserialize_session_drops_repos_without_url() {
        let v = json!({
            "cursorAgentId": "cu-1",
            "repos": [
                {"startingRef": "main"},
                {"url": "https://github.com/a/b"}
            ]
        });
        let s = deserialize_session(&v).unwrap();
        assert_eq!(s.repos.len(), 1);
    }

    #[test]
    fn deserialize_session_handles_null_value() {
        assert!(deserialize_session(&Value::Null).is_none());
    }

    #[test]
    fn serialize_session_skips_empty_fields() {
        let s = CursorCloudSession::new_minimal("cu-99");
        let out = serialize_session(&s);
        assert_eq!(out["cursorAgentId"], json!("cu-99"));
        assert_eq!(out["runtime"], json!("cloud"));
        assert!(out.get("latestRunId").is_none());
        assert!(out.get("envType").is_none());
        assert!(out.get("envName").is_none());
        assert!(out.get("repos").is_none());
    }

    #[test]
    fn serialize_session_round_trips_through_deserialize() {
        let original = CursorCloudSession {
            cursor_agent_id: "cu-1".to_owned(),
            latest_run_id: Some("r-1".to_owned()),
            runtime: "cloud",
            env_type: Some(RuntimeEnvType::Pool),
            env_name: Some("env-name".to_owned()),
            repos: vec![CursorCloudRepo {
                url: "https://github.com/a/b".to_owned(),
                starting_ref: Some("main".to_owned()),
                pr_url: Some("https://github.com/a/b/pull/9".to_owned()),
            }],
        };
        let v = serialize_session(&original);
        let parsed = deserialize_session(&v).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn display_id_returns_cursor_agent_id_string() {
        let s = CursorCloudSession::new_minimal("cu-42");
        assert_eq!(display_id(&s), "cu-42");
    }

    #[test]
    fn session_matches_true_when_all_fields_equal() {
        let s = CursorCloudSession {
            cursor_agent_id: "cu-1".to_owned(),
            latest_run_id: None,
            runtime: "cloud",
            env_type: Some(RuntimeEnvType::Pool),
            env_name: Some("env-a".to_owned()),
            repos: vec![CursorCloudRepo {
                url: "https://github.com/a/b".to_owned(),
                starting_ref: Some("main".to_owned()),
                pr_url: None,
            }],
        };
        let target_repos = vec![CursorCloudRepo {
            url: "https://github.com/a/b".to_owned(),
            starting_ref: Some("main".to_owned()),
            pr_url: None,
        }];
        assert!(session_matches(
            Some(&s),
            RuntimeEnvType::Pool,
            Some("env-a"),
            &target_repos
        ));
    }

    #[test]
    fn session_matches_false_on_env_type_change() {
        let s = CursorCloudSession::new_minimal("cu-1");
        assert!(!session_matches(
            Some(&s),
            RuntimeEnvType::Pool,
            None,
            &[] as &[CursorCloudRepo]
        ));
    }

    #[test]
    fn session_matches_false_on_repo_count_mismatch() {
        let s = CursorCloudSession {
            cursor_agent_id: "cu-1".to_owned(),
            latest_run_id: None,
            runtime: "cloud",
            env_type: Some(RuntimeEnvType::Cloud),
            env_name: None,
            repos: vec![CursorCloudRepo {
                url: "https://github.com/a/b".to_owned(),
                starting_ref: None,
                pr_url: None,
            }],
        };
        let targets: Vec<CursorCloudRepo> = vec![];
        assert!(!session_matches(
            Some(&s),
            RuntimeEnvType::Cloud,
            None,
            &targets
        ));
    }

    #[test]
    fn session_matches_false_on_null_session() {
        assert!(!session_matches(
            None,
            RuntimeEnvType::Cloud,
            None,
            &[] as &[CursorCloudRepo]
        ));
    }
}
