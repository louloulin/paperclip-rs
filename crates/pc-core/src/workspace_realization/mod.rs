//! `workspace_realization` 域（Round 265）。
//!
//! 与原 `paperclip/server/src/services/workspace-realization.ts` 1:1 对齐：
//! - 解析 `WorkspaceRealizationRequest`（从 `unknown` JSON 反序列化）
//! - 构建 `WorkspaceRealizationRequest`（从已知的 `RealizedExecutionWorkspace` + config）
//! - 构建 `WorkspaceRealizationRecord`（从 environment + lease + request + 驱动 metadata）
//! - 入口：驱动返回 raw cwd/providerMetadata 时回填为完整 record
//!
//! 设计目标：高内聚低耦合。
//! - **高内聚**：本模块是纯函数（解析/构建/判断 transport+mode）。零 IO、零 DB。
//! - **低耦合**：只依赖 `serde` / `serde_json` / `chrono`。调用方传 `Environment`/`EnvironmentLease` 等
//!   简化结构，不耦合 pc-db。
//!
//! 单一职责：把 driver 的 `cwd + providerMetadata` 投影成可被 UI / Agent 使用的 realization record。
//! 不创建工作区，不执行命令，不持有状态。

pub mod types;

pub use types::{
    Environment, EnvironmentLease, ExecutionWorkspaceConfig, RealizedAdditionalWorkspace,
    RealizedExecutionWorkspace, WorkspaceDriverWorkspace, WorkspaceRealizationBootstrap,
    WorkspaceRealizationLocalSource, WorkspaceRealizationMode, WorkspaceRealizationPathAlias,
    WorkspaceRealizationRebuild, WorkspaceRealizationRecord, WorkspaceRealizationRequest,
    WorkspaceRealizationRequestSource, WorkspaceRealizationSync, WorkspaceRealizationSyncStrategy,
    WorkspaceRealizationTransport, WorkspaceRuntimeOverlay,
};

// ============================================================================
// 工具函数（与 Node 中同名函数 1:1 对齐）
// ============================================================================

/// 把 `value` 规范为 `Record<string, unknown>`：非对象 → `{}`。
pub fn parse_object(
    value: Option<&serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    match value {
        Some(serde_json::Value::Object(map)) => map.clone(),
        _ => serde_json::Map::new(),
    }
}

/// 读取 `value`，trim 后非空字符串；否则返回 None。
pub fn read_string(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 读取有限 `number`，否则 None。
pub fn read_number(value: Option<&serde_json::Value>) -> Option<f64> {
    value.and_then(|v| v.as_f64()).filter(|n| n.is_finite())
}

/// 读取 `string[]`：每项 trim+非空校验。
pub fn read_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter().filter_map(|v| read_string(Some(v))).collect()
}

/// 读取 `Array<{ path, target }>`，丢弃任何缺字段项。
pub fn read_path_aliases(value: Option<&serde_json::Value>) -> Vec<WorkspaceRealizationPathAlias> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|entry| {
            let obj = parse_object(Some(entry));
            let path = read_string(obj.get("path"))?;
            let target = read_string(obj.get("target"))?;
            Some(WorkspaceRealizationPathAlias { path, target })
        })
        .collect()
}

/// 读取 additional sources 数组，丢弃没有 `localPath` 的项。
pub fn read_additional_sources(
    value: Option<&serde_json::Value>,
) -> Vec<WorkspaceRealizationRequestSource> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|entry| {
            let obj = parse_object(Some(entry));
            let local_path = read_string(obj.get("localPath"))?;
            Some(WorkspaceRealizationRequestSource {
                kind: "project_primary".to_string(),
                local_path,
                project_id: read_string(obj.get("projectId")),
                project_workspace_id: read_string(obj.get("projectWorkspaceId")),
                repo_url: read_string(obj.get("repoUrl")),
                repo_ref: read_string(obj.get("repoRef")),
                strategy: "project_primary".to_string(),
                branch_name: None,
                worktree_path: None,
            })
        })
        .collect()
}

// ============================================================================
// 入口 1：从 unknown 反序列化 Request
// ============================================================================

