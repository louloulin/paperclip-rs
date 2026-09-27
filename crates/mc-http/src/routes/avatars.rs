//! `GET /api/avatars/{sig}/*` —— **签名头像读取**（写者 M10-B4 / `LUM-2115` /
//! `docs/64` §4.2 第 4 行、`§6.5` 的 M10-B4 行）。
//!
//! 上游来源（pin `f41fae6b08fb`）：`server/cmd/server/router.go:1456`（条件注册在**会话
//! 中间件之外**）→ `server/internal/handler/avatar.go:398` `ServeAvatar`。
//!
//! ## 为什么这条路由是**公开**的（上游注释逐字）
//!
//! `auth cookie` 是 `SameSite=Strict`，所以「带鉴权的头像 URL」在 Desktop / 移动
//! webview（token 鉴权、非 API 文档源）与任何分源自托管 web 应用里**都加载不了**。
//! ⇒ **路径里的 HMAC 签名就是凭据**。这与 `CloudFront` 签名 URL 是同一套推理，
//! 并且**严格更紧**于本地存储后端 `GET /uploads/*`（那条按 key 直发、完全无鉴权）。
//!
//! ## 三样东西共同界定了「一条签名头像 URL 能reach什么」
//!
//! 1. **HMAC 覆盖 storage key** ⇒ 调用方无法为服务器从未作为头像发布过的 key 签名；
//! 2. **只有图片扩展名会解析**（[`avatar_content_type`]，8 项白名单，**故意没有 SVG**）；
//! 3. **对象必须是 avatar-class** —— 一份**未**挂到 issue / 评论 / chat 会话 /
//!    chat 消息 / task 上的独立图片上传（[`AvatarAccess::is_publishable`]）。
//!
//! 第 3 条是**授权边界**而不是体检项：能说出某个 storage 对象的名字**不等于**有权发布它。
//! 上游在**写侧**（`acceptAvatarURL`）与**读侧**（每次请求）**两侧**都查 —— 读侧那一遍
//! 才是让「这次检查存在之前写进去的行」也被收住的东西，并且对象日后被挂到评论上时
//! URL 立刻失效。
//!
//! ## 403 还是 404？（**与切片描述不一致，以逐字上游为准**）
//!
//! 切片描述写「未签名 ⇒ 403」；上游 `ServeAvatar` 对**四种**失败（签名错、未知 key、
//! 非图片 key、不是 avatar-class）**一律** `http.NotFound` —— 注释逐字写明这是为了
//! 「让这条路由不是任何一种情况的 oracle」。本仓照搬 ⇒ 全部 404。已登记 `docs/32` §9.23。
//!
//! ## 路径穿越三层判据（与 M10-B2 的 `uploads/serve.rs` 同一套，逐字复用）
//!
//! | 反例 | 判据 | 层 |
//! |---|---|---|
//! | `..`（含 `%2e%2e` 解码后） | [`guard_static_key`] → `DotDot` | 纯函数，读盘之前 |
//! | 绝对路径（`/etc/passwd`、`C:\`） | [`guard_static_key`] → `Absolute` | 纯函数 |
//! | **符号链接逃逸** | [`resolve_under`] 的包含性判定 | 读盘之前 |
//!
//! ## 形态
//!
//! 上游是 plain 的 `r.Get`（`router.go:1456`，不在任何 `Route(...)` 里）⇒ **只注册
//! 无尾斜杠形态**。`*` 用 matchit 0.7 的具名 catch-all（`*key`）：无名的 `*` 会被
//! `InsertError::ParamNameMissing` 拒绝（`scripts/route_parity.py::normalize` 的注释逐字）。
//! 取值走 [`Uri`] 而不是路径参数（与 `uploads/serve.rs` 同款判例：签名与 key 都要
//! **我们自己**做一次百分号解码，不能让匹配器先解一遍）。

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::routes::attachments::download::set_preview_security_headers;
use crate::routes::uploads::serve::{guard_static_key, not_found, percent_decode, resolve_under};
use crate::routes::uploads::{local_root, status_error, UPLOADS_BUCKET};
use crate::state::AppState;

/// 本文件 router：**1 条**注册键。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/avatars/:sig/*key", get(serve_avatar))
}

