//! `GET /uploads/*` 的**静态分发**那一半（**写者 M10-B2** / `LUM-2113`）。
//!
//! 上游来源（pin `f41fae6b08fb`）：`server/cmd/server/router.go:1435`（条件注册）
//! → `server/internal/handler/file.go:962` `ServeLocalUpload`
//! → `server/internal/storage/local.go` `ServeFile` / `isInternalLocalPath` / `isUnder`。
//!
//! ## 为什么单独一个文件
//!
//! 切片描述的写集写明「若超 700 行按『上传 / 静态分发』拆两文件」—— 本片就是那次拆分：
//! `uploads.rs` 装**上传**那一半（本文件之外的 `upload_file` / `read_form` / 嗅探），
//! 本文件装**静态分发**那一半。写集因此是**两个**新文件（见 `docs/32` §9.21 登记）。
//!
//! ## 三条路径穿越反例各自在哪一层被拒（**这一层不含任何字符串以外的信息**）
//!
//! | 反例 | 拦它的判据 | 位置 |
//! |---|---|---|
//! | `..` 段（含 `%2e%2e` 解码后） | [`guard_static_key`] 的 `DotDot` | 本文件，纯函数 |
//! | 绝对路径（`/etc/passwd`、`C:\…`） | [`guard_static_key`] 的 `Absolute` | 本文件，纯函数 |
//! | **符号链接逃逸** | [`resolve_under`] 的包含性判定 | 本文件，读盘**之前** |
//!
//! 🔴 第三条**只有** [`resolve_under`] 拦得住：链接的键是干净的（`escape.txt`），
//! `mc_storage::validate_key` 与 [`guard_static_key`] 都放行 ⇒ 必须解析符号链接之后
//! 再判「仍在上传根之下」⇒ 这也是本片要给 `mc-storage` 追加 `local_root()` 的原因。
//!
//! 三者对外**都是 404**（上游对三段都是 `http.NotFound`，见 `uploads.rs` 模块头第 3 条）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::Response;
use axum::routing::get;
use axum::Router;

use mc_errors::Error;

use crate::error::ApiError;
use crate::routes::attachments::download::set_preview_security_headers;
use crate::routes::uploads::{
    content_type_for, local_root, status_error, ERR_UPLOAD_FAILED, META_SUFFIX, STATIC_PREFIX,
    TEMP_SUFFIX, UPLOADS_BUCKET,
};
use crate::state::AppState;

/// 本文件的 router：**1 条**注册键。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/uploads/*key", get(serve_local_upload))
}

// ---------------------------------------------------------------------------
// GET /uploads/*
// ---------------------------------------------------------------------------

/// `GET /uploads/*`（上游 `file.go:962` `ServeLocalUpload`）。
///
/// 公开路由（**无认证**）：上游注释逐字 —— 本地部署下前端用 iframe 内联预览 PDF/HTML，
/// 所以它与 `/api/attachments/{id}/download` 带**同一套**预览安全响应头。
///
/// 取值走 [`Uri`] 而不是路径参数：注册键必须逐字是 `/uploads/*`（见模块头），
/// 而 matchit 0.7 的无名 catch-all **不产生可用的参数名**。
pub async fn serve_local_upload(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let Some(root) = local_root(&state) else {
        return not_found();
    };
    let Some(raw) = uri.path().strip_prefix(STATIC_PREFIX) else {
        return not_found();
    };
    let Some(key) = percent_decode(raw) else {
        return not_found();
    };
    if guard_static_key(&key).is_err() {
        return not_found();
    }
    // 符号链接逃逸：把**最终路径**（解析完符号链接）拿回来，判它是否仍在根之下。
    // 文件不存在时 `canonicalize` 失败 ⇒ 与上游的 `http.ServeFile` 一样是 404。
    if resolve_under(&root, &key).await.is_none() {
        return not_found();
    }

    let body = match state.storage.get(UPLOADS_BUCKET, &key).await {
        Ok(b) => b,
        Err(mc_storage::StorageError::NotFound(_)) => return not_found(),
        Err(e) => {
            tracing::warn!(error = %e, key = %key, "local upload read failed");
            return status_error(StatusCode::BAD_GATEWAY, ERR_UPLOAD_FAILED);
        }
    };

    let content_type = content_type_for(&key, &body);
    let len = body.len();
    let mut resp = Response::new(axum::body::Body::from(body));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(&content_type)
            .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(v) = header::HeaderValue::from_str(&len.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    // 上游 `file.go:971` 逐字：与认证下载端点同一套预览头。
    set_preview_security_headers(h);
    resp
}

/// 静态分发键的三道门 + 内部路径拒绝（上游 `storage/local.go::ServeFile` 的
/// `isInternalLocalPath` + `isUnder` 两道，逐字展开成**纯函数**便于单测）。
///
/// 三条路径穿越反例各自命中一条：
/// - `..` 段 → [`KeyError::DotDot`]；
/// - 绝对路径（`/etc/passwd`、Windows 盘符）→ [`KeyError::Absolute`]；
/// - 符号链接逃逸 → 本函数放行，由 [`resolve_under`] 的包含性判定兜住。
#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    Empty,
    DotDot,
    Absolute,
    /// 内部路径：sidecar（`.meta.json`）或暂存文件（`.tmp`）—— 上游逐字「不让它们
    /// 变成一个稳定的读 API」。
    Internal,
    BadPercent,
}

/// 键的**字符串层**判据（本仓 `mc_storage::validate_key` 的超集：它只拒 `..` 与
/// 绝对路径，这里再加上上游的内部路径拒绝与空段拒绝）。
pub fn guard_static_key(key: &str) -> Result<(), KeyError> {
    if key.is_empty() {
        return Err(KeyError::Empty);
    }
    if key.starts_with('/') || key.starts_with('\\') || has_drive_letter(key) {
        return Err(KeyError::Absolute);
    }
    if key.contains('\0') {
        return Err(KeyError::BadPercent);
    }
    let base = key.rsplit('/').next().unwrap_or(key);
    if key.ends_with(META_SUFFIX) || (base.starts_with('.') && base.ends_with(TEMP_SUFFIX)) {
        return Err(KeyError::Internal);
    }
    for seg in key.split('/') {
        match seg {
            "" => return Err(KeyError::Empty),
            // `..` 与 `.` 都让 `join` 逃出当前目录（`.` 不改变目录，但同样不是合法键）。
            ".." | "." => return Err(KeyError::DotDot),
            _ => {}
        }
    }
    Ok(())
}

/// 解析 `<root>/<桶>/<键>` 并判**包含性**（解析符号链接之后）。
///
/// `None` = 404 的三种成因之一：解析失败（不存在 / 权限）、或**逃出根目录**。
pub async fn resolve_under(root: &Path, key: &str) -> Option<PathBuf> {
    let root = tokio::fs::canonicalize(root).await.ok()?;
    let candidate = root.join(UPLOADS_BUCKET).join(key);
    let resolved = tokio::fs::canonicalize(&candidate).await.ok()?;
    if resolved.starts_with(&root) {
        Some(resolved)
    } else {
        None
    }
}

pub fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = hex_val(*bytes.get(i + 1)?)?;
            let lo = hex_val(*bytes.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn has_drive_letter(key: &str) -> bool {
    let b = key.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// 静态分发面的 404（上游 `http.NotFound`；上游那三段文案都是框架自带的，
/// 本仓统一成 `resource: "upload"` 的 404 体）。
pub fn not_found() -> Response {
    ApiError(Error::NotFound {
        resource: "upload".into(),
    })
    .respond_with(StatusCode::NOT_FOUND)
}
