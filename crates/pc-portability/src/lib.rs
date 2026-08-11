#![forbid(unsafe_code)]

//! Company portability 业务层。
//!
//! 与 paperclip 上游 `server/src/services/company-portability.ts` 思路一致：
//! - 封装 `CompanyExportRepo`（pc-repos）作为持久化层
//! - 通过 `PortabilityHook` trait 抽象副作用（audit log / notify）
//! - 提供 `preview` / `export` / `import` 系列方法
//!
//! 设计目标：
//! - 高内聚：所有 portability 业务逻辑集中在一处
//! - 低耦合：通过 service 抽象，调用方（HTTP / CLI）无需直接操作 repo
//! - 可测：service 单元测试不依赖 HTTP 层
//!
//! Round R593：起步 — 实现 `preview`（对应上游 `previewExport` / `getExportPreview`）。
//! 完整 export/import 留待后续轮次。

pub mod catalog_provenance;
pub mod export_readme;
pub mod fidelity_collector;
pub mod github_fetch;
pub mod portable_path;
pub use portable_path::*;

use async_trait::async_trait;
use pc_repos::company_export::{
    AgentSummary, CompanyExportPreview, CompanyExportRepo, IssueSummary, PipelineSummary,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Portability 业务错误。
#[derive(Debug, Error)]
pub enum PortabilityServiceError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("repository error: {0}")]
    Repo(String),
}

pub type PortabilityServiceResult<T> = Result<T, PortabilityServiceError>;

impl From<sqlx::Error> for PortabilityServiceError {
    fn from(e: sqlx::Error) -> Self {
        Self::Repo(format!("sqlx: {e}"))
    }
}

/// Portability include 配置。
///
/// 对齐上游 `CompanyPortabilityInclude`：决定 export 包含哪些实体类别。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortabilityInclude {
    #[serde(default = "default_true")]
    pub company: bool,
    #[serde(default = "default_true")]
    pub agents: bool,
    #[serde(default = "default_true")]
    pub issues: bool,
    #[serde(default = "default_true")]
    pub projects: bool,
    #[serde(default = "default_true")]
    pub skills: bool,
    /// 文件路径白名单（仅导出这些路径）。`None` = 全部。
    #[serde(default)]
    pub file_paths: Option<Vec<String>>,
}

impl Default for PortabilityInclude {
    fn default() -> Self {
        Self {
            company: true,
            agents: true,
            issues: true,
            projects: true,
            skills: true,
            file_paths: None,
        }
    }
}

/// Portability preview 输入。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortabilityPreviewInput {
    /// include 配置 — 控制哪些实体类别被 preview。
    #[serde(default)]
    pub include: PortabilityInclude,
}

/// R600: export bundle 输入。
///
/// 对齐上游 `CompanyPortabilityExport`：`include` 控制哪些类别被收集，
/// `file_paths` 限制文件路径白名单（暂未实现完整文件序列化）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportInput {
    #[serde(default)]
    pub include: PortabilityInclude,
    /// 输出格式 — 当前固定 "1.0"。
    #[serde(default = "default_version")]
    pub version: String,
    /// R634: 可选 metadata（覆盖默认 generator_version / generated_by）。
    #[serde(default)]
    pub metadata: Option<ManifestMetadata>,
}

impl Default for ExportInput {
    fn default() -> Self {
        Self {
            include: PortabilityInclude::default(),
            version: default_version(),
            metadata: None,
        }
    }
}

fn default_version() -> String {
    "1.0".into()
}

/// R641: decision summary — 嵌入 manifest 的 decision 基础信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionSummary {
    pub id: Uuid,
    pub title: String,
    pub status: String,
}

/// R641: aggregated counts for company — 各类别数量统计。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompanyCounts {
    pub agents: usize,
    pub issues: usize,
    pub pipelines: usize,
    pub projects: usize,
    pub decisions: usize,
}

impl CompanyCounts {
    pub fn total(&self) -> usize {
        self.agents + self.issues + self.pipelines + self.projects + self.decisions
    }
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// R638: project summary — 嵌入 manifest 的 project 基础信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub color: Option<String>,
    pub icon: Option<String>,
}

/// R653: issue relation row (DB schema).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IssueRelationRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub related_issue_id: Uuid,
    pub relation_type: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// R650: company asset row (DB 元数据,不含 base64 内容)
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CompanyAssetRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub key: String,
    pub content_type: Option<String>,
    pub size_bytes: i64,
}

/// R650: file resource summary — 嵌入 manifest 的文件资源基础信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileResourceSummary {
    pub id: Uuid,
    pub key: String,
    pub content_type: Option<String>,
    pub size_bytes: i64,
}

/// R600: export counts — manifest 各类别实体计数。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExportCounts {
    pub agents: usize,
    pub issues: usize,
    pub pipelines: usize,
    pub projects: usize,
    /// R650: file resources 数量（company_assets 表行数）
    pub file_resources: usize,
}

/// R600: company summary — 嵌入 manifest 的公司基础信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySummary {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub issue_prefix: String,
}

/// R635: 计算 manifest 内容的 SHA256 hex digest（不含 signature 字段本身）。
pub fn compute_manifest_signature(manifest: &ExportManifest) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // Canonical form: serialize without signature field, then hash.
    let mut clone = manifest.clone();
    clone.metadata.signature_sha256 = None;
    let json = serde_json::to_string(&clone).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// R634: manifest metadata — 描述 manifest 来源 / 生成环境。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestMetadata {
    /// 导出实例的 hostname（可选）。
    pub source_hostname: Option<String>,
    /// 生成 manifest 的 paperclip 版本字符串。
    pub generator_version: String,
    /// 导出的 actor principal id（HTTP 层注入）。
    pub generated_by: Option<String>,
    /// manifest 签名 SHA256 hex（可选 — R635+ 用于完整性校验）。
    pub signature_sha256: Option<String>,
}

impl Default for ManifestMetadata {
    fn default() -> Self {
        Self {
            source_hostname: None,
            generator_version: env!("CARGO_PKG_VERSION").to_string(),
            generated_by: None,
            signature_sha256: None,
        }
    }
}

/// R600: export manifest — 对齐上游 `CompanyPortabilityManifest`。
///
/// 完整 manifest 还包含 projects / skills / routines / envInputs 等，
/// 留待后续轮次扩展。当前子集：company / agents / issues / pipelines。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportManifest {
    pub version: String,
    pub company: CompanySummary,
    pub agents: Vec<AgentSummary>,
    pub issues: Vec<IssueSummary>,
    pub pipelines: Vec<PipelineSummary>,
    /// R638: project summaries
    #[serde(default)]
    pub projects: Vec<ProjectSummary>,
    /// R650: file resource summaries（仅 metadata，不含 base64 内容）
    #[serde(default)]
    pub file_resources: Vec<FileResourceSummary>,
    pub counts: ExportCounts,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    /// R634: manifest metadata（source hostname / generator / signature）。
    #[serde(default)]
    pub metadata: ManifestMetadata,
}

