//! M10-B2 的证据面（`docs/64` §6.5 的通用 `DoD` 第 5 条：每条路由至少一条用例）。
//!
//! 分两半，**判据不同**（与 `routes/attachments/tests.rs` 同手法）：
//!
//! - **不碰库的那一半**（门 ⑤，本文件）：2 条键的**字面量**与上游
//!   `upstream-routes.tsv` 逐字相等、router 真装得上、**无会话 401**、没配存储
//!   **403**、畸形 / 缺字段的 multipart **400**、静态分发面 200 + 字节 + 预览头、
//!   **路径穿越三条反例**（`..`、绝对路径、符号链接逃逸）**404**、内部路径
//!   （`.meta.json` / `.tmp`）**404**、**非本地 provider 时该路由不挂载**；
//! - **纯函数那一层**（门 ⑤，`pure.rs`）：大小上限、扩展名覆盖、魔数嗅探、百分号
//!   解码、键守卫 —— 都不需要磁盘也不需要库，可以精确到字节。
//! - **真库的那一半**（门 ⑥，`db.rs`，`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）：
//!   带 workspace 的上传落行、非成员 **403**、跨 workspace 的 `issue_id` **403**、
//!   `task_id` / `chat_session_id` fail-closed **403**、无 workspace 分支**不写行**、
//!   以及「上传完立刻能从 `/uploads/*` 取回同样字节」的端到端。
//!
//! 🔴 **本片零出站**（两条全是本地路由）⇒「替身纪律」的对应物是**零出站**。

#[path = "tests/db.rs"]
mod db;
#[path = "tests/fx.rs"]
mod fx;
#[path = "tests/pure.rs"]
mod pure;
#[path = "tests/support.rs"]
mod support;

use serde_json::Value;

use super::{
    capped_push, content_type_for, storage_filename, FormError, KeyError, ERR_FORM_INVALID,
    ERR_MISSING_FILE, ERR_NOT_CONFIGURED, ERR_NOT_MEMBER, ERR_TASK_UNSUPPORTED, MAX_UPLOAD_SIZE,
    STATIC_PREFIX,
};
use support::*;

/// 本片 2 条（method, 上游字面量）—— 与 `docs/fixtures/upstream-routes.tsv` 的
/// `M3+` 段里那两行逐字相等。
pub(crate) const TWO: [(&str, &str); 2] = [("POST", "/api/upload-file"), ("GET", "/uploads/*")];

// --------------------------------------------------------------------------- //
// 形态门
// --------------------------------------------------------------------------- //

#[test]
fn two_local_literals_are_the_upstream_literals() {
    let tsv = include_str!("../../../../../docs/fixtures/upstream-routes.tsv");
    for (method, path) in TWO {
        let want = format!("{method}\t{path}\tM3+");
        assert!(
            tsv.lines().any(|l| l.starts_with(&want)),
            "TSV 缺 {want}（本片的 2 条必须逐字等于上游登记）"
        );
    }
    // `docs/64` §4.2 的账：B2 = 2 行、**2 个缺口**（没有占位升级）。
    assert_eq!(TWO.len(), 2);
}

#[tokio::test]
async fn router_builds_without_duplicate_registration() {
    // 同 path+method 重复注册 ⇒ axum **启动时 panic**（`docs/15` §9.6.6）。
    let dir = TempDir::new("build");
    let _ = test_app(lazy_db(), dir.path());
}

#[tokio::test]
async fn no_trailing_slash_form_is_registered() {
    // 🔴 补尾斜杠 = `EXTRA_ALIAS`（本波 allowlist 0 数据行、没有豁免退路）。
    // 判据：真实 router + 尾斜杠 URI ⇒ 未注册时落 axum 的 404 兜底。
    let dir = TempDir::new("slash");
    let app = test_app(lazy_db(), dir.path());
    let (status, _, _) = call(&app, Call::authed("POST", "/api/upload-file/", new_uuid())).await;
    assert_eq!(status, 404, "尾斜杠形态被注册了 ⇒ EXTRA_ALIAS");
}

// --------------------------------------------------------------------------- //
// POST /api/upload-file（门 ⑤，零库）
// --------------------------------------------------------------------------- //