// ---------------------------------------------------------------------------
// 常量（上游 `avatar.go` 逐字）
// ---------------------------------------------------------------------------

/// 上游 `avatarURLPathPrefix`。注册键写 `:sig/*key`（matchit 语法），取值前缀用这一条。
pub const AVATAR_URL_PATH_PREFIX: &str = "/api/avatars/";

/// 上游 `avatarRedirectMaxAgeCap`。
pub const AVATAR_REDIRECT_MAX_AGE_CAP: i64 = 60;

/// 上游 `avatarProxyMaxAge`：代理体（本地后端走的那一支）的缓存秒数。
///
/// 头像是**按 key 不可变**的（每次上传都铸一个新的 uuidv7 key）⇒ 这个值只由
/// 「重新上传该多快生效」来封顶。
pub const AVATAR_PROXY_MAX_AGE: i64 = 300;

/// 上游 `workspaceUploadNamespace`。
pub const WORKSPACE_UPLOAD_NAMESPACE: &str = "workspaces/";

/// 上游 `defaultJWTSecret`（`internal/auth/jwt.go:14`）：`JWT_SECRET` 未配置时的回落值。
pub const DEFAULT_JWT_SECRET: &str = "multica-dev-secret-change-in-production";

/// 上游 `avatarExtContentTypes`（**故意没有 SVG**）。
///
/// SVG 缺席的理由逐字：代理模式下正文由 API 源直出，而内联的 `image/svg+xml` 文档在
/// 导航时是脚本执行面 —— 附件代理靠「强制下载」躲开它，而头像不能既强制下载又渲染。
/// SVG 头像保留它原本（修好之前）的行为：裸 storage URL，在每个公开桶的部署上都可用。
pub const AVATAR_EXT_CONTENT_TYPES: [(&str, &str); 8] = [
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".gif", "image/gif"),
    (".webp", "image/webp"),
    (".avif", "image/avif"),
    (".bmp", "image/bmp"),
    (".ico", "image/x-icon"),
];

// ---------------------------------------------------------------------------
// GET /api/avatars/{sig}/*
// ---------------------------------------------------------------------------