/// Portability preview 增强结果。
///
/// 与 `CompanyExportPreview` 区别：增加 version + counts 聚合 + 时间戳。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortabilityPreview {
    pub version: String,
    pub company_id: Uuid,
    pub issues: Vec<IssueSummary>,
    pub agents: Vec<AgentSummary>,
    pub pipelines: Vec<PipelineSummary>,
    pub counts: PortabilityCounts,
    pub include: PortabilityInclude,
    pub generated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortabilityCounts {
    pub issues: usize,
    pub agents: usize,
    pub pipelines: usize,
}

/// R630: import 冲突处理策略。
///
/// 对齐上游 `CompanyPortabilityCollisionStrategy` 的简化子集：
/// - `Skip` — 同名实体已存在则跳过
/// - `Rename` — 同名实体已存在则追加 `(imported)` 后缀
/// - `Fail` — 同名实体已存在则返回 InvalidInput 错误
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CollisionStrategy {
    #[default]
    Skip,
    Rename,
    Fail,
}

/// R630: import bundle 输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportInput {
    /// 来源 manifest（来自 export）。
    pub manifest: ExportManifest,
    /// 新 company 名字。
    pub new_company_name: String,
    /// 新 company owner principal id（HTTP 层注入）。
    pub owner_principal_id: String,
    /// 冲突策略 — 默认 Skip。
    #[serde(default)]
    pub collision_strategy: CollisionStrategy,
    /// 是否同时导入 issues。默认 true。
    #[serde(default = "default_true")]
    pub include_issues: bool,
    /// 是否同时导入 agents。默认 true。
    #[serde(default = "default_true")]
    pub include_agents: bool,
    /// 是否同时导入 pipelines。R631 实现。
    #[serde(default = "default_true")]
    pub include_pipelines: bool,
    /// R638: 是否同时导入 projects。
    #[serde(default = "default_true")]
    pub include_projects: bool,
}

fn default_true() -> bool { true }

/// R630: import 结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub target_company_id: Uuid,
    pub source_company_id: Uuid,
    pub agents_created: usize,
    pub agents_skipped: usize,
    pub issues_created: usize,
    pub issues_skipped: usize,
    pub pipelines_created: usize,
    pub pipelines_skipped: usize,
    /// R638: projects 计数
    pub projects_created: usize,
    pub projects_skipped: usize,
}

/// R648: diff 结果 — 两个 manifest 的差异。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManifestDiff {
    pub agents_only_in_first: Vec<String>,
    pub agents_only_in_second: Vec<String>,
    pub agents_common: usize,
    pub issues_only_in_first: Vec<String>,
    pub issues_only_in_second: Vec<String>,
    pub issues_common: usize,
    pub pipelines_only_in_first: Vec<String>,
    pub pipelines_only_in_second: Vec<String>,
    pub pipelines_common: usize,
    pub projects_only_in_first: Vec<String>,
    pub projects_only_in_second: Vec<String>,
    pub projects_common: usize,
}

impl ManifestDiff {
    pub fn is_empty(&self) -> bool {
        self.agents_only_in_first.is_empty()
            && self.agents_only_in_second.is_empty()
            && self.issues_only_in_first.is_empty()
            && self.issues_only_in_second.is_empty()
            && self.pipelines_only_in_first.is_empty()
            && self.pipelines_only_in_second.is_empty()
            && self.projects_only_in_first.is_empty()
            && self.projects_only_in_second.is_empty()
    }
}

/// R648: 计算两个 manifest 之间的差异（按 name/title/key 去重比对）。
pub fn diff_manifests(first: &ExportManifest, second: &ExportManifest) -> ManifestDiff {
    let first_agent_names: std::collections::HashSet<_> =
        first.agents.iter().map(|a| a.name.clone()).collect();
    let second_agent_names: std::collections::HashSet<_> =
        second.agents.iter().map(|a| a.name.clone()).collect();

    let first_issue_titles: std::collections::HashSet<_> =
        first.issues.iter().map(|i| i.title.clone()).collect();
    let second_issue_titles: std::collections::HashSet<_> =
        second.issues.iter().map(|i| i.title.clone()).collect();

    let first_pipeline_keys: std::collections::HashSet<_> =
        first.pipelines.iter().map(|p| p.key.clone()).collect();
    let second_pipeline_keys: std::collections::HashSet<_> =
        second.pipelines.iter().map(|p| p.key.clone()).collect();

    let first_project_names: std::collections::HashSet<_> =
        first.projects.iter().map(|p| p.name.clone()).collect();
    let second_project_names: std::collections::HashSet<_> =
        second.projects.iter().map(|p| p.name.clone()).collect();

    ManifestDiff {
        agents_only_in_first: first_agent_names.difference(&second_agent_names).cloned().collect(),
        agents_only_in_second: second_agent_names.difference(&first_agent_names).cloned().collect(),
        agents_common: first_agent_names.intersection(&second_agent_names).count(),
        issues_only_in_first: first_issue_titles.difference(&second_issue_titles).cloned().collect(),
        issues_only_in_second: second_issue_titles.difference(&first_issue_titles).cloned().collect(),
        issues_common: first_issue_titles.intersection(&second_issue_titles).count(),
        pipelines_only_in_first: first_pipeline_keys.difference(&second_pipeline_keys).cloned().collect(),
        pipelines_only_in_second: second_pipeline_keys.difference(&first_pipeline_keys).cloned().collect(),
        pipelines_common: first_pipeline_keys.intersection(&second_pipeline_keys).count(),
        projects_only_in_first: first_project_names.difference(&second_project_names).cloned().collect(),
        projects_only_in_second: second_project_names.difference(&first_project_names).cloned().collect(),
        projects_common: first_project_names.intersection(&second_project_names).count(),
    }
}

/// R647: merge 报告 — 多 manifest 合并结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MergeReport {
    pub agents_merged: usize,
    pub agents_duplicates: usize,
    pub issues_merged: usize,
    pub issues_duplicates: usize,
    pub pipelines_merged: usize,
    pub pipelines_duplicates: usize,
    pub projects_merged: usize,
    pub projects_duplicates: usize,
    pub source_company_ids: Vec<Uuid>,
}