#[tokio::test]
async fn upload_requires_a_session() {
    let dir = TempDir::new("noauth");
    let app = test_app(lazy_db(), dir.path());
    let (status, _, body) = call(&app, Call::upload(&[Part::file("a.png", &PNG_1X1)])).await;
    assert_eq!(status, 401, "{body}");
}

#[tokio::test]
async fn upload_without_local_provider_is_403() {
    // 上游 `file.go:380-383` 的 `writeFeatureDisabled` ⇒ **403** + 那句文案。
    // 🔴 与 `/uploads/*` 不同：这条路由**照常挂载**（上游 `router.go:1633` 在主 router
    // 里，不受存储类型影响）⇒ 判据是 403 而不是 404。
    let app = test_app_without_storage(lazy_db());
    let (status, _, body) = call(
        &app,
        Call::upload(&[Part::file("a.png", &PNG_1X1)]).with_user(new_uuid()),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains(ERR_NOT_CONFIGURED),
        "文案必须是上游逐字的那句：{body}"
    );
}

#[tokio::test]
async fn upload_rejects_a_missing_file_field() {
    let dir = TempDir::new("nofile");
    let app = test_app(lazy_db(), dir.path());
    let (status, _, body) = call(
        &app,
        Call::upload(&[Part::text("issue_id", "x")]).with_user(new_uuid()),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains(ERR_MISSING_FILE), "{body}");
}

#[tokio::test]
async fn upload_rejects_a_malformed_multipart_body() {
    // 上游 `ParseMultipartForm` 的失败分支（`file.go:395-397`）⇒ 400 + 那句文案。
    //
    // ⚠️ 判据**只能**是「`content-type` 对、body 坏」：axum 0.7 的 `Multipart::from_request`
    // 对非 multipart 的 `content-type` 是 **panic**（"Invalid `boundary` for
    // `multipart/form-data` request"），不是 4xx ⇒ 「发一个 JSON body 上去」这种反例
    // 在本仓**测不了**（上游 Go 那边是 400，本仓是提取器 panic）。已登记 `docs/32` §9.21。
    let dir = TempDir::new("malformed");
    let app = test_app(lazy_db(), dir.path());
    let (status, _, body) = call(
        &app,
        Call::bare("POST", "/api/upload-file")
            .with_user(new_uuid())
            .with_body(b"--m10b2boundary\r\ngarbage-without-headers\r\n".to_vec()),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains(ERR_FORM_INVALID), "{body}");
}

#[tokio::test]
async fn upload_rejects_task_id_and_chat_session_id() {
    // 本仓**未实现**上游那两道门 ⇒ fail-closed 403（登记见 `docs/32` §9.21）。
    let dir = TempDir::new("taskgate");
    let app = test_app(lazy_db(), dir.path());
    let user = new_uuid();
    for (field, value) in [("task_id", new_uuid()), ("chat_session_id", new_uuid())] {
        let (status, _, body) = call(
            &app,
            Call::upload(&[
                Part::file("a.png", &PNG_1X1),
                Part::text(field, &value.to_string()),
            ])
            .with_user(user),
        )
        .await;
        assert_eq!(status, 403, "{field} 必须 fail-closed：{body}");
    }
    let (_, _, body) = call(
        &app,
        Call::upload(&[
            Part::file("a.png", &PNG_1X1),
            Part::text("task_id", &new_uuid().to_string()),
        ])
        .with_user(user),
    )
    .await;
    assert!(body.contains(ERR_TASK_UNSUPPORTED), "{body}");
}

#[tokio::test]
async fn membership_is_checked_before_the_issue_id_is_parsed() {
    // 上游 `file.go:450` 的成员门在 `issue_id` 解析（`:458`）**之前** ⇒ 非成员拿到的
    // 永远是 403，**不是** 400。零库那一半只能钉这个**顺序**（不可达库 ⇒ 成员查询失败
    // ⇒ 403）；「成员 + 畸形 id ⇒ 400」那一格在真库那一半（`db.rs`）。
    let dir = TempDir::new("order");
    let app = test_app(lazy_db(), dir.path());
    let (status, _, body) = call(
        &app,
        Call::upload(&[
            Part::file("a.png", &PNG_1X1),
            Part::text("issue_id", "not-a-uuid"),
        ])
        .with_user(new_uuid())
        .with_workspace(new_uuid()),
    )
    .await;
    assert_eq!(status, 403, "成员门必须先于 issue_id 解析：{body}");
    assert!(body.contains(ERR_NOT_MEMBER), "{body}");
    assert!(
        !dir.path().join("uploads").exists(),
        "成员校验之前不许落对象"
    );
}