/// 解析 `value` 为 `WorkspaceRealizationRequest`；版本不匹配或关键字段缺失返回 None。
pub fn read_workspace_realization_request(
    value: Option<&serde_json::Value>,
) -> Option<WorkspaceRealizationRequest> {
    let parsed = parse_object(value);
    if parsed.get("version").and_then(|v| v.as_i64()) != Some(1) {
        return None;
    }
    let source = parse_object(parsed.get("source"));
    let runtime_overlay = parse_object(parsed.get("runtimeOverlay"));
    let local_path = read_string(source.get("localPath"))?;
    let company_id = read_string(parsed.get("companyId"))?;
    let environment_id = read_string(parsed.get("environmentId"))?;
    let heartbeat_run_id = read_string(parsed.get("heartbeatRunId"))?;
    let adapter_type = read_string(parsed.get("adapterType"))?;

    let kind = match source.get("kind").and_then(|v| v.as_str()) {
        Some("task_session") => "task_session".to_string(),
        Some("agent_home") => "agent_home".to_string(),
        _ => "project_primary".to_string(),
    };
    let strategy = match source.get("strategy").and_then(|v| v.as_str()) {
        Some("git_worktree") => "git_worktree".to_string(),
        _ => "project_primary".to_string(),
    };

    let source_struct = WorkspaceRealizationRequestSource {
        kind,
        local_path,
        project_id: read_string(source.get("projectId")),
        project_workspace_id: read_string(source.get("projectWorkspaceId")),
        repo_url: read_string(source.get("repoUrl")),
        repo_ref: read_string(source.get("repoRef")),
        strategy,
        branch_name: read_string(source.get("branchName")),
        worktree_path: read_string(source.get("worktreePath")),
    };

    let overlay_obj = parse_object(runtime_overlay.get("workspaceRuntime"));
    let workspace_runtime = if overlay_obj.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(overlay_obj))
    };

    Some(WorkspaceRealizationRequest {
        version: 1,
        adapter_type,
        company_id,
        environment_id,
        execution_workspace_id: read_string(parsed.get("executionWorkspaceId")),
        issue_id: read_string(parsed.get("issueId")),
        heartbeat_run_id,
        requested_mode: read_string(parsed.get("requestedMode")),
        source: source_struct,
        additional_sources: read_additional_sources(parsed.get("additionalSources")),
        runtime_overlay: WorkspaceRuntimeOverlay {
            provision_command: read_string(runtime_overlay.get("provisionCommand")),
            teardown_command: read_string(runtime_overlay.get("teardownCommand")),
            cleanup_command: read_string(runtime_overlay.get("cleanupCommand")),
            workspace_runtime,
        },
    })
}

// ============================================================================
// 入口 2：从已知结构构建 Request
// ============================================================================

/// 从 `RealizedExecutionWorkspace` + `ExecutionWorkspaceConfig` 构建 `WorkspaceRealizationRequest`。
pub fn build_workspace_realization_request(
    input: BuildRequestInput<'_>,
) -> WorkspaceRealizationRequest {
    let additional = input
        .workspace
        .additional_workspaces
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|w| WorkspaceRealizationRequestSource {
            kind: w.source.clone(),
            local_path: w.cwd.clone(),
            project_id: w.project_id.clone(),
            project_workspace_id: w.workspace_id.clone(),
            repo_url: w.repo_url.clone(),
            repo_ref: w.repo_ref.clone(),
            // additional sources 永远不参与 git-worktree realization：固定 strategy
            strategy: "project_primary".to_string(),
            branch_name: None,
            worktree_path: None,
        })
        .collect();

    WorkspaceRealizationRequest {
        version: 1,
        adapter_type: input.adapter_type.to_string(),
        company_id: input.company_id.to_string(),
        environment_id: input.environment_id.to_string(),
        execution_workspace_id: input.execution_workspace_id.map(|s| s.to_string()),
        issue_id: input.issue_id.map(|s| s.to_string()),
        heartbeat_run_id: input.heartbeat_run_id.to_string(),
        requested_mode: input.requested_mode.map(|s| s.to_string()),
        source: WorkspaceRealizationRequestSource {
            kind: input.workspace.source.clone(),
            local_path: input.workspace.cwd.clone(),
            project_id: input.workspace.project_id.clone(),
            project_workspace_id: input.workspace.workspace_id.clone(),
            repo_url: input.workspace.repo_url.clone(),
            repo_ref: input.workspace.repo_ref.clone(),
            strategy: input.workspace.strategy.clone(),
            branch_name: input.workspace.branch_name.clone(),
            worktree_path: input.workspace.worktree_path.clone(),
        },
        additional_sources: additional,
        runtime_overlay: WorkspaceRuntimeOverlay {
            provision_command: input
                .workspace_config
                .and_then(|c| c.provision_command.clone()),
            teardown_command: input
                .workspace_config
                .and_then(|c| c.teardown_command.clone()),
            cleanup_command: input
                .workspace_config
                .and_then(|c| c.cleanup_command.clone()),
            workspace_runtime: input
                .workspace_config
                .and_then(|c| c.workspace_runtime.clone()),
        },
    }
}

#[derive(Debug)]
pub struct BuildRequestInput<'a> {
    pub adapter_type: &'a str,
    pub company_id: &'a str,
    pub environment_id: &'a str,
    pub execution_workspace_id: Option<&'a str>,
    pub issue_id: Option<&'a str>,
    pub heartbeat_run_id: &'a str,
    pub requested_mode: Option<&'a str>,
    pub workspace: &'a RealizedExecutionWorkspace,
    pub workspace_config: Option<&'a ExecutionWorkspaceConfig>,
}