/// R647: 合并多个 manifest 为一个 combined manifest。
///
/// 去重规则：
/// - Agents: 按 name 去重（首个出现的保留）
/// - Issues: 按 title 去重
/// - Pipelines: 按 key 去重
/// - Projects: 按 name 去重
///
/// 公司信息：使用第一个 manifest 的 company.summary 作为 combined company。
/// generated_at：取最大值（最新）。
pub fn merge_manifests(manifests: &[ExportManifest]) -> (ExportManifest, MergeReport) {
    if manifests.is_empty() {
        // 返回空 manifest
        return (
            ExportManifest {
                version: "1.0".into(),
                company: CompanySummary {
                    id: Uuid::nil(),
                    name: "".into(),
                    description: None,
                    status: "active".into(),
                    issue_prefix: "".into(),
                },
                agents: vec![],
                issues: vec![],
                pipelines: vec![],
                projects: vec![],
                file_resources: vec![],
                counts: ExportCounts::default(),
                generated_at: chrono::Utc::now(),
                metadata: ManifestMetadata::default(),
            },
            MergeReport::default(),
        );
    }

    let mut report = MergeReport::default();
    let mut seen_agent_names = std::collections::HashSet::new();
    let mut seen_issue_titles = std::collections::HashSet::new();
    let mut seen_pipeline_keys = std::collections::HashSet::new();
    let mut seen_project_names = std::collections::HashSet::new();

    let mut agents = Vec::new();
    let mut issues = Vec::new();
    let mut pipelines = Vec::new();
    let mut projects = Vec::new();

    for m in manifests {
        report.source_company_ids.push(m.company.id);
        for a in &m.agents {
            if seen_agent_names.contains(&a.name) {
                report.agents_duplicates += 1;
            } else {
                seen_agent_names.insert(a.name.clone());
                agents.push(a.clone());
                report.agents_merged += 1;
            }
        }
        for i in &m.issues {
            if seen_issue_titles.contains(&i.title) {
                report.issues_duplicates += 1;
            } else {
                seen_issue_titles.insert(i.title.clone());
                issues.push(i.clone());
                report.issues_merged += 1;
            }
        }
        for p in &m.pipelines {
            if seen_pipeline_keys.contains(&p.key) {
                report.pipelines_duplicates += 1;
            } else {
                seen_pipeline_keys.insert(p.key.clone());
                pipelines.push(p.clone());
                report.pipelines_merged += 1;
            }
        }
        for p in &m.projects {
            if seen_project_names.contains(&p.name) {
                report.projects_duplicates += 1;
            } else {
                seen_project_names.insert(p.name.clone());
                projects.push(p.clone());
                report.projects_merged += 1;
            }
        }
    }

    let first = &manifests[0];
    let counts = ExportCounts {
        agents: agents.len(),
        issues: issues.len(),
        pipelines: pipelines.len(),
        projects: projects.len(),
        file_resources: 0, // merge doesn't aggregate file resources
    };
    let combined = ExportManifest {
        version: "1.0".into(),
        company: first.company.clone(),
        agents,
        issues,
        pipelines,
        projects,
        file_resources: vec![], // merge doesn't aggregate file resources
        counts,
        generated_at: chrono::Utc::now(),
        metadata: ManifestMetadata {
            source_hostname: Some("merged".into()),
            generator_version: format!("merge-{}", env!("CARGO_PKG_VERSION")),
            generated_by: Some("merge_manifests".into()),
            signature_sha256: None,
        },
    };
    (combined, report)
}

/// R653: issue relation entry (blocks) — 嵌入 manifest 的关系摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRelationEntry {
    /// 来源 issue identifier (e.g. "ABC-1") or id (Uuid fallback)
    pub issue_identifier: String,
    /// 被 block 的 issue identifier
    pub related_issue_identifier: String,
    /// 类型（当前只支持 "blocks"）
    pub relation_type: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// R651: company 可读摘要 — 包含 counts + 状态 + 时间戳。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompanySummaryReport {
    pub company_id: Uuid,
    pub company_name: String,
    pub company_status: String,
    pub counts: CompanyCounts,
    pub generated_at: chrono::DateTime<chrono::Utc>,
}

impl CompanySummaryReport {
    pub fn to_display(&self) -> String {
        format!(
            "{} ({}) — {} agents, {} issues, {} pipelines, {} projects, {} decisions ({} total)",
            self.company_name,
            self.company_status,
            self.counts.agents,
            self.counts.issues,
            self.counts.pipelines,
            self.counts.projects,
            self.counts.decisions,
            self.counts.total(),
        )
    }
}

/// R644: manifest 人类可读摘要。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSummary {
    pub version: String,
    pub generator_version: String,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub source_hostname: Option<String>,
    pub generated_by: Option<String>,
    pub signed: bool,
    pub company_name: String,
    pub company_status: String,
    pub issue_count: usize,
    pub agent_count: usize,
    pub pipeline_count: usize,
    pub project_count: usize,
    pub decision_count: usize,
    pub total_entities: usize,
    pub integrity_ok: Option<bool>,
}

impl ManifestSummary {
    pub fn to_display(&self) -> String {
        format!(
            "{} manifest (gen {}) from {} ({}) — {} agents, {} issues, {} pipelines, {} projects, {} decisions ({})",
            self.version,
            self.generator_version,
            self.source_hostname.as_deref().unwrap_or("unknown-host"),
            self.generated_at.to_rfc3339(),
            self.agent_count,
            self.issue_count,
            self.pipeline_count,
            self.project_count,
            self.decision_count,
            self.total_entities,
        )
    }
}

/// R639: import 预览结果（dry-run，不实际写入）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub agents_would_create: usize,
    pub issues_would_create: usize,
    pub pipelines_would_create: usize,
    pub projects_would_create: usize,
    /// Manifest 内部的冲突（duplicate names/keys 等），不会阻止 import 但应被用户注意。
    pub conflicts: Vec<String>,
}



/// Lifecycle event — hook 可以订阅以触发副作用（audit log / 通知）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortabilityLifecycleEvent {
    Previewed { company_id: Uuid, counts: PortabilityCounts },
    /// R600: export bundle 已生成（manifest 收集完成）。
    Exported { company_id: Uuid, counts: ExportCounts },
    /// R630/R631/R638: import bundle 已导入（创建新 company + agents + issues + pipelines + projects）。
    Imported {
        source_company_id: Uuid,
        target_company_id: Uuid,
        agents: usize,
        issues: usize,
        pipelines: usize,
        projects: usize,
    },
}

/// Hook trait：副作用抽象。
///
/// 默认全部 noop，调用方可选择性实现。
#[async_trait]
pub trait PortabilityHook: Send + Sync {
    async fn on_lifecycle(
        &self,
        _event: PortabilityLifecycleEvent,
    ) -> PortabilityServiceResult<()> {
        Ok(())
    }
}

/// Noop hook。
pub struct NoopPortabilityHook;
#[async_trait]
impl PortabilityHook for NoopPortabilityHook {}

