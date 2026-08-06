//! `workspace_runtime_template_render` — `{{path}}` 模板渲染 + env 构造。
//!
//! 与 Node `resolvePathValue` / `renderTemplate` (adapter-utils/server-utils)、
//! `buildTemplateData` / `renderRuntimeServiceEnv` (workspace-runtime.ts) 1:1 对齐。
//!
//! 设计目标：纯函数模块，无 DB/IO，仅依赖 `serde_json::Value`。
//! 不引入 sqlx/tokio。
use serde_json::{Map, Value};

use crate::stable_string::stable_stringify;

// ============================================================================
// resolvePathValue
// ============================================================================

/// `resolvePathValue(data, dottedPath)`：从 `data` 按 `a.b.c` 形式查找值。
///
/// 与 Node 1:1 对齐：
/// - 任何一段 cursor 非 object → ""
/// - null/undefined → ""
/// - string → 原样
/// - number/boolean → toString
/// - 其它 → JSON.stringify（失败时 ""）
pub fn resolve_path_value(data: &Value, dotted_path: &str) -> String {
    let parts: Vec<&str> = dotted_path.split('.').collect();
    let mut cursor: &Value = data;
    for part in parts {
        match cursor {
            Value::Object(map) => {
                match map.get(part) {
                    Some(v) => cursor = v,
                    None => return String::new(),
                }
            }
            _ => return String::new(),
        }
    }
    match cursor {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => serde_json::to_string(cursor).unwrap_or_default(),
    }
}

// ============================================================================
// renderTemplate
// ============================================================================

/// `renderTemplate(template, data)`：把 `{{ a.b.c }}` 占位符替换成 resolve 结果。
///
/// 与 Node 1:1 对齐：`{{` `}}` 之间允许任意空白，路径字符 `[a-zA-Z0-9_.-]+`。
pub fn render_template(template: &str, data: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // 找 `}}`
            if let Some(end_rel) = find_close(template, i + 2) {
                // 提取内部
                let inner = &template[i + 2..end_rel];
                let trimmed = inner.trim();
                if is_valid_path(trimmed) {
                    let resolved = resolve_path_value(data, trimmed);
                    out.push_str(&resolved);
                    i = end_rel + 2;
                    continue;
                }
            }
        }
        // 普通字符：注意 UTF-8 安全
        let ch_end = next_char_boundary(template, i);
        out.push_str(&template[i..ch_end]);
        i = ch_end;
    }
    out
}

/// 查找 `}}` 起始位置（相对绝对位置），找不到返回 None。
fn find_close(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 路径合法性：仅 `[a-zA-Z0-9_.-]`。
fn is_valid_path(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// 下一个 UTF-8 字符边界。
fn next_char_boundary(s: &str, i: usize) -> usize {
    let mut j = i + 1;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

// ============================================================================
// buildTemplateData
// ============================================================================

/// `ExecutionWorkspaceIssueRef`：minimal subset for template。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct IssueRef {
    pub id: String,
    pub identifier: Option<String>,
    pub title: Option<String>,
}

/// `ExecutionWorkspaceAgentRef`：minimal subset for template。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentRef {
    pub id: Option<String>,
    pub name: String,
}

/// `RealizedExecutionWorkspace`：minimal subset for template。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RealizedExecutionWorkspaceRef {
    pub cwd: String,
    pub branch_name: Option<String>,
    pub worktree_path: Option<String>,
    pub repo_url: Option<String>,
    pub repo_ref: Option<String>,
}

/// `BuildTemplateDataInput`：`buildTemplateData` 的输入。
#[derive(Debug, Clone)]
pub struct BuildTemplateDataInput<'a> {
    pub workspace: &'a RealizedExecutionWorkspaceRef,
    pub agent: &'a AgentRef,
    pub issue: Option<&'a IssueRef>,
    pub adapter_env: &'a Map<String, Value>,
    pub port: Option<i64>,
}