// ============================================================================
// 入口 3：构建 Record
// ============================================================================

fn derive_transport(driver: &str) -> WorkspaceRealizationTransport {
    match driver {
        "ssh" => "ssh",
        "sandbox" => "sandbox",
        "plugin" => "plugin",
        _ => "local",
    }
    .to_string()
}

fn derive_mode(metadata: &serde_json::Map<String, serde_json::Value>) -> WorkspaceRealizationMode {
    let v = metadata
        .get("mode")
        .or_else(|| metadata.get("realizationMode"));
    match v.and_then(|x| x.as_str()) {
        Some("in_place") => "in_place".to_string(),
        _ => "copy".to_string(),
    }
}

fn derive_sync(
    transport: &WorkspaceRealizationTransport,
    mode: &WorkspaceRealizationMode,
) -> WorkspaceRealizationSync {
    if mode == "in_place" || transport == "local" {
        return WorkspaceRealizationSync {
            strategy: "none".to_string(),
            prepare: "Use the realized local execution workspace directly.".to_string(),
            sync_back: None,
        };
    }
    match transport.as_str() {
        "ssh" => WorkspaceRealizationSync {
            strategy: "ssh_git_import_export".to_string(),
            prepare: "Import the local git workspace to the remote SSH workspace before adapter execution."
                .to_string(),
            sync_back: Some(
                "Export remote SSH workspace changes back to the local execution workspace after adapter execution."
                    .to_string(),
            ),
        },
        "sandbox" => WorkspaceRealizationSync {
            strategy: "sandbox_archive_upload_download".to_string(),
            prepare: "Upload a workspace archive into the sandbox filesystem before adapter execution."
                .to_string(),
            sync_back: Some(
                "Download a workspace archive from the sandbox and mirror it back locally after adapter execution."
                    .to_string(),
            ),
        },
        _ => WorkspaceRealizationSync {
            strategy: "provider_defined".to_string(),
            prepare: "Delegate workspace materialization to the plugin environment driver."
                .to_string(),
            sync_back: Some(
                "Delegate result synchronization to the plugin environment driver.".to_string(),
            ),
        },
    }
}

fn build_summary(
    transport: &WorkspaceRealizationTransport,
    realized_cwd: Option<&str>,
    remote_path: Option<&str>,
    username: Option<&str>,
    host: Option<&str>,
    port: Option<f64>,
    sandbox_id: Option<&str>,
    local_path: &str,
) -> String {
    match transport.as_str() {
        "local" => format!("Local workspace realized at {local_path}."),
        "ssh" => {
            let username = username.unwrap_or("user");
            let host = host.unwrap_or("host");
            let port = port.map(|p| p as u64).unwrap_or(22);
            let remote_path = remote_path.unwrap_or(local_path);
            format!("SSH workspace realized at {username}@{host}:{port}:{remote_path}.")
        }
        "sandbox" => {
            let remote_path = remote_path.unwrap_or("/");
            let suffix = match sandbox_id {
                Some(id) => format!(" in {id}"),
                None => String::new(),
            };
            format!("Sandbox workspace realized at {remote_path}{suffix}.")
        }
        _ => {
            let fallback = realized_cwd.or(remote_path).unwrap_or(local_path);
            format!("Plugin workspace realized at {fallback}.")
        }
    }
}

fn derive_provider(
    lease: &EnvironmentLease,
    transport: &WorkspaceRealizationTransport,
) -> Option<String> {
    if let Some(p) = &lease.provider {
        return Some(p.clone());
    }
    match transport.as_str() {
        "ssh" => Some("ssh".to_string()),
        "local" => Some("local".to_string()),
        _ => None,
    }
}

#[derive(Debug)]
pub struct BuildRecordInput<'a> {
    pub environment: &'a Environment,
    pub lease: &'a EnvironmentLease,
    pub request: &'a WorkspaceRealizationRequest,
    pub realized_cwd: Option<&'a str>,
    pub provider_metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// 从 environment + lease + request + 驱动 metadata 构建 `WorkspaceRealizationRecord`。