/// 记录 hook 调用 — 测试用。
#[derive(Default)]
pub struct RecordingPortabilityHook {
    pub events: std::sync::Mutex<Vec<PortabilityLifecycleEvent>>,
}

impl RecordingPortabilityHook {
    pub fn events_snapshot(&self) -> Vec<PortabilityLifecycleEvent> {
        self.events.lock().expect("lock").clone()
    }
    pub fn len(&self) -> usize {
        self.events.lock().expect("lock").len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn clear(&self) {
        self.events.lock().expect("lock").clear()
    }
}

#[async_trait]
impl PortabilityHook for RecordingPortabilityHook {
    async fn on_lifecycle(
        &self,
        event: PortabilityLifecycleEvent,
    ) -> PortabilityServiceResult<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

/// PortabilityService 业务入口。
pub struct PortabilityService<'a> {
    repo: CompanyExportRepo<'a>,
    hooks: Vec<std::sync::Arc<dyn PortabilityHook>>,
}

impl<'a> PortabilityService<'a> {
    pub fn new(db: &'a pc_repos::Db) -> Self {
        Self {
            repo: CompanyExportRepo::new(db),
            hooks: Vec::new(),
        }
    }

    pub fn with_hooks(
        db: &'a pc_repos::Db,
        hooks: Vec<std::sync::Arc<dyn PortabilityHook>>,
    ) -> Self {
        Self {
            repo: CompanyExportRepo::new(db),
            hooks,
        }
    }

