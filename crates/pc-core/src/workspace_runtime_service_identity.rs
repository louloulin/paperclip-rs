//! `workspace_runtime_service_identity` — runtime service scope/identity 解析。
//!
//! 与 Node `resolveServiceScopeId` / `resolveRuntimeServiceReuseIdentity` /
//! `resolveWorkspaceCommandExecution` 1:1 对齐。
//!
//! 设计目标：纯函数模块；依赖 `workspace_runtime_template_render` +
//! `workspace_runtime_string_utils` + `stable_string`。
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::workspace_runtime_string_utils::resolve_configured_path;
use crate::workspace_runtime_template_render::{
    build_template_data, render_runtime_service_env, stable_fingerprint,
    AgentRef as TemplateAgentRef, BuildTemplateDataInput, IssueRef as TemplateIssueRef,
    RealizedExecutionWorkspaceRef,
};
use sha2::{Digest, Sha256};

use crate::stable_string::stable_stringify;

// ============================================================================
// Reuse scope type
// ============================================================================

/// `reuseScope` 解析结果，与 Node `scopeType` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseScopeType {
    ProjectWorkspace,
    ExecutionWorkspace,
    Run,
    Agent,
}

impl Default for ReuseScopeType {
    fn default() -> Self {
        Self::ProjectWorkspace
    }
}

impl ReuseScopeType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProjectWorkspace => "project_workspace",
            Self::ExecutionWorkspace => "execution_workspace",
            Self::Run => "run",
            Self::Agent => "agent",
        }
    }
}

/// `runtimeService.lifecycle` 字符串字面量（与 Node 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeServiceLifecycle {
    Shared,
    Ephemeral,
}

impl Default for RuntimeServiceLifecycle {
    fn default() -> Self {
        Self::Shared
    }
}

impl RuntimeServiceLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Ephemeral => "ephemeral",
        }
    }
}

// ============================================================================
// resolveServiceScopeId
// ============================================================================

/// `ResolveServiceScopeIdInput`。
#[derive(Debug, Clone)]
pub struct ResolveServiceScopeIdInput<'a> {
    pub service: &'a Map<String, Value>,
    pub workspace: &'a RealizedExecutionWorkspaceRef,
    pub execution_workspace_id: Option<&'a str>,
    pub issue: Option<&'a TemplateIssueRef>,
    pub run_id: &'a str,
    pub agent: &'a TemplateAgentRef,
}

/// `ResolveServiceScopeIdOutput`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveServiceScopeIdOutput {
    pub scope_type: ReuseScopeType,
    pub scope_id: Option<String>,
}

/// `resolveServiceScopeId(input)`：解析 service reuse scope。
///
/// 与 Node 1:1 对齐：
/// - reuseScope 显式设置 → 用之（限定 4 种值）
/// - 隐式：lifecycle=ephemeral → "run" / 否则 "project_workspace"
/// - scopeId：project_workspace → projectWorkspaceId；execution_workspace → executionWorkspaceId；
///   run → runId；agent → agent.id
pub fn resolve_service_scope_id(
    input: ResolveServiceScopeIdInput<'_>,
) -> ResolveServiceScopeIdOutput {
    let lifecycle = lifecycle_from_service(input.service);
    let explicit = input
        .service
        .get("reuseScope")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let scope_type = match explicit {
        "project_workspace" => ReuseScopeType::ProjectWorkspace,
        "execution_workspace" => ReuseScopeType::ExecutionWorkspace,
        "agent" => ReuseScopeType::Agent,
        "" => {
            if lifecycle == RuntimeServiceLifecycle::Ephemeral {
                ReuseScopeType::Run
            } else {
                ReuseScopeType::ProjectWorkspace
            }
        }
        _ => {
            // 其它值 → 走 default
            if lifecycle == RuntimeServiceLifecycle::Ephemeral {
                ReuseScopeType::Run
            } else {
                ReuseScopeType::ProjectWorkspace
            }
        }
    };

    let scope_id = match scope_type {
        ReuseScopeType::ProjectWorkspace => input.workspace.branch_name.clone(),
        ReuseScopeType::ExecutionWorkspace => input.execution_workspace_id.map(|s| s.to_string()),
        ReuseScopeType::Run => Some(input.run_id.to_string()),
        ReuseScopeType::Agent => input.agent.id.clone(),
    };

    ResolveServiceScopeIdOutput {
        scope_type,
        scope_id,
    }
}

