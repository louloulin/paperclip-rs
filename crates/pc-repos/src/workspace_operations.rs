//! `workspace_operations` 域（Round 263）。
//!
//! 与原 `paperclip/server/src/services/workspace-operations.ts` 1:1 对齐：
//! - 持久化一次执行（shell/command）的生命周期（running → succeeded/failed）
//! - 维护 stdout/stderr 摘录（最多 4096 字节）、log 引用（log_store + log_ref + bytes/sha256）
//! - 支持 execution_workspace_id 后绑定
//! - listForRun / listForExecutionWorkspace / readLog 等查询接口
//!
//! 设计目标：高内聚低耦合。
//! - 输入：phase / command / cwd / metadata / run() 函数
//! - 输出：完成后的 WorkspaceOperationRow
//! - 不引入真实执行；只负责持久化与状态机推进

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

/// `recordOperation` 输入（与 Node 版对齐）。
#[derive(Debug, Clone)]
pub struct RecordOperationInput {
    pub phase: WorkspaceOperationPhase,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// `recordOperation.run()` 返回（与 Node 版对齐）。
#[derive(Debug, Clone, Default)]
pub struct RunOutput {
    pub status: Option<WorkspaceOperationStatus>,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub system: Option<String>,
    pub metadata: Option<serde_json::Value>,
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
}