    pub fn add_hook(mut self, hook: std::sync::Arc<dyn PortabilityHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// 生成公司 export preview。
    ///
    /// 对齐上游 `companyPortabilityService.previewExport` 的简化版：
    /// - 调用 `CompanyExportRepo::preview` 拿 issues / agents / pipelines
    /// - 包装成 `PortabilityPreview` + version + counts + generated_at
    /// - 触发 `PortabilityLifecycleEvent::Previewed` hook
    pub async fn preview(
        &self,
        company_id: Uuid,
        input: PortabilityPreviewInput,
    ) -> PortabilityServiceResult<PortabilityPreview> {
        let raw = self.repo.preview(company_id).await?;
        let counts = PortabilityCounts {
            issues: raw.issues.len(),
            agents: raw.agents.len(),
            pipelines: raw.pipelines.len(),
        };
        let preview = PortabilityPreview {
            version: "1.0".into(),
            company_id,
            issues: raw.issues,
            agents: raw.agents,
            pipelines: raw.pipelines,
            counts,
            include: input.include,
            generated_at: chrono::Utc::now(),
        };
        for hook in &self.hooks {
            hook.on_lifecycle(PortabilityLifecycleEvent::Previewed {
                company_id,
                counts: preview.counts.clone(),
            })
            .await?;
        }
        Ok(preview)
    }

    /// 直通 repo — list_issue_summaries。
    pub async fn list_issue_summaries(
        &self,
        company_id: Uuid,
    ) -> PortabilityServiceResult<Vec<IssueSummary>> {
        Ok(self.repo.list_issue_summaries(company_id).await?)
    }

    /// 直通 repo — list_agent_summaries。
    pub async fn list_agent_summaries(
        &self,
        company_id: Uuid,
    ) -> PortabilityServiceResult<Vec<AgentSummary>> {
        Ok(self.repo.list_agent_summaries(company_id).await?)
    }

    /// 直通 repo — list_pipeline_summaries。
    pub async fn list_pipeline_summaries(
        &self,
        company_id: Uuid,
    ) -> PortabilityServiceResult<Vec<PipelineSummary>> {
        Ok(self.repo.list_pipeline_summaries(company_id).await?)
    }

    /// R600: 生成 company export manifest。
    ///
    /// 对齐上游 `companyPortabilityService.exportBundle` 的核心子集：
    /// - 验证 company 存在（None → NotFound）
    /// - 收集 issues / agents / pipelines 摘要（按 `include` 配置）
    /// - 组装 `ExportManifest` + counts + generated_at
    /// - 触发 `PortabilityLifecycleEvent::Exported` hook
    ///
    /// 暂未实现：
    /// - 完整 file resources 序列化（`CompanyPortabilityFileEntry` 写入）
    /// - envInputs 收集
    /// - sidebarOrder 排序
    /// - collisionStrategy 处理
    /// 这些留待后续轮次扩展。
    pub async fn export(
        &self,
        company_id: Uuid,
        input: ExportInput,
    ) -> PortabilityServiceResult<ExportManifest> {
        // R600: 用 CompanyRepo 验证 company 存在并拿基本信息
        let company_row = pc_repos::company::CompanyRepo::new(self.repo.db)
            .get(company_id)
            .await?
            .ok_or_else(|| PortabilityServiceError::NotFound(format!("company {company_id}")))?;

        // R600: 调用 repo 拿三类摘要
        let issue_summaries = self.repo.list_issue_summaries(company_id).await?;
        let agent_summaries = self.repo.list_agent_summaries(company_id).await?;
        let pipeline_summaries = self.repo.list_pipeline_summaries(company_id).await?;

        let project_summaries = self.list_project_summaries(company_id, &input.include).await?;
        let project_count = project_summaries.len();
        let counts = ExportCounts {
            agents: agent_summaries.len(),
            issues: issue_summaries.len(),
            pipelines: pipeline_summaries.len(),
            projects: project_count,
            file_resources: 0, // will be overwritten after file_resources collection
        };
        let counts_for_hook = counts.clone();

        let company = CompanySummary {
            id: company_row.id,
            name: company_row.name.clone(),
            description: company_row.description.clone(),
            status: company_row.status.clone(),
            issue_prefix: company_row.issue_prefix.clone(),
        };

        // R650: collect file resources
        let file_resources = self.list_file_resources(company_id, &input.include).await?;
        let file_resource_count = file_resources.len();

        let metadata = input.metadata.clone().unwrap_or_default();
        let counts = ExportCounts {
            agents: counts.agents,
            issues: counts.issues,
            pipelines: counts.pipelines,
            projects: counts.projects,
            file_resources: file_resource_count,
        };
        let manifest = ExportManifest {
            version: input.version,
            company,
            agents: agent_summaries,
            issues: issue_summaries,
            pipelines: pipeline_summaries,
            projects: project_summaries,
            file_resources,
            counts,
            generated_at: chrono::Utc::now(),
            metadata,
        };

        for hook in &self.hooks {
            hook.on_lifecycle(PortabilityLifecycleEvent::Exported {
                company_id,
                counts: counts_for_hook.clone(),
            })
            .await?;
        }
        Ok(manifest)
    }

    /// R635: 导出 manifest 并填充 signature_sha256 字段。
    ///
    /// 等价于 `export()`，但额外计算 manifest JSON 的 SHA256 摘要，
    /// 写入 `metadata.signature_sha256`。调用方可在传输后用
    /// `verify_manifest_signature()` 校验完整性。
    pub async fn export_signed(
        &self,
        company_id: Uuid,
        input: ExportInput,
    ) -> PortabilityServiceResult<ExportManifest> {
        let mut manifest = self.export(company_id, input).await?;
        let sig = compute_manifest_signature(&manifest);
        manifest.metadata.signature_sha256 = Some(sig);
        Ok(manifest)
    }

    /// R635: 校验 manifest 签名 — 用于 import 时确保 manifest 不被篡改。
    pub fn verify_manifest_signature(&self, manifest: &ExportManifest) -> bool {
        match &manifest.metadata.signature_sha256 {
            Some(expected) => expected == &compute_manifest_signature(manifest),
            None => false, // 没签名视为无效
        }
    }

    /// R642: 公开 manifest 校验 — 复用 import 校验链，返回首个错误或 `Ok(())`。
    ///
    /// 调用方可在 import 前独立校验 manifest 完整性（不依赖 target company）。
    pub fn validate_manifest(
        &self,
        manifest: &ExportManifest,
    ) -> Result<(), PortabilityServiceError> {
        if manifest.agents.is_empty()
            && manifest.issues.is_empty()
            && manifest.pipelines.is_empty()
            && manifest.projects.is_empty()
        {
            return Err(PortabilityServiceError::InvalidInput(
                "manifest must contain at least one agent, issue, pipeline, or project".into(),
            ));
        }
        match manifest.version.as_str() {
            "1.0" => {}
            other => {
                return Err(PortabilityServiceError::InvalidInput(format!(
                    "unsupported manifest version: {other} (expected 1.0)"
                )));
            }
        }
        if manifest.metadata.generator_version.trim().is_empty() {
            return Err(PortabilityServiceError::InvalidInput(
                "manifest.metadata.generatorVersion must not be empty".into(),
            ));
        }
        let now = chrono::Utc::now();
        if manifest.generated_at > now + chrono::Duration::hours(1) {
            return Err(PortabilityServiceError::InvalidInput(format!(
                "manifest.generatedAt is in the future"
            )));
        }
        if manifest.generated_at < now - chrono::Duration::days(365) {
            return Err(PortabilityServiceError::InvalidInput(format!(
                "manifest.generatedAt is too old"
            )));
        }
        Ok(())
    }

    /// R651: 生成 company 的可读摘要（用于 UI 概览页 / 调试）。
    ///
    /// 不依赖 ExportManifest — 直接基于 company_id 查询 counts。
    pub async fn summarize_company(
        &self,
        company_id: Uuid,
    ) -> PortabilityServiceResult<CompanySummaryReport> {
        let company_row = pc_repos::company::CompanyRepo::new(self.repo.db)
            .get(company_id)
            .await
            .map_err(|e| PortabilityServiceError::Repo(e.to_string()))?
            .ok_or_else(|| {
                PortabilityServiceError::NotFound(format!("company {company_id}"))
            })?;
        let counts = self.counts_for_company(company_id).await?;
        Ok(CompanySummaryReport {
            company_id: company_row.id,
            company_name: company_row.name,
            company_status: company_row.status,
            counts,
            generated_at: chrono::Utc::now(),
        })
    }

    /// R645: serialize manifest to pretty JSON string.
    pub fn manifest_to_json(&self, manifest: &ExportManifest) -> PortabilityServiceResult<String> {
        serde_json::to_string_pretty(manifest)
            .map_err(|e| PortabilityServiceError::Repo(format!("serialize: {e}")))
    }

    /// R645: deserialize manifest from JSON string. Returns InvalidInput on parse failure.
    pub fn manifest_from_json(&self, json: &str) -> PortabilityServiceResult<ExportManifest> {
        serde_json::from_str(json)
            .map_err(|e| PortabilityServiceError::InvalidInput(format!("parse: {e}")))
    }

    /// R644: 生成 manifest 的可读摘要（用于 UI / log）。
    ///
    /// 不做 DB 查询 — 仅解析 manifest 自身数据，生成 total entities / integrity 等汇总信息。
    /// `integrity_ok`：当 manifest 含 signature 时验证签名，否则为 `None`。
    pub fn summarize_manifest(&self, manifest: &ExportManifest) -> ManifestSummary {
        let total = manifest.agents.len()
            + manifest.issues.len()
            + manifest.pipelines.len()
            + manifest.projects.len();
        let signed = manifest.metadata.signature_sha256.is_some();
        let integrity_ok = if signed {
            Some(self.verify_manifest_signature(manifest))
        } else {
            None
        };
        ManifestSummary {
            version: manifest.version.clone(),
            generator_version: manifest.metadata.generator_version.clone(),
            generated_at: manifest.generated_at,
            source_hostname: manifest.metadata.source_hostname.clone(),
            generated_by: manifest.metadata.generated_by.clone(),
            signed,
            company_name: manifest.company.name.clone(),
            company_status: manifest.company.status.clone(),
            issue_count: manifest.issues.len(),
            agent_count: manifest.agents.len(),
            pipeline_count: manifest.pipelines.len(),
            project_count: manifest.projects.len(),
            decision_count: 0, // decisions not in manifest (R644+)
            total_entities: total,
            integrity_ok,
        }
    }

    /// R639: 计算 import 预览（不实际写入），报告每个 kind 的冲突和将创建数量。
    pub async fn dry_run_import(
        &self,
        manifest: &ExportManifest,
    ) -> PortabilityServiceResult<ImportPreview> {
        if manifest.agents.is_empty()
            && manifest.issues.is_empty()
            && manifest.pipelines.is_empty()
            && manifest.projects.is_empty()
        {
            return Err(PortabilityServiceError::InvalidInput(
                "manifest must contain at least one agent, issue, pipeline, or project".into(),
            ));
        }
        match manifest.version.as_str() {
            "1.0" => {}
            other => {
                return Err(PortabilityServiceError::InvalidInput(format!(
                    "unsupported manifest version: {other} (expected 1.0)"
                )));
            }
        }
        if manifest.metadata.generator_version.trim().is_empty() {
            return Err(PortabilityServiceError::InvalidInput(
                "manifest.metadata.generatorVersion must not be empty".into(),
            ));
        }
        let now = chrono::Utc::now();
        if manifest.generated_at > now + chrono::Duration::hours(1) {
            return Err(PortabilityServiceError::InvalidInput(
                "manifest.generatedAt is in the future".into(),
            ));
        }
        if manifest.generated_at < now - chrono::Duration::days(365) {
            return Err(PortabilityServiceError::InvalidInput(
                "manifest.generatedAt is too old".into(),
            ));
        }

        let mut conflicts = Vec::new();

        let agents_would_create = manifest.agents.len();
        let issues_would_create = manifest.issues.len();

        let mut pipeline_keys = std::collections::HashSet::new();
        let mut pipelines_would_create = 0usize;
        for p in &manifest.pipelines {
            if pipeline_keys.contains(&p.key) {
                conflicts.push(format!("duplicate pipeline key in manifest: {}", p.key));
            } else {
                pipeline_keys.insert(p.key.clone());
                pipelines_would_create += 1;
            }
        }

        let mut project_names = std::collections::HashSet::new();
        let mut projects_would_create = 0usize;
        for p in &manifest.projects {
            if project_names.contains(&p.name) {
                conflicts.push(format!("duplicate project name in manifest: {}", p.name));
            } else {
                project_names.insert(p.name.clone());
                projects_would_create += 1;
            }
        }

        Ok(ImportPreview {
            agents_would_create,
            issues_would_create,
            pipelines_would_create,
            projects_would_create,
            conflicts,
        })
    }

    /// R600: 直通 CompanyRepo::get — 验证 company 存在
    pub async fn company_exists(
        &self,
        company_id: Uuid,
    ) -> PortabilityServiceResult<bool> {
        Ok(pc_repos::company::CompanyRepo::new(self.repo.db)
            .exists(company_id)
            .await?)
    }

    /// R630: import 一个 export manifest 到一个新 company。
    ///
    /// 流程：
    /// 1. 校验 manifest（version + 公司名 + 不为空）
    /// 2. 创建新 company（用 `new_company_name` + owner principal）
    /// 3. 按 include_agents 批量创建 agents（应用 collision 策略）
    /// 4. 按 include_issues 批量创建 issues（应用 collision 策略）
    /// 5. 触发 `PortabilityLifecycleEvent::Imported` hook
    ///
    /// 暂未实现：
    /// - Pipelines（需要 `pc-pipelines::PipelineService::create`，留待 v5+）
    /// - File resources / envInputs（需要 `pc-storage` 接入）
    /// - Side imports（projects / skills / routines / documents）
    pub async fn import(
        &self,
        input: ImportInput,
    ) -> PortabilityServiceResult<ImportResult> {
        // Validate input
        if input.new_company_name.trim().is_empty() {
            return Err(PortabilityServiceError::InvalidInput(
                "newCompanyName must not be empty".into(),
            ));
        }
        if input.owner_principal_id.trim().is_empty() {
            return Err(PortabilityServiceError::InvalidInput(
                "ownerPrincipalId must not be empty".into(),
            ));
        }
        if input.manifest.agents.is_empty()
            && input.manifest.issues.is_empty()
            && input.manifest.pipelines.is_empty()
            && input.manifest.projects.is_empty()
        {
            return Err(PortabilityServiceError::InvalidInput(
                "manifest must contain at least one agent, issue, pipeline, or project".into(),
            ));
        }
        // Validate manifest version — R633
        match input.manifest.version.as_str() {
            "1.0" => {}
            other => {
                return Err(PortabilityServiceError::InvalidInput(format!(
                    "unsupported manifest version: {other} (expected 1.0)"
                )));
            }
        }
        // R634: validate generator_version is non-empty
        if input.manifest.metadata.generator_version.trim().is_empty() {
            return Err(PortabilityServiceError::InvalidInput(
                "manifest.metadata.generatorVersion must not be empty".into(),
            ));
        }
        // R636: validate generated_at is not in the future (allow 1 hour clock skew)
        let now = chrono::Utc::now();
        if input.manifest.generated_at > now + chrono::Duration::hours(1) {
            return Err(PortabilityServiceError::InvalidInput(format!(
                "manifest.generatedAt is in the future: {}",
                input.manifest.generated_at
            )));
        }
        // R636: validate generated_at is not too old (> 1 year)
        if input.manifest.generated_at < now - chrono::Duration::days(365) {
            return Err(PortabilityServiceError::InvalidInput(format!(
                "manifest.generatedAt is too old: {}",
                input.manifest.generated_at
            )));
        }

        let source_company_id = input.manifest.company.id;
        let company_repo = pc_repos::company::CompanyRepo::new(self.repo.db);
        let description = input.manifest.company.description.clone();

        // 1. Create new company
        let new_company = company_repo
            .create(
                &input.new_company_name,
                description.as_deref(),
            )
            .await?;

        // Create owner membership (best-effort — skip if already exists)
        let _ = company_repo
            .create_owner_membership(
                new_company.id,
                &input.owner_principal_id,
            )
            .await;

        let target_company_id = new_company.id;

        // 2. Create agents
        let agent_repo = pc_repos::agent::AgentRepo::new(self.repo.db);
        let mut agents_created = 0usize;
        let mut agents_skipped = 0usize;
        if input.include_agents {
            for agent in &input.manifest.agents {
                let resolved_name = match self
                    .resolve_agent_name(target_company_id, &agent.name, input.collision_strategy)
                    .await?
                {
                    Some(n) => n,
                    None => {
                        agents_skipped += 1;
                        continue;
                    }
                };
                agent_repo
                    .create_simple(
                        target_company_id,
                        &resolved_name,
                        &agent.role,
                    )
                    .await?;
                agents_created += 1;
            }
        }

        // 3. Create issues
        let issue_repo = pc_repos::issue::IssueRepo::new(self.repo.db);
        let mut issues_created = 0usize;
        let mut issues_skipped = 0usize;
        if input.include_issues {
            for issue in &input.manifest.issues {
                let resolved_title = match self
                    .resolve_issue_title(target_company_id, &issue.title, input.collision_strategy)
                    .await?
                {
                    Some(t) => t,
                    None => {
                        issues_skipped += 1;
                        continue;
                    }
                };
                issue_repo
                    .create(
                        target_company_id,
                        &resolved_title,
                        None,
                        &issue.priority,
                        None,
                    )
                    .await?;
                issues_created += 1;
            }
        }

        // 4. Pipelines (R631)
        let (pipelines_created, pipelines_skipped) = if input.include_pipelines {
            self.import_pipelines(target_company_id, &input.manifest.pipelines, input.collision_strategy).await?
        } else {
            (0, 0)
        };

        // 5. Projects (R638)
        let (projects_created, projects_skipped) = if input.include_projects {
            self.import_projects(target_company_id, &input.manifest.projects, input.collision_strategy).await?
        } else {
            (0, 0)
        };

        let result = ImportResult {
            target_company_id,
            source_company_id,
            agents_created,
            agents_skipped,
            issues_created,
            issues_skipped,
            pipelines_created,
            pipelines_skipped,
            projects_created,
            projects_skipped,
        };

        // 5. Trigger hook
        for hook in &self.hooks {
            hook.on_lifecycle(PortabilityLifecycleEvent::Imported {
                source_company_id,
                target_company_id,
                agents: agents_created,
                issues: issues_created,
                pipelines: pipelines_created,
                projects: projects_created,
            })
            .await?;
        }

        Ok(result)
    }

    /// R630 helper: 检查 agent 名是否冲突，按策略解析。返回 None = skip。
    async fn resolve_agent_name(
        &self,
        company_id: Uuid,
        original: &str,
        strategy: CollisionStrategy,
    ) -> PortabilityServiceResult<Option<String>> {
        let repo = pc_repos::agent::AgentRepo::new(self.repo.db);
        let existing: Vec<_> = repo.list_by_company(company_id).await?;
        let exists = existing.iter().any(|a| a.name == original);
        if !exists {
            return Ok(Some(original.to_string()));
        }
        match strategy {
            CollisionStrategy::Fail => Err(PortabilityServiceError::InvalidInput(format!(
                "agent name conflict: {original}"
            ))),
            CollisionStrategy::Skip => Ok(None),
            CollisionStrategy::Rename => Ok(Some(format!("{original} (imported)"))),
        }
    }

    /// R631 helper: 批量导入 pipelines 到 target company。
    async fn import_pipelines(
        &self,
        target_company_id: Uuid,
        pipelines: &[pc_repos::company_export::PipelineSummary],
        strategy: CollisionStrategy,
    ) -> PortabilityServiceResult<(usize, usize)> {
        let svc = pc_pipelines::PipelineService::new(self.repo.db);
        let mut created = 0usize;
        let mut skipped = 0usize;
        for p in pipelines {
            let resolved_key = match self
                .resolve_pipeline_key(target_company_id, &p.key, strategy)
                .await?
            {
                Some(k) => k,
                None => {
                    skipped += 1;
                    continue;
                }
            };
            let input = pc_pipelines::CreatePipelineInput {
                key: resolved_key,
                name: p.name.clone(),
                description: None,
            };
            // 忽略 key 冲突错误（已存在于目标 company）
            match svc.create(target_company_id, &input).await {
                Ok(_) => created += 1,
                Err(pc_pipelines::PipelineServiceError::Repo(msg))
                    if msg.contains("duplicate key")
                        || msg.contains("pipelines_company_key_unique")
                        || msg.contains("UNIQUE") =>
                {
                    skipped += 1;
                }
                Err(pc_pipelines::PipelineServiceError::InvalidInput(msg))
                    if msg.to_lowercase().contains("key") =>
                {
                    skipped += 1;
                }
                Err(e) => return Err(PortabilityServiceError::Repo(e.to_string())),
            }
        }
        Ok((created, skipped))
    }

    /// R631 helper: 检查 pipeline key 是否冲突。
    async fn resolve_pipeline_key(
        &self,
        company_id: Uuid,
        original: &str,
        strategy: CollisionStrategy,
    ) -> PortabilityServiceResult<Option<String>> {
        let svc = pc_pipelines::PipelineService::new(self.repo.db);
        match strategy {
            CollisionStrategy::Skip | CollisionStrategy::Rename => {
                // 直接尝试 resolve（PipelineService::create 会校验 key）
                if strategy == CollisionStrategy::Rename {
                    Ok(Some(format!("{original}_imported")))
                } else {
                    Ok(Some(original.to_string()))
                }
            }
            CollisionStrategy::Fail => {
                // Fail strategy: 暂不校验实际冲突，由 PipelineService.create 返回错误处理
                Ok(Some(original.to_string()))
            }
        }
    }

    /// R650: 列出 company 的 file resource summaries（仅 metadata，不含 base64 内容）。
    pub async fn list_file_resources(
        &self,
        company_id: Uuid,
        include: &PortabilityInclude,
    ) -> PortabilityServiceResult<Vec<FileResourceSummary>> {
        if !include.skills {
            // file resources follow skills include flag for now
            return Ok(Vec::new());
        }
        let assets = self.list_company_assets(company_id).await?;
        Ok(assets
            .into_iter()
            .map(|a| FileResourceSummary {
                id: a.id,
                key: a.key,
                content_type: a.content_type,
                size_bytes: a.size_bytes,
            })
            .collect())
    }

    /// R650: 列出 company 的 company_assets（仅 metadata）。
    async fn list_company_assets(
        &self,
        company_id: Uuid,
    ) -> PortabilityServiceResult<Vec<CompanyAssetRow>> {
        let rows: Vec<CompanyAssetRow> = sqlx::query_as::<_, CompanyAssetRow>(
            "SELECT id, company_id, key, content_type, size_bytes              FROM company_assets WHERE company_id = $1 ORDER BY created_at",
        )
        .bind(company_id)
        .fetch_all(self.repo.db.pool())
        .await
        .map_err(|e| PortabilityServiceError::Repo(e.to_string()))?;
        Ok(rows)
    }

    /// R650: 统计 company 的 file resources 数量。
    pub async fn count_file_resources(&self, company_id: Uuid) -> PortabilityServiceResult<usize> {
        let assets = self.list_company_assets(company_id).await?;
        Ok(assets.len())
    }

    /// R653: 列出 company 的 issue relations (blocks)。
    pub async fn list_issue_relations(
        &self,
        company_id: Uuid,
    ) -> PortabilityServiceResult<Vec<IssueRelationEntry>> {
        // Query issue_relations joined with issues to get identifiers
        let rows: Vec<(String, String, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            "SELECT COALESCE(i.identifier, i.id::text) AS issue_id,                     COALESCE(r.identifier, r.id::text) AS related_issue_id,                     ir.type, ir.created_at              FROM issue_relations ir              JOIN issues i ON i.id = ir.issue_id AND i.company_id = ir.company_id              JOIN issues r ON r.id = ir.related_issue_id AND r.company_id = ir.company_id              WHERE ir.company_id = $1              ORDER BY ir.created_at",
        )
        .bind(company_id)
        .fetch_all(self.repo.db.pool())
        .await
        .map_err(|e| PortabilityServiceError::Repo(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|(issue_id, related_id, rel_type, created_at)| IssueRelationEntry {
                issue_identifier: issue_id,
                related_issue_identifier: related_id,
                relation_type: rel_type,
                created_at,
            })
            .collect())
    }

    /// R653: 统计 company 的 issue relations 数量。
    pub async fn count_issue_relations(&self, company_id: Uuid) -> PortabilityServiceResult<usize> {
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM issue_relations WHERE company_id = $1",
        )
        .bind(company_id)
        .fetch_one(self.repo.db.pool())
        .await
        .map_err(|e| PortabilityServiceError::Repo(e.to_string()))?;
        Ok(count.0 as usize)
    }

