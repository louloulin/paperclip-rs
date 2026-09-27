//! M10-B2 用例的共用件：`AppState` 字面量 + multipart 装置 + 本地磁盘暂存目录。
//!
//! 拆出来是门 ⑩（单文件 800 行硬上限）的要求，先例 = `routes/attachments/tests/support.rs`
//! （M10-B1）与 `routes/onboarding/tests/support.rs`（M9-3）。
//!
//! **本片零出站**（两条都是本地路由：一条落本地磁盘、一条读本地磁盘）⇒ 与 M9-1/M9-2
//! 相反，这份 support 只提供「应用装配 + 发请求 + 造 multipart」三件套。
//!
//! ⚠️ `AppState` 用真 URL、**不可达**地址的 `connect_lazy` 装：不碰库的那一半一旦有人
//! 真去查库就会拿到连接错误，而不是静默通过。

use std::sync::Arc;

use axum::body::Body as AxumBody;
use axum::http::Request as HttpRequest;
use http_body_util::BodyExt as _;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use tower::ServiceExt as _;
use uuid::Uuid;

use crate::routes::auth_user::USER_ID_HEADER;
use crate::state::cloud::{CloudConfig, EntitlementConfig};
use crate::state::integrations::{ComposioKeys, GithubKeys, VcsKeys};
use crate::state::{
    AdapterRegistry, AppState, ChannelKeys, ConfigSnapshot, GoogleOAuthConfig, RuntimeHandles,
};

/// 真 URL、**不可达**地址（与 `routes/attachments/tests/support.rs` 同款形状）。
pub const UNREACHABLE_DB: &str = "postgres://upload:upload@127.0.0.1:1/upload_probe";

/// multipart 边界（用例里手拼 body 用；真实客户端用什么边界与本片无关）。
pub const BOUNDARY: &str = "m10b2boundary";

/// 懒连接池（`connect_lazy` 不拨号）：不碰库的那一半一次都不连。
pub fn lazy_db() -> Db {
    Db::connect_lazy(UNREACHABLE_DB, 1, 0).expect("lazy db")
}

/// 真库那一半的连接（设了变量连不上 ⇒ **panic**，不许静默假装绿）。
pub async fn pool() -> Option<Db> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    Some(
        Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}")),
    )
}

/// 一个会自己清掉的临时目录（`LocalDiskStorage` 的根）。
///
/// **不用 `tempfile`**：它是 `mc-storage` 的 dev-dep，**不是** `mc-http` 的
/// ⇒ 用它要改 `mc-http/Cargo.toml`（本片写集之外）。判据：本仓惯例是
/// 「写集外的共享 manifest 一律不动」。
pub struct TempDir(std::path::PathBuf);

