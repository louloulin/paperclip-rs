//! 上传与静态分发面：**2 行**（**写者 M10-B2** / `LUM-2113` / `docs/64` §4.2 第 2 行、
//! `§6.5` 的 M10-B2 行）。
//!
//! | 文件（本文件） | 注册键 | 上游 handler（pin `f41fae6b08fb`） |
//! | --- | :-: | --- |
//! | 本文件 | `POST /api/upload-file` | `router.go:1633` → `file.go:379` `UploadFile` |
//! | 本文件 | `GET /uploads/*` | `router.go:1435` → `file.go:962` `ServeLocalUpload` |
//!
//! 账：**2** 行、**2** 个缺口（`known_gap −2`），`implemented_placeholder` **不动**
//! （本片两条都是纯新增）。复算见 `docs/64` §10 命令 2。
//!
//! ## 形态（`docs/64` §1.4：`dual-form required: 0`）
//!
//! 上游两条**全是 plain 注册**（`r.Post("/api/upload-file", h.UploadFile)`、
//! `r.Get("/uploads/*", h.ServeLocalUpload)`）⇒ 本片**只注册字面量那一形态**。
//! 补尾斜杠 = `EXTRA_ALIAS` 硬失败，漏字面量 = `MISSING_EXACT`（门 ⑦b）。
//!
//! 🔴 `/uploads/*` 那一行**必须逐字写 `*`**（不是 `*path`、不是 `{*path}`）：门 ⑦/⑦b 的
//! `normalize()` 把上游的 `/uploads/*` 折成 `/uploads/:wildcard`，只有 `*` 整段**恰好**
//! 等于 `"*"` 时才折成同一个键 ⇒ 写成 `*path` 会变成 `/uploads/*path` ⇒ 判成
//! **新路由（`local_only`）+ 上游缺口仍在**。matchit 0.7 接受无名的 `*`（`tree.rs:651`
//! 的 `find_wildcard` 对空名字不报错）⇒ 注册无碍；取值不靠路径参数而靠
//! [`Uri`](axum::http::Uri) 提取器（见 [`serve_local_upload`]）。
//!
//! ## 三处**与切片描述不一致**的地方（以**上游逐字**为准，已登记 `docs/32` §9.21）
//!
//! 切片描述的「错误码逐条（413 / 415 / 400）」与「类型白名单」**在本路由上不存在**：
//!
//! 1. `POST /api/upload-file` 的上游实现（`file.go:393-397`）只有一条失败文案
//!    `file too large or invalid multipart form` + **400**（`http.MaxBytesReader` 的
//!    超限与 `ParseMultipartForm` 的畸形**合并成同一条**）。上游 `file.go` 里的 413
//!    （`:1319`）与 415（`:1293`）属于 **`GET /api/attachments/{id}/content`**，
//!    是 **M10-B1** 那条路由的验收项，被写串了。
//! 2. 上游**没有类型白名单**：它嗅探字节（`http.DetectContentType`）再用扩展名覆盖
//!    六个嗅探错的类型（[`EXT_CONTENT_TYPES`]）。照搬，不新增拒绝面。
//! 3. `GET /uploads/*` 的 404 有**三个**独立成因（`..`、绝对路径、符号链接逃逸），
//!    上游对三者**都是** `http.NotFound` ⇒ 本片统一 404。
//!
//! ## 桶 / 键 / URL 的本仓口径（上游 `LocalStorage` 的 `Upload` 返回绝对 URL，
//! 本仓 `mc-storage` 的 `put` 返回 `Object` ⇒ URL 得自己拼）
//!
//! - 桶名恒 [`UPLOADS_BUCKET`]（= `"uploads"`）；
//! - 键**逐字**沿用上游的布局：`workspaces/<workspace>/<uuid><ext>` 或
//!   `users/<user>/<uuid><ext>`（`file.go:441-448`）；
//! - 落库的 `attachment.url` = `/uploads/<键>` ⇒ 与 M10-B1 的
//!   [`split_object_ref`](super::attachments::download::split_object_ref) 约定**自洽**：
//!   它取第一段当桶名，得到的正是 `(UPLOADS_BUCKET, <键>)`。
//!
//! ## 与别处的交集
//!
//! - `crates/mc-storage`（**追加**两处，见 `docs/32` §9.21 的写集偏离登记）：
//!   `StorageProvider::local_root` 的默认实现 + `Storage::local_root` 门面 +
//!   `LocalDiskStorage` 的覆写。**为什么非它不可**：`validate_key` 只看**键的字符串**
//!   （拒 `..`、拒绝对路径），**看不见磁盘上的符号链接** ⇒ 「符号链接逃逸」这条反例
//!   必须拿到根目录才能判。
//! - `mc-repos::attachment` —— **只读**：本片**不**给 `attachment` 加 `create`，
//!   插入走 `state.db` 上的 `sqlx`（`routes/onboarding/shim.rs` 的同款判例），
//!   写集零扩大。
//! - M10-B1 的 [`super::attachments::download::set_preview_security_headers`] —— **只读复用**
//!   （上游 `setAttachmentPreviewSecurityHeaders`，两处同一行 CSP）。

