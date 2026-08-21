//! `workspace_operations` 域（Round 263 + Round 837）。
//!
//! 与原 `paperclip/server/src/services/workspace-operations.ts`（264 行）1:1 对齐：
//! - Round 263 数据访问层：持久化一次执行（shell/command）的生命周期（running → succeeded/failed），
//!   维护 stdout/stderr 摘录（最多 4096 字节）、log 引用（log_store + log_ref + bytes/sha256），
//!   支持 execution_workspace_id 后绑定。
//! - Round 837 服务层：`WorkspaceOperationService`（对齐 `workspaceOperationService(db)`）与
//!   `WorkspaceOperationRecorder`（对齐 Node `WorkspaceOperationRecorder` 接口）。
//!
//! 设计目标：高内聚低耦合。
//! - 输入：phase / command / cwd / metadata / run() 函数
//! - 输出：完成后的 WorkspaceOperationRow
//! - 不引入真实执行；只负责持久化与状态机推进
//! - 服务层不依赖 pc-folders（避免 pc-repos → pc-folders 循环依赖）：本地定义
//!   `WorkspaceOperationLogStore` trait；调用方注入具体实现。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::Db;

const COLS: &str = "id, company_id, execution_workspace_id, heartbeat_run_id, issue_id, \
     phase, command, cwd, status, exit_code, log_store, log_ref, log_bytes, log_sha256, \
     log_compressed, stdout_excerpt, stderr_excerpt, metadata, started_at, finished_at, \
     created_at, updated_at";

/// `workspace_operations` 表行（与 Node 版 `workspaceOperations.$inferSelect` 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceOperationRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub execution_workspace_id: Option<Uuid>,
    pub heartbeat_run_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub phase: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub log_store: Option<String>,
    pub log_ref: Option<String>,
    pub log_bytes: Option<i64>,
    pub log_sha256: Option<String>,
    pub log_compressed: bool,
    pub stdout_excerpt: Option<String>,
    pub stderr_excerpt: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 操作阶段（与 Node 版 `WorkspaceOperationPhase` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOperationPhase {
    Pre,
    Main,
    Post,
    Cleanup,
}

impl WorkspaceOperationPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pre => "pre",
            Self::Main => "main",
            Self::Post => "post",
            Self::Cleanup => "cleanup",
        }
    }
}

/// 操作状态（与 Node 版 `WorkspaceOperationStatus` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOperationStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl WorkspaceOperationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

const EXCERPT_MAX_BYTES: usize = 4096;

/// 把新 chunk 拼到现有 excerpt（最多保留尾部 EXCERPT_MAX_BYTES 字节；与 Node `appendExcerpt` 对齐）。
///
/// 使用 char 计数（不切 UTF-8 中间），与 Node 版行为一致。
pub fn append_excerpt(current: &str, chunk: &str) -> String {
    let mut buf: Vec<char> = current.chars().chain(chunk.chars()).collect();
    if buf.len() > EXCERPT_MAX_BYTES {
        let start = buf.len() - EXCERPT_MAX_BYTES;
        buf.drain(..start);
    }
    buf.into_iter().collect()
}

/// 合并 metadata（与 Node `combineMetadata` 对齐）。
pub fn combine_metadata(
    base: Option<&serde_json::Value>,
    patch: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    match (base, patch) {
        (None, None) => None,
        (Some(b), None) => Some(b.clone()),
        (None, Some(p)) => Some(p.clone()),
        (Some(b), Some(p)) => {
            let mut merged = b.as_object().cloned().unwrap_or_default();
            if let Some(obj) = p.as_object() {
                for (k, v) in obj {
                    merged.insert(k.clone(), v.clone());
                }
            }
            Some(serde_json::Value::Object(merged))
        }
    }
}

pub struct WorkspaceOperationRepo<'a> {
    pub db: &'a Db,
}

