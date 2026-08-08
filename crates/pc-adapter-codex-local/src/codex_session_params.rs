//! Codex `sessionParams` 组装（对齐 Node `codex-local/src/server/execute.ts`
//! L1342-1353 的 `resolvedSessionParams`）。
//!
//! sessionParams 是与 session_id 一起持久化在 server 端
//! `agent_runs.session_params` 列的元数据，下次 resume 时用来校验 session
//! 是否仍然有效（见 `codex_remote_workspace::should_resume_remote_session`）。
//!
//! # 设计范围
//!
//! 本模块是**纯组装函数**，无 I/O、无远程调用：
//! - `build_resolved_session_params(input)` → `Option<serde_json::Value>`
//!
//! 对齐 Node 语义：
//! ```ts
//! const resolvedSessionParams = resolvedSessionId ? ({
//!   sessionId: resolvedSessionId,
//!   cwd: effectiveExecutionCwd,
//!   ...(executionTargetIsRemote
//!     ? { remoteExecution: adapterExecutionTargetSessionIdentity(runtimeExecutionTarget) }
//!     : {}),
//!   ...(workspaceId ? { workspaceId } : {}),
//!   ...(workspaceRepoUrl ? { repoUrl: workspaceRepoUrl } : {}),
//!   ...(workspaceRepoRef ? { repoRef: workspaceRepoRef } : {}),
//! }) : null;
//! ```
//!
//! 空字段一律不写入（对齐 Node spread 模式）。

use serde_json::{json, Map, Value};

/// `build_resolved_session_params` 的输入参数。
#[derive(Debug, Clone, Default)]
pub struct ResolvedSessionParamsInput<'a> {
    /// 已解析的 session_id（None / 空字符串 → 返回 None）
    pub session_id: Option<&'a str>,
    /// 当前执行 cwd（effectiveExecutionCwd，本地或受管远程目录）
    pub cwd: &'a str,
    /// 执行目标是否为远程（SSH / Sandbox）
    pub execution_target_is_remote: bool,
    /// 远程执行目标身份（SSH 4 元组 / Sandbox 5 元组），
    /// 由 `adapter_execution_target_session_identity` 生成；仅远程时填入
    pub remote_execution_identity: Option<Value>,
    /// workspace_id（可选，非空才写入）
    pub workspace_id: Option<&'a str>,
    /// repo_url（可选，非空才写入）
    pub repo_url: Option<&'a str>,
    /// repo_ref（可选，非空才写入）
    pub repo_ref: Option<&'a str>,
}