use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Multipart, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use mc_core::Id;
use mc_errors::Error;
use serde_json::json;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::routes::attachments::read::AttachmentResponse;
use crate::routes::auth_user::AuthUser;
use crate::routes::issues::{resolve_workspace, WorkspaceQuery};
use crate::routes::issues::{WORKSPACE_ID_HEADER, WORKSPACE_SLUG_HEADER};
use crate::state::AppState;

/// 静态分发那一半（`GET /uploads/*`）—— 切片描述的写集里「拆两文件」的那次拆分。
pub mod serve;

/// 静态分发那一半的公开面**平铺**回本模块（`uploads::guard_static_key` 这类路径
/// 保持与「不拆」时一致 ⇒ 用例与后续切片不必知道拆过）。
pub use serve::{
    guard_static_key, not_found, percent_decode, resolve_under, serve_local_upload, KeyError,
};

// ---------------------------------------------------------------------------
// 常量（上游 `file.go:29-38`、`storage/local.go:27,32`）
// ---------------------------------------------------------------------------

/// 上传字节上限（上游 `maxUploadSize = 100 << 20`）。
pub const MAX_UPLOAD_SIZE: usize = 100 << 20;

/// 本仓的存储桶名（上游没有「桶」这一层：`LocalStorage` 只有��个上传根目录）。
pub const UPLOADS_BUCKET: &str = "uploads";

/// 静态分发路由的前缀（= 落库 URL 的前缀，与上游 `router.go:1435` 逐字相同）。
pub const STATIC_PREFIX: &str = "/uploads/";

/// 端点未配置存储时的错误文案（上游 `writeFeatureDisabled` 的 code 面 +
/// `file.go:381` 的文案）。上游这里是 **403**。
pub const ERR_NOT_CONFIGURED: &str = "file upload not configured";

/// 上传表单畸形 / 超限（上游 `file.go:396` 逐字，**400**）。
pub const ERR_FORM_INVALID: &str = "file too large or invalid multipart form";

/// 缺 `file` 字段（上游 `file.go:404`，**400**）。
pub const ERR_MISSING_FILE: &str = "missing file field";

/// 非成员（上游 `file.go:452`，**403**）。
pub const ERR_NOT_MEMBER: &str = "not a member of this workspace";

/// `issue_id` / `comment_id` 校验失败（上游 `file.go:470` / `:479`，**403**）。
pub const ERR_BAD_ISSUE: &str = "invalid issue_id";
/// 同上，`comment_id` 那一支。
pub const ERR_BAD_COMMENT: &str = "invalid comment_id";

/// 存储后端失败（上游 `file.go:560`，**500**）。
pub const ERR_UPLOAD_FAILED: &str = "upload failed";

