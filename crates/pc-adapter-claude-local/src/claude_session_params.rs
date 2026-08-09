#![forbid(unsafe_code)]

//! Claude `sessionParams` 组装（对齐 Node `execute.ts` L1097-1110）。
//!
//! sessionParams 是与 session_id 一起持久化在 server 端 `agent_runs.session_params` 列的元数据，
//! 下次 resume 时会用来校验 session 是否仍然有效（见 `claude_session_resume.rs`）。
//!
//! 本模块是**纯组装函数**，无 I/O、无远程调用：
//! - `build_resolved_session_params(input)` → `Option<serde_json::Value>`
//!
//! 调用方负责提供：
//! - `session_id`：解析后非空才返回 Some
//! - `cwd`、`prompt_bundle_key`、`mcp_server_identity`
//! - `execution_target_session_identity`：远程 target 时填入
//! - `workspace_id` / `repo_url` / `repo_ref`：可选字段
//!
//! 空字段一律不写入（对齐 Node `...(workspaceId ? { workspaceId } : {})` 模式）。

use serde_json::{json, Map, Value};

/// `build_resolved_session_params` 的输入参数。
#[derive(Debug, Clone, Default)]
pub struct ResolvedSessionParamsInput<'a> {
    /// 已解析的 session_id（None / 空字符串 → 返回 None）
    pub session_id: Option<&'a str>,
    /// 当前执行 cwd
    pub cwd: &'a str,
    /// 当前 prompt bundle key（content-addressed）
    pub prompt_bundle_key: &'a str,
    /// 当前 runtime MCP server 集合的 JSON identity（由 collect_runtime_mcp_identity 生成）
    pub mcp_server_identity: &'a str,
    /// 是否远程执行
    pub execution_target_is_remote: bool,
    /// 远程 target 的 session identity（远程必填，本地为 None）
    pub execution_target_session_identity: Option<&'a Value>,
    /// 可选：workspace ID
    pub workspace_id: Option<&'a str>,
    /// 可选：仓库 URL
    pub repo_url: Option<&'a str>,
    /// 可选：仓库 ref（branch/tag）
    pub repo_ref: Option<&'a str>,
}

/// 组装 `sessionParams` JSON（对齐 Node execute.ts L1097-1110）。
///
/// 返回：
/// - `None`：session_id 为空（不持久化）
/// - `Some(Value::Object)`：完整 sessionParams
///
/// 字段写入策略：
/// - 必填：`sessionId` / `cwd`
/// - 始终写入（即使为空字符串）：`promptBundleKey` / `mcpServerIdentity`
/// - 条件写入：`remoteExecution`（仅 remote）、`workspaceId`/`repoUrl`/`repoRef`（非空时）
#[must_use]
pub fn build_resolved_session_params(input: &ResolvedSessionParamsInput<'_>) -> Option<Value> {
    let session_id = input.session_id?.trim();
    if session_id.is_empty() {
        return None;
    }

    let mut map = Map::new();
    map.insert("sessionId".to_owned(), Value::String(session_id.to_owned()));
    map.insert("cwd".to_owned(), Value::String(input.cwd.to_owned()));
    map.insert(
        "promptBundleKey".to_owned(),
        Value::String(input.prompt_bundle_key.to_owned()),
    );
    map.insert(
        "mcpServerIdentity".to_owned(),
        Value::String(input.mcp_server_identity.to_owned()),
    );

    if input.execution_target_is_remote {
        if let Some(identity) = input.execution_target_session_identity {
            map.insert("remoteExecution".to_owned(), identity.clone());
        }
    }

    if let Some(workspace_id) = input.workspace_id {
        let trimmed = workspace_id.trim();
        if !trimmed.is_empty() {
            map.insert("workspaceId".to_owned(), Value::String(trimmed.to_owned()));
        }
    }

    if let Some(repo_url) = input.repo_url {
        let trimmed = repo_url.trim();
        if !trimmed.is_empty() {
            map.insert("repoUrl".to_owned(), Value::String(trimmed.to_owned()));
        }
    }

    if let Some(repo_ref) = input.repo_ref {
        let trimmed = repo_ref.trim();
        if !trimmed.is_empty() {
            map.insert("repoRef".to_owned(), Value::String(trimmed.to_owned()));
        }
    }

    Some(Value::Object(map))
}