/// `GET /api/avatars/{sig}/*`（上游 `avatar.go:398` `ServeAvatar`）。
///
/// **公开路由**（无认证）：签名即凭据（见模块头）。
///
/// 顺序逐字照上游：**先**判扩展名白名单（它必须在碰存储之前决定），
/// **再**验签名，**再**判 avatar-class，**最后**才读对象。
pub async fn serve_avatar(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    // 上游 `h.Storage == nil` ⇒ 404（不是 403）：没有存储后端就没有任何可签的东西。
    let Some((sig, raw_key)) = split_avatar_path(uri.path()) else {
        return not_found();
    };
    let Some(key) = percent_decode(raw_key) else {
        return not_found();
    };
    // 扩展名白名单 —— 上游注释逐字：「键是这条端点掌握的全部信息，
    // 它必须在碰存储之前就决定」。
    if avatar_content_type(&key).is_none() {
        return not_found();
    }
    // 路径穿越三层判据的两条**纯函数**层（第三条符号链接在下面读盘之前）。
    if guard_static_key(&key).is_err() {
        return not_found();
    }
    if !avatar_key_signature_valid(&key, &sig) {
        return not_found();
    }
    if !AvatarAccess::new(&state).is_publishable(&key).await {
        return not_found();
    }
    // 符号链接逃逸：本地后端才有根目录；解析完符号链接后判「仍在根之下」。
    // 文件不存在时 `canonicalize` 失败 ⇒ 与上游的 `http.ServeFile` 一样是 404。
    if let Some(root) = local_root(&state) {
        if resolve_under(&root, &key).await.is_none() {
            return not_found();
        }
    }

    let body = match state.storage.get(UPLOADS_BUCKET, &key).await {
        Ok(b) => b,
        Err(mc_storage::StorageError::NotFound(_)) => return not_found(),
        Err(e) => {
            tracing::warn!(error = %e, key = %key, "avatar object read failed");
            return bad_gateway();
        }
    };

    let content_type = avatar_content_type(&key).unwrap_or("application/octet-stream");
    let len = body.len();
    let mut resp = Response::new(axum::body::Body::from(body));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(content_type)
            .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(v) = header::HeaderValue::from_str(&len.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    // 上游 `proxyAvatar` 逐字：`inline`（否则 `<img>` 会被当成下载触发）+
    // `nosniff`（不让浏览器猜类型）。
    h.insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_static("inline"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    set_avatar_cache_control(h, AVATAR_PROXY_MAX_AGE);
    // 上游 `h.setAttachmentPreviewSecurityHeaders(w)`：与认证下载端点同一套 CSP。
    set_preview_security_headers(h);
    resp
}

/// 切出 `(<sig>, <未解码的键>)`。
///
/// 形状是 `{sig}/{键…}`：**第一个** `/` 之后全是键（键里可以有 `/`）。
/// `sig` 是 base64url，**不含** `/`，所以第一个 `/` 就是分界。
pub fn split_avatar_path(path: &str) -> Option<(String, &str)> {
    let rest = path.strip_prefix(AVATAR_URL_PATH_PREFIX)?;
    let (sig, key) = rest.split_once('/')?;
    if sig.is_empty() || key.is_empty() {
        return None;
    }
    Some((sig.to_owned(), key))
}

/// 上游 `avatarContentType`：键的**扩展名**决定类型（大小写不敏感），
/// 不在白名单里返回 `None`（⇒ 上游的 `""`）。
pub fn avatar_content_type(key: &str) -> Option<&'static str> {
    let ext = key.rsplit_once('.').map(|(_, e)| e)?;
    let lower = format!(".{}", ext.to_ascii_lowercase());
    // 键里最后一个 `.` 之后若还带 `/`（例如 `a.png/b`），那不是扩展名。
    if lower.contains('/') {
        return None;
    }
    AVATAR_EXT_CONTENT_TYPES
        .iter()
        .find(|(k, _)| *k == lower)
        .map(|(_, v)| *v)
}

// ---------------------------------------------------------------------------
// 签名（上游 `avatarURLSigningKey` / `signAvatarKey` / `avatarKeySignatureValid`）
// ---------------------------------------------------------------------------

/// 上游 `avatarURLSigningKey`：从 `JWT_SECRET` 派生一个**头像专用** HMAC 键
/// （SHA-256），使这个签名域与会话令牌**永不**共享同一把键。
///
/// 上游用 `sync.Once` 缓存；本地用 [`OnceLock`] 同款。注意本地每次进程内求值一次，
/// 因此**测试里改 env 不生效** —— 与上游逐字同一条限制。
pub fn avatar_signing_key() -> &'static [u8; 32] {
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        let secret = std::env::var("JWT_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_JWT_SECRET.to_owned());
        Sha256::new()
            .chain_update(b"avatar-url:")
            .chain_update(secret.as_bytes())
            .finalize()
            .into()
    })
}

/// 上游 `signAvatarKey`：HMAC-SHA256(key)，**base64 raw-url** 编码。
pub fn sign_avatar_key(key: &str) -> String {
    base64_url_nopad(&avatar_mac(key).finalize().into_bytes())
}

/// 上游 `avatarKeySignatureValid`：**常量时间**比较（上游 `hmac.Equal`）。
///
/// ⚠️ 比的是 **base64 字符串的字节**，不是解码后的 32 字节 —— 上游逐字就是
/// `hmac.Equal([]byte(sig), []byte(signAvatarKey(key)))`，即两个 base64url 串。
/// 所以这里**不能**用 `Mac::verify_slice`（那条路吃的是原始 tag 字节，形状不同）。
pub fn avatar_key_signature_valid(key: &str, sig: &str) -> bool {
    let expected = sign_avatar_key(key);
    constant_time_eq(expected.as_bytes(), sig.as_bytes())
}

/// 定长常量时间比较（上游 `hmac.Equal` 的语义）。
///
/// 长度不等直接 false（与上游 `subtle.ConstantTimeEq` 的 `Eq` impl 一致：长度是
/// 公开信息，base64url 输出恒 43 字符）；长度相等时逐字节累积差异，**不**提前返回。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn avatar_mac(key: &str) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(avatar_signing_key())
        .expect("HMAC accepts keys of any length");
    mac.update(key.as_bytes());
    mac
}

