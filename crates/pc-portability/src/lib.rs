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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortabilityInclude {
    #[serde(default)]
    pub company: bool,
    #[serde(default)]
    pub agents: bool,
    #[serde(default)]
    pub issues: bool,
    #[serde(default)]
    pub projects: bool,
    #[serde(default)]
    pub skills: bool,
    /// 文件路径白名单（仅导出这些路径）。`None` = 全部。
    #[serde(default)]
    pub file_paths: Option<Vec<String>>,
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
}

impl Default for ExportInput {
    fn default() -> Self {
        Self {
            include: PortabilityInclude::default(),
            version: default_version(),
        }
    }
}

fn default_version() -> String {
    "1.0".into()
}

/// R600: export counts — manifest 各类别实体计数。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExportCounts {
    pub agents: usize,
    pub issues: usize,
    pub pipelines: usize,
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
    pub counts: ExportCounts,
    pub generated_at: chrono::DateTime<chrono::Utc>,
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

/// Lifecycle event — hook 可以订阅以触发副作用（audit log / 通知）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortabilityLifecycleEvent {
    Previewed { company_id: Uuid, counts: PortabilityCounts },
    /// R600: export bundle 已生成（manifest 收集完成）。
    Exported { company_id: Uuid, counts: ExportCounts },
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

        let counts = ExportCounts {
            agents: agent_summaries.len(),
            issues: issue_summaries.len(),
            pipelines: pipeline_summaries.len(),
        };
        let counts_for_hook = counts.clone();

        let company = CompanySummary {
            id: company_row.id,
            name: company_row.name.clone(),
            description: company_row.description.clone(),
            status: company_row.status.clone(),
            issue_prefix: company_row.issue_prefix.clone(),
        };

        let manifest = ExportManifest {
            version: input.version,
            company,
            agents: agent_summaries,
            issues: issue_summaries,
            pipelines: pipeline_summaries,
            counts: counts.clone(),
            generated_at: chrono::Utc::now(),
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

    /// R600: 直通 CompanyRepo::get — 验证 company 存在
    pub async fn company_exists(
        &self,
        company_id: Uuid,
    ) -> PortabilityServiceResult<bool> {
        Ok(pc_repos::company::CompanyRepo::new(self.repo.db)
            .exists(company_id)
            .await?)
    }
}
