#![forbid(unsafe_code)]

//! Pipeline 业务层。
//!
//! 与 paperclip 上游 `server/src/services/pipelines.ts` 思路一致：
//! - 封装 `PipelineRepo`（pc-repos）作为持久化层
//! - 通过 `PipelineHook` trait 抽象副作用（activity log / notifications / audit）
//! - 提供基础 `list` / `get` / `create` / `update` / `delete` / `archive` 流
//!
//! 设计目标：
//! - 高内聚：所有 pipeline 业务逻辑集中在一处
//! - 低耦合：通过 service 抽象，调用方（HTTP / CLI）无需直接操作 repo
//! - 可测：service 单元测试不依赖 HTTP 层
//!
//! **R603 范围（v4 累计）**
//! - 5 个 pipeline read/write 方法（v1）
//! - 5 个 stage read/write 方法 + 3 个 stage lifecycle hook（v2）
//! - 4 个 transition read/write 方法 + 2 个 transition lifecycle hook（v3）
//! - 8 个 case read/write 方法 + 4 个 case lifecycle hook（v4）
//! - Activity hook 端由 `pc-http/src/hooks/pipeline_activity_hook.rs` 实现
//!
//! 后续轮次扩展（v5+）：
//! - case 子资源扩展：`issue_link` / `pending_suggestion` / `blocker` / `breakdown`
//! - stage automation 与 routine 集成
//! - 路由层从 `PipelineRepo::new(&state.db)` 迁移到 `PipelineService`

use async_trait::async_trait;
use pc_repos::pipeline::{
    PipelineCaseEventRow, PipelineCaseRow, PipelineRepo, PipelineRow, PipelineStageRow,
    PipelineTransitionRow,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Pipeline 业务错误。
#[derive(Debug, Error)]
pub enum PipelineServiceError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("repository error: {0}")]
    Repo(String),
}

pub type PipelineServiceResult<T> = Result<T, PipelineServiceError>;

impl From<sqlx::Error> for PipelineServiceError {
    fn from(e: sqlx::Error) -> Self {
        Self::Repo(format!("sqlx: {e}"))
    }
}

impl From<pc_repos::RepoError> for PipelineServiceError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Repo(e.to_string())
    }
}

/// Pipeline stage kind（与上游 paperclip PipelineStageKind 对齐）。
///
/// 数据库约束（`pipeline_stages_kind_check`）限定 4 种合法状态：
/// - `working`：进行中（初始状态）
/// - `review`：待审阅
/// - `done`：已完成
/// - `cancelled`：已取消
///
/// 用 `#[serde(rename_all = "snake_case")]` 让 JSON 自动 lowercase，
/// DB 存的是字符串（见 `pipeline_stages.kind` 列）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    Working,
    Review,
    Done,
    Cancelled,
}

impl StageKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Review => "review",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "working" => Some(Self::Working),
            "review" => Some(Self::Review),
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// 创建 pipeline stage 的最小输入。
///
/// 对齐上游 `createPipelineStageSchema` 的语义子集：
/// - `key` 必填（pipeline 内唯一）
/// - `name` 必填
/// - `kind` 必填（五选一）
/// - `position` 必填（ordering）
/// - `config` 可选 JSON 负载
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateStageMinimalInput {
    pub key: String,
    pub name: String,
    pub kind: StageKind,
    pub position: i32,
    #[serde(default)]
    pub config: serde_json::Value,
}

/// 更新 pipeline stage 的可选字段集合。
///
/// `key` 不可变（与上游 paperclip 一致）；其余 4 字段可选。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStagePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<StageKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// 创建 pipeline transition 的最小输入。
///
/// 对齐上游 `pipelineService.createTransition` 的语义子集：
/// - `from_stage_id` 必填（必须属于该 pipeline）
/// - `to_stage_id` 必填（必须属于该 pipeline）
/// - `label` 可选（人类可读的边标签）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateTransitionInput {
    pub from_stage_id: Uuid,
    pub to_stage_id: Uuid,
    #[serde(default)]
    pub label: Option<String>,
}

/// 创建 pipeline case 的最小输入。
///
/// 对齐上游 `pipelineService.ingestCase` 的语义子集：
/// - `case_key` 必填（pipeline 内唯一）
/// - `title` 必填
/// - `stage_id` 必填（必须属于该 pipeline）
/// - `summary` / `fields` / `parent_case_id` 可选
/// - `created_by_user_id` / `created_by_agent_id` / `origin_run_id` 由调用方注入
///   （HTTP 层从 auth context 读取）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateCaseMinimalInput {
    pub case_key: String,
    pub title: String,
    pub stage_id: Uuid,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub fields: serde_json::Value,
    #[serde(default)]
    pub parent_case_id: Option<Uuid>,
    #[serde(default)]
    pub created_by_user_id: Option<String>,
    #[serde(default)]
    pub created_by_agent_id: Option<Uuid>,
    #[serde(default)]
    pub origin_run_id: Option<Uuid>,
}

/// Case stage 转换输入。
///
/// `from_stage_id` 用于乐观锁（确保 case 当前确实在该 stage）；
/// 与 repo `update_case_stage(case_id, new_stage_id, from_stage_id)` 的 WHERE 子句对应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCaseStageInput {
    pub from_stage_id: Uuid,
    pub to_stage_id: Uuid,
}

/// Case lease 持有者。
///
/// 派生 `Clone + PartialEq + Eq` 便于 hook 单元测试做相等断言。
///
/// 不实现 `Serialize` / `Deserialize`：service API 内部枚举，
/// HTTP 层（后续 R603 v5 路由层）按需映射为 JSON。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseOwner {
    /// Agent 持有（lease_owner_type="agent", lease_agent_id 写入）。
    Agent(Uuid),
    /// User 持有（lease_owner_type="user", lease_user_id 写入）。
    User(String),
}