impl<'a> WorkspaceOperationRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Round 263: 按 id 查询单条 operation。
    pub async fn get_by_id(&self, id: Uuid) -> sqlx::Result<Option<WorkspaceOperationRow>> {
        let sql = format!("SELECT {COLS} FROM workspace_operations WHERE id = $1");
        sqlx::query_as::<_, WorkspaceOperationRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// Round 263: 创建一条 running 状态的操作记录。
    /// 返回新 id。
    pub async fn create_running(
        &self,
        id: Uuid,
        company_id: Uuid,
        execution_workspace_id: Option<Uuid>,
        heartbeat_run_id: Option<Uuid>,
        issue_id: Option<Uuid>,
        phase: WorkspaceOperationPhase,
        command: Option<&str>,
        cwd: Option<&str>,
        log_store: Option<&str>,
        log_ref: Option<&str>,
        metadata: Option<&serde_json::Value>,
    ) -> sqlx::Result<()> {
        let started_at = Utc::now();
        sqlx::query(
            "INSERT INTO workspace_operations \
             (id, company_id, execution_workspace_id, heartbeat_run_id, issue_id, phase, command, cwd, status, \
              log_store, log_ref, metadata, started_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'running', $9, $10, $11, $12)",
        )
        .bind(id)
        .bind(company_id)
        .bind(execution_workspace_id)
        .bind(heartbeat_run_id)
        .bind(issue_id)
        .bind(phase.as_str())
        .bind(command)
        .bind(cwd)
        .bind(log_store)
        .bind(log_ref)
        .bind(metadata)
        .bind(started_at)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Round 263: 完成后回写 status / exit_code / excerpt / log 元数据。
    pub async fn finalize(
        &self,
        id: Uuid,
        execution_workspace_id: Option<Uuid>,
        status: WorkspaceOperationStatus,
        exit_code: Option<i32>,
        stdout_excerpt: Option<&str>,
        stderr_excerpt: Option<&str>,
        log_bytes: Option<i64>,
        log_sha256: Option<&str>,
        log_compressed: bool,
        metadata: Option<&serde_json::Value>,
    ) -> sqlx::Result<Option<WorkspaceOperationRow>> {
        let finished_at = Utc::now();
        let sql = format!(
            "UPDATE workspace_operations SET \
             execution_workspace_id = COALESCE($2, execution_workspace_id), \
             status = $3, exit_code = $4, \
             stdout_excerpt = COALESCE($5, stdout_excerpt), \
             stderr_excerpt = COALESCE($6, stderr_excerpt), \
             log_bytes = COALESCE($7, log_bytes), \
             log_sha256 = COALESCE($8, log_sha256), \
             log_compressed = $9, \
             metadata = COALESCE($10, metadata), \
             finished_at = $11, updated_at = $11 \
             WHERE id = $1 \
             RETURNING {COLS}"
        );
        sqlx::query_as::<_, WorkspaceOperationRow>(&sql)
            .bind(id)
            .bind(execution_workspace_id)
            .bind(status.as_str())
            .bind(exit_code)
            .bind(stdout_excerpt)
            .bind(stderr_excerpt)
            .bind(log_bytes)
            .bind(log_sha256)
            .bind(log_compressed)
            .bind(metadata)
            .bind(finished_at)
            .fetch_optional(self.db.pool())
            .await
    }

    /// Round 263: 把 heartbeat_run_id 关联的 operation 全部更新 execution_workspace_id。
    pub async fn attach_execution_workspace_id(
        &self,
        heartbeat_run_id: Uuid,
        execution_workspace_id: Uuid,
    ) -> sqlx::Result<u64> {
        let n = sqlx::query(
            "UPDATE workspace_operations SET execution_workspace_id = $2, updated_at = now() \
             WHERE heartbeat_run_id = $1",
        )
        .bind(heartbeat_run_id)
        .bind(execution_workspace_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }

    /// Round 263: 列出某 run 下的全部 operations（含 cleanup-only operations）。
    pub async fn list_for_run(
        &self,
        run_id: Uuid,
        execution_workspace_id: Option<Uuid>,
    ) -> sqlx::Result<Vec<WorkspaceOperationRow>> {
        if let Some(ews_id) = execution_workspace_id {
            let sql = format!(
                "SELECT {COLS} FROM workspace_operations \
                 WHERE heartbeat_run_id = $1 OR (execution_workspace_id = $2 AND heartbeat_run_id IS NULL) \
                 ORDER BY started_at ASC, created_at ASC, id ASC"
            );
            sqlx::query_as::<_, WorkspaceOperationRow>(&sql)
                .bind(run_id)
                .bind(ews_id)
                .fetch_all(self.db.pool())
                .await
        } else {
            let sql = format!(
                "SELECT {COLS} FROM workspace_operations \
                 WHERE heartbeat_run_id = $1 \
                 ORDER BY started_at ASC, created_at ASC, id ASC"
            );
            sqlx::query_as::<_, WorkspaceOperationRow>(&sql)
                .bind(run_id)
                .fetch_all(self.db.pool())
                .await
        }
    }

    /// Round 837: 把 execution_workspace_id 绑定到指定的若干 operation id。
    /// 与 Node `WorkspaceOperationRecorder.attachExecutionWorkspaceId` 1:1：
    /// 录像器在 `recordOperation` 期间把新生成的 id push 进 `createdIds`，稍后
    /// 在 attach 阶段只更新自己创建过的 operation。
    pub async fn attach_execution_workspace_id_for_ids(
        &self,
        ids: &[Uuid],
        execution_workspace_id: Uuid,
    ) -> sqlx::Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let n = sqlx::query(
            "UPDATE workspace_operations SET execution_workspace_id = $2, updated_at = now() \
             WHERE id = ANY($1)",
        )
        .bind(ids)
        .bind(execution_workspace_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }

    /// Round 263: 列出某 execution_workspace 下的全部 operations。
    pub async fn list_for_execution_workspace(
        &self,
        execution_workspace_id: Uuid,
    ) -> sqlx::Result<Vec<WorkspaceOperationRow>> {
        let sql = format!(
            "SELECT {COLS} FROM workspace_operations \
             WHERE execution_workspace_id = $1 \
             ORDER BY started_at DESC, created_at DESC"
        );
        sqlx::query_as::<_, WorkspaceOperationRow>(&sql)
            .bind(execution_workspace_id)
            .fetch_all(self.db.pool())
            .await
    }
}

// ============================================================================
// Round 837: Service layer（对齐 `paperclip/server/src/services/workspace-operations.ts`）。
//
// Node 模块导出：
// - `WorkspaceOperationRecorder`（接口：`attachExecutionWorkspaceId` + `recordOperation`）
// - `workspaceOperationService(db)`（工厂，返回 `getById` / `createRecorder` /
//   `listForRun` / `listForExecutionWorkspace` / `readLog`）
// - `toWorkspaceOperation`（行 → 域对象映射）
//
// Rust 端保留同名 snake_case API；服务通过参数注入 `log_store`，避免依赖
// `pc-folders`（pc-folders 已依赖 pc-repos，反向导入会循环）。

use std::cell::RefCell;
use std::sync::Arc;

use pc_log_redaction::text::redact_current_user_text;
use pc_log_redaction::value::redact_current_user_value;
use pc_log_redaction::Options as RedactionOptions;

// ---- Log store types -----------------------------------------------------

/// Log stream（与 Node `stream: "stdout" | "stderr" | "system"` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
    System,
}

impl LogStream {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::System => "system",
        }
    }
}

