//! R631 集成测试：file_resource 模块回归保护。

use pc_repos::file_resource::{
    DbLike, DefaultWorkspaceFileResourceService, FileContentResponse, FileListQuery,
    FileListResponse, FileResolveQuery, FileResourceError, FileResourceLimiter,
    FileResourceLimiterConfig, ReleaseGuard, ResolvedWorkspaceResource,
    WorkspaceFileResourceService,
};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[test]
fn limiter_allows_within_budget() {
    let l = FileResourceLimiter::new(FileResourceLimiterConfig {
        max_concurrent: 3,
        max_requests: 10,
        window_ms: 60_000,
        request_limit_message: "rl".into(),
        concurrency_limit_message: "cl".into(),
    });
    for i in 0..3 {
        let g: ReleaseGuard = l.acquire("k").unwrap_or_else(|_| panic!("acquire #{}", i));
        drop(g);
    }
}

#[test]
fn limiter_rejects_over_concurrency() {
    let l = FileResourceLimiter::new(FileResourceLimiterConfig {
        max_concurrent: 2,
        max_requests: 100,
        window_ms: 60_000,
        request_limit_message: "rl".into(),
        concurrency_limit_message: "cl".into(),
    });
    let _a = l.acquire("k").unwrap();
    let _b = l.acquire("k").unwrap();
    let err = l.acquire("k").unwrap_err();
    assert!(matches!(err, FileResourceError::ConcurrencyLimited(_)));
}

#[test]
fn limiter_rejects_over_request_rate() {
    let l = FileResourceLimiter::new(FileResourceLimiterConfig {
        max_concurrent: 100,
        max_requests: 3,
        window_ms: 60_000,
        request_limit_message: "rl".into(),
        concurrency_limit_message: "cl".into(),
    });
    let _a = l.acquire("k").unwrap();
    let _b = l.acquire("k").unwrap();
    let _c = l.acquire("k").unwrap();
    let err = l.acquire("k").unwrap_err();
    assert!(matches!(err, FileResourceError::RateLimited(_)));
}

#[test]
fn release_drop_decrements_active() {
    let l = FileResourceLimiter::new(FileResourceLimiterConfig {
        max_concurrent: 2,
        max_requests: 100,
        window_ms: 60_000,
        request_limit_message: "rl".into(),
        concurrency_limit_message: "cl".into(),
    });
    let g = l.acquire("k").unwrap();
    drop(g);
    let _g2 = l.acquire("k").unwrap();
}

#[test]
fn separate_keys_are_isolated() {
    let l = FileResourceLimiter::new(FileResourceLimiterConfig {
        max_concurrent: 1,
        max_requests: 100,
        window_ms: 60_000,
        request_limit_message: "rl".into(),
        concurrency_limit_message: "cl".into(),
    });
    let _a = l.acquire("k1").unwrap();
    let _b = l.acquire("k2").unwrap();
    let _c = l.acquire("k3").unwrap();
}

struct FakeDb {
    company_id: Uuid,
    files: Vec<(String, Option<String>, Option<i64>)>,
    content: HashMap<String, (String, Option<String>, Option<i64>)>,
}

#[async_trait::async_trait]
impl DbLike for FakeDb {
    async fn get_issue_company_id(&self, _: Uuid) -> Result<Option<Uuid>, FileResourceError> {
        Ok(Some(self.company_id))
    }
    async fn list_project_files(
        &self,
        _: Uuid,
    ) -> Result<Vec<(String, Option<String>, Option<i64>)>, FileResourceError> {
        Ok(self.files.clone())
    }
    async fn get_project_file_content(
        &self,
        _: Uuid,
        path: &str,
    ) -> Result<Option<(String, Option<String>, Option<i64>)>, FileResourceError> {
        Ok(self.content.get(path).cloned())
    }
}

#[tokio::test]
async fn service_list_returns_files() {
    let fake = Arc::new(FakeDb {
        company_id: Uuid::nil(),
        files: vec![
            ("src/main.rs".into(), Some("text/rust".into()), Some(123)),
            ("README.md".into(), Some("text/markdown".into()), Some(456)),
        ],
        content: HashMap::new(),
    });
    let svc = DefaultWorkspaceFileResourceService::new(fake);
    let q = FileListQuery::default();
    let resp: FileListResponse = svc.list(Uuid::new_v4(), &q).await.unwrap();
    assert_eq!(resp.files.len(), 2);
    assert_eq!(resp.total, 2);
}

#[tokio::test]
async fn service_list_filters_by_path_prefix() {
    let fake = Arc::new(FakeDb {
        company_id: Uuid::nil(),
        files: vec![
            ("src/main.rs".into(), None, Some(1)),
            ("docs/readme.md".into(), None, Some(2)),
            ("tests/x.rs".into(), None, Some(3)),
        ],
        content: HashMap::new(),
    });
    let svc = DefaultWorkspaceFileResourceService::new(fake);
    let q = FileListQuery {
        path: Some("src/".into()),
        ..Default::default()
    };
    let resp = svc.list(Uuid::new_v4(), &q).await.unwrap();
    assert_eq!(resp.files.len(), 1);
    assert_eq!(resp.files[0].path, "src/main.rs");
}

