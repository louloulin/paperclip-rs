//! `workspace_runtime_ready_presentation` — workspace ready 评论/Meta/呈现生成。
//!
//! 与 Node `buildWorkspaceReadyComment` / `buildWorkspaceReadyMetadata` /
//! `buildWorkspaceReadyPresentation` / `stableRuntimeServiceId` 1:1 对齐。
//!
//! 设计目标：纯函数模块，输入是 typed input，输出是 typed output。
//! 不引入 SQLx/tokio 依赖；可独立单元测试。
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::stable_string::stable_stringify;

// ============================================================================
// Constants
// ============================================================================

/// `COMMENT_METADATA_LABEL_MAX_LENGTH`：服务标签最大长度（Node 120）。
pub const COMMENT_METADATA_LABEL_MAX_LENGTH: usize = 120;

/// `COMMENT_TITLE_MAX_LENGTH`：呈现 title 最大长度（Node 160）。
pub const COMMENT_TITLE_MAX_LENGTH: usize = 160;

// ============================================================================
// Input types
// ============================================================================

/// `WorkspaceReadyCommentInput.workspace`：workspace summary。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadyWorkspaceSummary {
    pub strategy: String,
    pub branch_name: Option<String>,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub warnings: Vec<String>,
}

/// `WorkspaceReadyCommentInput.runtimeServices`：runtime service 列表。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadyRuntimeServiceSummary {
    pub service_name: String,
    pub url: Option<String>,
    pub reused: bool,
}

/// `WorkspaceReadyCommentInput`：Node `WorkspaceReadyCommentInput`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceReadyCommentInput {
    pub workspace: ReadyWorkspaceSummary,
    pub runtime_services: Vec<ReadyRuntimeServiceSummary>,
}

// ============================================================================
// Output types
// ============================================================================

/// `IssueCommentPresentation`：系统呈现描述。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueCommentPresentation {
    pub kind: String,
    pub tone: String,
    pub title: String,
    pub density: String,
    pub details_default_open: bool,
}

