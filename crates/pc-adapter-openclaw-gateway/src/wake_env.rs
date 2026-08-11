//! OpenClaw Gateway wake env builder — 对齐 Node
//! `execute.ts::buildPaperclipEnv` + wake payload 注入。
//!
//! 5 层优先级（高 → 低），与 cursor-cloud::wake_env 行为一致：
//! 1. harness `authToken` → `PAPERCLIP_API_KEY`
//! 2. config.env 中的 `OPENCLAW_API_KEY` / `CURSOR_*` 透传
//! 3. Paperclip 标准 env（agent / run_id）
//! 4. wake payload 字段（taskId / wakeReason / ...）
//! 5. workspace 字段映射（cwd / source / repoUrl / branch / ...）
//!
//! 安全保证：`PAPERCLIP_API_KEY` 永远不来自 config（被显式 drop）。

#![allow(dead_code)]

use serde_json::{json, Map, Value};

/// Wake env 构造输入。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WakeEnvInput<'a> {
    pub config_env: Map<String, Value>,
    pub agent: Value,
    pub run_id: &'a str,
    pub workspace: Value,
    pub wake: Option<&'a Value>,
    pub context_extras: Value,
    pub auth_token: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WakeEnvOutput {
    pub env: Map<String, Value>,
    pub dropped_keys: Vec<String>,
}

/// 构造最终 env（5 层优先级链）。
pub fn build_wake_env(input: &WakeEnvInput<'_>) -> WakeEnvOutput {
    let mut env: Map<String, Value> = Map::new();
    let mut dropped: Vec<String> = Vec::new();

    // 1. config_env：拒绝 PAPERCLIP_API_KEY
    for (k, v) in &input.config_env {
        if k == "PAPERCLIP_API_KEY" {
            dropped.push(k.clone());
            continue;
        }
        env.insert(k.clone(), v.clone());
    }

    // 2. 标准 Paperclip env
    for (k, v) in standard_paperclip_env(&input.agent, input.run_id) {
        env.insert(k, v);
    }

    // 3. wake payload
    if let Some(wake) = input.wake {
        if let Some(task_id) =
            read_trimmed(wake.get("taskId")).or_else(|| read_trimmed(wake.get("issueId")))
        {
            env.insert("PAPERCLIP_TASK_ID".to_owned(), json!(task_id));
        }
        if let Some(reason) = read_trimmed(wake.get("wakeReason")) {
            env.insert("PAPERCLIP_WAKE_REASON".to_owned(), json!(reason));
        }
        if let Some(comment_id) =
            read_trimmed(wake.get("wakeCommentId")).or_else(|| read_trimmed(wake.get("commentId")))
        {
            env.insert("PAPERCLIP_WAKE_COMMENT_ID".to_owned(), json!(comment_id));
        }
        if let Some(approval_id) = read_trimmed(wake.get("approvalId")) {
            env.insert("PAPERCLIP_APPROVAL_ID".to_owned(), json!(approval_id));
        }
        if let Some(approval_status) = read_trimmed(wake.get("approvalStatus")) {
            env.insert(
                "PAPERCLIP_APPROVAL_STATUS".to_owned(),
                json!(approval_status),
            );
        }
        if let Some(arr) = wake.get("issueIds").and_then(|v| v.as_array()) {
            let linked: Vec<String> = arr.iter().filter_map(|v| read_trimmed(Some(v))).collect();
            if !linked.is_empty() {
                env.insert(
                    "PAPERCLIP_LINKED_ISSUE_IDS".to_owned(),
                    json!(linked.join(",")),
                );
            }
        }
        // wake payload 整 JSON
        env.insert(
            "PAPERCLIP_WAKE_PAYLOAD_JSON".to_owned(),
            json!(serde_json::to_string(wake).unwrap_or_else(|_| "{}".to_owned())),
        );
    }

    // 4. workspace 字段（与 cursor-cloud 一样的 8 个）
    for (target, source_keys) in &[
        ("PAPERCLIP_WORKSPACE_CWD", "cwd"),
        ("PAPERCLIP_WORKSPACE_SOURCE", "source"),
        ("PAPERCLIP_WORKSPACE_ID", "workspaceId"),
        ("PAPERCLIP_WORKSPACE_REPO_URL", "repoUrl"),
        ("PAPERCLIP_WORKSPACE_REPO_REF", "repoRef"),
        ("PAPERCLIP_WORKSPACE_BRANCH", "branch"),
        ("PAPERCLIP_WORKSPACE_WORKTREE_PATH", "worktreePath"),
        ("AGENT_HOME", "agentHome"),
    ] {
        if let Some(v) = read_trimmed(input.workspace.get(source_keys)) {
            env.insert((*target).to_owned(), json!(v));
        }
    }

    // 5. auth token 注入 PAPERCLIP_API_KEY（覆盖来自 config 的 dropped）
    if let Some(token) = input.auth_token {
        env.insert("PAPERCLIP_API_KEY".to_owned(), json!(token));
    }

    // 6. context_extras 里的非 wake payload 字段
    if let Some(mode) = read_trimmed(input.context_extras.get("paperclipIssueWorkMode")) {
        env.insert("PAPERCLIP_ISSUE_WORK_MODE".to_owned(), json!(mode));
    }

    WakeEnvOutput {
        env,
        dropped_keys: dropped,
    }
}