fn lifecycle_from_service(service: &Map<String, Value>) -> RuntimeServiceLifecycle {
    match service.get("lifecycle").and_then(|v| v.as_str()) {
        Some("ephemeral") => RuntimeServiceLifecycle::Ephemeral,
        _ => RuntimeServiceLifecycle::Shared,
    }
}

fn as_string_from_map(service: &Map<String, Value>, key: &str, fallback: &str) -> String {
    service
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(fallback)
        .to_string()
}

fn as_number_from_map(service: &Map<String, Value>, key: &str, fallback: i64) -> i64 {
    service
        .get(key)
        .and_then(|v| v.as_i64())
        .unwrap_or(fallback)
}

// ============================================================================
// resolveRuntimeServiceReuseIdentity
// ============================================================================

/// `ResolveRuntimeServiceReuseIdentityInput`。
#[derive(Debug, Clone)]
pub struct ResolveRuntimeServiceReuseIdentityInput<'a> {
    pub service: &'a Map<String, Value>,
    pub workspace: &'a RealizedExecutionWorkspaceRef,
    pub agent: &'a TemplateAgentRef,
    pub issue: Option<&'a TemplateIssueRef>,
    pub adapter_env: &'a Map<String, Value>,
    pub scope_type: ReuseScopeType,
    pub scope_id: Option<&'a str>,
}

/// `ResolveRuntimeServiceReuseIdentityOutput`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveRuntimeServiceReuseIdentityOutput {
    pub service_name: String,
    pub lifecycle: RuntimeServiceLifecycle,
    pub command: String,
    pub service_cwd: String,
    pub env_config: Map<String, Value>,
    pub env_fingerprint: String,
    pub explicit_port: i64,
    pub identity_port: Option<i64>,
    pub reuse_key: Option<String>,
}

/// `resolveRuntimeServiceReuseIdentity(input)`：解析 service identity / reuse key。
///
/// 与 Node 1:1 对齐：
/// - serviceName: asString(name, "service")
/// - lifecycle: lifecycle="ephemeral" ? "ephemeral" : "shared"
/// - port: 优先 parseObject(port).value，否则 raw value；<0 → null
/// - serviceCwd: template + resolveConfiguredPath
/// - envFingerprint: SHA-256 hex of stableStringify(rendered env)
/// - reuseKey: lifecycle="shared" 才计算，SHA-256 hex of stableStringify({scopeType, scopeId, ...})
pub fn resolve_runtime_service_reuse_identity(
    input: ResolveRuntimeServiceReuseIdentityInput<'_>,
) -> ResolveRuntimeServiceReuseIdentityOutput {
    let service_name = as_string_from_map(input.service, "name", "service");
    let lifecycle = lifecycle_from_service(input.service);
    let command = as_string_from_map(input.service, "command", "");
    let service_cwd_template = as_string_from_map(input.service, "cwd", ".");

    // port 解析
    let port_obj = input.service.get("port").and_then(|v| v.as_object());
    let explicit_port = match port_obj
        .and_then(|o| o.get("value"))
        .and_then(|v| v.as_i64())
    {
        Some(v) => v,
        None => as_number_from_map(input.service, "port", 0),
    };
    let identity_port = if explicit_port > 0 {
        Some(explicit_port)
    } else {
        None
    };

    let template_data = build_template_data(BuildTemplateDataInput {
        workspace: input.workspace,
        agent: input.agent,
        issue: input.issue,
        adapter_env: input.adapter_env,
        port: identity_port,
    });

    // serviceCwd 渲染 + 路径解析
    let cwd_template_rendered = crate::workspace_runtime_template_render::render_template(
        &service_cwd_template,
        &template_data,
    );
    let service_cwd = resolve_configured_path(&cwd_template_rendered, &input.workspace.cwd)
        .to_string_lossy()
        .to_string();

    // envConfig 是原值
    let env_config = input
        .service
        .get("env")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    // rendered env (string -> string)
    let rendered_env = render_runtime_service_env(
        crate::workspace_runtime_template_render::RenderRuntimeServiceEnvInput {
            env_config: &env_config,
            template_data: &template_data,
        },
    );

    let env_fingerprint = stable_fingerprint(&rendered_env);

    let reuse_key = if lifecycle == RuntimeServiceLifecycle::Shared {
        // SHA-256 hex of stableStringify({...})
        let mut reuse_obj = Map::new();
        reuse_obj.insert(
            "scopeType".into(),
            Value::String(input.scope_type.as_str().to_string()),
        );
        reuse_obj.insert(
            "scopeId".into(),
            match input.scope_id {
                Some(s) => Value::String(s.to_string()),
                None => Value::Null,
            },
        );
        reuse_obj.insert("serviceName".into(), Value::String(service_name.clone()));
        reuse_obj.insert("command".into(), Value::String(command.clone()));
        reuse_obj.insert("cwd".into(), Value::String(service_cwd.clone()));
        reuse_obj.insert(
            "port".into(),
            match identity_port {
                Some(p) => serde_json::Number::from(p).into(),
                None => Value::Null,
            },
        );
        reuse_obj.insert("env".into(), Value::Object(rendered_env));
        let s = stable_stringify(&Value::Object(reuse_obj));
        let mut hasher = Sha256::new();
        hasher.update(s.as_bytes());
        Some(format!("{:x}", hasher.finalize()))
    } else {
        None
    };

    ResolveRuntimeServiceReuseIdentityOutput {
        service_name,
        lifecycle,
        command,
        service_cwd,
        env_config,
        env_fingerprint,
        explicit_port,
        identity_port,
        reuse_key,
    }
}

