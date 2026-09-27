//! 附件面的**取字节**三键：`/content`、`/download`、`/signed-download`
//! （**写者 M10-B1** / `LUM-2112` / `docs/64` §4.2 第 1 行）。
//!
//! 上游来源（pin `f41fae6b08fb`）：
//! - `server/internal/handler/file.go:1285` `GetAttachmentContent`
//! - `server/internal/handler/file.go:849` `DownloadAttachment`
//! - `server/internal/handler/attachment_capability.go:198` `DownloadAttachmentWithCapability`
//!
//! # 形态（`docs/64` §1.4：`dual-form required: 0`）
//!
//! 上游三条全是 **plain** 注册（`r.Get("/api/attachments/{id}/content", …)` 等，
//! `router.go:2155 / 1655 / 1447`）⇒ 本片**只注册无尾斜杠**那一形态。
//! 补尾斜杠 = `EXTRA_ALIAS` 硬失败，漏字面量 = `MISSING_EXACT`（门 ⑦b）。
//!
//! # 三条键的**授权模型各不相同**（本文件最要紧的一层，逐条对齐上游）
//!
//! | 键 | 认证 | 成员校验 | 跨 workspace |
//! |---|---|---|:--:|
//! | `/content` | `AuthUser` + workspace 头 | 成员门槛 | **404** |
//! | `/download` | `AuthUser`，**无 workspace 头** | 自行验（`loadAttachmentForDownload`） | **404** |
//! | `/signed-download` | **无**（签名本身就是凭据） | **不验**（验签即已验） | 不适用 |
//!
//! 为什么 `/download` 不能走 workspace 头：它必须能在**原生 `<img>` / `<video>` 加载**下工作
//! （上游 `router.go:1650-1654` 的注释逐字），而浏览器不会给 `<img>` 带 `X-Workspace-*`。
//! ⇒ workspace 只能从行本身解析 ⇒ 用 `get_by_id_only`（**授权中性**的查询）。
//! 🔴 代价 = 这条路由天然是 IDOR 的候选面；上游用「成员不达标也回 **404**」的形状堵住
//! （`file.go:809-812` 逐字："so non-member and non-existent look identical"）⇒ 本仓照搬。
//!
//! # `/signed-download` 为什么要存在（`MUL-5292`）
//!
//! 原生下载（Electron `webContents.downloadURL`、跨站 webview 的 `<img>`）**既没有**
//! `Authorization` 头**也没有** cookie ⇒ 走 `/download` 会 401，用户永远拿不到文件。
//! 能力链接（capability）就是补这一支的：已认证的 `GET /api/attachments/{id}` 现铸一个
//! **单附件、60 秒**的签名，另一条**公开**路由凭签名放行。
//! 成员资格**在铸签名时验过、兑换时不再验** —— 签名本身就是「验过了」的证据。
//!
//! 刻意**不是**通用凭据（四条，上游注释逐字）：
//! - 绑定**单个** attachment id ⇒ 换另一个 id 重放无效；
//! - **60 秒** TTL；
//! - 签名密钥**从根密钥派生并做域分隔** ⇒ 造不出会话 token，也伪造不出别的 HMAC；
//! - **不落库**、**不进列表响应**（只在 `GET /api/attachments/{id}` 一个响应里出现）。
//!
//! ## 本仓的两处取密钥口径（已登记为偏离，`docs/32` §50）
//!
//! 1. **不引 CloudFront、不引 S3 presign** ⇒ `resolveAttachmentDownloadMode` 的三个分支
//!    只剩 `proxy` 一支（`docs/64` §2.2 第 2 行：`/api/config` 的 `cdn_signed` 因此**恒 false**，
//!    两处互为断言）。
//! 2. **HMAC 自己算**：用 `mc-http` **已有**的 `sha2` 依赖手写 HMAC-SHA256（RFC 2104，
//!    20 行），**不**给 `mc-http/Cargo.toml` 加 `hmac` 边 ⇒ 写集零扩大、零新 package。
//!    （issue 描述写的是「用 `mc-storage` 自己的 HMAC」；`mc-storage` 不在本片写集内，
//!    且 `mc-http` 已有 `sha2`，就地实现是零依赖增量的那条路。）
//! 3. **取密钥**：`ATTACHMENT_DOWNLOAD_SECRET` → 回退 `JWT_SECRET`（与
//!    `crates/mc-composio/src/state.rs:151` 的 `COMPOSIO_STATE_SECRET|JWT_SECRET` **同款**）。
//!    🔴 **两者都没配 ⇒ 铸造侧返回空串（优雅降级）、兑换侧一律 403**（fail-closed），
//!    绝不退化成「无签名也能下载」。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::attachment::AttachmentRow;