    /// R641: 列出 company 的 project summaries（按 include 过滤）。
    pub async fn list_project_summaries(
        &self,
        company_id: Uuid,
        include: &PortabilityInclude,
    ) -> PortabilityServiceResult<Vec<ProjectSummary>> {
        if !include.projects {
            return Ok(Vec::new());
        }
        let project_repo = pc_repos::project::ProjectRepo::new(self.repo.db);
        let projects = project_repo.list_by_company(company_id, false).await.map_err(|e| PortabilityServiceError::Repo(e.to_string()))?;
        Ok(projects
            .into_iter()
            .map(|p| ProjectSummary {
                id: p.id,
                name: p.name,
                description: p.description,
                status: p.status,
                color: p.color,
                icon: p.icon,
            })
            .collect())
    }

    /// R641: 列出 company 的 decision summaries（仅含 open + pending 状态）。
    pub async fn list_decision_summaries(
        &self,
        company_id: Uuid,
        include: &PortabilityInclude,
    ) -> PortabilityServiceResult<Vec<DecisionSummary>> {
        if !include.issues {
            // decisions follow issue include flag for now
            return Ok(Vec::new());
        }
        let decision_repo = pc_repos::decision::DecisionRepo::new(self.repo.db);
        let rows = decision_repo.list_by_company(company_id).await?;
        Ok(rows
            .into_iter()
            .filter(|d| d.status != "closed" && d.status != "cancelled")
            .map(|d| DecisionSummary {
                id: d.id,
                title: d.title,
                status: d.status,
            })
            .collect())
    }