/// base64 的 **URL-safe、 无 padding** 变体（上游 `base64.RawURLEncoding`）。
fn base64_url_nopad(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------
// avatar-class 授权边界（上游 `avatarKeyIsPublishable` + `attachmentIsBound`）
// ---------------------------------------------------------------------------

/// 上游 `avatarKeyIsPublishable` / `attachmentIsBound` 的本仓形态。
pub struct AvatarAccess<'a> {
    state: &'a AppState,
}

impl<'a> AvatarAccess<'a> {
    #[must_use]
    pub fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    /// 这个 storage 对象可否经公开头像端点伺服（上游 `avatarKeyIsPublishable`）。
    ///
    /// - 非图片扩展名 ⇒ 拒；
    /// - **不在** `workspaces/` 命名空间 ⇒ 放行：上游注释逐字 —— `UploadFile` 只写
    ///   `workspaces/…` 或 `users/<uploader>/…`，而 per-user 分支**不造任何绑定**；
    ///   别的前缀是运维自己放进桶里的对象，他有权把头像指过去；
    /// - 在 `workspaces/` 里但**不是**上传行（渠道媒体 ingest 也写
    ///   `workspaces/<ws>/lark/…`）⇒ **fail closed**。
    ///
    /// 键的 basename 就是 attachment id（上游 `UploadFile` 一个 `UUIDv7` 同时当行 id
    /// 与对象文件名）⇒ 不必按 URL 反查，也不需要在它上面建索引。
    pub async fn is_publishable(&self, key: &str) -> bool {
        if avatar_content_type(key).is_none() {
            return false;
        }
        if !key.starts_with(WORKSPACE_UPLOAD_NAMESPACE) {
            return true;
        }
        let Some(att_id) = attachment_id_from_storage_key(key) else {
            return false;
        };
        let repo = mc_repos::attachment::AttachmentRepo::new(&self.state.db);
        // `get_by_id_only` 故意**不带** workspace 条件（逐字对应上游
        // `GetAttachmentByIDOnly`）⇒ 授权中性，成员门由调用链自己保证。
        let Ok(att) = repo.get_by_id_only(att_id).await else {
            return false;
        };
        att.content_type.to_ascii_lowercase().starts_with("image/") && !attachment_is_bound(&att)
    }
}

/// 上游 `attachmentIsBound`：这份上传挂在工作区内容上 ⇒ 它是别人的 issue/评论/chat
/// 文件，**永远**不是头像。
#[must_use]
pub fn attachment_is_bound(att: &mc_repos::attachment::AttachmentRow) -> bool {
    att.issue_id.is_some()
        || att.comment_id.is_some()
        || att.chat_session_id.is_some()
        || att.chat_message_id.is_some()
        || att.task_id.is_some()
}

/// 上游 `attachmentIDFromStorageKey`：从 `<prefix>/<uuid><ext>` 里取回 attachment id。
#[must_use]
pub fn attachment_id_from_storage_key(key: &str) -> Option<mc_core::Id> {
    let base = key.rsplit('/').next()?;
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
    mc_core::Id::parse(stem).ok()
}

/// 上游 `setAvatarCacheControl`：响应标记 **private**（URL 不可猜，但它仍是
/// 部署级的身份图像，共享代理没有理由留副本）。
///
/// `max_age <= 0` ⇒ `no-store` 而不是「存 0 秒」——上游注释逐字：有些中间件会把后者
/// 向上取整。
pub fn set_avatar_cache_control(h: &mut header::HeaderMap, max_age: i64) {
    let value = if max_age <= 0 {
        "no-store".to_owned()
    } else {
        format!("private, max-age={max_age}")
    };
    if let Ok(v) = header::HeaderValue::from_str(&value) {
        h.insert(header::CACHE_CONTROL, v);
    }
}

/// 上游 `proxyAvatar` 唯一的非 404 失败：对象读不出来（端点不健康）⇒ 502。
///
/// 文案逐字取上游 `writeError(w, http.StatusBadGateway, "failed to create avatar URL")`。
fn bad_gateway() -> Response {
    status_error(StatusCode::BAD_GATEWAY, ERR_AVATAR_UNAVAILABLE)
}