/// 🔴 **本仓 fail-closed 专有**文案：`chat_session_id` / `task_id` 两条门未实现
/// ⇒ 一律 403（上游分别是「成员可见的 chat 投影」与「task 令牌边界」两道门）。
/// 理由与登记见 `docs/32` §9.21：**宁可拒收，不要静默地把附件挂到不受校验的会话上**。
pub const ERR_CHAT_UNSUPPORTED: &str = "chat_session_id upload is not supported";
/// 同上，`task_id` 那一支（上游 `file.go:522` 的第一道门逐字同义）。
pub const ERR_TASK_UNSUPPORTED: &str = "task_id upload is only available from within an agent task";

/// 本地后端的 sidecar 后缀（上游 `storage/local.go:27`）：**拒绝**直接分发。
pub const META_SUFFIX: &str = ".meta.json";
/// 本地后端的暂存文件后缀（上游 `storage/local.go:32`）：**拒绝**直接分发。
pub const TEMP_SUFFIX: &str = ".tmp";

/// 扩展名覆盖表（上游 `file.go:29-36` 逐字）：嗅探器认不出、但浏览器认得出的那六个。
pub const EXT_CONTENT_TYPES: [(&str, &str); 6] = [
    (".svg", "image/svg+xml"),
    (".css", "text/css"),
    (".js", "application/javascript"),
    (".mjs", "application/javascript"),
    (".json", "application/json"),
    (".wasm", "application/wasm"),
];

/// 非文件字段（`issue_id` 等）的字节上限。
const MAX_TEXT_FIELD: usize = 8 << 10;

// ---------------------------------------------------------------------------
// router（2 条注册键）
// ---------------------------------------------------------------------------

/// **上游逐字的注册条件**（`router.go:1435` 的
/// `if _, ok := store.(*storage.LocalStorage); ok { r.Get("/uploads/*", …) }`）：
/// 该桶**没有**落在本地磁盘 provider 上时，`/uploads/*` **整条不挂载**
/// ⇒ 落到 axum 的 404 兜底，而不是 403 / 500。
///
/// `POST /api/upload-file` **无条件**挂载（上游 `router.go:1633` 在主 router 里，
/// 不受存储类型影响）—— 本仓同理：它的「没配存储」是 handler 内的 **403**
/// （上游 `file.go:380-383` 的 `writeFeatureDisabled`），不是「路由不存在」。
///
/// 门 ⑦ 统计的是**源码里的 `.route(...)` 字面量**（静态抽取），所以「不挂载」不影响
/// `local` 计数 —— 判据是本函数返回的 router 里那条键**不存在**。
///
/// ⚠️ 本函数是本面**唯一**的装配入口（刻意不另留一个「两条都装上」的 `router()`：
/// 门 ⑦ 按**注册点**计数，多一个同键字面量就会让 `local` 多 1，与 §158 的预测差一）。
pub fn mount(state: &AppState) -> Router<Arc<AppState>> {
    let base = Router::new().route("/api/upload-file", post(upload_file));
    if local_root(state).is_some() {
        base.merge(serve::router())
    } else {
        base
    }
}

/// 该桶背后的本地根目录（`None` = 非本地后端或桶没路由）。
pub fn local_root(state: &AppState) -> Option<std::path::PathBuf> {
    state.storage.local_root(UPLOADS_BUCKET)
}

// ---------------------------------------------------------------------------
// POST /api/upload-file
// ---------------------------------------------------------------------------