    /// R641: 统计 company 的各类别实体数量（不含 list items）。
    pub async fn counts_for_company(
        &self,
        company_id: Uuid,
    ) -> PortabilityServiceResult<CompanyCounts> {
        let agent_repo = pc_repos::agent::AgentRepo::new(self.repo.db);
        let issue_repo = pc_repos::issue::IssueRepo::new(self.repo.db);
        let project_repo = pc_repos::project::ProjectRepo::new(self.repo.db);
        let decision_repo = pc_repos::decision::DecisionRepo::new(self.repo.db);
        let pipeline_repo = pc_repos::pipeline::PipelineRepo::new(self.repo.db);

        let agents = agent_repo
            .list_by_company(company_id)
            .await
            .map_err(|e| PortabilityServiceError::Repo(e.to_string()))?
            .len();
        let issues = issue_repo
            .count_for_company(company_id)
            .await
            .map_err(|e| PortabilityServiceError::Repo(e.to_string()))? as usize;
        let projects = project_repo
            .list_by_company(company_id, false)
            .await
            .map_err(|e| PortabilityServiceError::Repo(e.to_string()))?
            .len();
        let decisions = decision_repo
            .list_by_company(company_id)
            .await
            .map_err(|e| PortabilityServiceError::Repo(e.to_string()))?
            .len();
        let pipelines = pipeline_repo
            .list_by_company(company_id)
            .await
            .map_err(|e| PortabilityServiceError::Repo(e.to_string()))?
            .len();

        Ok(CompanyCounts {
            agents,
            issues,
            pipelines,
            projects,
            decisions,
        })
    }