/// 上游的 502 文案。
pub const ERR_AVATAR_UNAVAILABLE: &str = "failed to create avatar URL";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sig_and_key_at_first_slash() {
        let (sig, key) = split_avatar_path("/api/avatars/AAAA/workspaces/w1/a.png").expect("split");
        assert_eq!(sig, "AAAA");
        assert_eq!(key, "workspaces/w1/a.png");
        // 键里可以再带斜杠。
        let (_, key) = split_avatar_path("/api/avatars/AAAA/users/u/x/y.PNG").expect("split");
        assert_eq!(key, "users/u/x/y.PNG");
        // 前缀不对 / 缺段 ⇒ None。
        assert!(split_avatar_path("/uploads/AAAA/x.png").is_none());
        assert!(split_avatar_path("/api/avatars/AAAA").is_none());
        assert!(split_avatar_path("/api/avatars/AAAA/").is_none());
        assert!(split_avatar_path("/api/avatars//x.png").is_none());
    }

    /// 白名单 8 项、大小写不敏感、**SVG 缺席**（模块头的理由）。
    #[test]
    fn content_type_allowlist_rejects_svg_and_non_images() {
        assert_eq!(avatar_content_type("a/b/c.PNG"), Some("image/png"));
        assert_eq!(avatar_content_type("x.jpeg"), Some("image/jpeg"));
        assert_eq!(avatar_content_type("x.ico"), Some("image/x-icon"));
        assert_eq!(avatar_content_type("x.svg"), None);
        assert_eq!(avatar_content_type("x.txt"), None);
        assert_eq!(avatar_content_type("noext"), None);
        // 「最后一个 . 之后仍含 /」⇒ 那不是扩展名。
        assert_eq!(avatar_content_type("a.png/b"), None);
    }

    /// 签名：确定、对 key 敏感、base64url 无 padding、验签恒等时间路径。
    #[test]
    fn signature_round_trips_and_is_key_bound() {
        let key = "workspaces/w1/8f14e45f.png";
        let sig = sign_avatar_key(key);
        assert!(!sig.contains('='), "raw-url 无 padding: {sig}");
        assert!(!sig.contains('+') && !sig.contains('/'), "url-safe: {sig}");
        assert_eq!(sig.len(), 43, "sha256 → 32 字节 → 43 个 base64url 字符");
        assert!(avatar_key_signature_valid(key, &sig));
        // 换 key ⇒ 签名不成立。
        assert!(!avatar_key_signature_valid("workspaces/w1/other.png", &sig));
        // 换签名（等长）⇒ 不成立。
        let mut bad = sig.clone();
        bad.replace_range(0..1, if sig.starts_with('A') { "B" } else { "A" });
        assert!(!avatar_key_signature_valid(key, &bad));
        // 长度不同 ⇒ 直接拒。
        assert!(!avatar_key_signature_valid(key, "short"));
    }

    /// 签名域与「原始 JWT 密钥」不是同一把键（上游用 `sha256("avatar-url:"+secret)`）。
    #[test]
    fn signing_key_is_domain_separated() {
        let key = avatar_signing_key();
        assert_eq!(key.len(), 32);
        let expected: [u8; 32] = Sha256::new()
            .chain_update(b"avatar-url:")
            .chain_update(DEFAULT_JWT_SECRET.as_bytes())
            .finalize()
            .into();
        assert_eq!(*key, expected);
    }

    /// 键 → attachment id：basename 去扩展名必须是 uuid，否则 None（⇒ fail closed）。
    #[test]
    fn attachment_id_recovered_from_object_filename() {
        let id = mc_core::Id::new();
        let key = format!("workspaces/w1/{id}.png");
        assert_eq!(attachment_id_from_storage_key(&key), Some(id));
        assert_eq!(
            attachment_id_from_storage_key("workspaces/w1/not-a-uuid.png"),
            None
        );
        assert_eq!(
            attachment_id_from_storage_key("workspaces/w1/lark/media"),
            None
        );
        assert_eq!(attachment_id_from_storage_key("workspaces/w1/noext"), None);
    }

    /// `attachment_is_bound`：五个绑定面任意一个非空即为真。
    #[test]
    fn bound_covers_all_five_owner_columns() {
        use mc_repos::attachment::AttachmentRow;
        use uuid::Uuid;
        let base = AttachmentRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            issue_id: None,
            comment_id: None,
            uploader_type: "user".into(),
            uploader_id: Uuid::new_v4(),
            filename: "a.png".into(),
            url: "/uploads/a.png".into(),
            content_type: "image/png".into(),
            size_bytes: 1,
            created_at: chrono::Utc::now(),
            chat_session_id: None,
            chat_message_id: None,
            task_id: None,
            source_context_id: None,
        };
        assert!(!attachment_is_bound(&base));
        for column in 0..5 {
            let mut row = AttachmentRow {
                id: base.id,
                workspace_id: base.workspace_id,
                issue_id: base.issue_id,
                comment_id: base.comment_id,
                uploader_type: base.uploader_type.clone(),
                uploader_id: base.uploader_id,
                filename: base.filename.clone(),
                url: base.url.clone(),
                content_type: base.content_type.clone(),
                size_bytes: base.size_bytes,
                created_at: base.created_at,
                chat_session_id: base.chat_session_id,
                chat_message_id: base.chat_message_id,
                task_id: base.task_id,
                source_context_id: base.source_context_id,
            };
            let v = Some(Uuid::new_v4());
            match column {
                0 => row.issue_id = v,
                1 => row.comment_id = v,
                2 => row.chat_session_id = v,
                3 => row.chat_message_id = v,
                _ => row.task_id = v,
            }
            assert!(attachment_is_bound(&row), "column {column} 必须算已绑定");
        }
    }

    /// `Cache-Control`：`no-store` 与 `private, max-age=N` 两支（上游逐字）。
    #[test]
    fn cache_control_is_private() {
        let mut h = header::HeaderMap::new();
        set_avatar_cache_control(&mut h, 0);
        assert_eq!(h.get(header::CACHE_CONTROL).expect("cc"), "no-store");
        set_avatar_cache_control(&mut h, AVATAR_PROXY_MAX_AGE);
        assert_eq!(
            h.get(header::CACHE_CONTROL).expect("cc"),
            "private, max-age=300"
        );
        assert_eq!(AVATAR_REDIRECT_MAX_AGE_CAP, 60);
    }

    /// 路由级：**签名不对** ⇒ **404**（不是 403 —— 上游的「不泄露 oracle」形状）。
    ///
    /// 零库：签名那一层在碰存储与库**之前**就拒。
    #[tokio::test]
    async fn route_rejects_a_forged_signature_with_404() {
        use std::sync::Arc as StdArc;

        use axum::body::Body as AxumBody;
        use axum::http::Request as HttpRequest;
        use tower::ServiceExt as _;

        use crate::routes::ws::test_support::{lazy_db, state_with};
        let state = state_with(
            lazy_db(),
            mc_storage::Storage::new(),
            StdArc::new(mc_ws::hub::Hub::new()),
        );
        let app = crate::apply_default_middleware(router()).with_state(state);
        let uri = format!("/api/avatars/{}/workspaces/w1/a.png", "A".repeat(43));
        let resp = app
            .oneshot(HttpRequest::get(&uri).body(AxumBody::empty()).expect("req"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// 路由级：非白名单扩展名同样 404（且**先于**签名判定）。
    #[tokio::test]
    async fn route_rejects_non_image_keys() {
        use std::sync::Arc as StdArc;

        use axum::body::Body as AxumBody;
        use axum::http::Request as HttpRequest;
        use tower::ServiceExt as _;

        use crate::routes::ws::test_support::{lazy_db, state_with};
        let state = state_with(
            lazy_db(),
            mc_storage::Storage::new(),
            StdArc::new(mc_ws::hub::Hub::new()),
        );
        let app = crate::apply_default_middleware(router()).with_state(state);
        // 签名**正确**（由本仓自己的 `sign_avatar_key` 铸）但类型不在白名单 ⇒ 仍 404。
        let sig = sign_avatar_key("workspaces/w1/a.svg");
        let uri = format!("/api/avatars/{sig}/workspaces/w1/a.svg");
        let resp = app
            .oneshot(HttpRequest::get(&uri).body(AxumBody::empty()).expect("req"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