// --------------------------------------------------------------------------- //
// GET /uploads/*（门 ⑤，零库）
// --------------------------------------------------------------------------- //

#[tokio::test]
async fn static_route_serves_the_object_verbatim() {
    let dir = TempDir::new("serve");
    dir.seed("workspaces/w1/a.png", &PNG_1X1);
    let app = test_app(lazy_db(), dir.path());
    let (status, headers, body) = call_bytes(
        &app,
        Call::anon("GET", format!("{STATIC_PREFIX}workspaces/w1/a.png")),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, PNG_1X1.to_vec(), "字节必须逐字不变");
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["content-length"], "67");
    // 上游 `file.go:971` 逐字：与认证下载端点同一套预览安全头。
    assert_eq!(
        headers["content-security-policy"], "default-src 'none'; frame-ancestors 'self'",
        "静态分发面必须带预览 CSP（上游 MUL-3821 / #4477 的全部意义就在这个头）"
    );
}

#[tokio::test]
async fn static_route_needs_no_session() {
    // 上游这条路由挂在**认证中间件之外**：原生 `<img>` / iframe 预览没有凭据可带。
    let dir = TempDir::new("noauth-static");
    dir.seed("users/u1/a.txt", b"hello");
    let app = test_app(lazy_db(), dir.path());
    let (status, _, body) = call(
        &app,
        Call::anon("GET", format!("{STATIC_PREFIX}users/u1/a.txt")),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("hello"), "{body}");
}

#[tokio::test]
async fn static_route_is_not_mounted_without_a_local_provider() {
    // 上游 `router.go:1434-1436` 逐字：只有 `storage.LocalStorage` 才注册这条路由。
    // 判据：同一个键在**有**本地 provider 时 200、在**没有**时 404（不是 403 / 501）。
    let dir = TempDir::new("nomount");
    dir.seed("users/u1/a.txt", b"hello");
    let uri = format!("{STATIC_PREFIX}users/u1/a.txt");
    let (mounted, _, _) = call_bytes(
        &test_app(lazy_db(), dir.path()),
        Call::anon("GET", uri.clone()),
    )
    .await;
    assert_eq!(mounted, 200);
    let (unmounted, _, _) =
        call_bytes(&test_app_without_storage(lazy_db()), Call::anon("GET", uri)).await;
    assert_eq!(unmounted, 404, "非本地 provider ⇒ 该路由不挂载（上游逐字）");
}

#[tokio::test]
async fn static_route_rejects_dot_dot_in_both_encodings() {
    let dir = TempDir::new("dotdot");
    // 根**之外**放一个诱饵文件：任何逃逸成功都会把它读出来。
    let outside = dir
        .path()
        .parent()
        .expect("parent")
        .join(format!("m10b2-secret-{}.txt", new_uuid().simple()));
    std::fs::write(&outside, b"TOP SECRET").expect("seed outside");
    let uri_escape = format!(
        "{STATIC_PREFIX}../{}",
        outside.file_name().expect("name").to_string_lossy()
    );
    dir.seed("users/u1/ok.txt", b"fine");
    let app = test_app(lazy_db(), dir.path());

    for uri in [
        uri_escape.clone(),
        // 百分号编码那一支才是真判据：键的**字符串**里没有 `..`，只有解码后才有。
        format!(
            "{STATIC_PREFIX}%2e%2e/{}",
            outside.file_name().expect("name").to_string_lossy()
        ),
    ] {
        let (status, _, bytes) = call_bytes(&app, Call::anon("GET", uri.clone())).await;
        let body = String::from_utf8_lossy(&bytes).into_owned();
        assert_eq!(status, 404, "{uri} 必须被拒：{body}");
        assert!(!body.contains("TOP SECRET"), "{uri} 读到了根外的文件");
    }
    // 同一前缀下的正常键仍然 200 ⇒ 证明拒的是「穿越」而不是「整条路由坏了」。
    let (status, _, _) = call_bytes(
        &app,
        Call::anon("GET", format!("{STATIC_PREFIX}users/u1/ok.txt")),
    )
    .await;
    assert_eq!(status, 200);
    let _ = std::fs::remove_file(&outside);
}