#[tokio::test]
async fn service_list_filters_by_q() {
    let fake = Arc::new(FakeDb {
        company_id: Uuid::nil(),
        files: vec![
            ("src/main.rs".into(), None, Some(1)),
            ("README.md".into(), None, Some(2)),
        ],
        content: HashMap::new(),
    });
    let svc = DefaultWorkspaceFileResourceService::new(fake);
    let q = FileListQuery {
        q: Some("README".into()),
        ..Default::default()
    };
    let resp = svc.list(Uuid::new_v4(), &q).await.unwrap();
    assert_eq!(resp.files.len(), 1);
    assert_eq!(resp.files[0].path, "README.md");
}

#[tokio::test]
async fn service_resolve_returns_metadata() {
    let fake = Arc::new(FakeDb {
        company_id: Uuid::nil(),
        files: vec![("src/main.rs".into(), Some("text/rust".into()), Some(123))],
        content: HashMap::new(),
    });
    let svc = DefaultWorkspaceFileResourceService::new(fake);
    let q = FileResolveQuery {
        path: Some("src/main.rs".into()),
        workspace: Some("execution".into()),
        project_id: None,
        workspace_id: None,
    };
    let resolved: ResolvedWorkspaceResource = svc.resolve(Uuid::new_v4(), &q).await.unwrap();
    assert_eq!(resolved.path, "src/main.rs");
    assert_eq!(resolved.workspace, "execution");
    assert_eq!(resolved.size_bytes, 123);
}

#[tokio::test]
async fn service_resolve_404_for_missing() {
    let fake = Arc::new(FakeDb {
        company_id: Uuid::nil(),
        files: vec![],
        content: HashMap::new(),
    });
    let svc = DefaultWorkspaceFileResourceService::new(fake);
    let q = FileResolveQuery {
        path: Some("nope.txt".into()),
        workspace: None,
        project_id: None,
        workspace_id: None,
    };
    let err = svc.resolve(Uuid::new_v4(), &q).await.unwrap_err();
    assert!(matches!(err, FileResourceError::NotFound(_)));
}

#[tokio::test]
async fn service_resolve_rejects_empty_path() {
    let fake = Arc::new(FakeDb {
        company_id: Uuid::nil(),
        files: vec![],
        content: HashMap::new(),
    });
    let svc = DefaultWorkspaceFileResourceService::new(fake);
    let q = FileResolveQuery {
        path: Some("   ".into()),
        workspace: None,
        project_id: None,
        workspace_id: None,
    };
    let err = svc.resolve(Uuid::new_v4(), &q).await.unwrap_err();
    assert!(matches!(err, FileResourceError::Invalid(_)));
}

#[tokio::test]
async fn service_read_content_returns_text() {
    let mut content = HashMap::new();
    content.insert(
        "hello.txt".into(),
        ("hello world".into(), Some("text/plain".into()), Some(11)),
    );
    let fake = Arc::new(FakeDb {
        company_id: Uuid::nil(),
        files: vec![("hello.txt".into(), Some("text/plain".into()), Some(11))],
        content,
    });
    let svc = DefaultWorkspaceFileResourceService::new(fake);
    let q = FileResolveQuery {
        path: Some("hello.txt".into()),
        workspace: None,
        project_id: None,
        workspace_id: None,
    };
    let resp: FileContentResponse = svc.read_content(Uuid::new_v4(), &q, 1024).await.unwrap();
    assert_eq!(resp.content, "hello world");
    assert_eq!(resp.encoding, "utf-8");
    assert!(!resp.truncated);
}

#[tokio::test]
async fn service_read_content_truncates_at_max_bytes() {
    let mut content = HashMap::new();
    content.insert("big.txt".into(), ("x".repeat(100), None, Some(100)));
    let fake = Arc::new(FakeDb {
        company_id: Uuid::nil(),
        files: vec![("big.txt".into(), None, Some(100))],
        content,
    });
    let svc = DefaultWorkspaceFileResourceService::new(fake);
    let q = FileResolveQuery {
        path: Some("big.txt".into()),
        workspace: None,
        project_id: None,
        workspace_id: None,
    };
    let resp = svc.read_content(Uuid::new_v4(), &q, 10).await.unwrap();
    assert!(resp.truncated);
    assert_eq!(resp.content.len(), 10);
}

#[tokio::test]
async fn service_prepare_download_returns_real_path() {
    let fake = Arc::new(FakeDb {
        company_id: Uuid::nil(),
        files: vec![("src/main.rs".into(), Some("text/rust".into()), Some(123))],
        content: HashMap::new(),
    });
    let svc = DefaultWorkspaceFileResourceService::new(fake);
    let q = FileResolveQuery {
        path: Some("src/main.rs".into()),
        workspace: None,
        project_id: None,
        workspace_id: None,
    };
    let (resolved, real_path) = svc.prepare_download(Uuid::new_v4(), &q).await.unwrap();
    assert_eq!(resolved.path, "src/main.rs");
    assert_eq!(real_path, "src/main.rs");
}

#[tokio::test]
async fn service_get_issue_company_id_via_fake() {
    let fake = Arc::new(FakeDb {
        company_id: Uuid::nil(),
        files: vec![],
        content: HashMap::new(),
    });
    let svc = DefaultWorkspaceFileResourceService::new(fake);
    let cid = svc.get_issue_company_id(Uuid::new_v4()).await.unwrap();
    assert_eq!(cid, Uuid::nil());
}
