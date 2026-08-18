#![forbid(unsafe_code)]
//! File resource service traits (pure abstraction, no IO).
//!
//! R792B split: pc-repos::file_resource::traits contains:
//! - WorkspaceFileResourceService -- service trait
//! - DbLike -- abstract DB type trait (default impl for crate::Db)


use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

use super::pure::{FileContentResponse, FileEntry, FileListQuery, FileListResponse, FileResolveQuery, FileResourceError, ResolvedWorkspaceResource};


#[async_trait::async_trait]
pub trait WorkspaceFileResourceService: Send + Sync {
    async fn get_issue_company_id(&self, issue_id: Uuid) -> Result<Uuid, FileResourceError>;

    async fn list(
        &self,
        issue_id: Uuid,
        query: &FileListQuery,
    ) -> Result<FileListResponse, FileResourceError>;

    async fn resolve(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
    ) -> Result<ResolvedWorkspaceResource, FileResourceError>;

    async fn read_content(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
        max_bytes: usize,
    ) -> Result<FileContentResponse, FileResourceError>;

    async fn prepare_download(
        &self,
        issue_id: Uuid,
        query: &FileResolveQuery,
    ) -> Result<(ResolvedWorkspaceResource, String), FileResourceError>;
}

// ============================================================================
// DbLike trait — 抽象 pc-repos::Db 操作，让 service 可 mockable
// ============================================================================

#[async_trait::async_trait]
pub trait DbLike: Send + Sync {
    /// 返回 issue 的 company_id；不存在 → None
    async fn get_issue_company_id(&self, issue_id: Uuid)
        -> Result<Option<Uuid>, FileResourceError>;

    async fn list_project_files(
        &self,
        issue_id: Uuid,
    ) -> Result<Vec<(String, Option<String>, Option<i64>)>, FileResourceError>;

    async fn get_project_file_content(
        &self,
        issue_id: Uuid,
        path: &str,
    ) -> Result<Option<(String, Option<String>, Option<i64>)>, FileResourceError>;
}

#[async_trait::async_trait]
impl<T> DbLike for Arc<T>
where
    T: DbLike + ?Sized,
{
    async fn get_issue_company_id(
        &self,
        issue_id: Uuid,
    ) -> Result<Option<Uuid>, FileResourceError> {
        (**self).get_issue_company_id(issue_id).await
    }
    async fn list_project_files(
        &self,
        issue_id: Uuid,
    ) -> Result<Vec<(String, Option<String>, Option<i64>)>, FileResourceError> {
        (**self).list_project_files(issue_id).await
    }
    async fn get_project_file_content(
        &self,
        issue_id: Uuid,
        path: &str,
    ) -> Result<Option<(String, Option<String>, Option<i64>)>, FileResourceError> {
        (**self).get_project_file_content(issue_id, path).await
    }
}

#[async_trait::async_trait]
impl DbLike for crate::Db {
    async fn get_issue_company_id(
        &self,
        issue_id: Uuid,
    ) -> Result<Option<Uuid>, FileResourceError> {
        let row = crate::issue::IssueRepo::new(self)
            .get(issue_id)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))?;
        Ok(row.map(|r| r.company_id))
    }

    async fn list_project_files(
        &self,
        issue_id: Uuid,
    ) -> Result<Vec<(String, Option<String>, Option<i64>)>, FileResourceError> {
        crate::issue::IssueRepo::new(self)
            .list_project_files(issue_id)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|(path, mime, size)| (path, Some(mime), size))
                    .collect()
            })
            .map_err(|e| FileResourceError::Io(e.to_string()))
    }

    async fn get_project_file_content(
        &self,
        issue_id: Uuid,
        path: &str,
    ) -> Result<Option<(String, Option<String>, Option<i64>)>, FileResourceError> {
        crate::issue::IssueRepo::new(self)
            .get_project_file_content(issue_id, path)
            .await
            .map_err(|e| FileResourceError::Io(e.to_string()))
    }
}