use crate::error::{ApiError, ApiResult};
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::not_found;
use crate::routes::issues::{resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 常量（上游 `file.go:55` + `attachment_capability.go:45-56`）
// ---------------------------------------------------------------------------

/// 内联文本预览的字节上限（上游 `maxPreviewTextSize = 2 << 20`）。
pub const PREVIEW_MAX_BYTES: usize = 2 << 20;

/// 能力链接的版本（进签名消息 ⇒ 消息格式以后能改而不让 v1 签名验过 v2 校验器）。
pub const CAPABILITY_VERSION: &str = "v1";

/// 能力 TTL（秒）。上游 **60**，注释逐字："short by design"。
pub const CAPABILITY_TTL_SECS: i64 = 60;

/// 签名域分隔前缀（上游 `attachmentCapabilityKeyDomain`）。
///
/// 逐字参与密钥派生：`key = SHA-256(域前缀 ‖ 根密钥)` ⇒ 能力签名与 JWT 签名
/// 不可能相撞；轮转根密钥顺带作废在途能力。
pub const CAPABILITY_KEY_DOMAIN: &str = "attachment-download-capability:";

/// 「下载按钮」意图的域分隔词（上游 `attachmentCapabilityDownloadIntent`）。
///
/// 🔴 上游注释明确说它**不是**当前威胁的缓解（load 链接翻成 download 只会**更安全**），
/// 留它是给「将来某个意图比 load 链接权限更高」用的**前向余量**。它进签名消息，
/// 所以 load 链接**不能**靠追加 `dl=1` 自升格（校验会失败）。
pub const CAPABILITY_DOWNLOAD_INTENT: &str = "attachment";

/// 根密钥的两个环境变量（顺序即优先级）。
pub const CAPABILITY_SECRET_ENV: &str = "ATTACHMENT_DOWNLOAD_SECRET";
pub const CAPABILITY_SECRET_FALLBACK_ENV: &str = "JWT_SECRET";

/// 上游的错误文案（逐字，`DoD` 的文案分叉面）。
pub const ERR_NOT_FOUND: &str = "attachment not found";
pub const ERR_BAD_LINK: &str = "invalid or expired download link";
pub const ERR_STORAGE_OFF: &str = "storage not configured";
pub const ERR_OBJECT_MISSING: &str = "attachment object not found";
pub const ERR_NOT_PREVIEWABLE: &str = "preview not supported for this file type";
pub const ERR_PREVIEW_TOO_LARGE: &str = "file too large for inline preview";
pub const ERR_PREVIEW_READ: &str = "failed to read attachment body";

/// 预览类响应的 CSP（上游 `attachmentPreviewCSPHeader`，本仓只有 `'self'` 那一档）。
pub const PREVIEW_CSP: &str = "default-src 'none'; frame-ancestors 'self'";

// ---------------------------------------------------------------------------
// router（3 条注册键）
// ---------------------------------------------------------------------------

/// 本文件的 router：`/content` + `/download` + `/signed-download`。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/attachments/:id/content", get(get_attachment_content))
        .route("/api/attachments/:id/download", get(download_attachment))
        .route(
            "/api/attachments/:id/signed-download",
            get(download_with_capability),
        )
}

