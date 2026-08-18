#![forbid(unsafe_code)]
//! Default DB-backed implementation of WorkspaceFileResourceService.
//!
//! R792B split: pc-repos::file_resource::db contains DefaultWorkspaceFileResourceService<DB>.


use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

use super::traits::{DbLike, WorkspaceFileResourceService};

use super::pure::{FileContentResponse, FileEntry, FileListQuery, FileListResponse, FileResolveQuery, FileResourceError, FileResourceLimiter, FileResourceLimiterConfig, ResolvedWorkspaceResource};


// ============================================================================
// Default impl — DB-backed listing + filesystem content reads
// ============================================================================

/// Default service backed by `IssueRepo::list_project_files` + filesystem reads.
pub struct DefaultWorkspaceFileResourceService<DB> {
    db: DB,
}

impl<DB: Clone> DefaultWorkspaceFileResourceService<DB> {
    pub fn new(db: DB) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl<DB> WorkspaceFileResourceService for DefaultWorkspaceFileResourceService<DB>
where
    DB: DbLike + Send + Sync,
{
    async fn get_issue_company_id(&self, issue_id: Uuid) -> Result<Uuid, FileResourceError> {
        self.db
            .get_issue_company_id(issue_id)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))?
            .ok_or_else(|| FileResourceError::NotFound(format!("issue {issue_id} not found")))
    }

    async fn list(
        &self,
        issue_id: Uuid,
        query: &FileListQuery,
    ) -> Result<FileListResponse, FileResourceError> {
        let workspace = query.workspace.clone().unwrap_or_else(|| "auto".into());
        let limit = query.limit.unwrap_or(100).min(1000);
        let offset = query.offset.unwrap_or(0);

        let raw = self
            .db
            .list_project_files(issue_id)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))?;

        let mut filtered: Vec<FileEntry> = raw
            .into_iter()
            .filter_map(|(path, mime, size)| {
                // path filter
                if let Some(p) = &query.path {
                    if !path.starts_with(p.as_str()) {
                        return None;
                    }
                }
                // q text filter
                if let Some(q) = &query.q {
                    if q.to_lowercase().chars().all(|c| c.is_whitespace()) {
                        // empty/whitespace-only q → ignore
                    } else if !path.to_lowercase().contains(&q.to_lowercase()) {
                        return None;
                    }
                }
                Some(FileEntry {
                    path,
                    mime_type: mime,
                    size_bytes: size,
                    workspace: workspace.clone(),
                    project_id: query.project_id,
                })
            })
            .collect();

        let total = filtered.len();
        // pagination
        filtered = filtered.into_iter().skip(offset).take(limit).collect();

        Ok(FileListResponse {
            files: filtered,
            issue_id,
            total,
            limit,
            offset,
        })
    }

    async fn resolve(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
    ) -> Result<ResolvedWorkspaceResource, FileResourceError> {
        if query.path.trim().is_empty() {
            return Err(FileResourceError::Invalid("path is required".into()));
        }
        let workspace = query.workspace.clone().unwrap_or_else(|| "auto".into());
        // Look up real file metadata
        let files = self
            .db
            .list_project_files(issue_id)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))?;
        let match_ = files
            .into_iter()
            .find(|(p, _, _)| p == &query.path)
            .ok_or_else(|| FileResourceError::NotFound(format!("{} not found", query.path)))?;
        let (path, mime, size) = match_;
        Ok(ResolvedWorkspaceResource {
            path: path.clone(),
            workspace,
            project_id: query.project_id,
            workspace_id: query.workspace_id,
            real_path: path,
            mime_type: mime,
            size_bytes: size.unwrap_or(0),
        })
    }

    async fn read_content(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
        max_bytes: usize,
    ) -> Result<FileContentResponse, FileResourceError> {
        let resolved = self.resolve(issue_id, query).await?;
        let row = self
            .db
            .get_project_file_content(issue_id, &resolved.path)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))?;
        let (raw, _mime, size) = row.unwrap_or_default();
        let truncated = raw.len() > max_bytes;
        let content = if truncated {
            // safe truncate at char boundary
            let mut end = max_bytes;
            while end > 0 && !raw.is_char_boundary(end) {
                end -= 1;
            }
            raw[..end].to_string()
        } else {
            raw
        };
        Ok(FileContentResponse {
            path: resolved.path,
            content,
            encoding: "utf-8".into(),
            mime_type: resolved.mime_type,
            size_bytes: size.unwrap_or(0),
            truncated,
        })
    }

    async fn prepare_download(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
    ) -> Result<(ResolvedWorkspaceResource, String), FileResourceError> {
        let resolved = self.resolve(issue_id, query).await?;
        // real_path used for streaming; identical for DB-backed case
        Ok((resolved.clone(), resolved.real_path.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn new_limiter(max_concurrent: usize, max_requests: usize) -> FileResourceLimiter {
        FileResourceLimiter::new(FileResourceLimiterConfig {
            max_concurrent,
            max_requests,
            window_ms: 60_000,
            request_limit_message: "rl".into(),
            concurrency_limit_message: "cl".into(),
        })
    }

    #[test]
    fn limiter_allows_within_budget() {
        let l = new_limiter(3, 10);
        for i in 0..3 {
            let g = l.acquire("k").unwrap_or_else(|_| panic!("acquire #{i}"));
            drop(g);
        }
    }

    #[test]
    fn limiter_rejects_over_concurrent() {
        let l = new_limiter(2, 100);
        let _a = l.acquire("k").unwrap();
        let _b = l.acquire("k").unwrap();
        let err = l.acquire("k").unwrap_err();
        assert_eq!(err, FileResourceError::ConcurrencyLimited("cl".into()));
    }

    #[test]
    fn limiter_rejects_over_request_rate() {
        let l = new_limiter(100, 3);
        let _a = l.acquire("k").unwrap();
        let _b = l.acquire("k").unwrap();
        let _c = l.acquire("k").unwrap();
        let err = l.acquire("k").unwrap_err();
        assert_eq!(err, FileResourceError::RateLimited("rl".into()));
    }

    #[test]
    fn release_guard_decrements_active() {
        let l = new_limiter(2, 100);
        let key = "k1";
        let g1 = l.acquire(key).unwrap();
        assert_eq!(*l.active_by_key.lock().unwrap().get(key).unwrap(), 1);
        drop(g1);
        assert!(l.active_by_key.lock().unwrap().get(key).is_none());
    }

    #[test]
    fn separate_keys_isolated() {
        let l = new_limiter(1, 100);
        let _a = l.acquire("k1").unwrap();
        let _b = l.acquire("k2").unwrap();
        // both should succeed since they have separate keys
        let _c = l.acquire("k3").unwrap();
    }

    #[tokio::test]
    async fn fake_service_returns_configured_files() {
        struct FakeDb;
        #[async_trait::async_trait]
        impl DbLike for FakeDb {
            async fn get_issue_company_id(
                &self,
                _: Uuid,
            ) -> Result<Option<Uuid>, FileResourceError> {
                Ok(Some(Uuid::nil()))
            }
            async fn list_project_files(
                &self,
                _: Uuid,
            ) -> Result<Vec<(String, Option<String>, Option<i64>)>, FileResourceError> {
                Ok(vec![
                    ("src/main.rs".into(), Some("text/rust".into()), Some(123)),
                    ("README.md".into(), Some("text/markdown".into()), Some(456)),
                ])
            }
            async fn get_project_file_content(
                &self,
                _: Uuid,
                _: &str,
            ) -> Result<Option<(String, Option<String>, Option<i64>)>, FileResourceError>
            {
                Ok(Some((
                    "hello world".into(),
                    Some("text/plain".into()),
                    Some(11),
                )))
            }
        }

        let svc = DefaultWorkspaceFileResourceService::new(Arc::new(FakeDb));
        let issue_id = Uuid::new_v4();
        let q = FileListQuery::default();
        let resp = svc.list(issue_id, &q).await.unwrap();
        assert_eq!(resp.files.len(), 2);
        assert_eq!(resp.total, 2);

        let rq = FileResolveQuery {
            path: "src/main.rs".into(),
            workspace: Some("execution".into()),
            project_id: None,
            workspace_id: None,
        };
        let resolved = svc.resolve(issue_id, &rq).await.unwrap();
        assert_eq!(resolved.path, "src/main.rs");
        assert_eq!(resolved.workspace, "execution");

        let content = svc.read_content(issue_id, &rq, 1024).await.unwrap();
        assert_eq!(content.content, "hello world");
        assert!(!content.truncated);
    }

    #[tokio::test]
    async fn read_content_truncates_at_max_bytes() {
        struct FakeDbLong;
        #[async_trait::async_trait]
        impl DbLike for FakeDbLong {
            async fn get_issue_company_id(
                &self,
                _: Uuid,
            ) -> Result<Option<Uuid>, FileResourceError> {
                Ok(Some(Uuid::nil()))
            }
            async fn list_project_files(
                &self,
                _: Uuid,
            ) -> Result<Vec<(String, Option<String>, Option<i64>)>, FileResourceError> {
                Ok(vec![("big.txt".into(), None, Some(100))])
            }
            async fn get_project_file_content(
                &self,
                _: Uuid,
                _: &str,
            ) -> Result<Option<(String, Option<String>, Option<i64>)>, FileResourceError>
            {
                Ok(Some(("x".repeat(100), None, Some(100))))
            }
        }
        let svc = DefaultWorkspaceFileResourceService::new(Arc::new(FakeDbLong));
        let q = FileResolveQuery {
            path: "big.txt".into(),
            workspace: None,
            project_id: None,
            workspace_id: None,
        };
        let c = svc.read_content(Uuid::new_v4(), &q, 10).await.unwrap();
        assert!(c.truncated);
        assert_eq!(c.content.len(), 10);
    }
}