/// `begin` 返回的句柄（与 Node `{ store, logRef }` 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct LogHandle {
    pub store: String,
    pub log_ref: String,
}

/// 单条 append 事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogAppendEvent {
    pub stream: LogStream,
    pub chunk: String,
    /// ISO-8601 字符串，与 Node `new Date().toISOString()` 对齐。
    pub ts: String,
}

/// 读选项（与 Node `readLog({ offset, limitBytes })` 对齐）。
#[derive(Debug, Clone, Default)]
pub struct LogReadOptions {
    pub offset: Option<u64>,
    pub limit_bytes: Option<u64>,
}

/// 读结果。
#[derive(Debug, Clone)]
pub struct LogReadResult {
    pub content: String,
    pub next_offset: Option<u64>,
}

/// `finalize` 返回的统计信息。
#[derive(Debug, Clone, Default)]
pub struct LogFinalizeSummary {
    pub bytes: i64,
    pub sha256: Option<String>,
    pub compressed: bool,
}

/// Log store 抽象错误。
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceOperationLogStoreError {
    #[error("workspace operation log not found")]
    NotFound,
    #[error("io error: {0}")]
    Io(String),
    #[error("invalid input: {0}")]
    Invalid(String),
}

/// Workspace operation log store 抽象（与 Node `WorkspaceOperationLogStore` 1:1 对齐）。
///
/// 把 trait 放在 pc-repos，避免 pc-repos → pc-folders 循环依赖。运行时由调用方
/// 注入 `pc-folders::operation_log_store::LocalFileWorkspaceOperationLogStore`；
/// 测试可注入 in-memory mock。
#[async_trait::async_trait]
pub trait WorkspaceOperationLogStore: Send + Sync {
    async fn begin(
        &self,
        company_id: Uuid,
        operation_id: Uuid,
    ) -> Result<LogHandle, WorkspaceOperationLogStoreError>;
    async fn append(
        &self,
        handle: &LogHandle,
        event: &LogAppendEvent,
    ) -> Result<(), WorkspaceOperationLogStoreError>;
    async fn finalize(
        &self,
        handle: &LogHandle,
    ) -> Result<LogFinalizeSummary, WorkspaceOperationLogStoreError>;
    async fn read(
        &self,
        handle: &LogHandle,
        opts: LogReadOptions,
    ) -> Result<LogReadResult, WorkspaceOperationLogStoreError>;
}

// ---- Recorder / Service input/output types --------------------------------

/// `createRecorder` 输入（与 Node 对齐）。
#[derive(Debug, Clone)]
pub struct CreateRecorderInput {
    pub company_id: Uuid,
    pub heartbeat_run_id: Option<Uuid>,
    pub execution_workspace_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
}