/// Case lease 申请输入（service 内部使用，不直接序列化）。
///
/// `lease_token` 由调用方生成（UUID）；repo 用它确保申请者持有合法 token。
/// 当前 service 不暴露给外部 token 验证 — 留待 R603 v5+ 与 authz 集成。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimCaseInput {
    pub owner: CaseOwner,
    pub lease_token: Uuid,
}

/// Case event 类型（与 `pipeline_case_events.type` 列对齐）。
///
/// 上游 paperclip 用字符串自由形式；这里枚举最常用的几种：
/// - `Created`：case 创建
/// - `Transitioned`：阶段转换
/// - `Commented`：评论（留待后续）
/// - `Other`：其它任意事件类型
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CaseEventKind {
    Created,
    Transitioned,
    Commented,
    Other,
}

impl CaseEventKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Transitioned => "transitioned",
            Self::Commented => "commented",
            Self::Other => "other",
        }
    }
}

/// Case event actor 类型。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CaseActorKind {
    User,
    Agent,
    System,
}

impl CaseActorKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
        }
    }
}

/// 创建 case event 的最小输入。
///
/// 对齐上游 `writeCaseEvent` 的语义子集：
/// - `kind` 必填（事件类型字符串）
/// - `actor` 必填（actor 类型 + 可选 actor id）
/// - `payload` 可选 JSON 负载
/// - `from_stage_id` / `to_stage_id` 可选（事件关联的 stage 转换）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateCaseEventInput {
    pub kind: CaseEventKind,
    pub actor: CaseActorKind,
    #[serde(default)]
    pub actor_user_id: Option<String>,
    #[serde(default)]
    pub actor_agent_id: Option<Uuid>,
    #[serde(default)]
    pub run_id: Option<Uuid>,
    #[serde(default)]
    pub from_stage_id: Option<Uuid>,
    #[serde(default)]
    pub to_stage_id: Option<Uuid>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// 创建 pipeline 的最小输入。