/// `POST /api/upload-file`（上游 `file.go:379` `UploadFile`）。
///
/// 两条分支（**逐字**）：
/// - **有 workspace 上下文** → 成员校验 → 落 `attachment` 行 → 回
///   [`AttachmentResponse`]；
/// - **无 workspace 上下文**（上游注释逐字："e.g. avatar upload"）→ 只落对象，
///   回 `{id, url, filename}` 三键，**不**写行（`attachment.workspace_id` 是 NOT NULL）。
///
/// 长度：超 100 行是因为它**逐条对照**上游 `file.go:379-628` 的分支顺序
/// （存���门 → 表单 → workspace → 两道 fail-closed 门 → 成员门 → 键 → 落对象 →
/// 无 workspace 分支 → 两个外键校验 → 落行）。拆开会让「上游哪一步对应本仓哪一步」
/// 这条对照断在函数之间 ⇒ 按本仓既有判例（`routes/squads/members.rs:60`）豁免。
#[allow(clippy::too_many_lines)]
pub async fn upload_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    mut multipart: Multipart,
) -> ApiResult<Response> {
    if local_root(&state).is_none() {
        return Ok(status_error(StatusCode::FORBIDDEN, ERR_NOT_CONFIGURED));
    }

    let form = match read_form(&mut multipart).await {
        Ok(f) => f,
        // 上游把「超限」与「畸形」并成**同一条** 400 文案（`file.go:396`）⇒ 两个变体同一支。
        Err(FormError::TooLarge | FormError::Malformed) => {
            return Ok(status_error(StatusCode::BAD_REQUEST, ERR_FORM_INVALID))
        }
        Err(FormError::MissingFile) => {
            return Ok(status_error(StatusCode::BAD_REQUEST, ERR_MISSING_FILE))
        }
    };

    // 上游 `resolveWorkspaceID` 缺上下文时返回 `""`（**不是**错误）⇒ 本仓只在
    // 「四个来源里至少有一个」时才调 `resolve_workspace`（它缺上下文会 400）。
    let workspace = if workspace_hinted(&headers, &query) {
        Some(resolve_workspace(&state, &headers, &query).await?)
    } else {
        None
    };

    // 🔴 两道门本仓**未实现** ⇒ fail-closed 403（不是静默忽略）。
    if form.task_id.is_some() {
        return Ok(status_error(StatusCode::FORBIDDEN, ERR_TASK_UNSUPPORTED));
    }
    if form.chat_session_id.is_some() {
        return Ok(status_error(StatusCode::FORBIDDEN, ERR_CHAT_UNSUPPORTED));
    }

    let id = Uuid::new_v4();
    let filename = storage_filename(&id, &form.filename);
    let content_type = content_type_for(&form.filename, &form.bytes);
    let key = match workspace {
        Some(ws) => format!("workspaces/{}/{filename}", ws.0),
        None => format!("users/{}/{filename}", user.id().0),
    };

    // 🔴 **上游的顺序**（`file.go:450-556` 在前、`file.go:557` 的 `Storage.Upload` 在后）：
    // 成员门与两个外键的校验**全部在落对象之前** ⇒ 一个会被拒的请求**不留垃圾对象**。
    // （本片第一版把 `put` 排在 `issue_id` 校验之前，被真库用例
    //  `upload_to_an_issue_in_another_workspace_is_403` 逮到：403 了却留下了一个对象。）
    let mut issue_id = None;
    let mut comment_id = None;
    if let Some(ws) = workspace {
        if !is_workspace_member(&state, ws, user.id()).await {
            return Ok(status_error(StatusCode::FORBIDDEN, ERR_NOT_MEMBER));
        }
        // 畸形 id ⇒ **400**（上游 `parseUUIDOrBadRequest`）；格式对但不在库里 ⇒ **403**。
        match optional_uuid(form.issue_id.as_deref()) {
            Err(()) => return Ok(status_error(StatusCode::BAD_REQUEST, ERR_BAD_ISSUE)),
            Ok(Some(issue)) => {
                if !issue_in_workspace(&state, ws, issue).await {
                    return Ok(status_error(StatusCode::FORBIDDEN, ERR_BAD_ISSUE));
                }
                issue_id = Some(issue);
            }
            Ok(None) => {}
        }
        match optional_uuid(form.comment_id.as_deref()) {
            Err(()) => return Ok(status_error(StatusCode::BAD_REQUEST, ERR_BAD_COMMENT)),
            Ok(Some(comment)) => {
                if !comment_attachable(&state, ws, comment).await {
                    return Ok(status_error(StatusCode::FORBIDDEN, ERR_BAD_COMMENT));
                }
                comment_id = Some(comment);
            }
            Ok(None) => {}
        }
    }

    let object = match state
        .storage
        .put(
            UPLOADS_BUCKET,
            &key,
            form.bytes.clone(),
            Some(&content_type),
        )
        .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, key = %key, "upload failed");
            return Ok(status_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                ERR_UPLOAD_FAILED,
            ));
        }
    };
    let url = format!("{STATIC_PREFIX}{key}");
    debug_assert_eq!(object.key, key);

    let Some(ws) = workspace else {
        // 上游 `file.go:620-625` 逐字：三键响应，**不**写行。
        return Ok(axum::Json(json!({
            "id": id.to_string(),
            "url": url,
            "filename": form.filename,
        }))
        .into_response());
    };

    // 上游 `file.go:571-580` 逐字：**行写失败也照样把链接返回**（对象已经在盘上，
    // 丢掉链接等于让用户重传一次）。这一支的响应体没有 `id`。
    let inserted: Result<(Uuid, chrono::DateTime<chrono::Utc>), _> = sqlx::query_as(
        "INSERT INTO attachment \
           (id, workspace_id, issue_id, comment_id, uploader_type, uploader_id, \
            filename, url, content_type, size_bytes) \
         VALUES ($1,$2,$3,$4,'member',$5,$6,$7,$8,$9) RETURNING id, created_at",
    )
    .bind(id)
    .bind(ws.0)
    .bind(issue_id)
    .bind(comment_id)
    .bind(user.id().0)
    .bind(&form.filename)
    .bind(&url)
    .bind(&content_type)
    .bind(i64::try_from(form.bytes.len()).unwrap_or(i64::MAX))
    .fetch_one(state.db.pool())
    .await;

    let (row_id, created_at) = match inserted {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, id = %id, "object uploaded but attachment row failed");
            return Ok(
                axum::Json(json!({ "id": "", "url": url, "filename": form.filename }))
                    .into_response(),
            );
        }
    };

    let row = mc_repos::attachment::AttachmentRow {
        id: row_id,
        workspace_id: ws.0,
        issue_id,
        comment_id,
        uploader_type: "member".into(),
        uploader_id: user.id().0,
        filename: form.filename.clone(),
        url: url.clone(),
        content_type: content_type.clone(),
        size_bytes: i64::try_from(form.bytes.len()).unwrap_or(i64::MAX),
        created_at,
        chat_session_id: None,
        chat_message_id: None,
        task_id: None,
        source_context_id: None,
    };
    Ok(axum::Json(AttachmentResponse::stable(&row)).into_response())
}