/// 与 `AdapterExecutionResult.session_params` 配合的便利函数：
/// 当输入 session_id 为空时返回 None，否则返回上面组装好的 JSON。
///
/// 提供一个 `quick` 入口以便调用方少传几个字段（远程字段为 None、可选字段为 None）。
#[must_use]
pub fn build_quick_session_params(
    session_id: Option<&str>,
    cwd: &str,
    prompt_bundle_key: &str,
    mcp_server_identity: &str,
) -> Option<Value> {
    let input = ResolvedSessionParamsInput {
        session_id,
        cwd,
        prompt_bundle_key,
        mcp_server_identity,
        execution_target_is_remote: false,
        execution_target_session_identity: None,
        workspace_id: None,
        repo_url: None,
        repo_ref: None,
    };
    build_resolved_session_params(&input)
}

/// 把 `sessionParams` JSON 转换为 `serde_json::Map`，便于按字段读取。
#[must_use]
pub fn session_params_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// 提取 `sessionId` 字段（短路：缺失或非 string → None）。
#[must_use]
pub fn session_params_session_id(value: &Value) -> Option<&str> {
    value.get("sessionId").and_then(|v| v.as_str())
}

/// 提取 `cwd` 字段。
#[must_use]
pub fn session_params_cwd(value: &Value) -> Option<&str> {
    value.get("cwd").and_then(|v| v.as_str())
}

/// 提取 `promptBundleKey` 字段。
#[must_use]
pub fn session_params_prompt_bundle_key(value: &Value) -> Option<&str> {
    value.get("promptBundleKey").and_then(|v| v.as_str())
}

/// 提取 `mcpServerIdentity` 字段。
#[must_use]
pub fn session_params_mcp_server_identity(value: &Value) -> Option<&str> {
    value.get("mcpServerIdentity").and_then(|v| v.as_str())
}

