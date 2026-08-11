//! Cursor Cloud wake env builder — 对齐 Node
//! `packages/adapters/cursor-cloud/src/server/execute.ts::buildWakeEnv`。
//!
//! 关键行为：
//! - `PAPERCLIP_*` 与 workspace 字段注入到子进程 env
//! - **永不** 接受 `config` 里写的 `PAPERCLIP_API_KEY`（必须来自 ctx.authToken）
//! - **永不** 保留 `config` 里写的 `CURSOR_API_KEY`（必须来自 config.env 处理后的 envMap）
//! - 保留 secret bindings (PAPERCLIP_*_SECRET 等) 后续 round 引入

#![allow(dead_code)]

use serde_json::{json, Map, Value};

use crate::constants::{FORBIDDEN_CONFIG_KEYS, PAPERCLIP_ENV_PREFIX};

/// 构造一个 Paperclip 子进程 env map。
///
/// 输入：
/// - `config_env` — `config.env` 解析后的 flat `Record<string, string>`（来自 `asStringEnvMap`）
/// - `agent` — adapter context 中的 agent（id, companyId, name, 等）
/// - `run_id` — 当前 run UUID 字符串
/// - `workspace` — `context.paperclipWorkspace` 解析后的对象
/// - `wake` — `context.paperclipWake` (wake payload)
/// - `auth_token` — harness-minted run token（用来注入 PAPERCLIP_API_KEY）

#[derive(Debug, Clone, PartialEq, Default)]
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

/// 列出所有 `PAPERCLIP_*` 键，按字典序排序（供 `renderPaperclipEnvNote` 使用）。
pub fn paperclip_keys(env: &Map<String, Value>) -> Vec<&str> {
    let mut keys: Vec<&str> = env
        .keys()
        .filter(|k| k.starts_with(PAPERCLIP_ENV_PREFIX))
        .map(|k| k.as_str())
        .collect();
    keys.sort_unstable();
    keys
}

/// 主要函数 — 拼接最终 env。
///
/// 优先级（高 → 低）：
/// 1. harness `auth_token` → `PAPERCLIP_API_KEY`（最高优先级，永远保留）
/// 2. config.env flat map（用户提供的 CURSOR_API_KEY 等）
/// 3. Paperclip 标准 env（buildPaperclipEnv(agent) + run_id）
/// 4. wake payload 导出（taskId / commentId / approvalId / linkedIssueIds / payload json）
/// 5. workspace 字段映射（cwd / source / workspaceId / repoUrl / branch / worktreePath / agentHome）
pub fn build_wake_env(input: &WakeEnvInput<'_>) -> WakeEnvOutput {
    let mut env: Map<String, Value> = Map::new();
    let mut dropped: Vec<String> = Vec::new();

    // 1. config_env（flat string→string）
    for (k, v) in &input.config_env {
        // 永远拒绝来自 config 的 PAPERCLIP_API_KEY
        if FORBIDDEN_CONFIG_KEYS.contains(&k.as_str()) {
            dropped.push(k.clone());
            continue;
        }
        env.insert(k.clone(), v.clone());
    }

    // 2. Paperclip standard env from agent
    for (k, v) in standard_paperclip_env(&input.agent, input.run_id) {
        env.insert(k, v);
    }

    // 3. wake payload → PAPERCLIP_WAKE_* / TASK_ID
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
    }

    // 4. workspace fields
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

    // 5. auth token 注入 PAPERCLIP_API_KEY（最高优先）
    if let Some(token) = input.auth_token {
        // 防御：如果之前被错误注入，这里覆盖（来自 config.env 的已被 dropped）
        env.insert("PAPERCLIP_API_KEY".to_owned(), json!(token));
    }

    // 6. context_extras 里允许 PAPERCLIP_ISSUE_WORK_MODE 这种非 wake payload 字段
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