#[tokio::test]
async fn static_route_rejects_an_absolute_path() {
    // `/uploads//etc/passwd` ⇒ 剥掉前缀后是 `/etc/passwd`（**绝对**）⇒ 404。
    let dir = TempDir::new("abs");
    let app = test_app(lazy_db(), dir.path());
    let (status, _, body) = call_bytes(
        &app,
        Call::anon("GET", format!("{STATIC_PREFIX}/etc/passwd")),
    )
    .await;
    assert_eq!(status, 404, "{:?}", String::from_utf8_lossy(&body));
    assert_eq!(guard_static_key("/etc/passwd"), Err(KeyError::Absolute));
}

#[cfg(unix)]
#[tokio::test]
async fn static_route_rejects_a_symlink_escape() {
    // 🔴 这是三条反例里**唯一** `validate_key` 与 `guard_static_key` 都放行的那条：
    // 键是干净的 `escape.txt`，逃逸只存在于**磁盘上** ⇒ 判据必须是「解析符号链接之后
    // 仍在根之下」（`resolve_under`），不是字符串检查。
    let dir = TempDir::new("symlink");
    let outside_dir = TempDir::new("symlink-outside");
    let target = outside_dir.path().join("secret.txt");
    std::fs::write(&target, b"TOP SECRET").expect("seed outside");
    dir.symlink("escape.txt", &target);
    dir.seed("users/u1/ok.txt", b"fine");

    let app = test_app(lazy_db(), dir.path());
    let (status, _, body) = call_bytes(
        &app,
        Call::anon("GET", format!("{STATIC_PREFIX}escape.txt")),
    )
    .await;
    assert_eq!(
        status,
        404,
        "符号链接逃逸必须被拒：{:?}",
        String::from_utf8_lossy(&body)
    );
    assert!(!String::from_utf8_lossy(&body).contains("TOP SECRET"));
    // 键的字符串层**放行**（证明拒它的不是字符串检查）。
    assert_eq!(guard_static_key("escape.txt"), Ok(()));
    assert!(super::resolve_under(dir.path(), "escape.txt")
        .await
        .is_none());

    // 指向**根之内**的符号链接仍然放行（上游 `http.ServeFile` 也放行）。
    let inner = dir.path().join("uploads/users/u1/ok.txt");
    dir.symlink("alias.txt", &inner);
    let (status, _, body) =
        call_bytes(&app, Call::anon("GET", format!("{STATIC_PREFIX}alias.txt"))).await;
    assert_eq!(
        status,
        200,
        "根内的符号链接不该被拒：{:?}",
        String::from_utf8_lossy(&body)
    );
}

#[tokio::test]
async fn static_route_refuses_internal_sidecar_and_staging_paths() {
    // 上游 `storage/local.go::isInternalLocalPath` 逐字：sidecar 与暂存文件是实现细节，
    // 不许变成一个稳定的读 API（在**任何磁盘工作之前**就拒）。
    let dir = TempDir::new("internal");
    dir.seed("users/u1/a.png.meta.json", b"{\"filename\":\"secret\"}");
    dir.seed("users/u1/.a.png.tmp", b"half written");
    let app = test_app(lazy_db(), dir.path());
    for key in ["users/u1/a.png.meta.json", "users/u1/.a.png.tmp"] {
        let (status, _, body) =
            call_bytes(&app, Call::anon("GET", format!("{STATIC_PREFIX}{key}"))).await;
        assert_eq!(
            status,
            404,
            "{key} 必须被拒：{:?}",
            String::from_utf8_lossy(&body)
        );
    }
    assert_eq!(
        guard_static_key("users/u1/a.png.meta.json"),
        Err(KeyError::Internal)
    );
}