// ---------------------------------------------------------------------------
// 表单读取
// ---------------------------------------------------------------------------

/// 读出来的一张上传表单（**只有** `file` 字段是必须的）。
#[derive(Debug, Default)]
pub struct UploadForm {
    pub filename: String,
    pub bytes: Bytes,
    pub issue_id: Option<String>,
    pub comment_id: Option<String>,
    pub chat_session_id: Option<String>,
    pub task_id: Option<String>,
}

/// 表单层的失败（**三合一**到上游那一条 400 文案上）。
#[derive(Debug, PartialEq, Eq)]
pub enum FormError {
    /// 超过 [`MAX_UPLOAD_SIZE`]（上游 `MaxBytesReader` 超限 ⇒ 400）。
    TooLarge,
    /// multipart 畸形 / 字段读失败。
    Malformed,
    /// 没有 `file` 字段（上游 `r.FormFile("file")` 的 err 分支）。
    MissingFile,
}

/// 逐字段读完整个 multipart。
///
/// 大小上限是**我们自己**数的（不是 axum 的 `DefaultBodyLimit`）：`Multipart` 在
/// axum 0.7 里**不**实现 `RequestBodyLimit` ⇒ 默认的 2MB 限制对它不生效，而
/// 上游要的是 100MB。逐 `chunk()` 累加 ⇒ 流式、不整份进内存两次。
pub async fn read_form(multipart: &mut Multipart) -> Result<UploadForm, FormError> {
    let mut form = UploadForm::default();
    let mut seen_file = false;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| FormError::Malformed)?
    {
        let name = field.name().unwrap_or_default().to_owned();
        match name.as_str() {
            "file" => {
                let bytes = read_capped(&mut field, MAX_UPLOAD_SIZE).await?;
                form.filename = field.file_name().unwrap_or_default().to_owned();
                form.bytes = bytes;
                seen_file = true;
            }
            "issue_id" | "comment_id" | "chat_session_id" | "task_id" => {
                let raw = read_capped(&mut field, MAX_TEXT_FIELD).await?;
                let value = String::from_utf8_lossy(&raw).trim().to_owned();
                if !value.is_empty() {
                    match name.as_str() {
                        "issue_id" => form.issue_id = Some(value),
                        "comment_id" => form.comment_id = Some(value),
                        "chat_session_id" => form.chat_session_id = Some(value),
                        _ => form.task_id = Some(value),
                    }
                }
            }
            _ => {
                // 未知字段：跳过但**照样读干净**（不读会让连接复用拿到错位的数据）。
                let _ = read_capped(&mut field, MAX_TEXT_FIELD).await?;
            }
        }
    }
    if seen_file {
        Ok(form)
    } else {
        Err(FormError::MissingFile)
    }
}

