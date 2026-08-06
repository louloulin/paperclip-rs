//! `execution_workspace_overview` 域（Round 279）。
//!
//! 与原 `paperclip/server/src/services/execution-workspaces.ts` 中多个 pure helper
//! 1:1 对齐：
//! - `maxDate(...values)` — 取最大时间
//! - `usesInheritedProjectRuntimeServices(row)` — 判断 shared_workspace 是否继承 project 运行时
//! - `selectPrimaryOverviewService(services)` — 选 primary service（ranking）
//! - `toRuntimeService(row)` — DB row → typed struct 转换器
//! - `toExecutionWorkspaceSummary(row)` — DB row → ExecutionWorkspaceSummary 转换器
//! - `toWorkspaceOverviewPrimaryService(service)` — 投影主 service 摘要
//!
//! 设计目标：高内聚低耦合。
//! - 高内聚：本模块只关心"execution workspace overview 选择/转换"的纯逻辑。
//! - 低耦合：依赖 `chrono` + `serde_json`；DB-coupled 部分（row 类型）由调用方提供。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::execution_workspace_config::{read_execution_workspace_config, ExecutionWorkspaceConfig};

// ============================================================================
// maxDate（Round 279）
// ============================================================================

/// `maxDate(...values)`: 取最大时间；空集合返回 epoch(0)。
///
/// 与 Node 1:1 对齐：
/// - Date 保留
/// - ISO string → Date
/// - 空/None/无效字符串忽略
pub fn max_date<I>(values: I) -> DateTime<Utc>
where
    I: IntoIterator,
    I::Item: AsDateLike,
{
    let epoch: DateTime<Utc> = DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now);
    let mut latest = epoch;
    for v in values {
        if let Some(d) = v.to_date_time() {
            if d > latest {
                latest = d;
            }
        }
    }
    latest
}

/// `DateLike`：抽象输入枚举，对应 Node 的 Date | string 输入。
#[derive(Debug, Clone)]
pub enum DateLike {
    DateTime(DateTime<Utc>),
    Str(String),
}

impl DateLike {
    pub fn to_datetime(&self) -> Option<DateTime<Utc>> {
        match self {
            DateLike::DateTime(d) => Some(*d),
            DateLike::Str(s) => {
                // Node Date.parse: 支持 RFC3339 + 一些自由格式；这里用 chrono 解析。
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            }
        }
    }
}

/// `AsDateLike`：抽象 trait，让 DateTime/&str/String 都能透明地作为输入。
pub trait AsDateLike {
    fn to_date_time(&self) -> Option<DateTime<Utc>>;
}

impl AsDateLike for DateLike {
    fn to_date_time(&self) -> Option<DateTime<Utc>> {
        self.to_datetime()
    }
}

impl AsDateLike for DateTime<Utc> {
    fn to_date_time(&self) -> Option<DateTime<Utc>> {
        Some(*self)
    }
}

impl AsDateLike for str {
    fn to_date_time(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(self)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }
}

impl AsDateLike for String {
    fn to_date_time(&self) -> Option<DateTime<Utc>> {
        self.as_str().to_date_time()
    }
}

impl AsDateLike for Option<DateTime<Utc>> {
    fn to_date_time(&self) -> Option<DateTime<Utc>> {
        *self
    }
}

/// 便利 helper：让 chrono DateTime 直接参与比较。
pub fn max_date_dt<I>(values: I) -> DateTime<Utc>
where
    I: IntoIterator<Item = Option<DateTime<Utc>>>,
{
    let epoch: DateTime<Utc> = DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now);
    let mut latest = epoch;
    for v in values {
        if let Some(d) = v {
            if d > latest {
                latest = d;
            }
        }
    }
    latest
}

// ============================================================================
// 类型投影
// ============================================================================

/// `WorkspaceRuntimeService` 字符串字面量联合（与 Node 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Starting,
    Running,
    Stopped,
    Failed,
}

impl ServiceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceStatus::Starting => "starting",
            ServiceStatus::Running => "running",
            ServiceStatus::Stopped => "stopped",
            ServiceStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Unknown,
    Healthy,
    Unhealthy,
}

