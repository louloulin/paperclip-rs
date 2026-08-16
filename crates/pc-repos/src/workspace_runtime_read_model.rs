//! `workspace_runtime_read_model` 域（Round 262）。
//!
//! 与原 `paperclip/server/src/services/workspace-runtime-read-model.ts` 1:1 对齐：
//! - `select_current_runtime_service_rows`：按 identity key 去重，保留最新行
//! - `select_configured_runtime_service_rows`：按 command 配置匹配 runtime 服务
//! - `list_current_runtime_services_for_project_workspaces`：批量按 project_workspace 查询
//! - `list_current_runtime_services_for_execution_workspaces`：批量按 execution_workspace 查询
//!
//! 设计目标：高内聚低耦合。
//! - 输入：`WorkspaceRuntimeServiceRow` 列表 / workspaceRuntime 配置
//! - 输出：去重后的当前运行时服务集合
//! - 不持有状态，不做 IO（除 list_* 方法的批量查询）

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::Db;

/// `workspace_runtime_services` 表行（与 Node 版 `workspaceRuntimeServices.$inferSelect` 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceRuntimeServiceRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub project_workspace_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub scope_type: String,
    pub scope_id: Option<String>,
    pub service_name: String,
    pub status: String,
    pub lifecycle: String,
    pub reuse_key: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub port: Option<i32>,
    pub url: Option<String>,
    pub provider: String,
    pub provider_ref: Option<String>,
    pub owner_agent_id: Option<Uuid>,
    pub started_by_run_id: Option<Uuid>,
    pub last_used_at: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub stop_policy: Option<serde_json::Value>,
    pub health_status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// identity key 用于去重（与 Node 版 `runtimeServiceIdentityKey` 1:1 对齐）。
///
/// 优先级：`reuse_key` > scope_type + scope_id + project_workspace_id + execution_workspace_id
///         + service_name + command + cwd。
pub fn runtime_service_identity_key(row: &WorkspaceRuntimeServiceRow) -> String {
    if let Some(reuse) = &row.reuse_key {
        return reuse.clone();
    }
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        row.scope_type,
        row.scope_id.as_deref().unwrap_or(""),
        row.project_workspace_id
            .map(|u| u.to_string())
            .unwrap_or_default(),
        "", // execution_workspace_id 不在 row 中；Node 端也是相同占位
        row.service_name,
        row.command.as_deref().unwrap_or(""),
        row.cwd.as_deref().unwrap_or(""),
    )
}

/// 同 identity 保留首条（与 Node 版 `selectCurrentRuntimeServiceRows` 对齐：保持首次出现）。
/// 调用方需传入已按 `updated_at DESC, created_at DESC` 排序的行集合。
pub fn select_current_runtime_service_rows(
    rows: Vec<WorkspaceRuntimeServiceRow>,
) -> Vec<WorkspaceRuntimeServiceRow> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let key = runtime_service_identity_key(&row);
        if seen.insert(key) {
            out.push(row);
        }
    }
    out
}

/// `WorkspaceRuntimeReadModelRepo` — 批量读取运行时服务视图。
pub struct WorkspaceRuntimeReadModelRepo<'a> {
    pub db: &'a Db,
}

impl<'a> WorkspaceRuntimeReadModelRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 按 company_id + project_workspace_ids 批量取运行时服务，按 updated_at DESC, created_at DESC 排序。
    /// 返回 `Map<project_workspace_id, current_rows>`，与 Node 版 `listCurrentRuntimeServicesForProjectWorkspaces` 对齐。
    pub async fn list_current_runtime_services_for_project_workspaces(
        &self,
        company_id: Uuid,
        project_workspace_ids: &[Uuid],
    ) -> sqlx::Result<std::collections::HashMap<Uuid, Vec<WorkspaceRuntimeServiceRow>>> {
        if project_workspace_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let rows: Vec<WorkspaceRuntimeServiceRow> = sqlx::query_as(
            "SELECT id, company_id, project_id, project_workspace_id, issue_id, scope_type, scope_id, \
             service_name, status, lifecycle, reuse_key, command, cwd, port, url, \
             provider, provider_ref, owner_agent_id, started_by_run_id, \
             last_used_at, started_at, stopped_at, stop_policy, health_status, \
             created_at, updated_at \
             FROM workspace_runtime_services \
             WHERE company_id = $1 AND project_workspace_id = ANY($2) AND scope_type = 'project_workspace' \
             ORDER BY updated_at DESC, created_at DESC",
        )
        .bind(company_id)
        .bind(project_workspace_ids)
        .fetch_all(self.db.pool())
        .await?;
        let mut grouped: std::collections::HashMap<Uuid, Vec<WorkspaceRuntimeServiceRow>> =
            std::collections::HashMap::new();
        for row in rows {
            if let Some(pid) = row.project_workspace_id {
                grouped.entry(pid).or_default().push(row);
            }
        }
        let out = grouped
            .into_iter()
            .map(|(ws_id, rs)| (ws_id, select_current_runtime_service_rows(rs)))
            .collect();
        Ok(out)
    }

    /// 按 company_id + execution_workspace_ids 批量取运行时服务（与 Node 版 `listCurrentRuntimeServicesForExecutionWorkspaces` 对齐）。
    ///
    /// 注：当前 schema 不直接存储 execution_workspace_id 列，所以使用 scope_type/execution_workspace_id 的 metadata 字段。
    /// 这里简化为按 project_workspace_id = ANY($2) 但 scope_type = 'execution_workspace'。
    pub async fn list_current_runtime_services_for_execution_workspaces(
        &self,
        company_id: Uuid,
        execution_workspace_ids: &[Uuid],
    ) -> sqlx::Result<std::collections::HashMap<Uuid, Vec<WorkspaceRuntimeServiceRow>>> {
        if execution_workspace_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        // 当前 schema 没有 execution_workspace_id 列；使用 project_workspace_id + scope_type 查询
        // （实际生产环境若有 execution_workspace_id 列，需要扩展 query）。
        let rows: Vec<WorkspaceRuntimeServiceRow> = sqlx::query_as(
            "SELECT id, company_id, project_id, project_workspace_id, issue_id, scope_type, scope_id, \
             service_name, status, lifecycle, reuse_key, command, cwd, port, url, \
             provider, provider_ref, owner_agent_id, started_by_run_id, \
             last_used_at, started_at, stopped_at, stop_policy, health_status, \
             created_at, updated_at \
             FROM workspace_runtime_services \
             WHERE company_id = $1 AND project_workspace_id = ANY($2) AND scope_type = 'execution_workspace' \
             ORDER BY updated_at DESC, created_at DESC",
        )
        .bind(company_id)
        .bind(execution_workspace_ids)
        .fetch_all(self.db.pool())
        .await?;
        let mut grouped: std::collections::HashMap<Uuid, Vec<WorkspaceRuntimeServiceRow>> =
            std::collections::HashMap::new();
        for row in rows {
            if let Some(pid) = row.project_workspace_id {
                grouped.entry(pid).or_default().push(row);
            }
        }
        let out = grouped
            .into_iter()
            .map(|(ws_id, rs)| (ws_id, select_current_runtime_service_rows(rs)))
            .collect();
        Ok(out)
    }
}


/// Helper struct produced by `select_configured_runtime_service_rows`.
/// Mirrors Node's `(WorkspaceRuntimeServiceRow & { configIndex: number | null })`.
#[derive(Debug, Clone)]
pub struct ConfiguredRuntimeServiceRow {
    pub row: WorkspaceRuntimeServiceRow,
    pub config_index: Option<usize>,
}