/// `buildTemplateData(input)`：构造模板数据对象。
///
/// 与 Node 1:1 对齐：
/// - workspace.* 字段，缺失 → ""  
/// - issue/agent 字段
/// - port 缺失 → ""
pub fn build_template_data(input: BuildTemplateDataInput<'_>) -> Value {
    let mut ws_obj = Map::new();
    ws_obj.insert("cwd".into(), Value::String(input.workspace.cwd.clone()));
    ws_obj.insert(
        "branchName".into(),
        Value::String(input.workspace.branch_name.clone().unwrap_or_default()),
    );
    ws_obj.insert(
        "worktreePath".into(),
        Value::String(input.workspace.worktree_path.clone().unwrap_or_default()),
    );
    ws_obj.insert(
        "repoUrl".into(),
        Value::String(input.workspace.repo_url.clone().unwrap_or_default()),
    );
    ws_obj.insert(
        "repoRef".into(),
        Value::String(input.workspace.repo_ref.clone().unwrap_or_default()),
    );
    ws_obj.insert(
        "env".into(),
        Value::Object(input.adapter_env.clone()),
    );

    let mut issue_obj = Map::new();
    issue_obj.insert(
        "id".into(),
        Value::String(input.issue.map(|i| i.id.clone()).unwrap_or_default()),
    );
    issue_obj.insert(
        "identifier".into(),
        Value::String(
            input
                .issue
                .and_then(|i| i.identifier.clone())
                .unwrap_or_default(),
        ),
    );
    issue_obj.insert(
        "title".into(),
        Value::String(
            input
                .issue
                .and_then(|i| i.title.clone())
                .unwrap_or_default(),
        ),
    );

    let mut agent_obj = Map::new();
    agent_obj.insert(
        "id".into(),
        Value::String(input.agent.id.clone().unwrap_or_default()),
    );
    agent_obj.insert("name".into(), Value::String(input.agent.name.clone()));

    let mut root = Map::new();
    root.insert("workspace".into(), Value::Object(ws_obj));
    root.insert("issue".into(), Value::Object(issue_obj));
    root.insert("agent".into(), Value::Object(agent_obj));
    root.insert(
        "port".into(),
        match input.port {
            Some(p) => Value::Number(serde_json::Number::from(p)),
            None => Value::String(String::new()),
        },
    );
    Value::Object(root)
}

// ============================================================================
// renderRuntimeServiceEnv
// ============================================================================

/// `RenderRuntimeServiceEnvInput`：`renderRuntimeServiceEnv` 输入。
#[derive(Debug, Clone)]
pub struct RenderRuntimeServiceEnvInput<'a> {
    pub env_config: &'a Map<String, Value>,
    pub template_data: &'a Value,
}

/// `renderRuntimeServiceEnv(input)`：把 envConfig 中 string 值用 template 渲染。
///
/// 与 Node 1:1 对齐：
/// - 只渲染 string 值，其它类型跳过
/// - 返回 `{ key: renderedString }`
pub fn render_runtime_service_env(input: RenderRuntimeServiceEnvInput<'_>) -> Map<String, Value> {
    let mut rendered: Map<String, Value> = Map::new();
    for (key, value) in input.env_config.iter() {
        if let Value::String(s) = value {
            rendered.insert(
                key.clone(),
                Value::String(render_template(s, input.template_data)),
            );
        }
    }
    rendered
}