// ============================================================================
// resolveWorkspaceCommandExecution
// ============================================================================

/// `ResolveWorkspaceCommandExecutionInput`。
#[derive(Debug, Clone)]
pub struct ResolveWorkspaceCommandExecutionInput<'a> {
    pub command: &'a Map<String, Value>,
    pub workspace: &'a RealizedExecutionWorkspaceRef,
    pub agent: &'a TemplateAgentRef,
    pub issue: Option<&'a TemplateIssueRef>,
    pub adapter_env: &'a Map<String, Value>,
    /// 来自 process.env 的 base env（已 sanitize），调用方负责传入。
    pub base_env: &'a Map<String, Value>,
}

/// `ResolveWorkspaceCommandExecutionOutput`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveWorkspaceCommandExecutionOutput {
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub env: Map<String, Value>,
}

/// `resolveWorkspaceCommandExecution(input)`：解析 workspace command 执行参数。
///
/// 与 Node 1:1 对齐：
/// - name: name ?? label ?? title ?? "workspace command"
/// - command: asString(command, "")
/// - cwd: 渲染 + resolveConfiguredPath(workspace.cwd)
/// - env: spread { baseEnv, adapterEnv, renderedEnv }
pub fn resolve_workspace_command_execution(
    input: ResolveWorkspaceCommandExecutionInput<'_>,
) -> ResolveWorkspaceCommandExecutionOutput {
    let name = as_string_from_map(input.command, "name", "")
        .if_empty_use(&as_string_from_map(input.command, "label", ""))
        .if_empty_use(&as_string_from_map(input.command, "title", ""))
        .if_empty_use(&"workspace command".to_string());

    let command = as_string_from_map(input.command, "command", "");

    let template_data = build_template_data(BuildTemplateDataInput {
        workspace: input.workspace,
        agent: input.agent,
        issue: input.issue,
        adapter_env: input.adapter_env,
        port: None,
    });

    let cwd_template = as_string_from_map(input.command, "cwd", ".");
    let cwd_rendered =
        crate::workspace_runtime_template_render::render_template(&cwd_template, &template_data);
    let cwd = resolve_configured_path(&cwd_rendered, &input.workspace.cwd)
        .to_string_lossy()
        .to_string();

    // env = { baseEnv, adapterEnv, renderedEnv }
    let mut env: Map<String, Value> = Map::new();
    for (k, v) in input.base_env.iter() {
        env.insert(k.clone(), v.clone());
    }
    for (k, v) in input.adapter_env.iter() {
        env.insert(k.clone(), v.clone());
    }
    let command_env = input
        .command
        .get("env")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let rendered_env = render_runtime_service_env(
        crate::workspace_runtime_template_render::RenderRuntimeServiceEnvInput {
            env_config: &command_env,
            template_data: &template_data,
        },
    );
    for (k, v) in rendered_env.iter() {
        env.insert(k.clone(), v.clone());
    }

    ResolveWorkspaceCommandExecutionOutput {
        name,
        command,
        cwd,
        env,
    }
}