fn standard_paperclip_env(agent: &Value, run_id: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    if let Some(id) = read_trimmed(agent.get("id")) {
        out.push(("PAPERCLIP_AGENT_ID".to_owned(), json!(id)));
    }
    if let Some(company_id) = read_trimmed(agent.get("companyId")) {
        out.push(("PAPERCLIP_COMPANY_ID".to_owned(), json!(company_id)));
    }
    if let Some(name) = read_trimmed(agent.get("name")) {
        out.push(("PAPERCLIP_AGENT_NAME".to_owned(), json!(name)));
    }
    if !run_id.trim().is_empty() {
        out.push(("PAPERCLIP_RUN_ID".to_owned(), json!(run_id)));
    }
    out
}

fn read_trimmed(v: Option<&Value>) -> Option<String> {
    let s = v.and_then(|x| x.as_str())?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_owned())
    }
}

/// 列出所有 `PAPERCLIP_*` 键（按字典序）。
pub fn paperclip_keys(env: &Map<String, Value>) -> Vec<&str> {
    let mut keys: Vec<&str> = env
        .keys()
        .filter(|k| k.starts_with("PAPERCLIP_"))
        .map(|k| k.as_str())
        .collect();
    keys.sort_unstable();
    keys
}

/// 渲染 PAPERCLIP_* env note（注入 prompt 末尾）。
pub fn render_paperclip_env_note(env: &Map<String, Value>) -> String {
    let keys = paperclip_keys(env);
    if keys.is_empty() {
        return String::new();
    }
    format!(
        "Paperclip runtime note:\nThe following PAPERCLIP_* environment variables are available in the gateway shell: {}\nUse them directly instead of assuming they are absent.",
        keys.join(", ")
    )
}

/// Env 描述（用于 onMeta metadata）。
#[derive(Debug, Clone, PartialEq)]
pub struct EnvDescription {
    pub paperclip_api_key_present: bool,
    pub paperclip_keys: Vec<String>,
}