///
/// 对齐上游 `createPipelineSchema` 的语义子集：
/// - `key` 必填（公司内唯一标识符）
/// - `name` 必填
/// - `description` 可选
/// - `company_id` 作为 service 方法参数显式传入
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreatePipelineInput {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// 更新 pipeline 的可选字段集合。
///
/// 所有字段都是可选 — `None` 表示保持当前值。这与上游 `updatePipelineSchema`
/// 的 partial-update 语义一致。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePipelinePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Pipeline lifecycle event — hook 可以订阅以触发副作用。
#[derive(Debug, Clone)]
pub enum PipelineLifecycleEvent {
    /// Pipeline 被创建。
    Created { row: PipelineRow },
    /// Pipeline 被更新（name / description 任一字段变更）。
    Updated { row: PipelineRow },
    /// Pipeline 被软删除（archived_at 写入）。
    Archived { row: PipelineRow },
    /// Pipeline 被硬删除（DB 行删除）。
    Deleted { id: Uuid, company_id: Uuid },
}

/// Hook trait：副作用抽象。
///
/// 默认全部 noop，调用方可选择性实现。
#[async_trait]
pub trait PipelineHook: Send + Sync {
    /// Pipeline 创建后调用。
    async fn on_created(&self, _row: &PipelineRow) -> PipelineServiceResult<()> {
        Ok(())
    }
    /// Pipeline 更新后调用。
    async fn on_updated(&self, _row: &PipelineRow) -> PipelineServiceResult<()> {
        Ok(())
    }
    /// Pipeline 软删除（archive）后调用。
    async fn on_archived(&self, _row: &PipelineRow) -> PipelineServiceResult<()> {
        Ok(())
    }
    /// Pipeline 硬删除后调用（row 已不可读）。
    async fn on_deleted(&self, _id: Uuid, _company_id: Uuid) -> PipelineServiceResult<()> {
        Ok(())
    }

    // -------- Stage 子资源 lifecycle hooks（R603 v2） --------

    /// Pipeline stage 创建后调用。
    ///
    /// 第一个参数 `pipeline_id` 用于 hook 推断上下文；
    /// 第二个参数是新创建的 stage row（已含 generated id / timestamps）。
    async fn on_stage_created(
        &self,
        _pipeline_id: Uuid,
        _stage: &PipelineStageRow,
    ) -> PipelineServiceResult<()> {
        Ok(())
    }
    /// Pipeline stage 更新后调用（任一字段变更）。
    async fn on_stage_updated(
        &self,
        _stage: &PipelineStageRow,
    ) -> PipelineServiceResult<()> {
        Ok(())
    }
    /// Pipeline stage 硬删除后调用。
    ///
    /// stage row 已不可读，所以只传 stage_id + pipeline_id。
    /// `company_id` 未传：hook 若需要可在外部订阅时缓存。
    async fn on_stage_deleted(
        &self,
        _stage_id: Uuid,
        _pipeline_id: Uuid,
    ) -> PipelineServiceResult<()> {
        Ok(())
    }

    // -------- Transition 子资源 lifecycle hooks（R603 v3） --------

    /// Pipeline transition 创建后调用。
    async fn on_transition_created(
        &self,
        _transition: &PipelineTransitionRow,
    ) -> PipelineServiceResult<()> {
        Ok(())
    }
    /// Pipeline transition 硬删除后调用。
    ///
    /// transition row 已不可读，所以只传 transition_id + pipeline_id。
    async fn on_transition_deleted(
        &self,
        _transition_id: Uuid,
        _pipeline_id: Uuid,
    ) -> PipelineServiceResult<()> {
        Ok(())
    }

    // -------- Case lifecycle hooks（R603 v4） --------

    /// Pipeline case 创建后调用。
    async fn on_case_created(
        &self,
        _case: &PipelineCaseRow,
    ) -> PipelineServiceResult<()> {
        Ok(())
    }
    /// Pipeline case 阶段转换后调用（service.update_case_stage）。
    ///
    /// `from_stage_id` 与 `to_stage_id` 是新 / 旧 stage；case row 已是最新版本
    /// （含 `version + 1` 与 `terminal_kind/at` 副作用）。
    async fn on_case_stage_transitioned(
        &self,
        _case: &PipelineCaseRow,
        _from_stage_id: Uuid,
        _to_stage_id: Uuid,
    ) -> PipelineServiceResult<()> {
        Ok(())
    }
    /// Pipeline case 硬删除后调用。
    async fn on_case_deleted(
        &self,
        _case_id: Uuid,
        _company_id: Uuid,
    ) -> PipelineServiceResult<()> {
        Ok(())
    }
    /// Pipeline case event 写入后调用（service.create_case_event）。
    async fn on_case_event_recorded(
        &self,
        _case: &PipelineCaseRow,
        _event: &PipelineCaseEventRow,
    ) -> PipelineServiceResult<()> {
        Ok(())
    }
}

/// Noop hook。
pub struct NoopPipelineHook;
#[async_trait]
impl PipelineHook for NoopPipelineHook {}

/// 记录 hook 调用 — 测试用。
#[derive(Default)]
pub struct RecordingPipelineHook {
    pub created: std::sync::Mutex<Vec<Uuid>>,
    pub updated: std::sync::Mutex<Vec<Uuid>>,
    pub archived: std::sync::Mutex<Vec<Uuid>>,
    pub deleted: std::sync::Mutex<Vec<(Uuid, Uuid)>>, // (id, company_id)
    // R603 v2: stage 子资源
    pub stage_created: std::sync::Mutex<Vec<(Uuid, Uuid)>>, // (pipeline_id, stage_id)
    pub stage_updated: std::sync::Mutex<Vec<Uuid>>,         // stage_id
    pub stage_deleted: std::sync::Mutex<Vec<(Uuid, Uuid)>>,  // (stage_id, pipeline_id)
    // R603 v3: transition 子资源
    pub transition_created: std::sync::Mutex<Vec<Uuid>>,          // transition_id
    pub transition_deleted: std::sync::Mutex<Vec<(Uuid, Uuid)>>,  // (transition_id, pipeline_id)
    // R603 v4: case 子资源
    pub case_created: std::sync::Mutex<Vec<Uuid>>,                // case_id
    pub case_stage_transitioned: std::sync::Mutex<Vec<(Uuid, Uuid, Uuid)>>, // (case_id, from_id, to_id)
    pub case_deleted: std::sync::Mutex<Vec<(Uuid, Uuid)>>,        // (case_id, company_id)
    pub case_event_recorded: std::sync::Mutex<Vec<(Uuid, Uuid)>>, // (case_id, event_id)
}

#[async_trait]
impl PipelineHook for RecordingPipelineHook {
    async fn on_created(&self, row: &PipelineRow) -> PipelineServiceResult<()> {
        self.created.lock().expect("lock").push(row.id);
        Ok(())
    }
    async fn on_updated(&self, row: &PipelineRow) -> PipelineServiceResult<()> {
        self.updated.lock().expect("lock").push(row.id);
        Ok(())
    }
    async fn on_archived(&self, row: &PipelineRow) -> PipelineServiceResult<()> {
        self.archived.lock().expect("lock").push(row.id);
        Ok(())
    }
    async fn on_deleted(&self, id: Uuid, company_id: Uuid) -> PipelineServiceResult<()> {
        self.deleted.lock().expect("lock").push((id, company_id));
        Ok(())
    }
    async fn on_stage_created(
        &self,
        pipeline_id: Uuid,
        stage: &PipelineStageRow,
    ) -> PipelineServiceResult<()> {
        self.stage_created
            .lock()
            .expect("lock")
            .push((pipeline_id, stage.id));
        Ok(())
    }
    async fn on_stage_updated(
        &self,
        stage: &PipelineStageRow,
    ) -> PipelineServiceResult<()> {
        self.stage_updated.lock().expect("lock").push(stage.id);
        Ok(())
    }
    async fn on_stage_deleted(
        &self,
        stage_id: Uuid,
        pipeline_id: Uuid,
    ) -> PipelineServiceResult<()> {
        self.stage_deleted
            .lock()
            .expect("lock")
            .push((stage_id, pipeline_id));
        Ok(())
    }
    async fn on_transition_created(
        &self,
        transition: &PipelineTransitionRow,
    ) -> PipelineServiceResult<()> {
        self.transition_created
            .lock()
            .expect("lock")
            .push(transition.id);
        Ok(())
    }
    async fn on_transition_deleted(
        &self,
        transition_id: Uuid,
        pipeline_id: Uuid,
    ) -> PipelineServiceResult<()> {
        self.transition_deleted
            .lock()
            .expect("lock")
            .push((transition_id, pipeline_id));
        Ok(())
    }
    async fn on_case_created(
        &self,
        case: &PipelineCaseRow,
    ) -> PipelineServiceResult<()> {
        self.case_created.lock().expect("lock").push(case.id);
        Ok(())
    }
    async fn on_case_stage_transitioned(
        &self,
        case: &PipelineCaseRow,
        from_stage_id: Uuid,
        to_stage_id: Uuid,
    ) -> PipelineServiceResult<()> {
        self.case_stage_transitioned
            .lock()
            .expect("lock")
            .push((case.id, from_stage_id, to_stage_id));
        Ok(())
    }
    async fn on_case_deleted(
        &self,
        case_id: Uuid,
        company_id: Uuid,
    ) -> PipelineServiceResult<()> {
        self.case_deleted
            .lock()
            .expect("lock")
            .push((case_id, company_id));
        Ok(())
    }
    async fn on_case_event_recorded(
        &self,
        case: &PipelineCaseRow,
        event: &PipelineCaseEventRow,
    ) -> PipelineServiceResult<()> {
        self.case_event_recorded
            .lock()
            .expect("lock")
            .push((case.id, event.id));
        Ok(())
    }
}

/// Pipeline 业务 service。
///
/// 设计：包装 `PipelineRepo` + `Vec<Arc<dyn PipelineHook>>`。
/// 路由层只调 service，不再直接操作 repo。
pub struct PipelineService<'a> {
    repo: PipelineRepo<'a>,
    hooks: Vec<std::sync::Arc<dyn PipelineHook>>,
}