/// `/api/attachments/{id}/signed-download` 的查询参数。
#[derive(Debug, Default, serde::Deserialize)]
pub struct CapabilityQuery {
    pub exp: Option<String>,
    pub sig: Option<String>,
    pub dl: Option<String>,
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `GET /api/attachments/{id}/content`（上游 `GetAttachmentContent`）。
///
/// 白名单内的文本走 `text/plain` 内联返回（**原始 MIME 只在 `X-Original-Content-Type`**），
/// 让敌意 HTML 载荷不被浏览器当文档重解释。
pub async fn get_attachment_content(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Response> {
    let att = load_attachment_for_request(&state, &headers, &query, &raw_id, user.id()).await?;

    if !is_text_previewable(&att.content_type, &att.filename) {
        return Ok(status_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ERR_NOT_PREVIEWABLE,
        ));
    }

    let (bucket, key) = split_object_ref(&att.url).ok_or_else(storage_disabled)?;
    let body = match read_object(&state, &bucket, &key).await {
        Ok(b) => b,
        Err(e) => return Ok(object_read_to_response(&e)),
    };

    // LimitReader(max+1) 的等价物：读 max+1 字节，用「长度 > max」区分「恰好在限上」与「超限」。
    if body.len() > PREVIEW_MAX_BYTES {
        return Ok(status_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            ERR_PREVIEW_TOO_LARGE,
        ));
    }