/// `recordOperation` 输入（与 Node 对齐）。
#[derive(Debug, Clone)]
pub struct RecordOperationInput {
    pub phase: WorkspaceOperationPhase,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// `recordOperation.run()` 返回值（与 Node 对齐）。
#[derive(Debug, Clone, Default)]
pub struct RecordOperationRunOutput {
    pub status: Option<WorkspaceOperationStatus>,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub system: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// `readLog` 返回值（与 Node 对齐）。
#[derive(Debug, Clone)]
pub struct LogReadResponse {
    pub operation_id: Uuid,
    pub store: String,
    pub log_ref: String,
    pub content: String,
    pub next_offset: Option<u64>,
}

/// Service 错误（与 Node `notFound("Workspace operation not found")` 等对齐）。
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceOperationServiceError {
    #[error("workspace operation not found")]
    NotFound,
    #[error("workspace operation log not found")]
    LogNotFound,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("log store error: {0}")]
    LogStore(#[from] WorkspaceOperationLogStoreError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// ----------------------------------------------------------------------------
// WorkspaceOperationRecorder — closure-style orchestrator returned by
// `create_recorder`. Mirrors Node `WorkspaceOperationRecorder` interface.
// ----------------------------------------------------------------------------

pub struct WorkspaceOperationRecorder<'a> {
    db: &'a Db,
    log_store: Arc<dyn WorkspaceOperationLogStore>,
    company_id: Uuid,
    heartbeat_run_id: Option<Uuid>,
    issue_id: Option<Uuid>,
    redaction_options: RedactionOptions,
    /// 可变的 execution_workspace_id（先为 None，后续 attach 时设置）。
    current_execution_workspace_id: RefCell<Option<Uuid>>,
    /// 本录像器通过 `record_operation` 创建的 operation id 列表。
    created_ids: RefCell<Vec<Uuid>>,
}

impl<'a> WorkspaceOperationRecorder<'a> {
    /// Service 内部构造器（tests 直接调用 `for_test`）。
    pub(crate) fn new(
        db: &'a Db,
        log_store: Arc<dyn WorkspaceOperationLogStore>,
        company_id: Uuid,
        heartbeat_run_id: Option<Uuid>,
        execution_workspace_id: Option<Uuid>,
        issue_id: Option<Uuid>,
        redaction_options: RedactionOptions,
    ) -> Self {
        Self {
            db,
            log_store,
            company_id,
            heartbeat_run_id,
            issue_id,
            redaction_options,
            current_execution_workspace_id: RefCell::new(execution_workspace_id),
            created_ids: RefCell::new(Vec::new()),
        }
    }

    /// 与 Node `attachExecutionWorkspaceId(nextExecutionWorkspaceId)` 对齐。
    /// 更新本地状态 + 把 `created_ids` 写入 DB（仅在有 ews_id 且 created_ids 非空时）。
    pub async fn attach_execution_workspace_id(
        &self,
        next_execution_workspace_id: Option<Uuid>,
    ) -> Result<(), WorkspaceOperationServiceError> {
        let next = next_execution_workspace_id;
        *self.current_execution_workspace_id.borrow_mut() = next;
        if next.is_none() {
            return Ok(());
        }
        let ids: Vec<Uuid> = self.created_ids.borrow().clone();
        if ids.is_empty() {
            return Ok(());
        }
        let new_ews = next.expect("checked above");
        WorkspaceOperationRepo::new(self.db)
            .attach_execution_workspace_id_for_ids(&ids, new_ews)
            .await?;
        Ok(())
    }