impl TempDir {
    #[must_use]
    pub fn new(tag: &str) -> Self {
        let p =
            std::env::temp_dir().join(format!("multica-m10b2-{tag}-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).expect("mkdir tempdir");
        Self(p)
    }

    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.0
    }

    /// 在 `<root>/uploads/<键>` 下写一个对象（静态分发面读的就是这里）。
    pub fn seed(&self, key: &str, body: &[u8]) {
        let path = self
            .0
            .join(crate::routes::uploads::UPLOADS_BUCKET)
            .join(key);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir nested");
        std::fs::write(path, body).expect("write object");
    }

    /// 造一个指向 `target` 的符号链接，返回**链接自身**的路径。
    ///
    /// 符号链接逃逸那条反例的夹具：链接放在上传根**之内**、指向根**之外** ⇒
    /// 只看键的字符串判不出来（`validate_key` 与 [`guard_static_key`] 都放行），
    /// 必须靠 `resolve_under` 的包含性判定。
    #[cfg(unix)]
    pub fn symlink(&self, name: &str, target: &std::path::Path) -> std::path::PathBuf {
        let link = self
            .0
            .join(crate::routes::uploads::UPLOADS_BUCKET)
            .join(name);
        std::fs::create_dir_all(link.parent().expect("parent")).expect("mkdir nested");
        std::os::unix::fs::symlink(target, &link).expect("symlink");
        link
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 一个已注册 + 已路由 `uploads` 桶的本地磁盘存储。
pub fn build_storage(root: &std::path::Path) -> mc_storage::Storage {
    let storage = mc_storage::Storage::new();
    storage
        .register(Arc::new(mc_storage::LocalDiskStorage::new(root)))
        .expect("register local_disk");
    storage
        .route_bucket(crate::routes::uploads::UPLOADS_BUCKET, "local_disk")
        .expect("route bucket");
    storage
}

/// 装了**本地磁盘 provider** 的 app（`/uploads/*` 会挂载）。
pub fn test_app(db: Db, root: &std::path::Path) -> axum::Router {
    assemble(build_state(db, build_storage(root)))
}

/// **没有**任何 provider 的 app（`/uploads/*` 不挂载，上传面回 403）。
pub fn test_app_without_storage(db: Db) -> axum::Router {
    assemble(build_state(db, mc_storage::Storage::new()))
}

fn assemble(state: Arc<AppState>) -> axum::Router {
    crate::apply_default_middleware(crate::routes::router(state.clone())).with_state(state)
}

fn build_state(db: Db, storage: mc_storage::Storage) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "upload-test"));
    Arc::new(AppState {
        db,
        runtime: RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        config: ConfigSnapshot::default(),
        storage,
        secrets: mc_secrets::Secrets::new(mc_auth::DefaultSecretsBackend::in_memory()),
        feature_flags: Arc::new(FeatureFlagCatalog::new()),
        realtime,
        ws,
        auth: mc_auth::SessionStoreContainer::new(),
        pat: mc_auth::PatStoreContainer::new(),
        verification: mc_auth::VerificationStoreContainer::new(),
        google_oauth: GoogleOAuthConfig::default(),
        daemon_hub: Arc::new(mc_ws::hub::Hub::new()),
        daemon_requests: Arc::new(crate::daemon_requests::RequestStore::new()),
        plugin_key: None,
        plugin_surface_origin: None,
        channel_keys: ChannelKeys::default(),
        github_keys: GithubKeys::default(),
        vcs_keys: VcsKeys::default(),
        composio_keys: ComposioKeys::default(),
        // 本片**零出站** ⇒ 云配没配都一样。
        cloud: CloudConfig::from_env_with(|_| None),
        entitlement: EntitlementConfig::from_env_with(|_| None),
    })
}

// ---------------------------------------------------------------------------
// 请求装置
// ---------------------------------------------------------------------------

/// multipart 里的一个字段。
pub struct Part {
    pub name: &'static str,
    pub filename: Option<&'static str>,
    pub content_type: Option<&'static str>,
    pub body: Vec<u8>,
}

impl Part {
    /// 一个文件字段（`name="file"`）。
    #[must_use]
    pub fn file(filename: &'static str, body: &[u8]) -> Self {
        Self {
            name: "file",
            filename: Some(filename),
            content_type: None,
            body: body.to_vec(),
        }
    }

    /// 一个纯文本字段（`issue_id` 等）。
    #[must_use]
    pub fn text(name: &'static str, value: &str) -> Self {
        Self {
            name,
            filename: None,
            content_type: None,
            body: value.as_bytes().to_vec(),
        }
    }
}

