//! Issue 文件资源 (list / resolve / readContent / prepareDownload).
//!
//! Replica of paperclip Node server/src/routes/file-resources.ts (722 LOC):
//! - FileResourceLimiter -- rate + concurrency limiter
//! - WorkspaceFileResourceService -- abstract service trait
//! - DefaultWorkspaceFileResourceService -- default DB + local fs impl
//! - FileResourceError -- unified error model
//!
//! R792B split:
//! - pure -- pure data + limiter (no IO)
//! - traits -- abstract traits
//! - db -- DB-backed default impl

pub mod pure;
pub mod traits;
pub mod db;

// Re-exports to keep external pc_repos::file_resource::* API unchanged
pub use db::DefaultWorkspaceFileResourceService;
pub use pure::{
    FileContentResponse, FileEntry, FileListQuery, FileListResponse, FileResolveQuery,
    FileResourceError, FileResourceLimiter, FileResourceLimiterConfig, ReleaseGuard,
    ResolvedWorkspaceResource,
};
pub use traits::{DbLike, WorkspaceFileResourceService};