impl<'a> PipelineService<'a> {
    /// 构造一个无副作用 hook 的 service。
    #[must_use]
    pub fn new(db: &'a pc_repos::Db) -> Self {
        Self {
            repo: PipelineRepo::new(db),
            hooks: Vec::new(),
        }
    }

    /// 构造时注入副作用 hooks。
    #[must_use]
    pub fn with_hooks(db: &'a pc_repos::Db, hooks: Vec<std::sync::Arc<dyn PipelineHook>>) -> Self {
        Self {
            repo: PipelineRepo::new(db),
            hooks,
        }
    }

    /// 追加一个 hook（builder 风格）。
    pub fn add_hook(mut self, hook: std::sync::Arc<dyn PipelineHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// 当前已注册的 hook 数量。
    #[must_use]
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    /// 内部：取 row + 公司校验。
    async fn ensure_in_company(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> PipelineServiceResult<PipelineRow> {
        let row = self
            .repo
            .get(id)
            .await?
            .ok_or_else(|| PipelineServiceError::NotFound(format!("pipeline {id}")))?;
        if row.company_id != company_id {
            return Err(PipelineServiceError::NotFound(format!("pipeline {id}")));
        }
        Ok(row)
    }

    // ---------- 查询 ----------

    /// 列出某公司的所有 pipeline（按 created_at DESC）。
    pub async fn list_by_company(
        &self,
        company_id: Uuid,
    ) -> PipelineServiceResult<Vec<PipelineRow>> {
        Ok(self.repo.list_by_company(company_id).await?)
    }

    /// 列出所有 pipeline（跨公司，limit 截断）。
    pub async fn list_all(&self, limit: i64) -> PipelineServiceResult<Vec<PipelineRow>> {
        Ok(self.repo.list_all(limit).await?)
    }

    /// 按 id 获取 pipeline（带公司作用域校验）。
    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> PipelineServiceResult<Option<PipelineRow>> {
        match self.ensure_in_company(company_id, id).await {
            Ok(row) => Ok(Some(row)),
            Err(PipelineServiceError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    // ---------- 创建 ----------

    /// 创建一个 pipeline。
    ///
    /// 业务校验：
    /// - key 非空（trim）
    /// - name 非空（trim）
    ///
    /// 副作用：依次调用每个 hook 的 `on_created`。
    pub async fn create(
        &self,
        company_id: Uuid,
        input: &CreatePipelineInput,
    ) -> PipelineServiceResult<PipelineRow> {
        if input.key.trim().is_empty() {
            return Err(PipelineServiceError::InvalidInput(
                "key must not be empty".into(),
            ));
        }
        if input.name.trim().is_empty() {
            return Err(PipelineServiceError::InvalidInput(
                "name must not be empty".into(),
            ));
        }
        let description_ref = input.description.as_deref();
        let key_str: &str = &input.key;
        let name_str: &str = &input.name;
        let row = self
            .repo
            .create(company_id, key_str, name_str, description_ref)
            .await?;
        for hook in &self.hooks {
            hook.on_created(&row).await?;
        }
        Ok(row)
    }

    // ---------- 更新 ----------

    /// 更新 pipeline（部分字段）。
    ///
    /// No-op 语义：所有 patch 字段都为 None → 返回 existing，不写库、不触发 hook。
    ///
    /// 副作用：依次调用每个 hook 的 `on_updated`。
    pub async fn update(
        &self,
        company_id: Uuid,
        id: Uuid,
        patch: &UpdatePipelinePatch,
    ) -> PipelineServiceResult<PipelineRow> {
        let existing = self.ensure_in_company(company_id, id).await?;
        let is_empty = patch.name.is_none() && patch.description.is_none();
        if is_empty {
            return Ok(existing);
        }
        // 复用 repo.update — 仅传 name（partial）。description 不在 repo.update 内（不更新）。
        // 注意：上游 paperclip update 仅支持 name，description 写入需独立操作。
        let updated = self
            .repo
            .update(id, patch.name.as_deref(), patch.description.as_deref())
            .await?
            .ok_or_else(|| PipelineServiceError::NotFound(format!("pipeline {id}")))?;
        for hook in &self.hooks {
            hook.on_updated(&updated).await?;
        }
        Ok(updated)
    }

    // ---------- 删除 / 归档 ----------

    /// 硬删除 pipeline。
    ///
    /// 返回值：
    /// - `Ok(true)` 删除成功
    /// - `Ok(false)` pipeline 不存在或跨公司
    /// - `Err(Repo)` DB 错误
    ///
    /// 副作用：依次调用每个 hook 的 `on_deleted(id, company_id)`。
    pub async fn delete(&self, company_id: Uuid, id: Uuid) -> PipelineServiceResult<bool> {
        // 先校验存在 + 跨公司；row 不存在 → 返回 false 不报错
        let exists = self
            .repo
            .get(id)
            .await?
            .map(|r| r.company_id == company_id)
            .unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        let deleted = self.repo.delete(id).await?;
        if deleted {
            for hook in &self.hooks {
                hook.on_deleted(id, company_id).await?;
            }
        }
        Ok(deleted)
    }

    /// 软删除（archive）pipeline —— 写 `archived_at = now()`。
    ///
    /// 注意：当前 PipelineRepo 没有 archive API；本子集通过 SQL 直写 archived_at。
    /// 后续 R603 v2 可扩展为完整 PipelineStage 业务时一并补 repo.archive。
    pub async fn archive(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> PipelineServiceResult<PipelineRow> {
        let existing = self.ensure_in_company(company_id, id).await?;
        if existing.archived_at.is_some() {
            return Ok(existing);
        }
        let sql = "UPDATE pipelines SET archived_at = now(), updated_at = now() \
                   WHERE id = $1 RETURNING *";
        let updated: PipelineRow = sqlx::query_as::<_, PipelineRow>(sql)
            .bind(id)
            .fetch_one(self.repo.db.pool())
            .await?;
        for hook in &self.hooks {
            hook.on_archived(&updated).await?;
        }
        Ok(updated)
    }

    // =========================================================================
    // Pipeline stage 子资源（R603 v2）
    // =========================================================================

    /// 列出某 pipeline 的全部 stage（按 position ASC, created_at ASC）。
    ///
    /// 业务校验：先校验 pipeline 属于传入 company（跨公司 → NotFound）。
    pub async fn list_stages(
        &self,
        company_id: Uuid,
        pipeline_id: Uuid,
    ) -> PipelineServiceResult<Vec<PipelineStageRow>> {
        self.ensure_in_company(company_id, pipeline_id).await?;
        Ok(self.repo.list_stages(pipeline_id).await?)
    }

    /// 按 id 获取单个 stage。
    ///
    /// 语义与 `PipelineService::get` 一致：
    /// - stage 不存在 → `Ok(None)`
    /// - 跨公司 → `Ok(None)`
    /// - DB 错误 → `Err(Repo)`
    pub async fn get_stage(
        &self,
        company_id: Uuid,
        stage_id: Uuid,
    ) -> PipelineServiceResult<Option<PipelineStageRow>> {
        // 不存在 → 直接 None
        let stage = match self.repo.get_stage(stage_id).await? {
            Some(s) => s,
            None => return Ok(None),
        };
        // 跨公司 → 视作不存在
        if self
            .stage_pipeline_or(company_id, stage.pipeline_id)
            .await
            .is_err()
        {
            return Ok(None);
        }
        Ok(Some(stage))
    }

    /// 内部辅助：通过 pipeline_id 拿 company_id（stage row 不存 company_id）。
    async fn stage_pipeline_or(
        &self,
        company_id: Uuid,
        pipeline_id: Uuid,
    ) -> PipelineServiceResult<Uuid> {
        let pipeline = self
            .repo
            .get(pipeline_id)
            .await?
            .ok_or_else(|| PipelineServiceError::NotFound(format!("pipeline {pipeline_id}")))?;
        if pipeline.company_id != company_id {
            return Err(PipelineServiceError::NotFound(format!(
                "pipeline {pipeline_id}"
            )));
        }
        Ok(pipeline_id)
    }

    /// 创建一个 stage。
    ///
    /// 业务校验：
    /// - pipeline 属于传入 company（跨公司 → NotFound）
    /// - key 非空（trim）
    /// - name 非空（trim）
    /// - kind ∈ {open, working, review, done, cancelled}（type system 保证）
    ///
    /// 副作用：依次调用每个 hook 的 `on_stage_created(pipeline_id, stage)`。
    pub async fn create_stage(
        &self,
        company_id: Uuid,
        pipeline_id: Uuid,
        input: &CreateStageMinimalInput,
    ) -> PipelineServiceResult<PipelineStageRow> {
        self.ensure_in_company(company_id, pipeline_id).await?;
        if input.key.trim().is_empty() {
            return Err(PipelineServiceError::InvalidInput(
                "stage key must not be empty".into(),
            ));
        }
        if input.name.trim().is_empty() {
            return Err(PipelineServiceError::InvalidInput(
                "stage name must not be empty".into(),
            ));
        }
        let kind_str = input.kind.as_str();
        let key_str: &str = &input.key;
        let name_str: &str = &input.name;
        let stage = self
            .repo
            .create_stage(
                pipeline_id,
                key_str,
                name_str,
                kind_str,
                input.position,
                &input.config,
            )
            .await?;
        for hook in &self.hooks {
            hook.on_stage_created(pipeline_id, &stage).await?;
        }
        Ok(stage)
    }

    /// 更新 stage（部分字段）。
    ///
    /// No-op 语义：所有 patch 字段都为 None → 返回 existing，不写库、不触发 hook。
    ///
    /// `key` 不可变（与上游 paperclip 一致）。
    ///
    /// 副作用：依次调用每个 hook 的 `on_stage_updated`。
    pub async fn update_stage(
        &self,
        company_id: Uuid,
        stage_id: Uuid,
        patch: &UpdateStagePatch,
    ) -> PipelineServiceResult<PipelineStageRow> {
        // 通过 stage 取 pipeline → company 校验
        let stage = self
            .repo
            .get_stage(stage_id)
            .await?
            .ok_or_else(|| PipelineServiceError::NotFound(format!("stage {stage_id}")))?;
        self.stage_pipeline_or(company_id, stage.pipeline_id).await?;

        let is_empty = patch.name.is_none()
            && patch.kind.is_none()
            && patch.position.is_none()
            && patch.config.is_none();
        if is_empty {
            return Ok(stage);
        }

        let kind_ref = patch.kind.map(StageKind::as_str);
        let config_ref = patch.config.as_ref();
        let updated = self
            .repo
            .update_stage(
                stage_id,
                patch.name.as_deref(),
                kind_ref,
                patch.position,
                config_ref,
            )
            .await?
            .ok_or_else(|| PipelineServiceError::NotFound(format!("stage {stage_id}")))?;
        for hook in &self.hooks {
            hook.on_stage_updated(&updated).await?;
        }
        Ok(updated)
    }

    /// 硬删除 stage。
    ///
    /// 返回值：
    /// - `Ok(true)` 删除成功
    /// - `Ok(false)` stage 不存在或跨公司
    /// - `Err(Repo)` DB 错误
    ///
    /// 副作用：依次调用每个 hook 的 `on_stage_deleted(stage_id, pipeline_id)`。
    pub async fn delete_stage(
        &self,
        company_id: Uuid,
        stage_id: Uuid,
    ) -> PipelineServiceResult<bool> {
        let stage = match self.repo.get_stage(stage_id).await? {
            Some(s) => s,
            None => return Ok(false),
        };
        // 跨公司 → 视作不存在
        if self
            .stage_pipeline_or(company_id, stage.pipeline_id)
            .await
            .is_err()
        {
            return Ok(false);
        }
        let deleted = self.repo.delete_stage(stage_id).await?;
        if deleted {
            for hook in &self.hooks {
                hook.on_stage_deleted(stage_id, stage.pipeline_id).await?;
            }
        }
        Ok(deleted)
    }

    // =========================================================================
    // Pipeline transition 子资源（R603 v3）
    // =========================================================================

    /// 列出某 pipeline 的全部 transition（按 created_at ASC）。
    ///
    /// 业务校验：pipeline 属于传入 company（跨公司 → NotFound）。
    pub async fn list_transitions(
        &self,
        company_id: Uuid,
        pipeline_id: Uuid,
    ) -> PipelineServiceResult<Vec<PipelineTransitionRow>> {
        self.ensure_in_company(company_id, pipeline_id).await?;
        Ok(self.repo.list_transitions(pipeline_id).await?)
    }

    /// 创建一个 transition。
    ///
    /// 业务校验：
    /// - pipeline 属于传入 company（跨公司 → NotFound）
    /// - `from_stage_id` 属于该 pipeline
    /// - `to_stage_id` 属于该 pipeline
    /// - `from_stage_id != to_stage_id`（自环无意义）
    /// - `label` 可选；若提供则 trim 非空（否则视为 None）
    ///
    /// 副作用：依次调用每个 hook 的 `on_transition_created`。
    pub async fn create_transition(
        &self,
        company_id: Uuid,
        pipeline_id: Uuid,
        input: &CreateTransitionInput,
    ) -> PipelineServiceResult<PipelineTransitionRow> {
        self.ensure_in_company(company_id, pipeline_id).await?;

        if input.from_stage_id == input.to_stage_id {
            return Err(PipelineServiceError::InvalidInput(
                "from_stage_id and to_stage_id must differ".into(),
            ));
        }

        // 校验 from / to stage 都属于该 pipeline
        let from_stage = self
            .repo
            .get_stage(input.from_stage_id)
            .await?
            .ok_or_else(|| {
                PipelineServiceError::NotFound(format!("stage {}", input.from_stage_id))
            })?;
        if from_stage.pipeline_id != pipeline_id {
            return Err(PipelineServiceError::InvalidInput(format!(
                "from_stage_id {} does not belong to pipeline {pipeline_id}",
                input.from_stage_id
            )));
        }
        let to_stage = self
            .repo
            .get_stage(input.to_stage_id)
            .await?
            .ok_or_else(|| {
                PipelineServiceError::NotFound(format!("stage {}", input.to_stage_id))
            })?;
        if to_stage.pipeline_id != pipeline_id {
            return Err(PipelineServiceError::InvalidInput(format!(
                "to_stage_id {} does not belong to pipeline {pipeline_id}",
                input.to_stage_id
            )));
        }

        // label trim 校验
        let label_ref: Option<&str> = match input.label.as_deref() {
            Some(s) if !s.trim().is_empty() => Some(s),
            _ => None,
        };

        let transition = self
            .repo
            .create_transition(pipeline_id, input.from_stage_id, input.to_stage_id, label_ref)
            .await?;
        for hook in &self.hooks {
            hook.on_transition_created(&transition).await?;
        }
        Ok(transition)
    }

    /// 硬删除 transition。
    ///
    /// 返回值：
    /// - `Ok(true)` 删除成功
    /// - `Ok(false)` transition 不存在或跨公司
    /// - `Err(Repo)` DB 错误
    ///
    /// 副作用：依次调用每个 hook 的 `on_transition_deleted(transition_id, pipeline_id)`。
    pub async fn delete_transition(
        &self,
        company_id: Uuid,
        transition_id: Uuid,
    ) -> PipelineServiceResult<bool> {
        // 通过 transition 取 pipeline_id → company 校验
        // repo 没有 get_transition，用直接 SQL 查。
        let pipeline_id_opt: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT pipeline_id FROM pipeline_transitions WHERE id = $1",
        )
        .bind(transition_id)
        .fetch_optional(self.repo.db.pool())
        .await?;
        let pipeline_id = match pipeline_id_opt {
            Some(p) => p,
            None => return Ok(false),
        };
        // 跨公司 → 视作不存在
        if self
            .stage_pipeline_or(company_id, pipeline_id)
            .await
            .is_err()
        {
            return Ok(false);
        }
        let deleted = self.repo.delete_transition(transition_id).await?;
        if deleted {
            for hook in &self.hooks {
                hook.on_transition_deleted(transition_id, pipeline_id).await?;
            }
        }
        Ok(deleted)
    }

    /// 校验 from→to 是否合法（在 transitions 表中存在）。
    ///
    /// 业务校验：pipeline 属于传入 company（跨公司 → NotFound）。
    pub async fn is_valid_transition(
        &self,
        company_id: Uuid,
        pipeline_id: Uuid,
        from_stage_id: Uuid,
        to_stage_id: Uuid,
    ) -> PipelineServiceResult<bool> {
        self.ensure_in_company(company_id, pipeline_id).await?;
        Ok(self
            .repo
            .is_valid_transition(pipeline_id, from_stage_id, to_stage_id)
            .await?)
    }

    // =========================================================================
    // Pipeline case 子资源（R603 v4）
    // =========================================================================

    /// 列出某 pipeline 的全部 case（可按 stage 过滤）。
    ///
    /// 业务校验：pipeline 属于传入 company（跨公司 → NotFound）。
    pub async fn list_cases(
        &self,
        company_id: Uuid,
        pipeline_id: Uuid,
        stage_id: Option<Uuid>,
    ) -> PipelineServiceResult<Vec<PipelineCaseRow>> {
        self.ensure_in_company(company_id, pipeline_id).await?;
        Ok(self.repo.list_cases(pipeline_id, stage_id).await?)
    }

    /// 按 id 获取单个 case。
    ///
    /// 语义与 `get` pipeline 一致：
    /// - case 不存在 → `Ok(None)`
    /// - 跨公司 → `Ok(None)`
    /// - DB 错误 → `Err(Repo)`
    pub async fn get_case(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> PipelineServiceResult<Option<PipelineCaseRow>> {
        let case = match self.repo.get_case(case_id).await? {
            Some(c) => c,
            None => return Ok(None),
        };
        if case.company_id != company_id {
            return Ok(None);
        }
        Ok(Some(case))
    }

    /// 内部辅助：通过 case 取 company_id（部分操作只需 case_row）。
    async fn case_company_or(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> PipelineServiceResult<PipelineCaseRow> {
        let case = self
            .repo
            .get_case(case_id)
            .await?
            .ok_or_else(|| PipelineServiceError::NotFound(format!("case {case_id}")))?;
        if case.company_id != company_id {
            return Err(PipelineServiceError::NotFound(format!("case {case_id}")));
        }
        Ok(case)
    }

    /// 创建一个 case。
    ///
    /// 业务校验：
    /// - pipeline 属于传入 company（跨公司 → NotFound）
    /// - `stage_id` 必须属于该 pipeline
    /// - `case_key` 非空（trim）
    /// - `title` 非空（trim）
    /// - 若 `parent_case_id` 提供，必须属于该 company 且属于该 pipeline
    ///
    /// 副作用：依次调用每个 hook 的 `on_case_created`。
    pub async fn create_case(
        &self,
        company_id: Uuid,
        pipeline_id: Uuid,
        input: &CreateCaseMinimalInput,
    ) -> PipelineServiceResult<PipelineCaseRow> {
        self.ensure_in_company(company_id, pipeline_id).await?;

        if input.case_key.trim().is_empty() {
            return Err(PipelineServiceError::InvalidInput(
                "case_key must not be empty".into(),
            ));
        }
        if input.title.trim().is_empty() {
            return Err(PipelineServiceError::InvalidInput(
                "title must not be empty".into(),
            ));
        }

        // 校验 stage 属于 pipeline
        let stage = self
            .repo
            .get_stage(input.stage_id)
            .await?
            .ok_or_else(|| {
                PipelineServiceError::NotFound(format!("stage {}", input.stage_id))
            })?;
        if stage.pipeline_id != pipeline_id {
            return Err(PipelineServiceError::InvalidInput(format!(
                "stage_id {} does not belong to pipeline {pipeline_id}",
                input.stage_id
            )));
        }

        // 校验 parent_case（可选）
        if let Some(parent_id) = input.parent_case_id {
            let parent = self
                .repo
                .get_case(parent_id)
                .await?
                .ok_or_else(|| {
                    PipelineServiceError::NotFound(format!("case {parent_id}"))
                })?;
            if parent.company_id != company_id || parent.pipeline_id != pipeline_id {
                return Err(PipelineServiceError::InvalidInput(format!(
                    "parent_case_id {parent_id} is not in this company/pipeline"
                )));
            }
        }

        let case = self
            .repo
            .create_case(
                company_id,
                pipeline_id,
                input.stage_id,
                &input.case_key,
                &input.title,
                input.summary.as_deref(),
                &input.fields,
                input.parent_case_id,
                input.created_by_user_id.as_deref(),
                input.created_by_agent_id,
                input.origin_run_id,
            )
            .await?;

        for hook in &self.hooks {
            hook.on_case_created(&case).await?;
        }
        Ok(case)
    }

    /// 转换 case 的 stage（乐观锁：必须当前在 `from_stage_id`）。
    ///
    /// 副作用：依次调用每个 hook 的 `on_case_stage_transitioned`。
    ///
    /// 注意：repo 还会写 `terminal_kind` / `terminal_at`（若 to_stage 是 done/cancelled），
    /// hook 拿到的 case row 已是最新版本。
    pub async fn update_case_stage(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        input: &UpdateCaseStageInput,
    ) -> PipelineServiceResult<PipelineCaseRow> {
        let existing = self.case_company_or(company_id, case_id).await?;
        if input.from_stage_id == input.to_stage_id {
            return Err(PipelineServiceError::InvalidInput(
                "from_stage_id and to_stage_id must differ".into(),
            ));
        }

        let updated = self
            .repo
            .update_case_stage(case_id, input.to_stage_id, input.from_stage_id)
            .await?
            .ok_or_else(|| {
                PipelineServiceError::InvalidInput(format!(
                    "case {case_id} is no longer in stage {} (optimistic lock failed)",
                    input.from_stage_id
                ))
            })?;

        for hook in &self.hooks {
            hook.on_case_stage_transitioned(&updated, existing.stage_id, updated.stage_id)
                .await?;
        }
        Ok(updated)
    }

    /// 硬删除 case。
    ///
    /// 返回值：
    /// - `Ok(true)` 删除成功
    /// - `Ok(false)` case 不存在或跨公司
    /// - `Err(Repo)` DB 错误
    ///
    /// 副作用：依次调用每个 hook 的 `on_case_deleted(case_id, company_id)`。
    pub async fn delete_case(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> PipelineServiceResult<bool> {
        let case = match self.repo.get_case(case_id).await? {
            Some(c) => c,
            None => return Ok(false),
        };
        if case.company_id != company_id {
            return Ok(false);
        }
        let deleted = self.repo.delete_case(case_id).await?;
        if deleted {
            for hook in &self.hooks {
                hook.on_case_deleted(case_id, company_id).await?;
            }
        }
        Ok(deleted)
    }

    /// 申请 case lease（持有 + lease_token）。
    ///
    /// 副作用：service 层不直接发 hook（lease 不是 lifecycle 事件）；
    /// 上游 paperclip 通过 `lease_acquired` event 记录到 case_events，
    /// 此处通过 `create_case_event` 显式写入。
    pub async fn claim_case(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        input: &ClaimCaseInput,
    ) -> PipelineServiceResult<PipelineCaseRow> {
        self.case_company_or(company_id, case_id).await?;

        let (owner_type, owner_agent_id, owner_user_id): (&str, Option<Uuid>, Option<&str>) =
            match &input.owner {
                CaseOwner::Agent(id) => ("agent", Some(*id), None),
                CaseOwner::User(name) => ("user", None, Some(name)),
            };

        let claimed = self
            .repo
            .claim_case(case_id, owner_type, owner_agent_id, owner_user_id, input.lease_token)
            .await?
            .ok_or_else(|| PipelineServiceError::NotFound(format!("case {case_id}")))?;
        Ok(claimed)
    }

    /// 释放 case lease。
    pub async fn release_case(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> PipelineServiceResult<PipelineCaseRow> {
        self.case_company_or(company_id, case_id).await?;
        let released = self
            .repo
            .release_case(case_id)
            .await?
            .ok_or_else(|| PipelineServiceError::NotFound(format!("case {case_id}")))?;
        Ok(released)
    }

    /// 列出某 case 的全部 event（按 created_at ASC）。
    pub async fn list_case_events(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> PipelineServiceResult<Vec<PipelineCaseEventRow>> {
        // 跨公司 case 不应可见
        self.case_company_or(company_id, case_id).await?;
        Ok(self.repo.list_case_events(case_id).await?)
    }

    /// 写入一条 case event。
    ///
    /// 副作用：依次调用每个 hook 的 `on_case_event_recorded(case, event)`。
    pub async fn create_case_event(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        input: &CreateCaseEventInput,
    ) -> PipelineServiceResult<PipelineCaseEventRow> {
        let case = self.case_company_or(company_id, case_id).await?;

        // repo 参数顺序：company_id, case_id, event_type, from_stage_id, to_stage_id,
        // payload, actor_type, actor_agent_id, actor_user_id, run_id
        // 空 payload 视为 None；非空（含 Null JSON value）传 Some
        let payload_ref = if input.payload.is_null() {
            None
        } else {
            Some(&input.payload)
        };
        let event = self
            .repo
            .create_case_event(
                company_id,
                case_id,
                input.kind.as_str(),
                input.from_stage_id,
                input.to_stage_id,
                payload_ref,
                input.actor.as_str(),
                input.actor_agent_id,
                input.actor_user_id.as_deref(),
                input.run_id,
            )
            .await?;

        for hook in &self.hooks {
            hook.on_case_event_recorded(&case, &event).await?;
        }
        Ok(event)
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn r603_create_input_serializes_camel() {
        let input = CreatePipelineInput {
            key: "p1".into(),
            name: "Pipeline 1".into(),
            description: Some("d".into()),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["key"], "p1");
        assert_eq!(json["description"], "d");
    }

    #[test]
    fn r603_recording_hook_starts_empty() {
        let hook = RecordingPipelineHook::default();
        assert_eq!(hook.created.lock().unwrap().len(), 0);
        assert_eq!(hook.updated.lock().unwrap().len(), 0);
        assert_eq!(hook.archived.lock().unwrap().len(), 0);
        assert_eq!(hook.deleted.lock().unwrap().len(), 0);
        assert_eq!(hook.stage_created.lock().unwrap().len(), 0);
        assert_eq!(hook.stage_updated.lock().unwrap().len(), 0);
        assert_eq!(hook.stage_deleted.lock().unwrap().len(), 0);
    }

    #[test]
    fn r603v2_stage_kind_serializes_snake_case() {
        let kind = StageKind::Review;
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json, "review");
        assert_eq!(kind.as_str(), "review");
    }

    #[test]
    fn r603v2_stage_kind_from_db_str_round_trip() {
        for kind in [
            StageKind::Working,
            StageKind::Review,
            StageKind::Done,
            StageKind::Cancelled,
        ] {
            let s = kind.as_str();
            assert_eq!(StageKind::from_db_str(s), Some(kind));
        }
        assert_eq!(StageKind::from_db_str("nope"), None);
        assert_eq!(StageKind::from_db_str("open"), None, "open is not a valid DB kind");
    }

    #[test]
    fn r603v2_create_stage_input_serializes_camel() {
        let input = CreateStageMinimalInput {
            key: "s1".into(),
            name: "Stage 1".into(),
            kind: StageKind::Working,
            position: 1,
            config: serde_json::json!({"k":"v"}),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["key"], "s1");
        assert_eq!(json["kind"], "working");
        assert_eq!(json["position"], 1);
        assert_eq!(json["config"]["k"], "v");
    }

    #[test]
    fn r603v2_update_stage_patch_default_is_empty() {
        let patch = UpdateStagePatch::default();
        let json = serde_json::to_value(&patch).unwrap();
        assert_eq!(json, serde_json::json!({}));
    }

    #[test]
    fn r603v3_create_transition_input_serializes_camel() {
        let id1 = Uuid::nil();
        let id2 = Uuid::nil();
        let input = CreateTransitionInput {
            from_stage_id: id1,
            to_stage_id: id2,
            label: Some("go".into()),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["fromStageId"], id1.to_string());
        assert_eq!(json["toStageId"], id2.to_string());
        assert_eq!(json["label"], "go");
    }

    #[test]
    fn r603v3_recording_hook_starts_empty() {
        let hook = RecordingPipelineHook::default();
        assert_eq!(hook.transition_created.lock().unwrap().len(), 0);
        assert_eq!(hook.transition_deleted.lock().unwrap().len(), 0);
    }
}