/// `stableFingerprint(env)`：SHA-256 hex digest of stable_stringify(env)。
///
/// 与 Node `createHash("sha256").update(stableStringify(env)).digest("hex")` 1:1 对齐。
pub fn stable_fingerprint(env: &Map<String, Value>) -> String {
    let v = Value::Object(env.clone());
    let s = stable_stringify(&v);
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ----- resolve_path_value -----

    #[test]
    fn resolve_path_value_string() {
        let v = json!({"a": {"b": "hello"}});
        assert_eq!(resolve_path_value(&v, "a.b"), "hello");
    }

    #[test]
    fn resolve_path_value_missing_returns_empty() {
        let v = json!({"a": {}});
        assert_eq!(resolve_path_value(&v, "a.b"), "");
    }

    #[test]
    fn resolve_path_value_null_returns_empty() {
        let v = json!({"a": null});
        assert_eq!(resolve_path_value(&v, "a"), "");
    }

    #[test]
    fn resolve_path_value_top_level_string() {
        let v = json!("hi");
        assert_eq!(resolve_path_value(&v, "anything"), "");
    }

    #[test]
    fn resolve_path_value_number() {
        let v = json!({"n": 42});
        assert_eq!(resolve_path_value(&v, "n"), "42");
    }

    #[test]
    fn resolve_path_value_bool() {
        let v = json!({"b": true});
        assert_eq!(resolve_path_value(&v, "b"), "true");
    }

    #[test]
    fn resolve_path_value_object_json_stringified() {
        let v = json!({"o": {"k": "v"}});
        assert_eq!(resolve_path_value(&v, "o"), "{\"k\":\"v\"}");
    }

    #[test]
    fn resolve_path_value_array_returns_empty() {
        // 在中间路径遇到数组 → ""
        let v = json!({"a": [1, 2, 3]});
        assert_eq!(resolve_path_value(&v, "a.b"), "");
    }

    // ----- render_template -----

    #[test]
    fn render_template_basic() {
        let v = json!({"name": "world"});
        assert_eq!(render_template("hello {{name}}", &v), "hello world");
    }

    #[test]
    fn render_template_nested() {
        let v = json!({"a": {"b": "deep"}});
        assert_eq!(render_template("{{a.b}}!", &v), "deep!");
    }

    #[test]
    fn render_template_missing_path_empty() {
        let v = json!({});
        assert_eq!(render_template("x={{missing}}", &v), "x=");
    }

    #[test]
    fn render_template_with_whitespace() {
        let v = json!({"name": "ws"});
        assert_eq!(render_template("[{{ name }}]", &v), "[ws]");
    }

    #[test]
    fn render_template_keeps_unmatched_braces() {
        let v = json!({"name": "ws"});
        // 单个 `}` 不算占位符
        assert_eq!(render_template("foo} bar {{name}}", &v), "foo} bar ws");
    }

    #[test]
    fn render_template_keeps_invalid_path_literal() {
        let v = json!({});
        // 含空格的路径 → 不合法，保持字面
        assert_eq!(
            render_template("{{ a b }} {{name}}", &v),
            "{{ a b }} "
        );
    }

    #[test]
    fn render_template_multiple_in_one() {
        let v = json!({"a": "1", "b": "2"});
        assert_eq!(
            render_template("{{a}}-{{b}}-{{a}}", &v),
            "1-2-1"
        );
    }

    // ----- build_template_data -----

    fn ws() -> RealizedExecutionWorkspaceRef {
        RealizedExecutionWorkspaceRef {
            cwd: "/repo".into(),
            branch_name: Some("feat/x".into()),
            worktree_path: Some("/wt".into()),
            repo_url: Some("git@x".into()),
            repo_ref: Some("refs/x".into()),
        }
    }

    fn ag() -> AgentRef {
        AgentRef {
            id: Some("a-1".into()),
            name: "agent-1".into(),
        }
    }

    fn iss() -> IssueRef {
        IssueRef {
            id: "iss-1".into(),
            identifier: Some("PROJ-1".into()),
            title: Some("fix bug".into()),
        }
    }

    #[test]
    fn build_template_data_with_all() {
        let mut env = Map::new();
        env.insert("X".into(), Value::String("1".into()));
        let data = build_template_data(BuildTemplateDataInput {
            workspace: &ws(),
            agent: &ag(),
            issue: Some(&iss()),
            adapter_env: &env,
            port: Some(3000_i64),
        });
        // 路径访问
        assert_eq!(resolve_path_value(&data, "workspace.cwd"), "/repo");
        assert_eq!(resolve_path_value(&data, "workspace.branchName"), "feat/x");
        assert_eq!(resolve_path_value(&data, "workspace.worktreePath"), "/wt");
        assert_eq!(resolve_path_value(&data, "workspace.repoUrl"), "git@x");
        assert_eq!(resolve_path_value(&data, "workspace.repoRef"), "refs/x");
        assert_eq!(resolve_path_value(&data, "issue.id"), "iss-1");
        assert_eq!(resolve_path_value(&data, "issue.identifier"), "PROJ-1");
        assert_eq!(resolve_path_value(&data, "issue.title"), "fix bug");
        assert_eq!(resolve_path_value(&data, "agent.id"), "a-1");
        assert_eq!(resolve_path_value(&data, "agent.name"), "agent-1");
        assert_eq!(resolve_path_value(&data, "port"), "3000");
    }

    #[test]
    fn build_template_data_no_issue() {
        let env = Map::new();
        let data = build_template_data(BuildTemplateDataInput {
            workspace: &ws(),
            agent: &ag(),
            issue: None,
            adapter_env: &env,
            port: None,
        });
        assert_eq!(resolve_path_value(&data, "issue.id"), "");
        assert_eq!(resolve_path_value(&data, "issue.identifier"), "");
        assert_eq!(resolve_path_value(&data, "port"), "");
    }

    #[test]
    fn build_template_data_missing_branch_and_worktree() {
        let w = RealizedExecutionWorkspaceRef {
            cwd: "/r".into(),
            branch_name: None,
            worktree_path: None,
            repo_url: None,
            repo_ref: None,
        };
        let env = Map::new();
        let data = build_template_data(BuildTemplateDataInput {
            workspace: &w,
            agent: &ag(),
            issue: None,
            adapter_env: &env,
            port: None,
        });
        assert_eq!(resolve_path_value(&data, "workspace.branchName"), "");
        assert_eq!(resolve_path_value(&data, "workspace.worktreePath"), "");
        assert_eq!(resolve_path_value(&data, "workspace.repoUrl"), "");
        assert_eq!(resolve_path_value(&data, "workspace.repoRef"), "");
    }

    // ----- render_runtime_service_env -----

    #[test]
    fn render_runtime_service_env_basic() {
        let mut env_cfg = Map::new();
        env_cfg.insert("URL".into(), Value::String("http://{{port}}".into()));
        env_cfg.insert("NAME".into(), Value::String("{{agent.name}}".into()));
        env_cfg.insert("NOT_STR".into(), Value::Number(serde_json::Number::from(42)));
        let env_cfg_view = env_cfg.clone();
        let ws_ref = ws();
        let ag_ref = ag();
        let env_obj = Map::new();
        let data = build_template_data(BuildTemplateDataInput {
            workspace: &ws_ref,
            agent: &ag_ref,
            issue: None,
            adapter_env: &env_obj,
            port: Some(8080_i64),
        });
        let rendered = render_runtime_service_env(RenderRuntimeServiceEnvInput {
            env_config: &env_cfg_view,
            template_data: &data,
        });
        if let Some(Value::String(s)) = rendered.get("URL") {
            assert_eq!(s, "http://8080");
        } else {
            panic!("expected URL string");
        }
        if let Some(Value::String(s)) = rendered.get("NAME") {
            assert_eq!(s, "agent-1");
        } else {
            panic!("expected NAME string");
        }
        assert!(rendered.get("NOT_STR").is_none()); // 跳过非 string
    }

    // ----- stable_fingerprint -----

    #[test]
    fn stable_fingerprint_deterministic() {
        let mut m1 = Map::new();
        m1.insert("a".into(), Value::String("1".into()));
        m1.insert("b".into(), Value::String("2".into()));
        let mut m2 = Map::new();
        m2.insert("b".into(), Value::String("2".into()));
        m2.insert("a".into(), Value::String("1".into()));
        assert_eq!(stable_fingerprint(&m1), stable_fingerprint(&m2));
    }

    #[test]
    fn stable_fingerprint_differs_for_diff_keys() {
        let mut m1 = Map::new();
        m1.insert("a".into(), Value::String("1".into()));
        let mut m2 = Map::new();
        m2.insert("a".into(), Value::String("2".into()));
        assert_ne!(stable_fingerprint(&m1), stable_fingerprint(&m2));
    }

    #[test]
    fn stable_fingerprint_returns_hex_64() {
        let m = Map::new();
        let out = stable_fingerprint(&m);
        assert_eq!(out.len(), 64);
        assert!(out.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