pub fn describe_env(env: &Map<String, Value>) -> EnvDescription {
    EnvDescription {
        paperclip_api_key_present: env.contains_key("PAPERCLIP_API_KEY"),
        paperclip_keys: paperclip_keys(env).into_iter().map(String::from).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(items: &[(&str, &str)]) -> Map<String, Value> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect()
    }

    #[test]
    fn empty_inputs_yields_empty_env() {
        let input = WakeEnvInput::default();
        let out = build_wake_env(&input);
        assert!(out.env.is_empty());
        assert!(out.dropped_keys.is_empty());
    }

    #[test]
    fn config_env_openclaw_key_passes_through() {
        let input = WakeEnvInput {
            config_env: flat(&[("OPENCLAW_API_KEY", "ok-1"), ("DEBUG", "true")]),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["OPENCLAW_API_KEY"], json!("ok-1"));
        assert_eq!(out.env["DEBUG"], json!("true"));
        assert!(out.dropped_keys.is_empty());
    }

    #[test]
    fn config_env_paperclip_api_key_dropped() {
        let input = WakeEnvInput {
            config_env: flat(&[("PAPERCLIP_API_KEY", "ok-bad")]),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert!(out.env.get("PAPERCLIP_API_KEY").is_none());
        assert_eq!(out.dropped_keys, vec!["PAPERCLIP_API_KEY".to_owned()]);
    }

    #[test]
    fn agent_fields_become_paperclip_env() {
        let agent = json!({"id": "ag-1", "companyId": "co-1", "name": "Foo"});
        let input = WakeEnvInput {
            agent,
            run_id: "run-9",
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_AGENT_ID"], json!("ag-1"));
        assert_eq!(out.env["PAPERCLIP_COMPANY_ID"], json!("co-1"));
        assert_eq!(out.env["PAPERCLIP_AGENT_NAME"], json!("Foo"));
        assert_eq!(out.env["PAPERCLIP_RUN_ID"], json!("run-9"));
    }

    #[test]
    fn wake_payload_fields_propagate() {
        let wake = json!({
            "taskId": "t-1", "wakeReason": "manual",
            "wakeCommentId": "c-1", "approvalId": "a-1",
            "approvalStatus": "approved",
            "issueIds": ["i-1", "i-2"]
        });
        let input = WakeEnvInput {
            wake: Some(&wake),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_TASK_ID"], json!("t-1"));
        assert_eq!(out.env["PAPERCLIP_WAKE_REASON"], json!("manual"));
        assert_eq!(out.env["PAPERCLIP_WAKE_COMMENT_ID"], json!("c-1"));
        assert_eq!(out.env["PAPERCLIP_APPROVAL_ID"], json!("a-1"));
        assert_eq!(out.env["PAPERCLIP_APPROVAL_STATUS"], json!("approved"));
        assert_eq!(out.env["PAPERCLIP_LINKED_ISSUE_IDS"], json!("i-1,i-2"));
        assert!(out.env.get("PAPERCLIP_WAKE_PAYLOAD_JSON").is_some());
    }

    #[test]
    fn wake_issue_id_fallback_to_task_id() {
        let wake = json!({"issueId": "iss-9"});
        let input = WakeEnvInput {
            wake: Some(&wake),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_TASK_ID"], json!("iss-9"));
    }

    #[test]
    fn workspace_fields_map_correctly() {
        let ws = json!({
            "cwd": "/tmp/w", "source": "managed", "workspaceId": "ws-1",
            "repoUrl": "https://github.com/a/b", "repoRef": "refs/heads/main",
            "branch": "main", "worktreePath": "/tmp/w/main",
            "agentHome": "/tmp/ah"
        });
        let input = WakeEnvInput {
            workspace: ws,
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_WORKSPACE_CWD"], json!("/tmp/w"));
        assert_eq!(out.env["PAPERCLIP_WORKSPACE_SOURCE"], json!("managed"));
        assert_eq!(out.env["PAPERCLIP_WORKSPACE_ID"], json!("ws-1"));
        assert_eq!(
            out.env["PAPERCLIP_WORKSPACE_REPO_URL"],
            json!("https://github.com/a/b")
        );
        assert_eq!(out.env["AGENT_HOME"], json!("/tmp/ah"));
    }

    #[test]
    fn auth_token_overrides_dropped_paperclip_api_key() {
        let input = WakeEnvInput {
            config_env: flat(&[("PAPERCLIP_API_KEY", "ok-bad")]),
            auth_token: Some("ok-real"),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_API_KEY"], json!("ok-real"));
        assert_eq!(out.dropped_keys, vec!["PAPERCLIP_API_KEY".to_owned()]);
    }

    #[test]
    fn auth_token_injects_when_no_config() {
        let input = WakeEnvInput {
            auth_token: Some("ok-real"),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_API_KEY"], json!("ok-real"));
    }

    #[test]
    fn paperclip_keys_returns_sorted_filtered() {
        let mut env = Map::new();
        let keys = paperclip_keys(&env);
        assert!(keys.is_empty());
        env.insert("PAPERCLIP_TASK_ID".into(), json!("t"));
        env.insert("PAPERCLIP_AGENT_ID".into(), json!("a"));
        env.insert("OTHER".into(), json!("o"));
        assert_eq!(
            paperclip_keys(&env),
            vec!["PAPERCLIP_AGENT_ID", "PAPERCLIP_TASK_ID"]
        );
    }

    #[test]
    fn render_paperclip_env_note_empty_when_no_paperclip_keys() {
        let mut env = Map::new();
        env.insert("DEBUG".into(), json!("true"));
        assert_eq!(render_paperclip_env_note(&env), "");
    }

    #[test]
    fn render_paperclip_env_note_includes_sorted_keys() {
        let mut env = Map::new();
        env.insert("PAPERCLIP_TASK_ID".into(), json!("t"));
        env.insert("PAPERCLIP_AGENT_ID".into(), json!("a"));
        let s = render_paperclip_env_note(&env);
        assert!(s.contains("PAPERCLIP_AGENT_ID, PAPERCLIP_TASK_ID"));
        assert!(s.contains("gateway shell"));
    }

    #[test]
    fn describe_env_reports_required_keys() {
        let mut env = Map::new();
        env.insert("PAPERCLIP_API_KEY".into(), json!("ok"));
        env.insert("PAPERCLIP_AGENT_ID".into(), json!("a"));
        let d = describe_env(&env);
        assert!(d.paperclip_api_key_present);
        assert!(d.paperclip_keys.iter().any(|k| k == "PAPERCLIP_AGENT_ID"));
    }

    #[test]
    fn context_extras_issue_work_mode_propagates() {
        let input = WakeEnvInput {
            context_extras: json!({"paperclipIssueWorkMode": "issue_only"}),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_ISSUE_WORK_MODE"], json!("issue_only"));
    }

    #[test]
    fn read_trimmed_skips_empty_and_whitespace() {
        assert!(read_trimmed(None).is_none());
        assert!(read_trimmed(Some(&json!(""))).is_none());
        assert!(read_trimmed(Some(&json!("   "))).is_none());
        assert_eq!(read_trimmed(Some(&json!(" v "))).as_deref(), Some("v"));
    }

    #[test]
    fn full_pipeline_includes_all_priority_layers() {
        let env = flat(&[
            ("OPENCLAW_API_KEY", "ok-1"),
            ("DEBUG", "true"),
            ("PAPERCLIP_API_KEY", "ok-bad"),
        ]);
        let agent = json!({"id": "ag-9", "companyId": "co-9", "name": "X"});
        let wake = json!({"taskId": "task-1", "wakeReason": "manual"});
        let workspace = json!({"cwd": "/tmp", "branch": "main"});
        let input = WakeEnvInput {
            config_env: env,
            agent,
            run_id: "run-x",
            workspace,
            wake: Some(&wake),
            auth_token: Some("ok-real"),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        // Layer 1 (config_env except dropped)
        assert_eq!(out.env["OPENCLAW_API_KEY"], json!("ok-1"));
        // Layer 2 (standard paperclip)
        assert_eq!(out.env["PAPERCLIP_AGENT_ID"], json!("ag-9"));
        // Layer 3 (wake)
        assert_eq!(out.env["PAPERCLIP_TASK_ID"], json!("task-1"));
        // Layer 4 (workspace)
        assert_eq!(out.env["PAPERCLIP_WORKSPACE_CWD"], json!("/tmp"));
        // Layer 5 (auth token overrides dropped)
        assert_eq!(out.env["PAPERCLIP_API_KEY"], json!("ok-real"));
        assert_eq!(out.dropped_keys, vec!["PAPERCLIP_API_KEY".to_owned()]);
    }
}