/// `IssueCommentMetadata`：结构化元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueCommentMetadata {
    pub version: i32,
    pub sections: Vec<MetadataSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataSection {
    pub title: String,
    pub rows: Vec<MetadataRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MetadataRow {
    KeyValue { label: String, value: String },
    Text { text: String },
}

// ============================================================================
// Helpers
// ============================================================================

/// `workspaceReadyServiceLabel(serviceName)`：trim + 截断。
///
/// 与 Node 1:1 对齐：
/// - trim 后空字符串 → "Service"
/// - 长度 > 120 → 截断到 119 + "…"
pub fn workspace_ready_service_label(service_name: &str) -> String {
    let label = service_name.trim();
    let label = if label.is_empty() { "Service" } else { label };
    if label.chars().count() > COMMENT_METADATA_LABEL_MAX_LENGTH {
        // 截断时按字符计数（Node slice 按 UTF-16 code unit，但我们用 chars 更稳）
        let mut s: String = label.chars().take(COMMENT_METADATA_LABEL_MAX_LENGTH - 1).collect();
        s.push('…');
        s
    } else {
        label.to_string()
    }
}

// ============================================================================
// buildWorkspaceReadyPresentation
// ============================================================================

/// `buildWorkspaceReadyPresentation(input)`：构造 IssueCommentPresentation。
///
/// 与 Node 1:1 对齐：
/// - title = "Workspace ready · {branchName ?? strategy}"
/// - hasWarnings = workspace.warnings.length > 0 → tone=warning / detailsDefaultOpen=true
/// - 否则 tone=info / detailsDefaultOpen=false
/// - title 长度 > 160 → 截断到 159 + "…"
pub fn build_workspace_ready_presentation(
    input: &WorkspaceReadyCommentInput,
) -> IssueCommentPresentation {
    let workspace_label = input
        .workspace
        .branch_name
        .clone()
        .unwrap_or_else(|| input.workspace.strategy.clone());
    let title = format!("Workspace ready · {}", workspace_label);
    let has_warnings = !input.workspace.warnings.is_empty();

    let final_title = if title.chars().count() > COMMENT_TITLE_MAX_LENGTH {
        let mut s: String = title.chars().take(COMMENT_TITLE_MAX_LENGTH - 1).collect();
        s.push('…');
        s
    } else {
        title
    };

    IssueCommentPresentation {
        kind: "system_notice".to_string(),
        tone: if has_warnings { "warning".to_string() } else { "info".to_string() },
        title: final_title,
        density: "compact".to_string(),
        details_default_open: has_warnings,
    }
}

// ============================================================================
// buildWorkspaceReadyMetadata
// ============================================================================

/// `buildWorkspaceReadyMetadata(input)`：构造 IssueCommentMetadata。
///
/// 与 Node 1:1 对齐：
/// - sections[0] = Workspace (Strategy/Branch/CWD/Worktree)
/// - 可选 Services section
/// - 可选 Warnings section
pub fn build_workspace_ready_metadata(
    input: &WorkspaceReadyCommentInput,
) -> IssueCommentMetadata {
    let mut workspace_rows: Vec<MetadataRow> = Vec::new();
    workspace_rows.push(MetadataRow::KeyValue {
        label: "Strategy".to_string(),
        value: input.workspace.strategy.clone(),
    });
    if let Some(branch) = &input.workspace.branch_name {
        workspace_rows.push(MetadataRow::KeyValue {
            label: "Branch".to_string(),
            value: branch.clone(),
        });
    }
    workspace_rows.push(MetadataRow::KeyValue {
        label: "CWD".to_string(),
        value: input.workspace.cwd.clone(),
    });
    if let Some(wt) = &input.workspace.worktree_path {
        if wt != &input.workspace.cwd {
            workspace_rows.push(MetadataRow::KeyValue {
                label: "Worktree".to_string(),
                value: wt.clone(),
            });
        }
    }

    let service_rows: Vec<MetadataRow> = input
        .runtime_services
        .iter()
        .map(|service| {
            let value = match &service.url {
                Some(url) => {
                    if service.reused {
                        format!("{} (reused)", url)
                    } else {
                        url.clone()
                    }
                }
                None => {
                    if service.reused {
                        "running (reused)".to_string()
                    } else {
                        "running".to_string()
                    }
                }
            };
            MetadataRow::KeyValue {
                label: workspace_ready_service_label(&service.service_name),
                value,
            }
        })
        .collect();

    let mut sections: Vec<MetadataSection> = vec![MetadataSection {
        title: "Workspace".to_string(),
        rows: workspace_rows,
    }];
    if !service_rows.is_empty() {
        sections.push(MetadataSection {
            title: "Services".to_string(),
            rows: service_rows,
        });
    }
    if !input.workspace.warnings.is_empty() {
        let warning_rows: Vec<MetadataRow> = input
            .workspace
            .warnings
            .iter()
            .map(|w| MetadataRow::Text { text: w.clone() })
            .collect();
        sections.push(MetadataSection {
            title: "Warnings".to_string(),
            rows: warning_rows,
        });
    }

    IssueCommentMetadata {
        version: 1,
        sections,
    }
}

// ============================================================================
// buildWorkspaceReadyComment
// ============================================================================

/// `buildWorkspaceReadyComment(input)`：构造 markdown 评论正文。
///
/// 与 Node 1:1 对齐：
/// - 固定标题 "## Workspace Ready"
/// - 顺序：Strategy/Branch/CWD/Worktree/Warnings/Services
pub fn build_workspace_ready_comment(input: &WorkspaceReadyCommentInput) -> String {
    let mut lines: Vec<String> = vec!["## Workspace Ready".to_string(), String::new()];
    lines.push(format!("- Strategy: `{}`", input.workspace.strategy));
    if let Some(branch) = &input.workspace.branch_name {
        lines.push(format!("- Branch: `{}`", branch));
    }
    lines.push(format!("- CWD: `{}`", input.workspace.cwd));
    if let Some(wt) = &input.workspace.worktree_path {
        if wt != &input.workspace.cwd {
            lines.push(format!("- Worktree: `{}`", wt));
        }
    }
    for warning in &input.workspace.warnings {
        lines.push(format!("- Warning: {}", warning));
    }
    for service in &input.runtime_services {
        let detail = match &service.url {
            Some(url) => format!("{}: {}", service.service_name, url),
            None => format!("{}: running", service.service_name),
        };
        let suffix = if service.reused { " (reused)" } else { "" };
        lines.push(format!("- Service: {}{}", detail, suffix));
    }
    lines.join("\n")
}

// ============================================================================
// stableRuntimeServiceId
// ============================================================================

/// `RuntimeServiceScopeType`：stableRuntimeServiceId 的 scopeType 输入。
///
/// 与 Node `RuntimeServiceRef["scopeType"]` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeServiceScopeType {
    Project,
    ProjectWorkspace,
    ExecutionWorkspace,
    Issue,
}