pub fn build_workspace_realization_record(
    input: BuildRecordInput<'_>,
) -> WorkspaceRealizationRecord {
    let lease_metadata = parse_object(input.lease.metadata.as_ref());
    let provider_metadata = input.provider_metadata.clone().unwrap_or_default();
    let transport = derive_transport(&input.environment.driver);
    let remote_path = read_string(provider_metadata.get("remoteCwd"))
        .or_else(|| read_string(lease_metadata.get("remoteCwd")))
        .or_else(|| read_string(provider_metadata.get("remotePath")));
    let host = read_string(lease_metadata.get("host"));
    let port = read_number(lease_metadata.get("port"));
    let username = read_string(lease_metadata.get("username"));
    let sandbox_id = read_string(lease_metadata.get("sandboxId"))
        .or_else(|| read_string(provider_metadata.get("sandboxId")));

    // realizationMetadata = {...lease.workspaceRealization, ...provider.workspaceRealization, ...providerMetadata}
    let mut realization_metadata = serde_json::Map::new();
    if let Some(v) = lease_metadata.get("workspaceRealization") {
        if let serde_json::Value::Object(map) = v {
            realization_metadata.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    }
    if let Some(v) = provider_metadata.get("workspaceRealization") {
        if let serde_json::Value::Object(map) = v {
            realization_metadata.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    }
    realization_metadata.extend(
        provider_metadata
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );

    let mode = derive_mode(&realization_metadata);
    let authoritative_root = read_string(realization_metadata.get("authoritativeRoot"))
        .or_else(|| {
            if mode == "in_place" {
                remote_path.clone()
            } else {
                None
            }
        })
        .unwrap_or_else(|| input.request.source.local_path.clone());

    let path_aliases = read_path_aliases(
        realization_metadata
            .get("pathAliases")
            .or_else(|| realization_metadata.get("workspaceAliases")),
    );
    let outbound_restore_paths =
        read_string_array(realization_metadata.get("outboundRestorePaths"));

    let sync = derive_sync(&transport, &mode);
    let provider = derive_provider(input.lease, &transport);
    let local_path = input.request.source.local_path.clone();

    let summary = build_summary(
        &transport,
        input.realized_cwd,
        remote_path.as_deref(),
        username.as_deref(),
        host.as_deref(),
        port,
        sandbox_id.as_deref(),
        &local_path,
    );

    let additional = input
        .request
        .additional_sources
        .iter()
        .map(|src| WorkspaceRealizationLocalSource {
            path: src.local_path.clone(),
            source: src.kind.clone(),
            strategy: src.strategy.clone(),
            project_id: src.project_id.clone(),
            project_workspace_id: src.project_workspace_id.clone(),
            repo_url: src.repo_url.clone(),
            repo_ref: src.repo_ref.clone(),
            branch_name: src.branch_name.clone(),
            worktree_path: src.worktree_path.clone(),
        })
        .collect();

    let mut remote_obj = serde_json::Map::new();
    remote_obj.insert(
        "path".to_string(),
        serde_json::Value::from(remote_path.clone()),
    );
    if let Some(h) = &host {
        remote_obj.insert("host".to_string(), serde_json::Value::from(h.clone()));
    }
    if let Some(p) = port {
        remote_obj.insert("port".to_string(), serde_json::json!(p));
    }
    if let Some(u) = &username {
        remote_obj.insert("username".to_string(), serde_json::Value::from(u.clone()));
    }
    if let Some(s) = &sandbox_id {
        remote_obj.insert("sandboxId".to_string(), serde_json::Value::from(s.clone()));
    }

    let mut rebuild_metadata = serde_json::Map::new();
    rebuild_metadata.insert(
        "source".to_string(),
        serde_json::to_value(&input.request.source).unwrap_or(serde_json::Value::Null),
    );
    rebuild_metadata.insert(
        "runtimeOverlay".to_string(),
        serde_json::to_value(&input.request.runtime_overlay).unwrap_or(serde_json::Value::Null),
    );
    rebuild_metadata.insert(
        "environmentDriver".to_string(),
        serde_json::Value::from(input.environment.driver.clone()),
    );
    rebuild_metadata.insert(
        "provider".to_string(),
        provider
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    if !provider_metadata.is_empty() {
        rebuild_metadata.insert(
            "providerMetadata".to_string(),
            serde_json::Value::Object(provider_metadata.clone()),
        );
    }

    WorkspaceRealizationRecord {
        version: 1,
        mode,
        authoritative_root,
        path_aliases,
        outbound_restore_paths,
        transport: transport.clone(),
        provider: provider.clone(),
        environment_id: input.environment.id.clone(),
        lease_id: input.lease.id.clone(),
        provider_lease_id: input.lease.provider_lease_id.clone(),
        local: WorkspaceRealizationLocalSource {
            path: local_path,
            source: input.request.source.kind.clone(),
            strategy: input.request.source.strategy.clone(),
            project_id: input.request.source.project_id.clone(),
            project_workspace_id: input.request.source.project_workspace_id.clone(),
            repo_url: input.request.source.repo_url.clone(),
            repo_ref: input.request.source.repo_ref.clone(),
            branch_name: input.request.source.branch_name.clone(),
            worktree_path: input.request.source.worktree_path.clone(),
        },
        additional,
        remote: serde_json::Value::Object(remote_obj),
        sync: WorkspaceRealizationSync {
            strategy: sync.strategy,
            prepare: sync.prepare,
            sync_back: sync.sync_back,
        },
        bootstrap: WorkspaceRealizationBootstrap {
            command: input.request.runtime_overlay.provision_command.clone(),
        },
        rebuild: WorkspaceRealizationRebuild {
            execution_workspace_id: input.request.execution_workspace_id.clone(),
            mode: input.request.requested_mode.clone(),
            repo_url: input.request.source.repo_url.clone(),
            repo_ref: input.request.source.repo_ref.clone(),
            local_path: input.request.source.local_path.clone(),
            remote_path: remote_path.clone(),
            provider_lease_id: input.lease.provider_lease_id.clone(),
            metadata: rebuild_metadata,
        },
        summary,
    }
}

// ============================================================================
// 入口 4：从 driver 入口的最小输入构建完整 Record
// ============================================================================

/// 从驱动（plugin/ssh/sandbox）返回的最小信息构建 `WorkspaceRealizationRecord`。
pub fn build_workspace_realization_record_from_driver_input(
    input: DriverInput,
) -> Result<WorkspaceRealizationRecord, RealizationRequestError> {
    let workspace_meta = input.workspace.metadata.as_ref();
    let request_opt: Option<WorkspaceRealizationRequest> = read_workspace_realization_request(
        workspace_meta.and_then(|m| m.get("workspaceRealizationRequest")),
    )
    .or_else(|| read_workspace_realization_request(workspace_meta.and_then(|m| m.get("request"))));
    let request = match request_opt {
        Some(r) => r,
        None => {
            let cwd = input
                .workspace
                .local_path
                .clone()
                .or_else(|| input.cwd.clone())
                .or_else(|| input.workspace.remote_path.clone())
                .unwrap_or_else(|| "/".to_string());
            WorkspaceRealizationRequest {
                version: 1,
                adapter_type: "unknown".to_string(),
                company_id: input.lease.company_id.clone(),
                environment_id: input.environment.id.clone(),
                execution_workspace_id: input.lease.execution_workspace_id.clone(),
                issue_id: input.lease.issue_id.clone(),
                heartbeat_run_id: input
                    .lease
                    .heartbeat_run_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                requested_mode: input.workspace.mode.clone(),
                source: WorkspaceRealizationRequestSource {
                    kind: "task_session".to_string(),
                    local_path: cwd,
                    project_id: None,
                    project_workspace_id: None,
                    repo_url: None,
                    repo_ref: None,
                    strategy: "project_primary".to_string(),
                    branch_name: None,
                    worktree_path: None,
                },
                additional_sources: Vec::new(),
                runtime_overlay: WorkspaceRuntimeOverlay {
                    provision_command: None,
                    teardown_command: None,
                    cleanup_command: None,
                    workspace_runtime: None,
                },
            }
        }
    };

    Ok(build_workspace_realization_record(BuildRecordInput {
        environment: &input.environment,
        lease: &input.lease,
        request: &request,
        realized_cwd: input.cwd.as_deref(),
        provider_metadata: input.provider_metadata.clone(),
    }))
}

#[derive(Debug, thiserror::Error)]
pub enum RealizationRequestError {
    #[error("driver input missing lease.companyId")]
    MissingCompanyId,
}

/// Driver-side 最小输入。
#[derive(Debug)]
pub struct DriverInput {
    pub environment: Environment,
    pub lease: EnvironmentLease,
    pub workspace: WorkspaceDriverWorkspace,
    pub cwd: Option<String>,
    pub provider_metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_environment() -> Environment {
        Environment {
            id: "env-1".to_string(),
            driver: "local".to_string(),
            name: None,
        }
    }

    fn sample_lease_local() -> EnvironmentLease {
        EnvironmentLease {
            id: "lease-1".to_string(),
            company_id: "company-1".to_string(),
            execution_workspace_id: Some("exec-1".to_string()),
            issue_id: Some("issue-1".to_string()),
            heartbeat_run_id: Some("run-1".to_string()),
            provider: Some("local".to_string()),
            provider_lease_id: None,
            metadata: None,
        }
    }

    #[test]
    fn parse_object_handles_non_object() {
        assert!(parse_object(None).is_empty());
        assert!(parse_object(Some(&json!("x"))).is_empty());
        let m = parse_object(Some(&json!({"a": 1})));
        assert_eq!(m.get("a").and_then(|v| v.as_i64()), Some(1));
    }

    #[test]
    fn read_string_trims_and_filters_empty() {
        assert_eq!(read_string(None), None);
        assert_eq!(read_string(Some(&json!(""))), None);
        assert_eq!(read_string(Some(&json!("  "))), None);
        assert_eq!(read_string(Some(&json!(" hi "))), Some("hi".to_string()));
        assert_eq!(read_string(Some(&json!(42))), None);
    }

    #[test]
    fn read_number_filters_non_finite() {
        assert_eq!(read_number(None), None);
        assert_eq!(read_number(Some(&json!(1.5))), Some(1.5));
        assert_eq!(read_number(Some(&json!(42))), Some(42.0));
        assert_eq!(read_number(Some(&json!(f64::NAN))), None);
        assert_eq!(read_number(Some(&json!("x"))), None);
    }

    #[test]
    fn read_string_array_filters_empty() {
        let v = json!([1, "ok", "", "  ", "world"]);
        let out = read_string_array(Some(&v));
        assert_eq!(out, vec!["ok".to_string(), "world".to_string()]);
    }

    #[test]
    fn read_path_aliases_filters_incomplete() {
        let v = json!([
            {"path": "/a", "target": "/b"},
            {"path": "/c"},
            {"target": "/d"},
            {}
        ]);
        let out = read_path_aliases(Some(&v));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "/a");
        assert_eq!(out[0].target, "/b");
    }

    #[test]
    fn read_additional_sources_filters_no_local_path() {
        let v = json!([
            {"localPath": "/p1", "projectId": "prj-1"},
            {"projectId": "no-path"}
        ]);
        let out = read_additional_sources(Some(&v));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].local_path, "/p1");
        assert_eq!(out[0].project_id.as_deref(), Some("prj-1"));
    }

    #[test]
    fn read_request_rejects_wrong_version() {
        let v = json!({"version": 2, "companyId": "c"});
        assert!(read_workspace_realization_request(Some(&v)).is_none());
    }

    #[test]
    fn read_request_rejects_missing_required() {
        let v = json!({"version": 1});
        assert!(read_workspace_realization_request(Some(&v)).is_none());
        let v = json!({
            "version": 1, "companyId": "c", "environmentId": "e",
            "heartbeatRunId": "h", "adapterType": "a"
        });
        assert!(read_workspace_realization_request(Some(&v)).is_none());
    }

    #[test]
    fn read_request_parses_full_payload() {
        let v = json!({
            "version": 1,
            "adapterType": "codex-local",
            "companyId": "company-1",
            "environmentId": "env-1",
            "executionWorkspaceId": "exec-1",
            "issueId": "issue-1",
            "heartbeatRunId": "run-1",
            "requestedMode": "operator_branch",
            "source": {
                "kind": "project_primary",
                "localPath": "/tmp/ws",
                "projectId": "proj-1",
                "projectWorkspaceId": "pws-1",
                "repoUrl": "https://example.com/repo.git",
                "repoRef": "main",
                "strategy": "git_worktree",
                "branchName": "feat/x",
                "worktreePath": "/tmp/ws/feat-x"
            },
            "additionalSources": [
                {"localPath": "/tmp/ref1", "projectId": "p2"}
            ],
            "runtimeOverlay": {
                "provisionCommand": "pnpm i",
                "teardownCommand": "rm -rf",
                "cleanupCommand": null,
                "workspaceRuntime": {"key": "value"}
            }
        });
        let req = read_workspace_realization_request(Some(&v)).expect("parse");
        assert_eq!(req.adapter_type, "codex-local");
        assert_eq!(req.source.kind, "project_primary");
        assert_eq!(req.source.strategy, "git_worktree");
        assert_eq!(
            req.runtime_overlay.provision_command.as_deref(),
            Some("pnpm i")
        );
        assert!(req.runtime_overlay.workspace_runtime.is_some());
        assert_eq!(req.additional_sources.len(), 1);
    }

    #[test]
    fn read_request_normalizes_kind_and_strategy() {
        let v = json!({
            "version": 1,
            "adapterType": "a",
            "companyId": "c",
            "environmentId": "e",
            "heartbeatRunId": "h",
            "source": {
                "kind": "future_unknown_kind",
                "localPath": "/x",
                "strategy": "future_unknown_strategy"
            }
        });
        let req = read_workspace_realization_request(Some(&v)).expect("parse");
        assert_eq!(req.source.kind, "project_primary");
        assert_eq!(req.source.strategy, "project_primary");
    }

    #[test]
    fn build_request_maps_workspace_fields() {
        let workspace = RealizedExecutionWorkspace {
            cwd: "/tmp/w".to_string(),
            source: "project_primary".to_string(),
            project_id: Some("p".to_string()),
            workspace_id: Some("ws".to_string()),
            repo_url: Some("https://x".to_string()),
            repo_ref: Some("main".to_string()),
            strategy: "git_worktree".to_string(),
            branch_name: Some("feat/x".to_string()),
            worktree_path: Some("/tmp/w/fx".to_string()),
            additional_workspaces: None,
        };
        let cfg = ExecutionWorkspaceConfig {
            environment_id: Some("env-1".to_string()),
            provision_command: Some("pnpm i".to_string()),
            teardown_command: Some("rm".to_string()),
            cleanup_command: Some("clean".to_string()),
            workspace_runtime: Some(serde_json::json!({"k": "v"})),
            desired_state: Some("running".to_string()),
            service_states: None,
        };
        let req = build_workspace_realization_request(BuildRequestInput {
            adapter_type: "codex",
            company_id: "c1",
            environment_id: "e1",
            execution_workspace_id: Some("x"),
            issue_id: Some("i"),
            heartbeat_run_id: "h1",
            requested_mode: Some("isolated_workspace"),
            workspace: &workspace,
            workspace_config: Some(&cfg),
        });
        assert_eq!(req.adapter_type, "codex");
        assert_eq!(
            req.runtime_overlay.provision_command.as_deref(),
            Some("pnpm i")
        );
        assert_eq!(req.runtime_overlay.teardown_command.as_deref(), Some("rm"));
        assert_eq!(
            req.runtime_overlay.cleanup_command.as_deref(),
            Some("clean")
        );
    }

    #[test]
    fn build_record_local_transport_is_none_sync() {
        let req = sample_request();
        let env = sample_environment();
        let lease = sample_lease_local();
        let rec = build_workspace_realization_record(BuildRecordInput {
            environment: &env,
            lease: &lease,
            request: &req,
            realized_cwd: Some("/realized"),
            provider_metadata: None,
        });
        assert_eq!(rec.transport, "local");
        assert_eq!(rec.sync.strategy, "none");
        assert_eq!(rec.sync.sync_back, None);
        assert!(rec.summary.contains("Local workspace realized"));
        assert_eq!(
            rec.bootstrap.command.as_deref(),
            req.runtime_overlay.provision_command.as_deref()
        );
        assert!(rec.additional.is_empty());
        assert_eq!(rec.rebuild.local_path, req.source.local_path);
    }

    #[test]
    fn build_record_ssh_transport_picks_ssh_strategy() {
        let mut env = sample_environment();
        env.driver = "ssh".to_string();
        let mut lease = sample_lease_local();
        lease.metadata = Some(json!({
            "host": "example.com",
            "port": 2222,
            "username": "deploy",
            "remoteCwd": "/home/deploy/app"
        }));
        let req = sample_request();
        let rec = build_workspace_realization_record(BuildRecordInput {
            environment: &env,
            lease: &lease,
            request: &req,
            realized_cwd: Some("/tmp/.paperclip/.exec"),
            provider_metadata: None,
        });
        assert_eq!(rec.transport, "ssh");
        assert_eq!(rec.sync.strategy, "ssh_git_import_export");
        assert!(rec.sync.sync_back.is_some());
        assert!(rec.summary.contains("SSH workspace realized"));
    }

    #[test]
    fn build_record_sandbox_transport_picks_archive() {
        let mut env = sample_environment();
        env.driver = "sandbox".to_string();
        let mut lease = sample_lease_local();
        lease.metadata = Some(json!({"sandboxId": "sb-123", "remoteCwd": "/work"}));
        let req = sample_request();
        let rec = build_workspace_realization_record(BuildRecordInput {
            environment: &env,
            lease: &lease,
            request: &req,
            realized_cwd: Some("/host"),
            provider_metadata: {
                let mut m = serde_json::Map::new();
                m.insert("sandboxId".to_string(), json!("sb-123"));
                Some(m)
            },
        });
        assert_eq!(rec.transport, "sandbox");
        assert_eq!(rec.sync.strategy, "sandbox_archive_upload_download");
        assert!(rec.summary.contains("Sandbox workspace realized"));
        assert!(rec.summary.contains("sb-123"));
    }

    #[test]
    fn build_record_in_place_mode_overrides_authoritative_root() {
        let mut env = sample_environment();
        env.driver = "ssh".to_string();
        let mut lease = sample_lease_local();
        lease.metadata = Some(json!({
            "remoteCwd": "/remote/app",
            "host": "example.com"
        }));
        let req = sample_request();
        let mut provider = serde_json::Map::new();
        provider.insert("mode".to_string(), json!("in_place"));
        let rec = build_workspace_realization_record(BuildRecordInput {
            environment: &env,
            lease: &lease,
            request: &req,
            realized_cwd: None,
            provider_metadata: Some(provider),
        });
        assert_eq!(rec.mode, "in_place");
        assert_eq!(rec.authoritative_root, "/remote/app");
        assert_eq!(rec.sync.strategy, "none");
    }

    #[test]
    fn build_record_falls_back_authoritative_root_to_local_when_no_mode() {
        let env = sample_environment();
        let lease = sample_lease_local();
        let req = sample_request();
        let rec = build_workspace_realization_record(BuildRecordInput {
            environment: &env,
            lease: &lease,
            request: &req,
            realized_cwd: None,
            provider_metadata: None,
        });
        assert_eq!(rec.mode, "copy");
        assert_eq!(rec.authoritative_root, req.source.local_path);
    }

    #[test]
    fn build_record_includes_additional_sources() {
        let env = sample_environment();
        let lease = sample_lease_local();
        let mut req = sample_request();
        req.additional_sources = vec![WorkspaceRealizationRequestSource {
            kind: "project_primary".to_string(),
            local_path: "/tmp/ref".to_string(),
            project_id: Some("p2".to_string()),
            project_workspace_id: Some("ws2".to_string()),
            repo_url: Some("https://ref".to_string()),
            repo_ref: None,
            strategy: "project_primary".to_string(),
            branch_name: None,
            worktree_path: None,
        }];
        let rec = build_workspace_realization_record(BuildRecordInput {
            environment: &env,
            lease: &lease,
            request: &req,
            realized_cwd: None,
            provider_metadata: None,
        });
        assert_eq!(rec.additional.len(), 1);
        assert_eq!(rec.additional[0].path, "/tmp/ref");
    }

    #[test]
    fn build_record_provider_metadata_overrides_lease_metadata() {
        let mut env = sample_environment();
        env.driver = "ssh".to_string();
        let mut lease = sample_lease_local();
        lease.metadata = Some(json!({"host": "lease-host", "remoteCwd": "/from-lease"}));
        let req = sample_request();
        let mut provider = serde_json::Map::new();
        provider.insert("host".to_string(), json!("provider-host"));
        provider.insert("remoteCwd".to_string(), json!("/from-provider"));
        let rec = build_workspace_realization_record(BuildRecordInput {
            environment: &env,
            lease: &lease,
            request: &req,
            realized_cwd: None,
            provider_metadata: Some(provider),
        });
        // provider_metadata 覆盖 lease_metadata 中 remoteCwd / remotePath /
        // sandboxId 等"远程路径"字段；但 host/port/username 是 lease 专属字段，
        // 不会被 provider 覆盖（与 Node 一致）。
        let remote_path = rec
            .remote
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(remote_path, "/from-provider");
    }

    #[test]
    fn build_record_from_driver_input_uses_saved_request() {
        let env = sample_environment();
        let lease = sample_lease_local();
        let saved_request = serde_json::to_value(sample_request()).unwrap();
        let mut meta = serde_json::Map::new();
        meta.insert("workspaceRealizationRequest".to_string(), saved_request);
        let input = DriverInput {
            environment: env,
            lease,
            workspace: WorkspaceDriverWorkspace {
                local_path: Some("/host/ws".to_string()),
                remote_path: None,
                mode: None,
                metadata: Some(meta),
            },
            cwd: Some("/host/ws".to_string()),
            provider_metadata: None,
        };
        let rec = build_workspace_realization_record_from_driver_input(input).expect("ok");
        assert_eq!(rec.transport, "local");
    }

    #[test]
    fn build_record_from_driver_input_falls_back_to_minimal_request() {
        let env = sample_environment();
        let lease = sample_lease_local();
        let input = DriverInput {
            environment: env,
            lease,
            workspace: WorkspaceDriverWorkspace {
                local_path: Some("/host/ws".to_string()),
                remote_path: None,
                mode: Some("isolated_workspace".to_string()),
                metadata: None,
            },
            cwd: Some("/host/ws".to_string()),
            provider_metadata: None,
        };
        let rec = build_workspace_realization_record_from_driver_input(input).expect("ok");
        // record 不直接存 adapter_type；通过 source.kind 验证 request 被使用
        assert_eq!(rec.local.source, "task_session");
    }

    #[test]
    fn record_serializes_with_expected_fields() {
        let env = sample_environment();
        let lease = sample_lease_local();
        let req = sample_request();
        let rec = build_workspace_realization_record(BuildRecordInput {
            environment: &env,
            lease: &lease,
            request: &req,
            realized_cwd: None,
            provider_metadata: None,
        });
        let v = serde_json::to_value(&rec).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["transport"], "local");
        assert_eq!(v["sync"]["strategy"], "none");
        assert_eq!(v["mode"], "copy");
        assert!(v["rebuild"]["metadata"].is_object());
    }

    fn sample_request() -> WorkspaceRealizationRequest {
        WorkspaceRealizationRequest {
            version: 1,
            adapter_type: "codex-local".to_string(),
            company_id: "company-1".to_string(),
            environment_id: "env-1".to_string(),
            execution_workspace_id: Some("exec-1".to_string()),
            issue_id: Some("issue-1".to_string()),
            heartbeat_run_id: "run-1".to_string(),
            requested_mode: Some("isolated_workspace".to_string()),
            source: WorkspaceRealizationRequestSource {
                kind: "project_primary".to_string(),
                local_path: "/tmp/ws".to_string(),
                project_id: Some("proj-1".to_string()),
                project_workspace_id: Some("pws-1".to_string()),
                repo_url: Some("https://example.com/repo.git".to_string()),
                repo_ref: Some("main".to_string()),
                strategy: "git_worktree".to_string(),
                branch_name: Some("feat/x".to_string()),
                worktree_path: Some("/tmp/ws/feat-x".to_string()),
            },
            additional_sources: vec![],
            runtime_overlay: WorkspaceRuntimeOverlay {
                provision_command: Some("pnpm i".to_string()),
                teardown_command: None,
                cleanup_command: None,
                workspace_runtime: None,
            },
        }
    }
}