    let len = body.len();
    let mut resp = Response::new(axum::body::Body::from(body));
    let h = resp.headers_mut();
    // 恒 `text/plain`；原始 MIME 只走这个头（上游 `file.go:1341-1343` 逐字）。
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if let Ok(v) = header::HeaderValue::from_str(&att.content_type) {
        h.insert("x-original-content-type", v);
    }
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    h.insert(
        HeaderName::from_static("x-content-type-options"),
        header::HeaderValue::from_static("nosniff"),
    );
    set_preview_security_headers(h);
    if let Ok(v) = header::HeaderValue::from_str(&len.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    Ok(resp)
}

/// `GET /api/attachments/{id}/download`（上游 `DownloadAttachment`，**proxy 模式**）。
///
/// 本仓只有 proxy 一支（无 CloudFront、无 presign）⇒ 恒内联出字节，**永不**发跨源 302。
/// 成员校验走 404 形状的 [`load_attachment_for_download`]。
pub async fn download_attachment(
    State(state): State<Arc<AppState>>,
    Path(raw_id): Path<String>,
    user: AuthUser,
) -> ApiResult<Response> {
    let att = load_attachment_for_download(&state, &raw_id, user.id()).await?;
    stream_object(&state, &att, false).await
}

/// `GET /api/attachments/{id}/signed-download`（上游 `DownloadAttachmentWithCapability`）。
///
/// **公开路由**：能力签名**就是**凭据，原生下载请求没有东西给 `middleware.Auth` 读，
/// 挂到 Auth 后面会直接废掉它唯一的用途。
///
/// `dl=1` 是「下载按钮」意图，由**另一条**签名覆盖 ⇒ 校验不过，load 链接无法自升格。
pub async fn download_with_capability(
    State(state): State<Arc<AppState>>,
    Path(raw_id): Path<String>,
    Query(q): Query<CapabilityQuery>,
) -> ApiResult<Response> {
    let attachment_id = parse_attachment_id(&raw_id)?;

    let download_intent = q.dl.as_deref() == Some("1");
    let intent = if download_intent {
        CAPABILITY_DOWNLOAD_INTENT
    } else {
        ""
    };
    let key = capability_key();
    let ok = verify_capability(
        key.as_ref(),
        &attachment_id.to_string(),
        q.exp.as_deref().unwrap_or_default(),
        q.sig.as_deref().unwrap_or_default(),
        intent,
        now_unix(),
    );
    if !ok {
        // 每一个失败原因**同一个**拒绝：不让调用方靠「过期 / 伪造 / 换 id」的差别
        // 去探签名器（上游 `attachment_capability.go:229-233` 逐字）。
        return Err(Error::Forbidden {
            message: ERR_BAD_LINK.into(),
        }
        .into());
    }

    let att = fetch_by_id_only(&state, attachment_id).await?;
    let mut resp = stream_object(&state, &att, download_intent).await?;
    // 签名走 query；正文若是会加载子资源的文档，no-referrer 把它挡在出站 Referer 之外。
    resp.headers_mut().insert(
        "referrer-policy",
        header::HeaderValue::from_static("no-referrer"),
    );
    Ok(resp)
}

// ---------------------------------------------------------------------------
// 加载（两条 workspace 模型）
// ---------------------------------------------------------------------------

/// `loadAttachmentForRequest`：`workspace 头 → GetAttachment(id, workspace)`。
///
/// 跨 workspace / 非成员 ⇒ **404**（不是 403）。
pub(crate) async fn load_attachment_for_request(
    state: &AppState,
    headers: &HeaderMap,
    query: &WorkspaceQuery,
    raw_id: &str,
    user_id: Id,
) -> ApiResult<AttachmentRow> {
    let workspace_id = resolve_workspace(state, headers, query).await?;
    let attachment_id = parse_attachment_id(raw_id)?;
    if is_not_member(state, workspace_id, user_id).await {
        return Err(not_found("attachment").into());
    }
    fetch_scoped(state, workspace_id, attachment_id).await
}

/// `loadAttachmentForDownload`：`GetAttachmentByIDOnly` + 成员校验（**404 形状**）。
///
/// 「非成员」与「不存在」返回**同一个 404** ⇒ 这条路由不是附件 id 的 IDOR oracle。
pub(crate) async fn load_attachment_for_download(
    state: &AppState,
    raw_id: &str,
    user_id: Id,
) -> ApiResult<AttachmentRow> {
    let attachment_id = parse_attachment_id(raw_id)?;
    let att = fetch_by_id_only(state, attachment_id).await?;
    if is_not_member(state, att.workspace_id(), user_id).await {
        return Err(not_found("attachment").into());
    }
    Ok(att)
}

/// 成员判定（**只判真假，不区分原因**）—— 上游两个 deny 分支都是同一个 404。
async fn is_not_member(state: &AppState, workspace_id: Id, user_id: Id) -> bool {
    crate::routes::invitations::require_workspace_member(state, workspace_id, user_id)
        .await
        .is_err()
}

pub use super::download_pure::*;

// ---------------------------------------------------------------------------
// DB / 存储 I/O
// ---------------------------------------------------------------------------

async fn fetch_scoped(state: &AppState, workspace_id: Id, id: Id) -> ApiResult<AttachmentRow> {
    match repo(state).get(workspace_id, id).await {
        // 跨 workspace / 抓取上下文副本 / 不存在 ⇒ 一律 404（不区分）。
        Ok(row) if !row.is_captured() => Ok(row),
        Ok(_) | Err(_) => Err(not_found("attachment").into()),
    }
}

async fn fetch_by_id_only(state: &AppState, id: Id) -> ApiResult<AttachmentRow> {
    match repo(state).get_by_id_only(id).await {
        Ok(row) if !row.is_captured() => Ok(row),
        Ok(_) | Err(_) => Err(not_found("attachment").into()),
    }
}

/// 读对象字节的失败（两种，映射到两个**不同**的状态码）。
enum ObjectRead {
    /// 对象不存在（上游 `"attachment object not found"` ⇒ **404**）。
    Missing,
    /// 存储后端坏了（桶没路由 / 权限 / IO ⇒ **502**）。
    Unavailable(String),
}

/// 读对象字节。`NotFound` ⇒ 404，其余 IO 错误 ⇒ 502。
///
/// ⚠️ 为什么不直接返 `ApiError`：`mc_errors::Error` **没有** 502 变体
/// （`Error::Upstream` 的 `http_status()` 恒 500，`mc-errors/src/http.rs:86`），
/// 而 `ApiError::respond_with` 只从 `Response` 侧覆盖状态码 ⇒ 先在这里保住
/// 「哪一种失败」的区分，再由调用点决定 404 还是 502。
async fn read_object(
    state: &AppState,
    bucket: &str,
    key: &str,
) -> std::result::Result<mc_storage::bytes::Bytes, ObjectRead> {
    match state.storage.get(bucket, key).await {
        Ok(data) => Ok(data),
        Err(mc_storage::StorageError::NotFound(_)) => Err(ObjectRead::Missing),
        Err(other) => Err(ObjectRead::Unavailable(other.to_string())),
    }
}

/// `proxyAttachmentDownload` 的本仓等价物（proxy 是唯一一支 ⇒ 恒内联出字节，永不 302）。
async fn stream_object(
    state: &AppState,
    att: &AttachmentRow,
    force_attachment: bool,
) -> ApiResult<Response> {
    let (bucket, key) = split_object_ref(&att.url).ok_or_else(storage_disabled)?;
    let body = match read_object(state, &bucket, &key).await {
        Ok(b) => b,
        Err(e) => return Ok(object_read_to_response(&e)),
    };

    let disposition = if force_attachment {
        attachment_content_disposition(&att.filename)
    } else {
        content_disposition(&att.content_type, &att.filename)
    };
    let content_type = if att.content_type.is_empty() {
        "application/octet-stream"
    } else {
        att.content_type.as_str()
    };

    let mut resp = Response::new(axum::body::Body::from(body));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(content_type)
            .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(v) = header::HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    h.insert(
        HeaderName::from_static("x-content-type-options"),
        header::HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::ACCEPT_RANGES,
        header::HeaderValue::from_static("bytes"),
    );
    set_preview_security_headers(h);
    Ok(resp)
}

/// 预览类响应的 CSP 头（上游 `setAttachmentPreviewSecurityHeaders`）。
pub(crate) fn set_preview_security_headers(h: &mut header::HeaderMap) {
    h.insert(
        "content-security-policy",
        header::HeaderValue::from_static(PREVIEW_CSP),
    );
}

fn storage_disabled() -> ApiError {
    // 上游 `writeFeatureDisabled` ⇒ 403 + code `storage_not_configured`。
    ApiError(Error::Forbidden {
        message: ERR_STORAGE_OFF.into(),
    })
}

/// `ObjectRead` → 错误**响应**（404 / 502 两个状态码，逐字文案）。
fn object_read_to_response(err: &ObjectRead) -> Response {
    match err {
        ObjectRead::Missing => ApiError(Error::NotFound {
            resource: ERR_OBJECT_MISSING.into(),
        })
        .respond_with(StatusCode::NOT_FOUND),
        ObjectRead::Unavailable(detail) => bad_gateway(detail),
    }
}

fn bad_gateway(detail: &str) -> Response {
    status_error(
        StatusCode::BAD_GATEWAY,
        &format!("{ERR_PREVIEW_READ}: {detail}"),
    )
}

/// 本仓 `mc_errors::Error` **没有** 415 / 413 两个变体（`mc-errors/src/lib.rs` 只有
/// `Validation` / `NotFound` / `Forbidden` / `Upstream` …），而上游这两条各有独立文案。
/// ⇒ 用 `ApiError::respond_with` 指定状态码（`routes/projects/helpers.rs:121` 的同款判例）。
///
/// ⚠️ 代价：错误体里的 `code` 字段会是 `validation_error`（不是 415 专有的 code）。
/// **状态码与 `message` 文案逐字对齐上游**（本片 `DoD` 断言的就是这两样）；已登记为偏离。
fn status_error(status: StatusCode, message: &str) -> Response {
    ApiError(Error::Validation {
        message: message.to_owned(),
        details: vec![],
    })
    .respond_with(status)
}

fn repo(state: &AppState) -> mc_repos::attachment::AttachmentRepo {
    mc_repos::attachment::AttachmentRepo::new(&state.db)
}

/// `:id` 只接受 UUID（上游 `parseUUIDOrBadRequest(w, id, "attachment id")`）。
pub(crate) fn parse_attachment_id(raw: &str) -> Result<Id, ApiError> {
    Id::parse(raw.trim()).map_err(|_| {
        ApiError(Error::Validation {
            message: "attachment id must be a uuid".into(),
            details: vec![],
        })
    })
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}