#[tokio::test]
async fn static_route_404s_a_missing_object() {
    let dir = TempDir::new("missing");
    let app = test_app(lazy_db(), dir.path());
    let (status, _, _) = call_bytes(
        &app,
        Call::anon("GET", format!("{STATIC_PREFIX}users/u1/nope.png")),
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn static_route_rejects_a_malformed_percent_escape() {
    let dir = TempDir::new("pct");
    let app = test_app(lazy_db(), dir.path());
    let (status, _, _) = call_bytes(
        &app,
        Call::anon("GET", format!("{STATIC_PREFIX}users/u1/%zz.png")),
    )
    .await;
    assert_eq!(status, 404);
}

// --------------------------------------------------------------------------- //
// 跨两条路由的一致性
// --------------------------------------------------------------------------- //

#[tokio::test]
async fn upload_without_workspace_writes_the_object_under_users_and_serves_it_back() {
    // 端到端（**不碰库**的那一支：无 workspace 上下文 ⇒ 上游不写行 ⇒ 零 DB 依赖）：
    // 上传 ⇒ 响应里的 `url` ⇒ 立刻 `GET` 那个 url ⇒ 字节逐字相同。
    let dir = TempDir::new("e2e");
    let app = test_app(lazy_db(), dir.path());
    let user = new_uuid();
    let (status, _, body) = call(
        &app,
        Call::upload(&[Part::file("shot.png", &PNG_1X1)]).with_user(user),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    let url = v["url"].as_str().expect("url").to_owned();
    assert_eq!(v["filename"], "shot.png", "{body}");
    assert!(!v["id"].as_str().unwrap_or_default().is_empty(), "{body}");
    // 键布局逐字对齐上游 `file.go:441-448` 的无 workspace 那一支。
    assert!(
        url.starts_with(&format!("{STATIC_PREFIX}users/{user}/")),
        "键布局偏离上游：{url}"
    );
    // 扩展名比对刻意**大小写敏感**（本仓 `storage_filename` 原样保留客户端给的扩展名）。
    assert!(has_png_extension(&url), "{url}");

    let (status, headers, got) = call_bytes(&app, Call::anon("GET", url)).await;
    assert_eq!(status, 200);
    assert_eq!(got, PNG_1X1.to_vec(), "取回的字节必须与上传的逐字相同");
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(
        v["id"].as_str().unwrap_or_default().len(),
        36,
        "id 必须是 uuid"
    );
}

#[test]
fn max_upload_size_is_the_upstream_100mb() {
    // 上游 `file.go:38`：`const maxUploadSize = 100 << 20`。
    assert_eq!(MAX_UPLOAD_SIZE, 100 << 20);
    let mut buf: Vec<u8> = Vec::new();
    assert_eq!(capped_push(&mut buf, &[0; 8], 8), Ok(()), "恰好在限上要收");
    assert_eq!(
        capped_push(&mut buf, &[0; 1], 8),
        Err(FormError::TooLarge),
        "超一字节就要拒"
    );
    assert_eq!(buf.len(), 8, "被拒的那一 chunk 不许进缓冲");
}

#[test]
fn storage_filename_keeps_the_extension_and_id() {
    let id = new_uuid();
    assert_eq!(
        storage_filename(&id, "shot.png"),
        format!("{id}.png"),
        "上游 `file.go:439` 逐字：uuid + 原扩展名"
    );
    assert_eq!(storage_filename(&id, "noext"), id.to_string());
    // 路径分隔符 / 超长 / 非字母数字的扩展名一律丢掉（键里绝不许出现 `/`）。
    assert!(!storage_filename(&id, "../../etc/passwd").contains('/'));
    assert!(!storage_filename(&id, "a.").ends_with('.'));
    assert!(storage_filename(&id, &format!("a.{}", "x".repeat(64))).ends_with(&id.to_string()));
}

#[test]
fn content_type_uses_bytes_not_the_client_header() {
    // 上游 `file.go:411-418` 逐字：**不信客户端头**，嗅探字节。
    assert_eq!(content_type_for("x.bin", &PNG_1X1), "image/png");
    assert_eq!(
        content_type_for("x.png", b"not a png at all"),
        "text/plain; charset=utf-8"
    );
    // 扩展名覆盖表（上游那六个）逐字。
    for (name, want) in [
        ("a.svg", "image/svg+xml"),
        ("a.css", "text/css"),
        ("a.js", "application/javascript"),
        ("a.mjs", "application/javascript"),
        ("a.json", "application/json"),
        ("a.wasm", "application/wasm"),
    ] {
        assert_eq!(
            content_type_for(name, b"\x00\x01\x02binary"),
            want,
            "{name}"
        );
    }
}