/// “Paperclip env note” — 列出可用 PAPERCLIP_* 变量（注入到 prompt 末尾）。
/// 对齐 Node `renderPaperclipEnvNote`。
pub fn render_paperclip_env_note(env: &Map<String, Value>) -> String {
    let keys = paperclip_keys(env);
    if keys.is_empty() {
        return String::new();
    }
    format!(
        "Paperclip runtime note:\nThe following PAPERCLIP_* environment variables are available in the cloud agent shell: {}\nUse them directly instead of assuming they are absent.",
        keys.join(", ")
    )
}

/// 渲染 CURSOR_API_KEY 提取后的命令提示（供 execute 错误信息）。
pub fn describe_env(env: &Map<String, Value>) -> EnvDescription {
    EnvDescription {
        cursor_api_key_present: env.contains_key("CURSOR_API_KEY"),
        paperclip_api_key_present: env.contains_key("PAPERCLIP_API_KEY"),
        paperclip_keys: paperclip_keys(env).into_iter().map(String::from).collect(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvDescription {
    pub cursor_api_key_present: bool,
    pub paperclip_api_key_present: bool,
    pub paperclip_keys: Vec<String>,
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
    fn config_env_passes_through_for_legal_keys() {
        let input = WakeEnvInput {
            config_env: flat(&[("CURSOR_API_KEY", "ck-1"), ("DEBUG", "true")]),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["CURSOR_API_KEY"], json!("ck-1"));
        assert_eq!(out.env["DEBUG"], json!("true"));
        assert!(out.dropped_keys.is_empty());
    }

    #[test]
    fn config_env_paperclip_api_key_is_dropped() {
        let input = WakeEnvInput {
            config_env: flat(&[("PAPERCLIP_API_KEY", "ck-bad")]),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert!(out.env.get("PAPERCLIP_API_KEY").is_none());
        assert!(out.dropped_keys.iter().any(|k| k == "PAPERCLIP_API_KEY"));
    }

    #[test]
    fn agent_fields_become_paperclip_env() {
        let agent = json!({"id": "ag-1", "companyId": "co-1", "name": "My Agent"});
        let input = WakeEnvInput {
            agent,
            run_id: "run-9",
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_AGENT_ID"], json!("ag-1"));
        assert_eq!(out.env["PAPERCLIP_COMPANY_ID"], json!("co-1"));
        assert_eq!(out.env["PAPERCLIP_AGENT_NAME"], json!("My Agent"));
        assert_eq!(out.env["PAPERCLIP_RUN_ID"], json!("run-9"));
    }

    #[test]
    fn wake_payload_fields_propagate() {
        let wake = json!({
            "taskId": "task-1",
            "wakeReason": "manual_reopen",
            "wakeCommentId": "c-1",
            "approvalId": "ap-1",
            "approvalStatus": "approved",
            "issueIds": ["i-1", "i-2"]
        });
        let input = WakeEnvInput {
            wake: Some(&wake),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_TASK_ID"], json!("task-1"));
        assert_eq!(out.env["PAPERCLIP_WAKE_REASON"], json!("manual_reopen"));
        assert_eq!(out.env["PAPERCLIP_WAKE_COMMENT_ID"], json!("c-1"));
        assert_eq!(out.env["PAPERCLIP_APPROVAL_ID"], json!("ap-1"));
        assert_eq!(out.env["PAPERCLIP_APPROVAL_STATUS"], json!("approved"));
        assert_eq!(out.env["PAPERCLIP_LINKED_ISSUE_IDS"], json!("i-1,i-2"));
    }

    #[test]
    fn wake_issue_id_fallback() {
        let wake = json!({"issueId": "iss-9"});
        let input = WakeEnvInput {
            wake: Some(&wake),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_TASK_ID"], json!("iss-9"));
    }

    #[test]
    fn workspace_fields_become_paperclip_env() {
        let ws = json!({
            "cwd": "/tmp/work",
            "source": "managed",
            "workspaceId": "ws-1",
            "repoUrl": "https://github.com/a/b",
            "repoRef": "refs/heads/main",
            "branch": "feat/x",
            "worktreePath": "/tmp/work/feat-x",
            "agentHome": "/tmp/agent-home"
        });
        let input = WakeEnvInput {
            workspace: ws,
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_WORKSPACE_CWD"], json!("/tmp/work"));
        assert_eq!(out.env["PAPERCLIP_WORKSPACE_SOURCE"], json!("managed"));
        assert_eq!(out.env["PAPERCLIP_WORKSPACE_ID"], json!("ws-1"));
        assert_eq!(
            out.env["PAPERCLIP_WORKSPACE_REPO_URL"],
            json!("https://github.com/a/b")
        );
        assert_eq!(
            out.env["PAPERCLIP_WORKSPACE_REPO_REF"],
            json!("refs/heads/main")
        );
        assert_eq!(out.env["PAPERCLIP_WORKSPACE_BRANCH"], json!("feat/x"));
        assert_eq!(
            out.env["PAPERCLIP_WORKSPACE_WORKTREE_PATH"],
            json!("/tmp/work/feat-x")
        );
        assert_eq!(out.env["AGENT_HOME"], json!("/tmp/agent-home"));
    }

    #[test]
    fn auth_token_overrides_dropped_paperclip_api_key() {
        let input = WakeEnvInput {
            config_env: flat(&[("PAPERCLIP_API_KEY", "ck-bad")]),
            auth_token: Some("ck-real"),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_API_KEY"], json!("ck-real"));
        assert!(out.dropped_keys.iter().any(|k| k == "PAPERCLIP_API_KEY"));
    }

    #[test]
    fn auth_token_injects_when_no_config_key() {
        let input = WakeEnvInput {
            auth_token: Some("ck-real"),
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_API_KEY"], json!("ck-real"));
    }

    #[test]
    fn workspace_partial_fields_only_set_what_present() {
        let ws = json!({"cwd": "/x"});
        let input = WakeEnvInput {
            workspace: ws,
            ..WakeEnvInput::default()
        };
        let out = build_wake_env(&input);
        assert_eq!(out.env["PAPERCLIP_WORKSPACE_CWD"], json!("/x"));
        assert!(out.env.get("PAPERCLIP_WORKSPACE_BRANCH").is_none());
    }

    #[test]
    fn paperclip_keys_returns_sorted_and_empty_aware() {
        let mut env = Map::new();
        let keys = paperclip_keys(&env);
        assert!(keys.is_empty());
        env.insert("PAPERCLIP_TASK_ID".into(), json!("t"));
        env.insert("PAPERCLIP_AGENT_ID".into(), json!("a"));
        env.insert("OTHER_KEY".into(), json!("o"));
        let keys = paperclip_keys(&env);
        assert_eq!(keys, vec!["PAPERCLIP_AGENT_ID", "PAPERCLIP_TASK_ID"]);
    }

    #[test]
    fn render_paperclip_env_note_returns_empty_when_no_paperclip_keys() {
        let mut env = Map::new();
        env.insert("DEBUG".into(), json!("true"));
        let s = render_paperclip_env_note(&env);
        assert!(s.is_empty());
    }

    #[test]
    fn render_paperclip_env_note_includes_sorted_keys() {
        let mut env = Map::new();
        env.insert("PAPERCLIP_TASK_ID".into(), json!("t"));
        env.insert("PAPERCLIP_AGENT_ID".into(), json!("a"));
        let s = render_paperclip_env_note(&env);
        assert!(s.contains("PAPERCLIP_AGENT_ID, PAPERCLIP_TASK_ID"));
    }

    #[test]
    fn describe_env_reports_required_keys() {
        let mut env = Map::new();
        env.insert("CURSOR_API_KEY".into(), json!("ck"));
        env.insert("PAPERCLIP_API_KEY".into(), json!("pk"));
        env.insert("PAPERCLIP_AGENT_ID".into(), json!("a"));
        let d = describe_env(&env);
        assert!(d.cursor_api_key_present);
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
    fn read_trimmed_skips_empty() {
        assert!(read_trimmed(None).is_none());
        assert!(read_trimmed(Some(&json!(""))).is_none());
        assert!(read_trimmed(Some(&json!("   "))).is_none());
        assert_eq!(read_trimmed(Some(&json!(" v "))).as_deref(), Some("v"));
    }
}