impl RuntimeServiceScopeType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::ProjectWorkspace => "project_workspace",
            Self::ExecutionWorkspace => "execution_workspace",
            Self::Issue => "issue",
        }
    }
}

/// `StableRuntimeServiceIdInput`：stableRuntimeServiceId 的输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StableRuntimeServiceIdInput {
    pub adapter_type: String,
    pub run_id: String,
    pub scope_type: RuntimeServiceScopeType,
    pub scope_id: Option<String>,
    pub service_name: String,
    pub report_id: Option<String>,
    pub provider_ref: Option<String>,
    pub reuse_key: Option<String>,
}

impl Default for StableRuntimeServiceIdInput {
    fn default() -> Self {
        Self {
            adapter_type: String::new(),
            run_id: String::new(),
            scope_type: RuntimeServiceScopeType::Project,
            scope_id: None,
            service_name: String::new(),
            report_id: None,
            provider_ref: None,
            reuse_key: None,
        }
    }
}

/// `stableRuntimeServiceId(input)`：构造稳定的 service id。
///
/// 与 Node 1:1 对齐：
/// - reportId 存在 → 直接返回 reportId
/// - 否则：SHA-256(stableStringify({...})) 取 hex 前 32 字符
/// - 返回 "{adapterType}-{hex32}"
pub fn stable_runtime_service_id(input: &StableRuntimeServiceIdInput) -> String {
    if let Some(rid) = &input.report_id {
        if !rid.is_empty() {
            return rid.clone();
        }
    }
    let mut obj: Map<String, Value> = Map::new();
    obj.insert("adapterType".into(), Value::String(input.adapter_type.clone()));
    obj.insert("runId".into(), Value::String(input.run_id.clone()));
    obj.insert(
        "scopeType".into(),
        Value::String(input.scope_type.as_str().to_string()),
    );
    if let Some(sid) = &input.scope_id {
        obj.insert("scopeId".into(), Value::String(sid.clone()));
    } else {
        obj.insert("scopeId".into(), Value::Null);
    }
    obj.insert("serviceName".into(), Value::String(input.service_name.clone()));
    if let Some(pr) = &input.provider_ref {
        obj.insert("providerRef".into(), Value::String(pr.clone()));
    } else {
        obj.insert("providerRef".into(), Value::Null);
    }
    if let Some(rk) = &input.reuse_key {
        obj.insert("reuseKey".into(), Value::String(rk.clone()));
    } else {
        obj.insert("reuseKey".into(), Value::Null);
    }
    let canonical = stable_stringify(&Value::Object(obj));
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{:x}", digest);
    let short = &hex[..32.min(hex.len())];
    format!("{}-{}", input.adapter_type, short)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn base_input() -> WorkspaceReadyCommentInput {
        WorkspaceReadyCommentInput {
            workspace: ReadyWorkspaceSummary {
                strategy: "hosted".into(),
                branch_name: Some("feat/x".into()),
                cwd: "/repo".into(),
                worktree_path: Some("/wt/repo".into()),
                warnings: vec![],
            },
            runtime_services: vec![],
        }
    }

    #[test]
    fn workspace_ready_service_label_trims_empty() {
        assert_eq!(workspace_ready_service_label(""), "Service");
        assert_eq!(workspace_ready_service_label("   "), "Service");
        assert_eq!(workspace_ready_service_label("web"), "web");
        assert_eq!(workspace_ready_service_label("  web  "), "web");
    }

    #[test]
    fn workspace_ready_service_label_truncates_long() {
        let long: String = "a".repeat(200);
        let out = workspace_ready_service_label(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), COMMENT_METADATA_LABEL_MAX_LENGTH);
    }

    #[test]
    fn presentation_no_warnings_uses_info_tone() {
        let input = base_input();
        let p = build_workspace_ready_presentation(&input);
        assert_eq!(p.kind, "system_notice");
        assert_eq!(p.tone, "info");
        assert_eq!(p.density, "compact");
        assert!(!p.details_default_open);
        assert!(p.title.contains("feat/x"));
    }

    #[test]
    fn presentation_uses_strategy_when_no_branch() {
        let mut input = base_input();
        input.workspace.branch_name = None;
        let p = build_workspace_ready_presentation(&input);
        assert_eq!(p.title, "Workspace ready · hosted");
    }

    #[test]
    fn presentation_warnings_use_warning_tone() {
        let mut input = base_input();
        input.workspace.warnings.push("something off".into());
        let p = build_workspace_ready_presentation(&input);
        assert_eq!(p.tone, "warning");
        assert!(p.details_default_open);
    }

    #[test]
    fn presentation_long_title_truncated() {
        let mut input = base_input();
        input.workspace.strategy = "x".repeat(200);
        input.workspace.branch_name = None;
        let p = build_workspace_ready_presentation(&input);
        assert!(p.title.ends_with('…'));
        assert!(p.title.chars().count() <= COMMENT_TITLE_MAX_LENGTH);
    }

    #[test]
    fn metadata_basic_sections() {
        let input = base_input();
        let m = build_workspace_ready_metadata(&input);
        assert_eq!(m.version, 1);
        assert_eq!(m.sections.len(), 1);
        assert_eq!(m.sections[0].title, "Workspace");
        assert_eq!(m.sections[0].rows.len(), 4); // Strategy, Branch, CWD, Worktree
    }

    #[test]
    fn metadata_omits_branch_when_absent() {
        let mut input = base_input();
        input.workspace.branch_name = None;
        let m = build_workspace_ready_metadata(&input);
        assert_eq!(m.sections[0].rows.len(), 3);
    }

    #[test]
    fn metadata_omits_worktree_when_same_as_cwd() {
        let mut input = base_input();
        input.workspace.worktree_path = Some("/repo".into());
        let m = build_workspace_ready_metadata(&input);
        assert_eq!(m.sections[0].rows.len(), 3);
    }

    #[test]
    fn metadata_includes_services_section() {
        let mut input = base_input();
        input.runtime_services.push(ReadyRuntimeServiceSummary {
            service_name: "web".into(),
            url: Some("http://localhost:3000".into()),
            reused: false,
        });
        input.runtime_services.push(ReadyRuntimeServiceSummary {
            service_name: "db".into(),
            url: None,
            reused: true,
        });
        let m = build_workspace_ready_metadata(&input);
        assert_eq!(m.sections.len(), 2);
        assert_eq!(m.sections[1].title, "Services");
        assert_eq!(m.sections[1].rows.len(), 2);
    }

    #[test]
    fn metadata_includes_warnings_section() {
        let mut input = base_input();
        input.workspace.warnings.push("warning 1".into());
        let m = build_workspace_ready_metadata(&input);
        assert_eq!(m.sections.len(), 2);
        assert_eq!(m.sections[1].title, "Warnings");
        match &m.sections[1].rows[0] {
            MetadataRow::Text { text } => assert_eq!(text, "warning 1"),
            _ => panic!("expected Text row"),
        }
    }

    #[test]
    fn metadata_service_value_with_url_and_reused() {
        let mut input = base_input();
        input.runtime_services.push(ReadyRuntimeServiceSummary {
            service_name: "web".into(),
            url: Some("http://x".into()),
            reused: true,
        });
        let m = build_workspace_ready_metadata(&input);
        if let MetadataRow::KeyValue { label, value } = &m.sections[1].rows[0] {
            assert_eq!(label, "web");
            assert_eq!(value, "http://x (reused)");
        } else {
            panic!("expected key_value row");
        }
    }

    #[test]
    fn metadata_service_value_without_url() {
        let mut input = base_input();
        input.runtime_services.push(ReadyRuntimeServiceSummary {
            service_name: "db".into(),
            url: None,
            reused: false,
        });
        let m = build_workspace_ready_metadata(&input);
        if let MetadataRow::KeyValue { value, .. } = &m.sections[1].rows[0] {
            assert_eq!(value, "running");
        } else {
            panic!("expected key_value row");
        }
    }

    #[test]
    fn comment_basic_lines() {
        let input = base_input();
        let c = build_workspace_ready_comment(&input);
        assert!(c.starts_with("## Workspace Ready"));
        assert!(c.contains("- Strategy: `hosted`"));
        assert!(c.contains("- Branch: `feat/x`"));
        assert!(c.contains("- CWD: `/repo`"));
        assert!(c.contains("- Worktree: `/wt/repo`"));
        assert!(!c.contains("- Warning:"));
        assert!(!c.contains("- Service:"));
    }

    #[test]
    fn comment_omits_branch_when_none() {
        let mut input = base_input();
        input.workspace.branch_name = None;
        let c = build_workspace_ready_comment(&input);
        assert!(!c.contains("Branch"));
    }

    #[test]
    fn comment_omits_worktree_when_same_as_cwd() {
        let mut input = base_input();
        input.workspace.worktree_path = Some("/repo".into());
        let c = build_workspace_ready_comment(&input);
        assert!(!c.contains("Worktree"));
    }

    #[test]
    fn comment_includes_warnings_and_services() {
        let mut input = base_input();
        input.workspace.warnings.push("disk full".into());
        input.runtime_services.push(ReadyRuntimeServiceSummary {
            service_name: "web".into(),
            url: Some("http://x".into()),
            reused: true,
        });
        input.runtime_services.push(ReadyRuntimeServiceSummary {
            service_name: "db".into(),
            url: None,
            reused: false,
        });
        let c = build_workspace_ready_comment(&input);
        assert!(c.contains("- Warning: disk full"));
        assert!(c.contains("- Service: web: http://x (reused)"));
        assert!(c.contains("- Service: db: running"));
    }

    #[test]
    fn stable_runtime_service_id_uses_report_id_when_present() {
        let input = StableRuntimeServiceIdInput {
            adapter_type: "docker".into(),
            run_id: "run-1".into(),
            scope_type: RuntimeServiceScopeType::ExecutionWorkspace,
            scope_id: Some("ws-1".into()),
            service_name: "web".into(),
            report_id: Some("report-abc".into()),
            provider_ref: None,
            reuse_key: None,
        };
        assert_eq!(stable_runtime_service_id(&input), "report-abc");
    }

    #[test]
    fn stable_runtime_service_id_deterministic() {
        let mk = || StableRuntimeServiceIdInput {
            adapter_type: "docker".into(),
            run_id: "run-1".into(),
            scope_type: RuntimeServiceScopeType::ExecutionWorkspace,
            scope_id: Some("ws-1".into()),
            service_name: "web".into(),
            report_id: None,
            provider_ref: Some("prov-1".into()),
            reuse_key: Some("k-1".into()),
        };
        let a = stable_runtime_service_id(&mk());
        let b = stable_runtime_service_id(&mk());
        assert_eq!(a, b);
        assert!(a.starts_with("docker-"));
        // 后缀是 hex 32 字符
        let parts: Vec<&str> = a.splitn(2, '-').collect();
        assert_eq!(parts[0], "docker");
        assert_eq!(parts[1].len(), 32);
        assert!(parts[1].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn stable_runtime_service_id_differs_with_input() {
        let mk = |adapter: &str, name: &str| StableRuntimeServiceIdInput {
            adapter_type: adapter.into(),
            run_id: "run-1".into(),
            scope_type: RuntimeServiceScopeType::ExecutionWorkspace,
            scope_id: Some("ws-1".into()),
            service_name: name.into(),
            report_id: None,
            provider_ref: None,
            reuse_key: None,
        };
        let a = stable_runtime_service_id(&mk("docker", "web"));
        let b = stable_runtime_service_id(&mk("docker", "api"));
        assert_ne!(a, b);
    }

    #[test]
    fn stable_runtime_service_id_ignores_empty_report_id() {
        let input = StableRuntimeServiceIdInput {
            adapter_type: "docker".into(),
            run_id: "run-1".into(),
            scope_type: RuntimeServiceScopeType::ExecutionWorkspace,
            scope_id: Some("ws-1".into()),
            service_name: "web".into(),
            report_id: Some("".into()),
            provider_ref: None,
            reuse_key: None,
        };
        let out = stable_runtime_service_id(&input);
        // 空字符串 reportId 视为不存在
        assert!(out.starts_with("docker-"));
    }

    #[test]
    fn scope_type_as_str_matches_node() {
        assert_eq!(RuntimeServiceScopeType::Project.as_str(), "project");
        assert_eq!(RuntimeServiceScopeType::ProjectWorkspace.as_str(), "project_workspace");
        assert_eq!(RuntimeServiceScopeType::ExecutionWorkspace.as_str(), "execution_workspace");
        assert_eq!(RuntimeServiceScopeType::Issue.as_str(), "issue");
    }
}