fn read_reuse_scope(raw_config: &serde_json::Value) -> Option<String> {
    let obj = raw_config.as_object()?;
    let v = obj.get("reuseScope")?;
    v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Mirrors Node's `expectedScope` resolution order:
/// 1. If `command.rawConfig.reuseScope` is a known scope literal, use that.
/// 2. Else if `command.lifecycle === "shared"`, `"project_workspace"`.
/// 3. Else `"run"`.
fn resolve_expected_scope(
    reuse_scope: Option<&str>,
    lifecycle: Option<pc_workspace_commands::WorkspaceCommandLifecycle>,
) -> &'static str {
    if let Some(scope) = reuse_scope {
        match scope {
            "project_workspace" | "execution_workspace" | "agent" | "run" => {
                return match scope {
                    "project_workspace" => "project_workspace",
                    "execution_workspace" => "execution_workspace",
                    "agent" => "agent",
                    _ => "run",
                };
            }
            _ => {}
        }
    }
    match lifecycle {
        Some(pc_workspace_commands::WorkspaceCommandLifecycle::Shared) => "project_workspace",
        _ => "run",
    }
}

impl WorkspaceRuntimeServiceRow {
    fn to_match_input(&self, config_index: Option<usize>) -> pc_workspace_commands::WorkspaceRuntimeServiceMatchInput {
        pc_workspace_commands::WorkspaceRuntimeServiceMatchInput {
            config_index,
            service_name: Some(self.service_name.clone()),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            id: self.id.to_string(),
        }
    }
}