/// 往 `buf` 追加一个 chunk，超 `cap` 即 [`FormError::TooLarge`]（**不**继续吞字节）。
///
/// 拆成独立纯函数是为了让「大小上限」这条判据**不**需要一个 100MB 的夹具：
/// 用例用 `cap = 8` 就能钉住「恰好在限上收、超一字节就拒」。
pub fn capped_push(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> Result<(), FormError> {
    if buf.len().saturating_add(chunk.len()) > cap {
        return Err(FormError::TooLarge);
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

/// 读一个字段（逐 `chunk()` 累加 ⇒ 流式，不把整份 body 进内存两次）。
async fn read_capped(
    field: &mut axum::extract::multipart::Field<'_>,
    cap: usize,
) -> Result<Bytes, FormError> {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|_| FormError::Malformed)? {
        capped_push(&mut buf, &chunk, cap)?;
    }
    Ok(Bytes::from(buf))
}

// ---------------------------------------------------------------------------
// 纯函数层
// ---------------------------------------------------------------------------

/// 落盘文件名 = `<uuid><原扩展名>`（上游 `file.go:439` 逐字：uuidv7 同时当 id 与键）。
///
/// 🔴 **偏离**：上游是 **UUID v7**（时间有序 ⇒ 键在对象存储里按时间聚簇），本仓用
/// `Uuid::new_v4()` —— `uuid` crate 的 `v7` 特性在本仓未启用，而启用它要动
/// 共享 manifest（写集外）。**功能上无差**（键的**唯一性**与**不可猜测性**都成立），
/// 登记在 `docs/32` §9.21。
#[must_use]
pub fn storage_filename(id: &Uuid, original: &str) -> String {
    let ext = Path::new(original)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .filter(|e| !e.is_empty() && e.len() <= 16 && e.chars().all(char::is_alphanumeric))
        .map_or_else(String::new, |e| format!(".{e}"));
    format!("{id}{ext}")
}

/// 字节嗅探 + 扩展名覆盖（上游 `file.go:411-418` 逐字）。
///
/// ⚠️ 上游用的是 Go 标准库的 `http.DetectContentType`（一张 15 项的魔数表）；
/// 本仓是那张表的**子集**（PNG / JPEG / GIF / WebP / PDF / ZIP / GZIP + 文本兜底），
/// 其余一律 `application/octet-stream`（Go 的兜底值逐字相同）。**不引入 mime 猜��库**。
#[must_use]
pub fn content_type_for(filename: &str, bytes: &[u8]) -> String {
    let sniffed = detect_content_type(bytes);
    let ext = Path::new(filename)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    EXT_CONTENT_TYPES
        .iter()
        .find(|(k, _)| *k == ext)
        .map_or(sniffed, |(_, v)| (*v).to_owned())
}

/// 魔数嗅探的子集（见 [`content_type_for`] 的口径说明）。
#[must_use]
pub fn detect_content_type(bytes: &[u8]) -> String {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.starts_with(PNG) {
        return "image/png".into();
    }
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        return "image/jpeg".into();
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif".into();
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return "image/webp".into();
    }
    if bytes.starts_with(b"%PDF-") {
        return "application/pdf".into();
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return "application/zip".into();
    }
    if bytes.starts_with(b"\x1F\x8B") {
        return "application/gzip".into();
    }
    // Go 的 `DetectContentType` 对「看起来是文本」的内容给
    // `text/plain; charset=utf-8`（它的判据是不可见控制字符）。
    if looks_like_text(bytes) {
        "text/plain; charset=utf-8".into()
    } else {
        "application/octet-stream".into()
    }
}

fn looks_like_text(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    !head
        .iter()
        .any(|b| matches!(b, 0x00..=0x08 | 0x0B | 0x0E..=0x1A | 0x1C..=0x1F))
}

/// `%XX` 解码（上游的 `r.URL.Path` 是**已解码**的路径；axum 给的是原始串）。
///
/// `None` = 百分号转义畸形（上游 `net/url` 会直接 400，这里归到 404 —— 静态分发面
/// 对「读不到」的所有成因统一 404，见模块头第 3 条）。
#[must_use]
fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// 四个来源里是否**至少有一个**带了 workspace（上游 `resolveWorkspaceID` 返回 `""`）。
fn workspace_hinted(headers: &HeaderMap, query: &WorkspaceQuery) -> bool {
    header_str(headers, WORKSPACE_ID_HEADER).is_some_and(|v| !v.trim().is_empty())
        || header_str(headers, WORKSPACE_SLUG_HEADER).is_some_and(|v| !v.trim().is_empty())
        || query
            .workspace_id
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
        || query
            .workspace_slug
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
}

/// `None` = 字段没带；`Err(())` = 带了但不是 uuid。
///
/// 🔴 刻意**不**返回 `Response`：那会让 `Err` 变体 128 字节（`clippy::result_large_err`）
/// ⇒ 状态码与文案由调用点决定（畸形 ⇒ 400，不在库里 ⇒ 403，两条不同）。
fn optional_uuid(raw: Option<&str>) -> Result<Option<Uuid>, ()> {
    match raw {
        None => Ok(None),
        Some(r) => Uuid::parse_str(r).map(Some).map_err(|_| ()),
    }
}

// ---------------------------------------------------------------------------
// DB 小查询
// ---------------------------------------------------------------------------

async fn is_workspace_member(state: &AppState, ws: Id, user: Id) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2")
        .bind(ws.0)
        .bind(user.0)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn issue_in_workspace(state: &AppState, ws: Id, issue: Uuid) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT 1 FROM issue WHERE id = $1 AND workspace_id = $2")
        .bind(issue)
        .bind(ws.0)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten()
        .is_some()
}

/// 墓碑（软删）评论不接受附件（上游 `file.go:474-479` 逐字：`deleted_at` 有效即拒）。
async fn comment_attachable(state: &AppState, ws: Id, comment: Uuid) -> bool {
    sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM comment WHERE id = $1 AND workspace_id = $2 AND deleted_at IS NULL",
    )
    .bind(comment)
    .bind(ws.0)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten()
    .is_some()
}

// ---------------------------------------------------------------------------
// 响应小工具
// ---------------------------------------------------------------------------

/// 本仓 `mc_errors::Error` **没有** 400/403/413/415 的专用变体 ⇒ 用
/// `ApiError::respond_with` 指定状态码（与 M10-B1 的 `status_error` 同款判例）。
/// **状态码与文案逐字对齐上游**；错误体里的 `code` 会是 `validation_error`（已登记偏离）。
pub(crate) fn status_error(status: StatusCode, message: &str) -> Response {
    ApiError(Error::Validation {
        message: message.to_owned(),
        details: vec![],
    })
    .respond_with(status)
}

// 本片的证据面（门 ⑤ 零库 + 门 ⑥ 真库）。
#[cfg(test)]
#[path = "uploads/tests.rs"]
mod tests;