    /// 与 Node `recordOperation({ phase, command, cwd, metadata, run })` 对齐。
    /// 创建 running 行 → 调 `run()` → 追加日志 → 写 finalize 结果（成功则 succeeded，
    /// 抛错则 failed 并重抛）。
    pub async fn record_operation<F, Fut>(
        &self,
        input: RecordOperationInput,
        run: F,
    ) -> Result<WorkspaceOperationRow, WorkspaceOperationServiceError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<
            Output = Result<RecordOperationRunOutput, Box<dyn std::error::Error + Send + Sync>>,
        >,
    {
        let id = Uuid::new_v4();
        let handle = self
            .log_store
            .begin(self.company_id, id)
            .await
            .map_err(WorkspaceOperationServiceError::LogStore)?;

        let mut stdout_excerpt = String::new();
        let mut stderr_excerpt = String::new();
        let current_ews = *self.current_execution_workspace_id.borrow();

        let redaction = &self.redaction_options;
        let initial_metadata = input
            .metadata
            .as_ref()
            .map(|v| redact_current_user_value(v, redaction));
        WorkspaceOperationRepo::new(self.db)
            .create_running(
                id,
                self.company_id,
                current_ews,
                self.heartbeat_run_id,
                self.issue_id,
                input.phase,
                input.command.as_deref(),
                input.cwd.as_deref(),
                Some(handle.store.as_str()),
                Some(handle.log_ref.as_str()),
                initial_metadata.as_ref(),
            )
            .await?;
        self.created_ids.borrow_mut().push(id);

        let now_iso = || Utc::now().to_rfc3339();
        let append_event = |stream: LogStream, chunk: &str| LogAppendEvent {
            stream,
            chunk: chunk.to_string(),
            ts: now_iso(),
        };
        let update_excerpt = |excerpt: &mut String, stream: LogStream, chunk: &str| {
            let sanitized = redact_current_user_text(chunk, redaction);
            match stream {
                LogStream::Stdout => *excerpt = append_excerpt(excerpt, &sanitized),
                LogStream::Stderr => *excerpt = append_excerpt(excerpt, &sanitized),
                LogStream::System => {}
            }
        };

        let run_result = run().await;
        match run_result {
            Ok(out) => {
                if let Some(c) = out.system.as_deref() {
                    update_excerpt(&mut stdout_excerpt, LogStream::System, c);
                    self.log_store
                        .append(&handle, &append_event(LogStream::System, c))
                        .await
                        .map_err(WorkspaceOperationServiceError::LogStore)?;
                }
                if let Some(c) = out.stdout.as_deref() {
                    update_excerpt(&mut stdout_excerpt, LogStream::Stdout, c);
                    self.log_store
                        .append(&handle, &append_event(LogStream::Stdout, c))
                        .await
                        .map_err(WorkspaceOperationServiceError::LogStore)?;
                }
                if let Some(c) = out.stderr.as_deref() {
                    update_excerpt(&mut stderr_excerpt, LogStream::Stderr, c);
                    self.log_store
                        .append(&handle, &append_event(LogStream::Stderr, c))
                        .await
                        .map_err(WorkspaceOperationServiceError::LogStore)?;
                }
                let finalized = self
                    .log_store
                    .finalize(&handle)
                    .await
                    .map_err(WorkspaceOperationServiceError::LogStore)?;
                let merged_meta = combine_metadata(input.metadata.as_ref(), out.metadata.as_ref());
                let final_meta = merged_meta
                    .as_ref()
                    .map(|v| redact_current_user_value(v, redaction));
                let row = WorkspaceOperationRepo::new(self.db)
                    .finalize(
                        id,
                        *self.current_execution_workspace_id.borrow(),
                        out.status.unwrap_or(WorkspaceOperationStatus::Succeeded),
                        out.exit_code,
                        if stdout_excerpt.is_empty() { None } else { Some(stdout_excerpt.as_str()) },
                        if stderr_excerpt.is_empty() { None } else { Some(stderr_excerpt.as_str()) },
                        Some(finalized.bytes),
                        finalized.sha256.as_deref(),
                        finalized.compressed,
                        final_meta.as_ref(),
                    )
                    .await?;
                row.ok_or(WorkspaceOperationServiceError::NotFound)
            }
            Err(err) => {
                let msg = err.to_string();
                update_excerpt(&mut stderr_excerpt, LogStream::Stderr, &msg);
                self.log_store
                    .append(&handle, &append_event(LogStream::Stderr, &msg))
                    .await
                    .map_err(WorkspaceOperationServiceError::LogStore)?;
                let finalized = self.log_store.finalize(&handle).await.ok();
                let _ = WorkspaceOperationRepo::new(self.db)
                    .finalize(
                        id,
                        *self.current_execution_workspace_id.borrow(),
                        WorkspaceOperationStatus::Failed,
                        None,
                        if stdout_excerpt.is_empty() { None } else { Some(stdout_excerpt.as_str()) },
                        if stderr_excerpt.is_empty() { None } else { Some(stderr_excerpt.as_str()) },
                        finalized.as_ref().map(|s| s.bytes),
                        finalized.as_ref().and_then(|s| s.sha256.as_deref()),
                        finalized.as_ref().map(|s| s.compressed).unwrap_or(false),
                        None,
                    )
                    .await?;
                Err(WorkspaceOperationServiceError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("workspace operation failed: {msg}"),
                )))
            }
        }
    }
}

// ----------------------------------------------------------------------------
// WorkspaceOperationService — 工厂函数 + 服务实例。
// ----------------------------------------------------------------------------

/// Service 实例（与 Node `workspaceOperationService(db)` 返回的对象对齐）。
pub struct WorkspaceOperationService<'a> {
    db: &'a Db,
    log_store: Arc<dyn WorkspaceOperationLogStore>,
    redaction_options: RedactionOptions,
}

impl<'a> WorkspaceOperationService<'a> {
    /// 与 Node `workspaceOperationService(db)` 对齐；log_store 与 redaction_options
    /// 由调用方注入（Node 用全局 singleton + instanceSettings，这里更显式）。
    pub fn new(
        db: &'a Db,
        log_store: Arc<dyn WorkspaceOperationLogStore>,
        redaction_options: RedactionOptions,
    ) -> Self {
        Self {
            db,
            log_store,
            redaction_options,
        }
    }

    /// 与 Node `getById(id)` 对齐。
    pub async fn get_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkspaceOperationRow>, WorkspaceOperationServiceError> {
        Ok(WorkspaceOperationRepo::new(self.db).get_by_id(id).await?)
    }