/// `selectConfiguredRuntimeServiceRows` — 1:1 Node parity.
///
/// Iterates `list_workspace_service_command_definitions(workspaceRuntime)`.
/// For each command, picks an `expected_scope` via `reuseScope`/lifecycle rules
/// then runs `match_workspace_runtime_service_to_command` against the rows
/// scoped by `expected_scope`. The first match wins, is removed from the pool,
/// and is returned with its originating `serviceIndex`.
///
/// Implementational notes vs Node:
/// - Node pool type is a plain JS array; we use `Vec<usize>` index storage since
///   `WorkspaceRuntimeServiceRow` is not `Clone`.
/// - `pc-workspace-commands::match_workspace_runtime_service_to_command` returns
///   `Option<usize>` (the index into the input slice), which maps directly to
///   `config_index` (Node's `serviceIndex`).
pub fn select_configured_runtime_service_rows(
    rows: &[WorkspaceRuntimeServiceRow],
    workspace_runtime: Option<&serde_json::Value>,
) -> Vec<ConfiguredRuntimeServiceRow> {
    use pc_workspace_commands::{
        list_workspace_service_command_definitions, match_workspace_runtime_service_to_command,
    };

    let commands = list_workspace_service_command_definitions(workspace_runtime);

    // Pool of candidate row indices (Node: availableRows). Initial entries get
    // `configIndex = null` (Node: `availableRows.map(row => ({ ...row, configIndex: null }))`).
    let mut pool: Vec<usize> = (0..rows.len()).collect();

    let mut selected: Vec<ConfiguredRuntimeServiceRow> = Vec::new();

    for command in commands {
        let reuse_scope = read_reuse_scope(&command.raw_config);
        let lifecycle = command.lifecycle;
        let expected_scope = resolve_expected_scope(reuse_scope.as_deref(), lifecycle);

        // Filter pool entries whose row.scope_type == expected_scope.
        let scoped: Vec<usize> = pool
            .iter()
            .copied()
            .filter(|idx| rows[*idx].scope_type == expected_scope)
            .collect();
        if scoped.is_empty() {
            continue;
        }

        // Convert scoped indices to match inputs.
        let match_inputs: Vec<pc_workspace_commands::WorkspaceRuntimeServiceMatchInput> = scoped
            .iter()
            .map(|idx| rows[*idx].to_match_input(command.service_index))
            .collect();

        // Run matcher.
        let matched_idx = match match_workspace_runtime_service_to_command(
            &command,
            Some(&match_inputs),
        ) {
            Some(idx) => idx,
            None => continue,
        };
        let row_idx = scoped[matched_idx];

        // Push the selected row with the command's serviceIndex.
        selected.push(ConfiguredRuntimeServiceRow {
            row: rows[row_idx].clone(),
            config_index: command.service_index,
        });

        // Remove the matched row from the pool (Node: availableRows.splice(idx, 1)).
        if let Some(pos) = pool.iter().position(|idx| *idx == row_idx) {
            pool.remove(pos);
        }
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        reuse: Option<&str>,
        scope: &str,
        name: &str,
        command: Option<&str>,
    ) -> WorkspaceRuntimeServiceRow {
        WorkspaceRuntimeServiceRow {
            id: Uuid::new_v4(),
            company_id: Uuid::nil(),
            project_id: None,
            project_workspace_id: None,
            issue_id: None,
            scope_type: scope.into(),
            scope_id: None,
            service_name: name.into(),
            status: "running".into(),
            lifecycle: "shared".into(),
            reuse_key: reuse.map(String::from),
            command: command.map(String::from),
            cwd: None,
            port: None,
            url: None,
            provider: "local".into(),
            provider_ref: None,
            owner_agent_id: None,
            started_by_run_id: None,
            last_used_at: chrono::Utc::now(),
            started_at: chrono::Utc::now(),
            stopped_at: None,
            stop_policy: None,
            health_status: "unknown".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn reuse_key_takes_priority() {
        let r = row(Some("shared-1"), "run", "postgres", None);
        assert_eq!(runtime_service_identity_key(&r), "shared-1");
    }

    #[test]
    fn composite_key_when_no_reuse() {
        let r = row(None, "run", "postgres", Some("pg-start"));
        let key = runtime_service_identity_key(&r);
        assert!(key.contains("run"));
        assert!(key.contains("postgres"));
        assert!(key.contains("pg-start"));
    }

    #[test]
    fn dedupe_keeps_first_per_identity() {
        let r1 = row(None, "run", "postgres", Some("a"));
        let r2 = row(None, "run", "postgres", Some("a"));
        let r3 = row(None, "run", "postgres", Some("b"));
        let cur = select_current_runtime_service_rows(vec![r1.clone(), r2, r3.clone()]);
        assert_eq!(cur.len(), 2);
        // 首次出现保留
        assert_eq!(cur[0].id, r1.id);
        assert_eq!(cur[1].service_name, "postgres");
        assert_eq!(cur[1].command.as_deref(), Some("b"));
    }

    #[test]
    fn dedupe_respects_reuse_key() {
        let r1 = row(Some("shared-x"), "run", "postgres", Some("a"));
        let r2 = row(Some("shared-x"), "project_workspace", "postgres", Some("b"));
        let cur = select_current_runtime_service_rows(vec![r1.clone(), r2]);
        assert_eq!(cur.len(), 1);
        assert_eq!(cur[0].id, r1.id);
    }

    // ===== select_configured_runtime_service_rows parity tests =====

    fn svc_row_with_scope(
        scope: &str,
        name: &str,
        command: Option<&str>,
    ) -> WorkspaceRuntimeServiceRow {
        let mut r = row(None, scope, name, command);
        r.scope_id = Some(format!("scope-{}", name));
        r
    }

    fn wr_with_service(name: &str, command: &str, lifecycle: &str, reuse_scope: Option<&str>) -> serde_json::Value {
        let mut svc = serde_json::json!({
            "name": name,
            "command": command,
            "lifecycle": lifecycle,
        });
        if let Some(scope) = reuse_scope {
            svc["reuseScope"] = serde_json::Value::String(scope.to_string());
        }
        serde_json::json!({"services": [svc]})
    }

    #[test]
    fn r676_returns_empty_when_no_runtime() {
        let rows = vec![svc_row_with_scope("run", "postgres", Some("pg-start"))];
        let out = select_configured_runtime_service_rows(&rows, None);
        assert!(out.is_empty());
    }

    #[test]
    fn r676_matches_single_command() {
        let rows = vec![svc_row_with_scope("run", "postgres", Some("pg-start"))];
        let wr = wr_with_service("postgres", "pg-start", "ephemeral", None);
        let out = select_configured_runtime_service_rows(&rows, Some(&wr));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].row.service_name, "postgres");
        assert_eq!(out[0].config_index, Some(0));
    }

    #[test]
    fn r676_no_match_skips() {
        let rows = vec![svc_row_with_scope("run", "redis", Some("redis-start"))];
        let wr = wr_with_service("postgres", "pg-start", "ephemeral", None);
        let out = select_configured_runtime_service_rows(&rows, Some(&wr));
        assert!(out.is_empty());
    }

    #[test]
    fn r676_reuse_scope_overrides_default() {
        // command has reuseScope=execution_workspace; row is "execution_workspace".
        let rows = vec![svc_row_with_scope("execution_workspace", "postgres", Some("pg-start"))];
        let wr = wr_with_service("postgres", "pg-start", "ephemeral", Some("execution_workspace"));
        let out = select_configured_runtime_service_rows(&rows, Some(&wr));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].row.service_name, "postgres");
    }

    #[test]
    fn r676_reuse_scope_invalid_falls_through() {
        let rows = vec![svc_row_with_scope("run", "postgres", Some("pg-start"))];
        // reuseScope not in 4 allowed values → fall back to lifecycle / run
        let wr = wr_with_service("postgres", "pg-start", "ephemeral", Some("bogus"));
        let out = select_configured_runtime_service_rows(&rows, Some(&wr));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn r676_lifecycle_shared_uses_project_workspace() {
        // lifecycle=shared, no reuseScope → expect "project_workspace"
        let rows = vec![svc_row_with_scope("project_workspace", "postgres", Some("pg-start"))];
        let wr = wr_with_service("postgres", "pg-start", "shared", None);
        let out = select_configured_runtime_service_rows(&rows, Some(&wr));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].row.service_name, "postgres");
    }

    #[test]
    fn r676_lifecycle_shared_does_not_match_run_scope() {
        let rows = vec![svc_row_with_scope("run", "postgres", Some("pg-start"))];
        let wr = wr_with_service("postgres", "pg-start", "shared", None);
        let out = select_configured_runtime_service_rows(&rows, Some(&wr));
        assert!(out.is_empty());
    }

    #[test]
    fn r676_matched_row_removed_from_pool() {
        // Two rows of same scope/service; only first should be picked & second stays in pool
        // until the next command runs (which it doesn't here).
        let rows = vec![
            svc_row_with_scope("run", "postgres", Some("pg-start")),
            svc_row_with_scope("run", "postgres", Some("pg-start")),
        ];
        let wr = wr_with_service("postgres", "pg-start", "ephemeral", None);
        let out = select_configured_runtime_service_rows(&rows, Some(&wr));
        // Node loop matches first eligible row in pool order, removes it from pool.
        // The second row is not requested by any subsequent command, so it stays in pool (not selected).
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn r676_skips_unsupported_service_kind() {
        // pc-workspace-commands only includes kind=Service rows; "jobs" should be filtered out.
        let rows = vec![svc_row_with_scope("run", "build_thing", Some("pnpm build"))];
        let wr = serde_json::json!({
            "jobs": [{"name":"build_thing","command":"pnpm build","lifecycle":"ephemeral"}]
        });
        let out = select_configured_runtime_service_rows(&rows, Some(&wr));
        assert!(out.is_empty());
    }

    #[test]
    fn r676_mixed_scopes() {
        // Two rows in different scopes, both with matching command so match succeeds.
        let rows = vec![
            svc_row_with_scope("run", "postgres", Some("pg-cmd")),
            svc_row_with_scope("project_workspace", "postgres", Some("pg-cmd")),
        ];
        let wr = wr_with_service("postgres", "pg-cmd", "shared", None);
        let out = select_configured_runtime_service_rows(&rows, Some(&wr));
        // lifecycle=shared → expect "project_workspace" pool → matches the second row.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].row.scope_type, "project_workspace");
    }

    #[test]
    fn r676_cd_does_not_match_different_command() {
        // Same name, different command → match_workspace_runtime_service_to_command returns -1,
        // so this command should be skipped.
        let rows = vec![svc_row_with_scope("run", "postgres", Some("pg-other"))];
        let wr = wr_with_service("postgres", "pg-start", "ephemeral", None);
        let out = select_configured_runtime_service_rows(&rows, Some(&wr));
        assert!(out.is_empty(), "name+command mismatch should not match");
    }
}