trait IfEmpty {
    fn if_empty_use(&self, fallback: &String) -> String;
}
impl IfEmpty for String {
    fn if_empty_use(&self, fallback: &String) -> String {
        if self.is_empty() {
            fallback.clone()
        } else {
            self.clone()
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ws() -> RealizedExecutionWorkspaceRef {
        RealizedExecutionWorkspaceRef {
            cwd: "/repo".into(),
            branch_name: Some("feat/x".into()),
            worktree_path: Some("/wt".into()),
            repo_url: None,
            repo_ref: None,
        }
    }

    fn ag() -> TemplateAgentRef {
        TemplateAgentRef {
            id: Some("a-1".into()),
            name: "agent-1".into(),
        }
    }

    fn iss() -> TemplateIssueRef {
        TemplateIssueRef {
            id: "iss-1".into(),
            identifier: Some("PROJ-1".into()),
            title: Some("fix".into()),
        }
    }

    // ----- resolveServiceScopeId -----

    #[test]
    fn scope_id_default_shared_project_workspace() {
        let s: Map<String, Value> = Map::new();
        let out = resolve_service_scope_id(ResolveServiceScopeIdInput {
            service: &s,
            workspace: &ws(),
            execution_workspace_id: Some("ws-1"),
            issue: Some(&iss()),
            run_id: "run-1",
            agent: &ag(),
        });
        assert_eq!(out.scope_type, ReuseScopeType::ProjectWorkspace);
        assert_eq!(out.scope_id.as_deref(), Some("feat/x"));
    }

    #[test]
    fn scope_id_default_ephemeral_run() {
        let mut s = Map::new();
        s.insert("lifecycle".into(), Value::String("ephemeral".into()));
        let out = resolve_service_scope_id(ResolveServiceScopeIdInput {
            service: &s,
            workspace: &ws(),
            execution_workspace_id: Some("ws-1"),
            issue: Some(&iss()),
            run_id: "run-1",
            agent: &ag(),
        });
        assert_eq!(out.scope_type, ReuseScopeType::Run);
        assert_eq!(out.scope_id.as_deref(), Some("run-1"));
    }

    #[test]
    fn scope_id_explicit_execution_workspace() {
        let mut s = Map::new();
        s.insert(
            "reuseScope".into(),
            Value::String("execution_workspace".into()),
        );
        let out = resolve_service_scope_id(ResolveServiceScopeIdInput {
            service: &s,
            workspace: &ws(),
            execution_workspace_id: Some("ws-1"),
            issue: Some(&iss()),
            run_id: "run-1",
            agent: &ag(),
        });
        assert_eq!(out.scope_type, ReuseScopeType::ExecutionWorkspace);
        assert_eq!(out.scope_id.as_deref(), Some("ws-1"));
    }

    #[test]
    fn scope_id_explicit_agent() {
        let mut s = Map::new();
        s.insert("reuseScope".into(), Value::String("agent".into()));
        let out = resolve_service_scope_id(ResolveServiceScopeIdInput {
            service: &s,
            workspace: &ws(),
            execution_workspace_id: Some("ws-1"),
            issue: Some(&iss()),
            run_id: "run-1",
            agent: &ag(),
        });
        assert_eq!(out.scope_type, ReuseScopeType::Agent);
        assert_eq!(out.scope_id.as_deref(), Some("a-1"));
    }

    #[test]
    fn scope_id_unknown_falls_back_to_default() {
        let mut s = Map::new();
        s.insert("reuseScope".into(), Value::String("weird".into()));
        let out = resolve_service_scope_id(ResolveServiceScopeIdInput {
            service: &s,
            workspace: &ws(),
            execution_workspace_id: Some("ws-1"),
            issue: Some(&iss()),
            run_id: "run-1",
            agent: &ag(),
        });
        // "weird" 不匹配任何 → 走 default（shared → project_workspace）
        assert_eq!(out.scope_type, ReuseScopeType::ProjectWorkspace);
    }

    // ----- resolveRuntimeServiceReuseIdentity -----

    #[test]
    fn reuse_identity_basic() {
        let mut s = Map::new();
        s.insert("name".into(), Value::String("web".into()));
        s.insert("command".into(), Value::String("pnpm dev".into()));
        let mut port = Map::new();
        port.insert(
            "value".into(),
            Value::Number(serde_json::Number::from(3000)),
        );
        s.insert("port".into(), Value::Object(port));
        let adapter_env = Map::new();
        let out = resolve_runtime_service_reuse_identity(ResolveRuntimeServiceReuseIdentityInput {
            service: &s,
            workspace: &ws(),
            agent: &ag(),
            issue: None,
            adapter_env: &adapter_env,
            scope_type: ReuseScopeType::ProjectWorkspace,
            scope_id: Some("feat/x"),
        });
        assert_eq!(out.service_name, "web");
        assert_eq!(out.lifecycle, RuntimeServiceLifecycle::Shared);
        assert_eq!(out.command, "pnpm dev");
        assert_eq!(out.explicit_port, 3000);
        assert_eq!(out.identity_port, Some(3000));
        assert!(out.reuse_key.is_some());
        assert_eq!(out.reuse_key.as_ref().unwrap().len(), 64);
    }

    #[test]
    fn reuse_identity_ephemeral_no_reuse_key() {
        let mut s = Map::new();
        s.insert("lifecycle".into(), Value::String("ephemeral".into()));
        let adapter_env = Map::new();
        let out = resolve_runtime_service_reuse_identity(ResolveRuntimeServiceReuseIdentityInput {
            service: &s,
            workspace: &ws(),
            agent: &ag(),
            issue: None,
            adapter_env: &adapter_env,
            scope_type: ReuseScopeType::Run,
            scope_id: Some("run-1"),
        });
        assert_eq!(out.lifecycle, RuntimeServiceLifecycle::Ephemeral);
        assert!(out.reuse_key.is_none());
    }

    #[test]
    fn reuse_identity_zero_port_no_identity_port() {
        let s: Map<String, Value> = Map::new();
        let adapter_env = Map::new();
        let out = resolve_runtime_service_reuse_identity(ResolveRuntimeServiceReuseIdentityInput {
            service: &s,
            workspace: &ws(),
            agent: &ag(),
            issue: None,
            adapter_env: &adapter_env,
            scope_type: ReuseScopeType::ProjectWorkspace,
            scope_id: None,
        });
        assert_eq!(out.identity_port, None);
        assert_eq!(out.explicit_port, 0);
    }

    #[test]
    fn reuse_identity_service_name_default() {
        let s: Map<String, Value> = Map::new();
        let adapter_env = Map::new();
        let out = resolve_runtime_service_reuse_identity(ResolveRuntimeServiceReuseIdentityInput {
            service: &s,
            workspace: &ws(),
            agent: &ag(),
            issue: None,
            adapter_env: &adapter_env,
            scope_type: ReuseScopeType::ProjectWorkspace,
            scope_id: None,
        });
        assert_eq!(out.service_name, "service");
    }

    #[test]
    fn reuse_identity_reuse_key_stable() {
        let mut s = Map::new();
        s.insert("name".into(), Value::String("web".into()));
        let adapter_env = Map::new();
        let mk = |scope_id: Option<&str>| {
            resolve_runtime_service_reuse_identity(ResolveRuntimeServiceReuseIdentityInput {
                service: &s,
                workspace: &ws(),
                agent: &ag(),
                issue: None,
                adapter_env: &adapter_env,
                scope_type: ReuseScopeType::ProjectWorkspace,
                scope_id,
            })
        };
        let a = mk(Some("feat/x"));
        let b = mk(Some("feat/x"));
        let c = mk(Some("feat/y"));
        assert_eq!(a.reuse_key, b.reuse_key);
        assert_ne!(a.reuse_key, c.reuse_key);
    }

    // ----- resolveWorkspaceCommandExecution -----

    #[test]
    fn workspace_command_basic() {
        let mut cmd = Map::new();
        cmd.insert("name".into(), Value::String("install".into()));
        cmd.insert("command".into(), Value::String("pnpm install".into()));
        cmd.insert("cwd".into(), Value::String("{{workspace.cwd}}".into()));
        let base_env = Map::new();
        let adapter_env = Map::new();
        let out = resolve_workspace_command_execution(ResolveWorkspaceCommandExecutionInput {
            command: &cmd,
            workspace: &ws(),
            agent: &ag(),
            issue: None,
            adapter_env: &adapter_env,
            base_env: &base_env,
        });
        assert_eq!(out.name, "install");
        assert_eq!(out.command, "pnpm install");
        assert_eq!(out.cwd, "/repo");
    }

    #[test]
    fn workspace_command_default_name() {
        let cmd: Map<String, Value> = Map::new();
        let base_env = Map::new();
        let adapter_env = Map::new();
        let out = resolve_workspace_command_execution(ResolveWorkspaceCommandExecutionInput {
            command: &cmd,
            workspace: &ws(),
            agent: &ag(),
            issue: None,
            adapter_env: &adapter_env,
            base_env: &base_env,
        });
        assert_eq!(out.name, "workspace command");
        assert_eq!(out.command, "");
    }

    #[test]
    fn workspace_command_name_fallback_label_title() {
        let mut cmd = Map::new();
        cmd.insert("title".into(), Value::String("from-title".into()));
        let base_env = Map::new();
        let adapter_env = Map::new();
        let out = resolve_workspace_command_execution(ResolveWorkspaceCommandExecutionInput {
            command: &cmd,
            workspace: &ws(),
            agent: &ag(),
            issue: None,
            adapter_env: &adapter_env,
            base_env: &base_env,
        });
        assert_eq!(out.name, "from-title");
    }

    #[test]
    fn workspace_command_env_layers() {
        let mut cmd = Map::new();
        let mut env_cfg = Map::new();
        env_cfg.insert("RENDERED".into(), Value::String("{{agent.name}}".into()));
        cmd.insert("env".into(), Value::Object(env_cfg));

        let mut base_env = Map::new();
        base_env.insert("BASE".into(), Value::String("b".into()));
        let mut adapter_env = Map::new();
        adapter_env.insert("ADAPTER".into(), Value::String("a".into()));

        let out = resolve_workspace_command_execution(ResolveWorkspaceCommandExecutionInput {
            command: &cmd,
            workspace: &ws(),
            agent: &ag(),
            issue: None,
            adapter_env: &adapter_env,
            base_env: &base_env,
        });
        assert_eq!(out.env.get("BASE").unwrap(), &json!("b"));
        assert_eq!(out.env.get("ADAPTER").unwrap(), &json!("a"));
        assert_eq!(out.env.get("RENDERED").unwrap(), &json!("agent-1"));
    }

    #[test]
    fn workspace_command_relative_cwd() {
        let mut cmd = Map::new();
        cmd.insert("cwd".into(), Value::String("./subdir".into()));
        let base_env = Map::new();
        let adapter_env = Map::new();
        let out = resolve_workspace_command_execution(ResolveWorkspaceCommandExecutionInput {
            command: &cmd,
            workspace: &ws(),
            agent: &ag(),
            issue: None,
            adapter_env: &adapter_env,
            base_env: &base_env,
        });
        assert!(out.cwd.starts_with("/repo"));
        assert!(out.cwd.ends_with("subdir"));
    }
}