/// 组装 resolved sessionParams。
///
/// `session_id` 为空 → `None`；否则返回 JSON 对象：
/// `{ sessionId, cwd, remoteExecution?, workspaceId?, repoUrl?, repoRef? }`。
/// 对齐 Node `resolvedSessionParams`（codex-local execute.ts L1342-1353）。
#[must_use]
pub fn build_resolved_session_params(input: &ResolvedSessionParamsInput<'_>) -> Option<Value> {
    let session_id = input.session_id.map(str::trim).filter(|s| !s.is_empty())?;
    let mut obj = Map::new();
    obj.insert("sessionId".to_string(), json!(session_id));
    obj.insert("cwd".to_string(), json!(input.cwd));
    if input.execution_target_is_remote {
        if let Some(identity) = input.remote_execution_identity.as_ref() {
            obj.insert("remoteExecution".to_string(), identity.clone());
        }
    }
    if let Some(v) = non_empty(input.workspace_id) {
        obj.insert("workspaceId".to_string(), json!(v));
    }
    if let Some(v) = non_empty(input.repo_url) {
        obj.insert("repoUrl".to_string(), json!(v));
    }
    if let Some(v) = non_empty(input.repo_ref) {
        obj.insert("repoRef".to_string(), json!(v));
    }
    Some(Value::Object(obj))
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// 便捷读取器：从 sessionParams JSON 中提取字段（对齐 Node `parseObject` +
/// `asString` 的常用读取）。
#[must_use]
pub fn session_params_session_id(value: &Value) -> Option<&str> {
    value.get("sessionId").and_then(Value::as_str)
}

#[must_use]
pub fn session_params_cwd(value: &Value) -> Option<&str> {
    value.get("cwd").and_then(Value::as_str)
}

#[must_use]
pub fn session_params_remote_execution(value: &Value) -> Option<&Value> {
    value.get("remoteExecution")
}

#[must_use]
pub fn session_params_workspace_id(value: &Value) -> Option<&str> {
    value.get("workspaceId").and_then(Value::as_str)
}

#[must_use]
pub fn session_params_repo_url(value: &Value) -> Option<&str> {
    value.get("repoUrl").and_then(Value::as_str)
}

#[must_use]
pub fn session_params_repo_ref(value: &Value) -> Option<&str> {
    value.get("repoRef").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SSH_IDENTITY: &str = r#"{"transport":"ssh","host":"127.0.0.1","username":"fixture","port":2222,"remoteCwd":"/remote/workspace"}"#;

    fn base_input() -> ResolvedSessionParamsInput<'static> {
        ResolvedSessionParamsInput {
            session_id: Some("session-123"),
            cwd: "/remote/workspace",
            execution_target_is_remote: true,
            remote_execution_identity: Some(serde_json::from_str(SSH_IDENTITY).unwrap()),
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
        }
    }

    #[test]
    fn build_returns_none_for_missing_session_id() {
        let input = ResolvedSessionParamsInput {
            session_id: None,
            ..base_input()
        };
        assert!(build_resolved_session_params(&input).is_none());
    }

    #[test]
    fn build_returns_none_for_blank_session_id() {
        let input = ResolvedSessionParamsInput {
            session_id: Some("   "),
            ..base_input()
        };
        assert!(build_resolved_session_params(&input).is_none());
    }

    #[test]
    fn build_remote_includes_remote_execution_identity() {
        let params = build_resolved_session_params(&base_input()).unwrap();
        assert_eq!(session_params_session_id(&params), Some("session-123"));
        assert_eq!(session_params_cwd(&params), Some("/remote/workspace"));
        let remote = session_params_remote_execution(&params).unwrap();
        assert_eq!(remote.get("host").and_then(Value::as_str), Some("127.0.0.1"));
        assert_eq!(remote.get("port").and_then(Value::as_u64), Some(2222));
    }

    #[test]
    fn build_local_omits_remote_execution() {
        let input = ResolvedSessionParamsInput {
            execution_target_is_remote: false,
            ..base_input()
        };
        let params = build_resolved_session_params(&input).unwrap();
        assert!(session_params_remote_execution(&params).is_none());
    }

    #[test]
    fn build_remote_without_identity_omits_remote_execution() {
        let input = ResolvedSessionParamsInput {
            remote_execution_identity: None,
            ..base_input()
        };
        let params = build_resolved_session_params(&input).unwrap();
        assert!(session_params_remote_execution(&params).is_none());
    }

    #[test]
    fn build_includes_workspace_fields_when_present() {
        let input = ResolvedSessionParamsInput {
            workspace_id: Some("workspace-1"),
            repo_url: Some("https://github.com/paperclipai/paperclip.git"),
            repo_ref: Some("main"),
            ..base_input()
        };
        let params = build_resolved_session_params(&input).unwrap();
        assert_eq!(session_params_workspace_id(&params), Some("workspace-1"));
        assert_eq!(
            session_params_repo_url(&params),
            Some("https://github.com/paperclipai/paperclip.git")
        );
        assert_eq!(session_params_repo_ref(&params), Some("main"));
    }

    #[test]
    fn build_omits_blank_workspace_fields() {
        let input = ResolvedSessionParamsInput {
            workspace_id: Some("   "),
            repo_url: None,
            repo_ref: Some(""),
            ..base_input()
        };
        let params = build_resolved_session_params(&input).unwrap();
        assert!(session_params_workspace_id(&params).is_none());
        assert!(session_params_repo_url(&params).is_none());
        assert!(session_params_repo_ref(&params).is_none());
    }

    #[test]
    fn build_matches_node_key_set() {
        // Node 键集合：sessionId, cwd, remoteExecution?, workspaceId?, repoUrl?, repoRef?
        // serde_json::Map 是排序 map（键序稳定），语义上与 Node 插入序等价。
        let input = ResolvedSessionParamsInput {
            workspace_id: Some("workspace-1"),
            repo_url: Some("https://github.com/paperclipai/paperclip.git"),
            repo_ref: Some("main"),
            ..base_input()
        };
        let params = build_resolved_session_params(&input).unwrap();
        let obj = params.as_object().unwrap();
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                &"cwd".to_string(),
                &"remoteExecution".to_string(),
                &"repoRef".to_string(),
                &"repoUrl".to_string(),
                &"sessionId".to_string(),
                &"workspaceId".to_string(),
            ]
        );
    }
}