impl HealthStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            HealthStatus::Unknown => "unknown",
            HealthStatus::Healthy => "healthy",
            HealthStatus::Unhealthy => "unhealthy",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRuntimeService {
    pub id: String,
    pub company_id: String,
    pub project_id: Option<String>,
    pub project_workspace_id: Option<String>,
    pub execution_workspace_id: Option<String>,
    pub issue_id: Option<String>,
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
    pub owner_agent_id: Option<String>,
    pub started_by_run_id: Option<String>,
    pub last_used_at: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub stop_policy: Option<serde_json::Value>,
    pub health_status: String,
    pub config_index: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// DB row 输入（最小字段，参考 Node `WorkspaceRuntimeServiceRow & { configIndex? }`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceRuntimeServiceRow {
    pub id: String,
    pub company_id: String,
    pub project_id: Option<String>,
    pub project_workspace_id: Option<String>,
    pub execution_workspace_id: Option<String>,
    pub issue_id: Option<String>,
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
    pub owner_agent_id: Option<String>,
    pub started_by_run_id: Option<String>,
    pub last_used_at: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub stop_policy: Option<serde_json::Value>,
    pub health_status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub config_index: Option<i32>,
}

/// DB row → typed struct：`toRuntimeService(row)`.
pub fn to_runtime_service(row: &WorkspaceRuntimeServiceRow) -> WorkspaceRuntimeService {
    WorkspaceRuntimeService {
        id: row.id.clone(),
        company_id: row.company_id.clone(),
        project_id: row.project_id.clone(),
        project_workspace_id: row.project_workspace_id.clone(),
        execution_workspace_id: row.execution_workspace_id.clone(),
        issue_id: row.issue_id.clone(),
        scope_type: row.scope_type.clone(),
        scope_id: row.scope_id.clone(),
        service_name: row.service_name.clone(),
        status: row.status.clone(),
        lifecycle: row.lifecycle.clone(),
        reuse_key: row.reuse_key.clone(),
        command: row.command.clone(),
        cwd: row.cwd.clone(),
        port: row.port,
        url: row.url.clone(),
        provider: row.provider.clone(),
        provider_ref: row.provider_ref.clone(),
        owner_agent_id: row.owner_agent_id.clone(),
        started_by_run_id: row.started_by_run_id.clone(),
        last_used_at: row.last_used_at,
        started_at: row.started_at,
        stopped_at: row.stopped_at,
        stop_policy: row.stop_policy.clone(),
        health_status: row.health_status.clone(),
        config_index: row.config_index,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

// ============================================================================
// ExecutionWorkspaceSummary & toExecutionWorkspaceSummary
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionWorkspaceSummary {
    pub id: String,
    pub name: String,
    pub mode: String,
    pub status: String,
    pub cwd: Option<String>,
    pub branch_name: Option<String>,
    pub project_workspace_id: Option<String>,
    pub last_used_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionWorkspaceSummaryRow {
    pub id: String,
    pub name: String,
    pub mode: String,
    pub status: String,
    pub cwd: Option<String>,
    pub branch_name: Option<String>,
    pub project_workspace_id: Option<String>,
    pub last_used_at: DateTime<Utc>,
}

/// `toExecutionWorkspaceSummary(row)`：DB row → typed summary。
pub fn to_execution_workspace_summary(row: &ExecutionWorkspaceSummaryRow) -> ExecutionWorkspaceSummary {
    ExecutionWorkspaceSummary {
        id: row.id.clone(),
        name: row.name.clone(),
        mode: row.mode.clone(),
        status: row.status.clone(),
        cwd: row.cwd.clone(),
        branch_name: row.branch_name.clone(),
        project_workspace_id: row.project_workspace_id.clone(),
        last_used_at: row.last_used_at,
    }
}

// ============================================================================
// WorkspaceOverviewPrimaryService & selectPrimaryOverviewService
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceOverviewPrimaryService {
    pub id: String,
    pub service_name: String,
    pub status: String,
    pub url: Option<String>,
    pub port: Option<i32>,
    pub health_status: String,
    pub updated_at: DateTime<Utc>,
}

pub fn to_workspace_overview_primary_service(
    service: Option<&WorkspaceRuntimeService>,
) -> Option<WorkspaceOverviewPrimaryService> {
    service.map(|s| WorkspaceOverviewPrimaryService {
        id: s.id.clone(),
        service_name: s.service_name.clone(),
        status: s.status.clone(),
        url: s.url.clone(),
        port: s.port,
        health_status: s.health_status.clone(),
        updated_at: s.updated_at,
    })
}

/// `selectPrimaryOverviewService(services)`: 按优先级挑选：
/// 1. running + 有 url
/// 2. 有 url
/// 3. running
/// 4. 第一个
/// 5. 无 → null
pub fn select_primary_overview_service(
    services: &[WorkspaceRuntimeService],
) -> Option<&WorkspaceRuntimeService> {
    let r#fn = |s: &&WorkspaceRuntimeService| s.status == "running" && s.url.is_some();
    if let Some(s) = services.iter().find(r#fn) {
        return Some(s);
    }
    if let Some(s) = services.iter().find(|s| s.url.is_some()) {
        return Some(s);
    }
    if let Some(s) = services.iter().find(|s| s.status == "running") {
        return Some(s);
    }
    services.first()
}

// ============================================================================
// usesInheritedProjectRuntimeServices
// ============================================================================

/// `usesInheritedProjectRuntimeServices(row)`：
/// - mode != "shared_workspace" 或 无 projectWorkspaceId → false
/// - metadata.config.workspaceRuntime == null → true（继承）
/// - 否则 → false
pub fn uses_inherited_project_runtime_services(
    mode: &str,
    project_workspace_id: Option<&str>,
    metadata: Option<&serde_json::Value>,
) -> bool {
    if mode != "shared_workspace" || project_workspace_id.is_none() {
        return false;
    }
    let map = metadata.and_then(|v| v.as_object());
    let cfg: Option<ExecutionWorkspaceConfig> = read_execution_workspace_config(map);
    cfg.map(|c| c.workspace_runtime.flatten().is_none()).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    #[test]
    fn max_date_with_all_dt() {
        let out = max_date_dt([
            Some(dt(2024, 1, 1, 0, 0, 0)),
            Some(dt(2025, 1, 1, 0, 0, 0)),
            Some(dt(2023, 1, 1, 0, 0, 0)),
            None,
        ]);
        assert_eq!(out, dt(2025, 1, 1, 0, 0, 0));
    }

    #[test]
    fn max_date_empty_returns_epoch() {
        let out = max_date_dt::<std::iter::Empty<Option<DateTime<Utc>>>>(std::iter::empty());
        assert_eq!(out.timestamp(), 0);
    }

    #[test]
    fn max_date_iso_strings() {
        let out = max_date([
            DateLike::Str("2024-01-01T00:00:00Z".to_string()),
            DateLike::Str("2025-06-15T12:34:56Z".to_string()),
            DateLike::Str("2023-12-31T23:59:59Z".to_string()),
        ]);
        assert_eq!(out, dt(2025, 6, 15, 12, 34, 56));
    }

    #[test]
    fn max_date_ignores_invalid_string() {
        let out = max_date([
            DateLike::Str("2024-01-01T00:00:00Z".to_string()),
            DateLike::Str("not-a-date".to_string()),
        ]);
        assert_eq!(out, dt(2024, 1, 1, 0, 0, 0));
    }

    #[test]
    fn service_status_strings_match_node() {
        assert_eq!(ServiceStatus::Starting.as_str(), "starting");
        assert_eq!(ServiceStatus::Running.as_str(), "running");
        assert_eq!(ServiceStatus::Stopped.as_str(), "stopped");
        assert_eq!(ServiceStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn health_status_strings_match_node() {
        assert_eq!(HealthStatus::Unknown.as_str(), "unknown");
        assert_eq!(HealthStatus::Healthy.as_str(), "healthy");
        assert_eq!(HealthStatus::Unhealthy.as_str(), "unhealthy");
    }

    #[test]
    fn to_runtime_service_keeps_all_fields() {
        let row = WorkspaceRuntimeServiceRow {
            id: "s1".into(),
            company_id: "c1".into(),
            service_name: "web".into(),
            status: "running".into(),
            lifecycle: "shared".into(),
            provider: "adapter_managed".into(),
            scope_type: "run".into(),
            health_status: "healthy".into(),
            last_used_at: dt(2024, 1, 1, 0, 0, 0),
            started_at: dt(2024, 1, 1, 0, 0, 0),
            created_at: dt(2024, 1, 1, 0, 0, 0),
            updated_at: dt(2024, 1, 2, 0, 0, 0),
            url: Some("http://x".into()),
            config_index: Some(0),
            ..Default::default()
        };
        let s = to_runtime_service(&row);
        assert_eq!(s.id, "s1");
        assert_eq!(s.url.as_deref(), Some("http://x"));
        assert_eq!(s.config_index, Some(0));
    }

    #[test]
    fn to_execution_workspace_summary_basic() {
        let row = ExecutionWorkspaceSummaryRow {
            id: "ws-1".into(),
            name: "Workspace 1".into(),
            mode: "isolated_workspace".into(),
            status: "active".into(),
            cwd: Some("/p".into()),
            branch_name: Some("main".into()),
            project_workspace_id: None,
            last_used_at: dt(2024, 1, 1, 0, 0, 0),
        };
        let s = to_execution_workspace_summary(&row);
        assert_eq!(s.id, "ws-1");
        assert_eq!(s.mode, "isolated_workspace");
    }

    #[test]
    fn select_primary_running_with_url_wins() {
        let services = vec![
            make_service("s1", "stopped", None),
            make_service("s2", "running", Some("http://a")),
            make_service("s3", "running", None),
        ];
        let s = select_primary_overview_service(&services).unwrap();
        assert_eq!(s.id, "s2");
    }

    #[test]
    fn select_primary_falls_back_to_url_only() {
        let services = vec![
            make_service("s1", "stopped", None),
            make_service("s2", "stopped", Some("http://b")),
        ];
        let s = select_primary_overview_service(&services).unwrap();
        assert_eq!(s.id, "s2");
    }

    #[test]
    fn select_primary_falls_back_to_running() {
        let services = vec![
            make_service("s1", "stopped", None),
            make_service("s2", "running", None),
        ];
        let s = select_primary_overview_service(&services).unwrap();
        assert_eq!(s.id, "s2");
    }

    #[test]
    fn select_primary_returns_first_when_none_match() {
        let services = vec![make_service("s1", "stopped", None)];
        let s = select_primary_overview_service(&services).unwrap();
        assert_eq!(s.id, "s1");
    }

    #[test]
    fn select_primary_empty_returns_none() {
        assert!(select_primary_overview_service(&[]).is_none());
    }

    #[test]
    fn to_workspace_overview_primary_service_projects_fields() {
        let s = make_service("a", "running", Some("http://x"));
        let out = to_workspace_overview_primary_service(Some(&s)).unwrap();
        assert_eq!(out.id, "a");
        assert_eq!(out.url.as_deref(), Some("http://x"));
    }

    #[test]
    fn to_workspace_overview_primary_service_none() {
        assert!(to_workspace_overview_primary_service(None).is_none());
    }

    #[test]
    fn uses_inherited_shared_no_runtime_in_metadata() {
        let meta = serde_json::json!({"config": {"provisionCommand": "pnpm i"}});
        let v = uses_inherited_project_runtime_services(
            "shared_workspace",
            Some("pws-1"),
            Some(&meta),
        );
        assert!(v);
    }

    #[test]
    fn uses_inherited_shared_with_runtime_in_metadata() {
        let meta = serde_json::json!({"config": {"workspaceRuntime": {"k": 1}}});
        let v = uses_inherited_project_runtime_services(
            "shared_workspace",
            Some("pws-1"),
            Some(&meta),
        );
        assert!(!v);
    }

    #[test]
    fn uses_inherited_not_shared_workspace() {
        let v = uses_inherited_project_runtime_services("isolated_workspace", Some("pws-1"), None);
        assert!(!v);
    }

    #[test]
    fn uses_inherited_no_project_workspace() {
        let v = uses_inherited_project_runtime_services("shared_workspace", None, None);
        assert!(!v);
    }

    fn make_service(id: &str, status: &str, url: Option<&str>) -> WorkspaceRuntimeService {
        WorkspaceRuntimeService {
            id: id.to_string(),
            company_id: "c".into(),
            project_id: None,
            project_workspace_id: None,
            execution_workspace_id: None,
            issue_id: None,
            scope_type: "run".into(),
            scope_id: None,
            service_name: "web".into(),
            status: status.into(),
            lifecycle: "shared".into(),
            reuse_key: None,
            command: None,
            cwd: None,
            port: None,
            url: url.map(|s| s.to_string()),
            provider: "adapter_managed".into(),
            provider_ref: None,
            owner_agent_id: None,
            started_by_run_id: None,
            last_used_at: dt(2024, 1, 1, 0, 0, 0),
            started_at: dt(2024, 1, 1, 0, 0, 0),
            stopped_at: None,
            stop_policy: None,
            health_status: "healthy".into(),
            config_index: None,
            created_at: dt(2024, 1, 1, 0, 0, 0),
            updated_at: dt(2024, 1, 1, 0, 0, 0),
        }
    }
}