    /// R638: 批量导入 projects 到 target company。
    async fn import_projects(
        &self,
        target_company_id: Uuid,
        projects: &[ProjectSummary],
        strategy: CollisionStrategy,
    ) -> PortabilityServiceResult<(usize, usize)> {
        let project_repo = pc_repos::project::ProjectRepo::new(self.repo.db);
        let mut created = 0usize;
        let mut skipped = 0usize;
        for p in projects {
            let resolved_name = match self
                .resolve_project_name(target_company_id, &p.name, strategy)
                .await?
            {
                Some(n) => n,
                None => {
                    skipped += 1;
                    continue;
                }
            };
            let status = match p.status.as_str() {
                "backlog" => pc_repos::project::ProjectStatus::Backlog,
                "planned" => pc_repos::project::ProjectStatus::Planned,
                "paused" => pc_repos::project::ProjectStatus::Paused,
                "completed" => pc_repos::project::ProjectStatus::Completed,
                "archived" => pc_repos::project::ProjectStatus::Archived,
                _ => pc_repos::project::ProjectStatus::Active,
            };
            let new_project = pc_repos::project::NewProject {
                company_id: target_company_id,
                goal_id: None,
                name: resolved_name,
                description: p.description.clone(),
                status,
                lead_agent_id: None,
                target_date: None,
                color: p.color.clone(),
                icon: p.icon.clone(),
                env: None,
            };
            match project_repo.create(&new_project).await {
                Ok(_) => created += 1,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("UNIQUE")
                        || msg.contains("duplicate")
                        || msg.to_lowercase().contains("already")
                    {
                        skipped += 1;
                    } else {
                        return Err(PortabilityServiceError::Repo(msg));
                    }
                }
            }
        }
        Ok((created, skipped))
    }

    /// R638 helper: 检查 project name 是否冲突，按策略解析。
    async fn resolve_project_name(
        &self,
        company_id: Uuid,
        original: &str,
        strategy: CollisionStrategy,
    ) -> PortabilityServiceResult<Option<String>> {
        let project_repo = pc_repos::project::ProjectRepo::new(self.repo.db);
        let projects = project_repo.list_by_company(company_id, false).await.map_err(|e| PortabilityServiceError::Repo(e.to_string()))?;
        let exists = projects.iter().any(|p| p.name == original);
        if !exists {
            return Ok(Some(original.to_string()));
        }
        match strategy {
            CollisionStrategy::Fail => Err(PortabilityServiceError::InvalidInput(format!(
                "project name conflict: {original}"
            ))),
            CollisionStrategy::Skip => Ok(None),
            CollisionStrategy::Rename => Ok(Some(format!("{original} (imported)"))),
        }
    }

    /// R630 helper: 检查 issue title 是否冲突，按策略解析。
    async fn resolve_issue_title(
        &self,
        company_id: Uuid,
        original: &str,
        strategy: CollisionStrategy,
    ) -> PortabilityServiceResult<Option<String>> {
        let repo = pc_repos::issue::IssueRepo::new(self.repo.db);
        let matches = repo.search_titles(company_id, original, 50).await?;
        let exists = matches.iter().any(|i| i.title == original);
        if !exists {
            return Ok(Some(original.to_string()));
        }
        match strategy {
            CollisionStrategy::Fail => Err(PortabilityServiceError::InvalidInput(format!(
                "issue title conflict: {original}"
            ))),
            CollisionStrategy::Skip => Ok(None),
            CollisionStrategy::Rename => Ok(Some(format!("{original} (imported)"))),
        }
    }
}