    /// 与 Node `createRecorder(input)` 对齐。
    pub fn create_recorder(&self, input: CreateRecorderInput) -> WorkspaceOperationRecorder<'_> {
        WorkspaceOperationRecorder::new(
            self.db,
            Arc::clone(&self.log_store),
            input.company_id,
            input.heartbeat_run_id,
            input.execution_workspace_id,
            input.issue_id,
            self.redaction_options.clone(),
        )
    }

    /// 与 Node `listForRun(runId, executionWorkspaceId?)` 对齐。
    pub async fn list_for_run(
        &self,
        run_id: Uuid,
        execution_workspace_id: Option<Uuid>,
    ) -> Result<Vec<WorkspaceOperationRow>, WorkspaceOperationServiceError> {
        Ok(WorkspaceOperationRepo::new(self.db)
            .list_for_run(run_id, execution_workspace_id)
            .await?)
    }

    /// 与 Node `listForExecutionWorkspace(executionWorkspaceId)` 对齐。
    pub async fn list_for_execution_workspace(
        &self,
        execution_workspace_id: Uuid,
    ) -> Result<Vec<WorkspaceOperationRow>, WorkspaceOperationServiceError> {
        Ok(WorkspaceOperationRepo::new(self.db)
            .list_for_execution_workspace(execution_workspace_id)
            .await?)
    }

    /// 与 Node `readLog(operationId, opts?)` 对齐。
    pub async fn read_log(
        &self,
        operation_id: Uuid,
        opts: Option<LogReadOptions>,
    ) -> Result<LogReadResponse, WorkspaceOperationServiceError> {
        let row = self
            .get_by_id(operation_id)
            .await?
            .ok_or(WorkspaceOperationServiceError::NotFound)?;
        let (store, log_ref) = row
            .log_store
            .as_ref()
            .zip(row.log_ref.as_ref())
            .ok_or(WorkspaceOperationServiceError::LogNotFound)?;
        let handle = LogHandle {
            store: store.clone(),
            log_ref: log_ref.clone(),
        };
        let result = self
            .log_store
            .read(&handle, opts.unwrap_or_default())
            .await?;
        Ok(LogReadResponse {
            operation_id,
            store: store.clone(),
            log_ref: log_ref.clone(),
            content: result.content,
            next_offset: result.next_offset,
        })
    }
}