/// 提取 `workspaceId` 字段。
#[must_use]
pub fn session_params_workspace_id(value: &Value) -> Option<&str> {
    value.get("workspaceId").and_then(|v| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_session_id_returns_none() {
        let input = ResolvedSessionParamsInput {
            session_id: Some(""),
            cwd: "/a",
            prompt_bundle_key: "bk",
            mcp_server_identity: "[]",
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
        };
        assert!(build_resolved_session_params(&input).is_none());
    }

    #[test]
    fn none_session_id_returns_none() {
        let input = ResolvedSessionParamsInput {
            session_id: None,
            cwd: "/a",
            prompt_bundle_key: "bk",
            mcp_server_identity: "[]",
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
        };
        assert!(build_resolved_session_params(&input).is_none());
    }

    #[test]
    fn whitespace_session_id_returns_none() {
        let input = ResolvedSessionParamsInput {
            session_id: Some("   "),
            cwd: "/a",
            prompt_bundle_key: "bk",
            mcp_server_identity: "[]",
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
        };
        assert!(build_resolved_session_params(&input).is_none());
    }

    #[test]
    fn minimal_session_params_only_required_fields() {
        let params =
            build_quick_session_params(Some("session-1"), "/workspace", "bundle-key", "[]")
                .expect("Some expected");
        assert_eq!(params["sessionId"], "session-1");
        assert_eq!(params["cwd"], "/workspace");
        assert_eq!(params["promptBundleKey"], "bundle-key");
        assert_eq!(params["mcpServerIdentity"], "[]");
        assert!(params.get("remoteExecution").is_none());
        assert!(params.get("workspaceId").is_none());
        assert!(params.get("repoUrl").is_none());
        assert!(params.get("repoRef").is_none());
    }

    #[test]
    fn remote_execution_added_when_remote() {
        let identity = json!({"id": "ssh-x", "port": 22});
        let input = ResolvedSessionParamsInput {
            session_id: Some("session-1"),
            cwd: "/workspace",
            prompt_bundle_key: "bk",
            mcp_server_identity: "[]",
            execution_target_is_remote: true,
            execution_target_session_identity: Some(&identity),
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
        };
        let params = build_resolved_session_params(&input).expect("Some expected");
        assert_eq!(params["remoteExecution"], identity);
    }

    #[test]
    fn remote_execution_omitted_when_remote_but_identity_none() {
        let input = ResolvedSessionParamsInput {
            session_id: Some("session-1"),
            cwd: "/workspace",
            prompt_bundle_key: "bk",
            mcp_server_identity: "[]",
            execution_target_is_remote: true,
            execution_target_session_identity: None,
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
        };
        let params = build_resolved_session_params(&input).expect("Some expected");
        assert!(params.get("remoteExecution").is_none());
    }

    #[test]
    fn remote_execution_omitted_when_not_remote() {
        let identity = json!({"id": "ssh-x", "port": 22});
        let input = ResolvedSessionParamsInput {
            session_id: Some("session-1"),
            cwd: "/workspace",
            prompt_bundle_key: "bk",
            mcp_server_identity: "[]",
            execution_target_is_remote: false,
            execution_target_session_identity: Some(&identity),
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
        };
        let params = build_resolved_session_params(&input).expect("Some expected");
        assert!(params.get("remoteExecution").is_none());
    }

    #[test]
    fn optional_fields_omitted_when_empty() {
        let input = ResolvedSessionParamsInput {
            session_id: Some("session-1"),
            cwd: "/workspace",
            prompt_bundle_key: "bk",
            mcp_server_identity: "[]",
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            workspace_id: Some(""),
            repo_url: Some("   "),
            repo_ref: Some(""),
        };
        let params = build_resolved_session_params(&input).expect("Some expected");
        assert!(params.get("workspaceId").is_none());
        assert!(params.get("repoUrl").is_none());
        assert!(params.get("repoRef").is_none());
    }

    #[test]
    fn optional_fields_trimmed() {
        let input = ResolvedSessionParamsInput {
            session_id: Some("session-1"),
            cwd: "/workspace",
            prompt_bundle_key: "bk",
            mcp_server_identity: "[]",
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            workspace_id: Some("  ws-1  "),
            repo_url: Some("  git@github.com:foo/bar.git  "),
            repo_ref: Some(" main "),
        };
        let params = build_resolved_session_params(&input).expect("Some expected");
        assert_eq!(params["workspaceId"], "ws-1");
        assert_eq!(params["repoUrl"], "git@github.com:foo/bar.git");
        assert_eq!(params["repoRef"], "main");
    }

    #[test]
    fn full_session_params_local() {
        let input = ResolvedSessionParamsInput {
            session_id: Some("550e8400-e29b-41d4-a716-446655440000"),
            cwd: "/repo",
            prompt_bundle_key: "bundle-a",
            mcp_server_identity: "[{\"name\":\"a\"}]",
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            workspace_id: Some("ws-1"),
            repo_url: Some("git@github.com:foo/bar.git"),
            repo_ref: Some("main"),
        };
        let params = build_resolved_session_params(&input).expect("Some expected");
        let obj = session_params_object(&params).expect("object");
        assert_eq!(obj.len(), 7);
        assert_eq!(params["sessionId"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params["cwd"], "/repo");
        assert_eq!(params["promptBundleKey"], "bundle-a");
        assert_eq!(params["mcpServerIdentity"], "[{\"name\":\"a\"}]");
        assert_eq!(params["workspaceId"], "ws-1");
        assert_eq!(params["repoUrl"], "git@github.com:foo/bar.git");
        assert_eq!(params["repoRef"], "main");
    }

    #[test]
    fn full_session_params_remote() {
        let identity = json!({
            "kind": "remote",
            "transport": "ssh",
            "host": "example.com",
            "port": 22,
        });
        let input = ResolvedSessionParamsInput {
            session_id: Some("session-2"),
            cwd: "/remote/repo",
            prompt_bundle_key: "bundle-z",
            mcp_server_identity: "[]",
            execution_target_is_remote: true,
            execution_target_session_identity: Some(&identity),
            workspace_id: Some("ws-2"),
            repo_url: Some("git@github.com:foo/bar.git"),
            repo_ref: Some("develop"),
        };
        let params = build_resolved_session_params(&input).expect("Some expected");
        assert_eq!(params["remoteExecution"], identity);
        assert_eq!(params["workspaceId"], "ws-2");
        assert_eq!(params["repoRef"], "develop");
    }

    #[test]
    fn accessor_helpers_return_fields() {
        let params = build_quick_session_params(Some("session-x"), "/x", "bk-x", "[{\"n\":\"a\"}]")
            .expect("Some expected");
        assert_eq!(session_params_session_id(&params), Some("session-x"));
        assert_eq!(session_params_cwd(&params), Some("/x"));
        assert_eq!(session_params_prompt_bundle_key(&params), Some("bk-x"));
        assert_eq!(
            session_params_mcp_server_identity(&params),
            Some("[{\"n\":\"a\"}]")
        );
        assert_eq!(session_params_workspace_id(&params), None);
    }
}