/// 拼一个 `multipart/form-data` 请求体。
#[must_use]
pub fn multipart_body(parts: &[Part]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for p in parts {
        out.extend_from_slice(b"--");
        out.extend_from_slice(BOUNDARY.as_bytes());
        out.extend_from_slice(b"\r\nContent-Disposition: form-data; name=\"");
        out.extend_from_slice(p.name.as_bytes());
        out.push(b'"');
        if let Some(f) = p.filename {
            out.extend_from_slice(b"; filename=\"");
            out.extend_from_slice(f.as_bytes());
            out.push(b'"');
        }
        out.extend_from_slice(b"\r\n");
        if let Some(ct) = p.content_type {
            out.extend_from_slice(b"Content-Type: ");
            out.extend_from_slice(ct.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&p.body);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"--");
    out.extend_from_slice(BOUNDARY.as_bytes());
    out.extend_from_slice(b"--\r\n");
    out
}

/// 一条待发请求。
pub struct Call {
    pub method: &'static str,
    pub uri: String,
    pub user: Option<Uuid>,
    pub workspace: Option<Uuid>,
    /// 额外要带的头（`x-workspace-slug` 那一支用）。
    pub extra_header: Option<(String, String)>,
    /// 手工给的请求体（`None` ⇒ 空体；非 `multipart` 时用它做畸形输入）。
    pub body: Option<Vec<u8>>,
    /// 手工给的 `content-type`（`None` ⇒ multipart 的那一个）。
    pub content_type: Option<String>,
}

impl Call {
    /// 带会话 + workspace 头的一条请求。
    #[must_use]
    pub fn new(method: &'static str, uri: impl Into<String>, user: Uuid, workspace: Uuid) -> Self {
        Self::bare(method, uri)
            .with_user(user)
            .with_workspace(workspace)
    }

    /// 只带会话（**不带** workspace 头 —— 无上下文分支要测的就是「没有它」）。
    #[must_use]
    pub fn authed(method: &'static str, uri: impl Into<String>, user: Uuid) -> Self {
        Self::bare(method, uri).with_user(user)
    }

    /// 匿名（用来钉 401）。
    #[must_use]
    pub fn anon(method: &'static str, uri: impl Into<String>) -> Self {
        Self::bare(method, uri)
    }

    #[must_use]
    pub fn bare(method: &'static str, uri: impl Into<String>) -> Self {
        Self {
            method,
            uri: uri.into(),
            user: None,
            workspace: None,
            extra_header: None,
            body: None,
            content_type: None,
        }
    }

    #[must_use]
    pub fn with_user(mut self, user: Uuid) -> Self {
        self.user = Some(user);
        self
    }

    #[must_use]
    pub fn with_workspace(mut self, workspace: Uuid) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// 带一个 multipart 体的 `POST /api/upload-file`。
    #[must_use]
    pub fn upload(parts: &[Part]) -> Self {
        Self::bare("POST", "/api/upload-file").with_body(multipart_body(parts))
    }

    #[must_use]
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }

    /// 额外带一个头（`x-workspace-slug` 那一支）。
    #[must_use]
    pub fn with_extra_header(mut self, name: &str, value: &str) -> Self {
        self.extra_header = Some((name.to_owned(), value.to_owned()));
        self
    }
}

/// 发一条请求，返回 `(状态码, 响应头, 正文)`。
pub async fn call(app: &axum::Router, c: Call) -> (u16, axum::http::HeaderMap, String) {
    let (status, headers, bytes) = call_bytes(app, c).await;
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// 发一条请求并**原样取回字节**（静态分发面用它验「字节逐字不变」）。
pub async fn call_bytes(app: &axum::Router, c: Call) -> (u16, axum::http::HeaderMap, Vec<u8>) {
    let mut req = HttpRequest::builder().method(c.method).uri(&c.uri);
    if let Some(u) = c.user {
        req = req.header(USER_ID_HEADER.as_str(), u.to_string());
    }
    if let Some(w) = c.workspace {
        req = req.header("x-workspace-id", w.to_string());
    }
    if let Some((name, value)) = &c.extra_header {
        req = req.header(name.as_str(), value.as_str());
    }
    let ct = c
        .content_type
        .clone()
        .unwrap_or_else(|| format!("multipart/form-data; boundary={BOUNDARY}"));
    req = req.header("content-type", ct);
    let body = AxumBody::from(c.body.unwrap_or_default());
    let resp = app
        .clone()
        .oneshot(req.body(body).expect("build request"))
        .await
        .expect("router responded");
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    (status, headers, bytes.to_vec())
}

/// 造一个 uuid（用例只拿它当**不同的**标识符用，不作任何确定性断言）。
#[must_use]
pub fn new_uuid() -> Uuid {
    Uuid::new_v4()
}

/// 1×1 的 PNG（判据用真魔数，不拿假字节糊弄嗅探器）。
pub const PNG_1X1: [u8; 67] = [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// `guard_static_key`（被本目录的用例直接调用，导出让它成为**活**判据）。
pub use super::super::guard_static_key;

/// 键的扩展名是不是 `.png`。
///
/// 刻意**大小写敏感**（本仓 `storage_filename` 原样保留客户端给的扩展名，不做小写化）
/// ⇒ 不写 `str::ends_with(".png")`（那会触发 `clippy::case_sensitive_file_extension_comparisons`）。
#[must_use]
pub fn has_png_extension(url: &str) -> bool {
    std::path::Path::new(url)
        .extension()
        .is_some_and(|e| e == "png")
}