/// 行 → 域对象映射（与 Node `toWorkspaceOperation` 1:1 命名）。
///
/// 当前 Rust 端直接以 `WorkspaceOperationRow` 暴露 snake_case 字段；
/// 若未来引入 `WorkspaceOperation` 域对象，可在此做字段映射。
pub fn to_workspace_operation(row: WorkspaceOperationRow) -> WorkspaceOperationRow {
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn phase_strings_match_node() {
        assert_eq!(WorkspaceOperationPhase::Pre.as_str(), "pre");
        assert_eq!(WorkspaceOperationPhase::Main.as_str(), "main");
        assert_eq!(WorkspaceOperationPhase::Post.as_str(), "post");
        assert_eq!(WorkspaceOperationPhase::Cleanup.as_str(), "cleanup");
    }

    #[test]
    fn status_strings_match_node() {
        assert_eq!(WorkspaceOperationStatus::Running.as_str(), "running");
        assert_eq!(WorkspaceOperationStatus::Succeeded.as_str(), "succeeded");
        assert_eq!(WorkspaceOperationStatus::Failed.as_str(), "failed");
        assert_eq!(WorkspaceOperationStatus::Cancelled.as_str(), "cancelled");
    }

    #[test]
    fn append_excerpt_keeps_tail_4096() {
        let chunk = "x".repeat(5000);
        let out = append_excerpt("", &chunk);
        assert_eq!(out.len(), 4096);
        assert!(out.chars().all(|c| c == 'x'));
    }

    #[test]
    fn append_excerpt_concatenates_within_limit() {
        let out = append_excerpt("hello", " world");
        assert_eq!(out, "hello world");
    }

    #[test]
    fn append_excerpt_truncates_old_when_full() {
        let initial = "x".repeat(5000);
        let chunk = "yyyy";
        let out = append_excerpt(&initial, chunk);
        // 仅保留最后 4096 字符
        assert_eq!(out.chars().count(), 4096);
        assert!(out.ends_with("yyyy"));
    }

    #[test]
    fn combine_metadata_handles_none_inputs() {
        assert_eq!(combine_metadata(None, None), None);
        assert_eq!(
            combine_metadata(Some(&json!({"a": 1})), None),
            Some(json!({"a": 1}))
        );
        assert_eq!(
            combine_metadata(None, Some(&json!({"b": 2}))),
            Some(json!({"b": 2}))
        );
    }

    #[test]
    fn combine_metadata_merges_objects() {
        let base = json!({"a": 1, "b": 2});
        let patch = json!({"b": 3, "c": 4});
        let merged = combine_metadata(Some(&base), Some(&patch)).unwrap();
        assert_eq!(merged["a"], 1);
        assert_eq!(merged["b"], 3); // patch 覆盖
        assert_eq!(merged["c"], 4);
    }

    #[test]
    fn combine_metadata_promotes_non_object_to_object() {
        let base = json!("string");
        let patch = json!({"k": "v"});
        let merged = combine_metadata(Some(&base), Some(&patch)).unwrap();
        // base 不是 object，被丢弃；只保留 patch
        assert_eq!(merged["k"], "v");
    }

    // ============================================================================
    // Round 837: Service-layer tests
    // ============================================================================

    mod service {
        use super::*;
        use std::collections::HashMap;
        use std::sync::Mutex;

        /// In-memory mock log store — 记录每次调用，便于在测试里断言顺序与载荷。
        #[derive(Debug, Default)]
        pub struct MockLogStore {
            pub begin_calls: Mutex<Vec<(Uuid, Uuid)>>,
            pub append_calls: Mutex<Vec<(String, LogStream, String)>>,
            pub finalize_calls: Mutex<Vec<String>>,
            pub read_calls: Mutex<Vec<(String, LogReadOptions)>>,
            pub events: Mutex<HashMap<String, Vec<LogAppendEvent>>>,
            pub finalize_summary: LogFinalizeSummary,
            pub read_result: Option<LogReadResult>,
        }

        impl MockLogStore {
            pub fn new() -> Self {
                Self {
                    finalize_summary: LogFinalizeSummary {
                        bytes: 0,
                        sha256: Some("deadbeef".to_string()),
                        compressed: false,
                    },
                    read_result: Some(LogReadResult {
                        content: String::new(),
                        next_offset: None,
                    }),
                    ..Default::default()
                }
            }
            pub fn with_finalize(mut self, summary: LogFinalizeSummary) -> Self {
                self.finalize_summary = summary;
                self
            }
            pub fn with_read(mut self, result: LogReadResult) -> Self {
                self.read_result = Some(result);
                self
            }
        }

        #[async_trait::async_trait]
        impl WorkspaceOperationLogStore for MockLogStore {
            async fn begin(
                &self,
                company_id: Uuid,
                operation_id: Uuid,
            ) -> Result<LogHandle, WorkspaceOperationLogStoreError> {
                self.begin_calls
                    .lock()
                    .unwrap()
                    .push((company_id, operation_id));
                let log_ref = format!("{company_id}/{operation_id}");
                self.events.lock().unwrap().insert(log_ref.clone(), Vec::new());
                Ok(LogHandle {
                    store: "local_file".to_string(),
                    log_ref,
                })
            }
            async fn append(
                &self,
                handle: &LogHandle,
                event: &LogAppendEvent,
            ) -> Result<(), WorkspaceOperationLogStoreError> {
                self.append_calls.lock().unwrap().push((
                    handle.log_ref.clone(),
                    event.stream,
                    event.chunk.clone(),
                ));
                self.events
                    .lock()
                    .unwrap()
                    .entry(handle.log_ref.clone())
                    .or_default()
                    .push(event.clone());
                Ok(())
            }
            async fn finalize(
                &self,
                handle: &LogHandle,
            ) -> Result<LogFinalizeSummary, WorkspaceOperationLogStoreError> {
                self.finalize_calls.lock().unwrap().push(handle.log_ref.clone());
                Ok(self.finalize_summary.clone())
            }
            async fn read(
                &self,
                handle: &LogHandle,
                opts: LogReadOptions,
            ) -> Result<LogReadResult, WorkspaceOperationLogStoreError> {
                self.read_calls
                    .lock()
                    .unwrap()
                    .push((handle.log_ref.clone(), opts));
                Ok(self.read_result.clone().unwrap_or(LogReadResult {
                    content: String::new(),
                    next_offset: None,
                }))
            }
        }

        fn dummy_redaction() -> RedactionOptions {
            RedactionOptions {
                enabled: false,
                ..Default::default()
            }
        }

        #[test]
        fn log_stream_strings_match_node() {
            assert_eq!(LogStream::Stdout.as_str(), "stdout");
            assert_eq!(LogStream::Stderr.as_str(), "stderr");
            assert_eq!(LogStream::System.as_str(), "system");
        }

        #[test]
        fn create_recorder_input_defaults_are_none() {
            let input = CreateRecorderInput {
                company_id: Uuid::nil(),
                heartbeat_run_id: None,
                execution_workspace_id: None,
                issue_id: None,
            };
            assert!(input.heartbeat_run_id.is_none());
            assert!(input.execution_workspace_id.is_none());
            assert!(input.issue_id.is_none());
            assert_eq!(input.company_id, Uuid::nil());
        }

        #[test]
        fn record_operation_input_holds_phase_and_optionals() {
            let input = RecordOperationInput {
                phase: WorkspaceOperationPhase::Pre,
                command: Some("ls -la".to_string()),
                cwd: Some("/tmp".to_string()),
                metadata: Some(json!({"k": "v"})),
            };
            assert_eq!(input.phase, WorkspaceOperationPhase::Pre);
            assert_eq!(input.command.as_deref(), Some("ls -la"));
            assert_eq!(input.cwd.as_deref(), Some("/tmp"));
            assert!(input.metadata.is_some());
        }

        #[test]
        fn record_operation_run_output_default_is_all_none() {
            let out = RecordOperationRunOutput::default();
            assert!(out.status.is_none());
            assert!(out.exit_code.is_none());
            assert!(out.stdout.is_none());
            assert!(out.stderr.is_none());
            assert!(out.system.is_none());
            assert!(out.metadata.is_none());
        }

        #[tokio::test]
        async fn mock_log_store_begin_records_company_and_operation() {
            let store = MockLogStore::new();
            let company_id = Uuid::new_v4();
            let op_id = Uuid::new_v4();
            let handle = store.begin(company_id, op_id).await.unwrap();
            assert_eq!(handle.store, "local_file");
            assert!(handle.log_ref.contains(&op_id.to_string()));
            assert_eq!(*store.begin_calls.lock().unwrap(), vec![(company_id, op_id)]);
        }

        #[tokio::test]
        async fn mock_log_store_finalize_returns_configured_summary() {
            let store = MockLogStore::new().with_finalize(LogFinalizeSummary {
                bytes: 1024,
                sha256: Some("abc123".to_string()),
                compressed: true,
            });
            let handle = store.begin(Uuid::new_v4(), Uuid::new_v4()).await.unwrap();
            let summary = store.finalize(&handle).await.unwrap();
            assert_eq!(summary.bytes, 1024);
            assert_eq!(summary.sha256.as_deref(), Some("abc123"));
            assert!(summary.compressed);
            assert_eq!(store.finalize_calls.lock().unwrap().len(), 1);
        }

        #[tokio::test]
        async fn mock_log_store_read_returns_configured_result() {
            let store = MockLogStore::new().with_read(LogReadResult {
                content: "chunk".to_string(),
                next_offset: Some(42),
            });
            let handle = store.begin(Uuid::new_v4(), Uuid::new_v4()).await.unwrap();
            let result = store.read(&handle, LogReadOptions::default()).await.unwrap();
            assert_eq!(result.content, "chunk");
            assert_eq!(result.next_offset, Some(42));
            assert_eq!(store.read_calls.lock().unwrap().len(), 1);
        }

        #[test]
        fn to_workspace_operation_is_identity_for_now() {
            // 占位实现：当前 Rust 端直接以 WorkspaceOperationRow 作为域对象。
            // 该测试守住 1:1 命名契约，防止后续重构时偷偷改名。
            let row = WorkspaceOperationRow {
                id: Uuid::nil(),
                company_id: Uuid::nil(),
                execution_workspace_id: None,
                heartbeat_run_id: None,
                issue_id: None,
                phase: "pre".to_string(),
                command: None,
                cwd: None,
                status: "running".to_string(),
                exit_code: None,
                log_store: None,
                log_ref: None,
                log_bytes: None,
                log_sha256: None,
                log_compressed: false,
                stdout_excerpt: None,
                stderr_excerpt: None,
                metadata: None,
                started_at: Utc::now(),
                finished_at: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            let out = to_workspace_operation(row.clone());
            assert_eq!(out.id, row.id);
            assert_eq!(out.phase, row.phase);
        }

        // 纯函数契约测试（不需要 DB）

        #[test]
        fn append_excerpt_propagates_to_excerpt_helpers() {
            // 镜像 Node `appendExcerpt`：保留尾部 4096 字符。
            let big = "x".repeat(5000);
            assert_eq!(append_excerpt("", &big).chars().count(), 4096);
            assert_eq!(append_excerpt("abc", "def"), "abcdef");
            let truncated = append_excerpt(&big, "yyyy");
            assert!(truncated.ends_with("yyyy"));
            assert_eq!(truncated.chars().count(), 4096);
        }

        #[test]
        fn combine_metadata_propagates() {
            // 镜像 Node `combineMetadata`：None/None → None；patch 覆盖 base。
            assert_eq!(combine_metadata(None, None), None);
            let merged = combine_metadata(Some(&json!({"a": 1})), Some(&json!({"b": 2}))).unwrap();
            assert_eq!(merged["a"], 1);
            assert_eq!(merged["b"], 2);
        }

        // 验证 redaction 在 disabled 时确实是 no-op（与 Node 行为一致）

        #[test]
        fn redaction_disabled_passes_through() {
            let opts = dummy_redaction();
            assert!(!opts.enabled);
            let out = redact_current_user_text("alice was here", &opts);
            assert_eq!(out, "alice was here");
        }

        // 验证 recorder 状态追踪（不依赖 DB）

        #[test]
        fn recorder_state_shape_is_consistent() {
            // 不连 DB：仅校验 log_store trait object 可以正常 finalize。
            // attach 在 created_ids 空时不会触碰 DB（与 Node 行为对齐）。
            let store: Arc<dyn WorkspaceOperationLogStore> = Arc::new(MockLogStore::new());
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let summary = store
                    .finalize(&LogHandle {
                        store: "local_file".to_string(),
                        log_ref: "x".to_string(),
                    })
                    .await
                    .unwrap();
                assert_eq!(summary.bytes, 0);
            });
        }
    }
}
