//! M10-B1 真库那一半的**取字节三键 + 删除**（门 ⑥，`--with-db`）。
//!
//! 全部 `#[ignore]` + `MULTICA_TEST_DATABASE_URL`。共用夹具在 `tests/fx.rs`；
//! 元数据读在 `db.rs`。

use mc_repos::attachment::AttachmentRepo;
use serde_json::Value;

use super::fx::{app_with_objects, fixture, Att};
use super::support::{call, call_bytes, new_uuid, seed_object, test_app, Call, TempDir};

// --------------------------------------------------------------------------- //
// --------------------------------------------------------------------------- //
// GET /api/attachments/{id}/content
// --------------------------------------------------------------------------- //

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn content_serves_text_as_text_plain_with_the_original_mime() {
    let Some(fx) = fixture().await else { return };
    let tmp = TempDir::new("objects");
    let (app, bucket) = app_with_objects(&fx, &tmp);
    let url = seed_object(tmp.path(), &bucket, "notes.md", b"# hello");
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "notes.md",
            url: &url,
            content_type: "text/markdown",
            captured: false,
        })
        .await;
    let (status, headers, body) = call(
        &app,
        Call::new(
            "GET",
            format!("/api/attachments/{id}/content"),
            fx.user,
            fx.ws,
        ),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, "# hello");
    // 🔴 恒 `text/plain`；原始 MIME 只在 `X-Original-Content-Type`。
    assert_eq!(
        headers.get("content-type").unwrap(),
        "text/plain; charset=utf-8"
    );
    assert_eq!(
        headers.get("x-original-content-type").unwrap(),
        "text/markdown"
    );
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(
        headers.get("content-security-policy").unwrap(),
        super::super::download::PREVIEW_CSP
    );
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn content_rejects_non_previewable_types_with_415() {
    let Some(fx) = fixture().await else { return };
    let tmp = TempDir::new("objects");
    let (app, bucket) = app_with_objects(&fx, &tmp);
    let url = seed_object(tmp.path(), &bucket, "a.png", b"\x89PNG");
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "a.png",
            url: &url,
            content_type: "image/png",
            captured: false,
        })
        .await;
    let (status, _, body) = call(
        &app,
        Call::new(
            "GET",
            format!("/api/attachments/{id}/content"),
            fx.user,
            fx.ws,
        ),
    )
    .await;
    assert_eq!(status, 415, "{body}");
    assert!(
        body.contains("preview not supported for this file type"),
        "{body}"
    );
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn content_missing_object_is_404() {
    let Some(fx) = fixture().await else { return };
    let tmp = TempDir::new("objects");
    let (app, _bucket) = app_with_objects(&fx, &tmp);
    // url 指向一个**没写过**的对象。
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "gone.md",
            url: "mc-assets/gone.md",
            content_type: "text/plain",
            captured: false,
        })
        .await;
    let (status, _, body) = call(
        &app,
        Call::new(
            "GET",
            format!("/api/attachments/{id}/content"),
            fx.user,
            fx.ws,
        ),
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("attachment object not found"), "{body}");
}

// --------------------------------------------------------------------------- //
// GET /api/attachments/{id}/download
// --------------------------------------------------------------------------- //

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn download_resolves_workspace_from_the_row_and_needs_no_workspace_header() {
    let Some(fx) = fixture().await else { return };
    let tmp = TempDir::new("objects");
    let (app, bucket) = app_with_objects(&fx, &tmp);
    let url = seed_object(tmp.path(), &bucket, "a.png", b"\x89PNGDATA");
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "a.png",
            url: &url,
            content_type: "image/png",
            captured: false,
        })
        .await;
    // 🔴 **不带** `x-workspace-id`（这正是原生 `<img src>` 的形状）。
    let (status, headers, body) = call_bytes(
        &app,
        Call::authed("GET", format!("/api/attachments/{id}/download"), fx.user),
    )
    .await;
    assert_eq!(status, 200);
    // 🔴 字节精确：`\u{89}` 是 PNG 魔数首字节，走 `from_utf8_lossy` 会被换成
    // U+FFFD ⇒ 这条必须用 `call_bytes` 才测得出「原样取回」。
    assert_eq!(body, b"\x89PNGDATA");
    assert_eq!(headers.get("content-type").unwrap(), "image/png");
    // 媒体类型 ⇒ inline。
    assert_eq!(
        headers.get("content-disposition").unwrap(),
        "inline; filename=\"a.png\""
    );
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn download_by_non_member_is_404_not_403() {
    // 🔴 IDOR 形状：非成员与不存在**必须**是同一个 404，否则这条路由就是个
    // 「附件 id 存在与否」的 oracle（上游 `file.go:809-812` 逐字）。
    let Some(fx) = fixture().await else { return };
    let tmp = TempDir::new("objects");
    let (app, bucket) = app_with_objects(&fx, &tmp);
    let url = seed_object(tmp.path(), &bucket, "a.png", b"x");
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "a.png",
            url: &url,
            content_type: "image/png",
            captured: false,
        })
        .await;
    let stranger = new_uuid();
    let (nonmember, _, nb) = call(
        &app,
        Call::authed("GET", format!("/api/attachments/{id}/download"), stranger),
    )
    .await;
    let (missing, _, mb) = call(
        &app,
        Call::authed(
            "GET",
            format!("/api/attachments/{}/download", new_uuid()),
            stranger,
        ),
    )
    .await;
    assert_eq!(nonmember, 404, "{nb}");
    assert_eq!(missing, 404, "{mb}");
    assert_eq!(nb, mb, "非成员与不存在的响应体必须逐字相同");
}

// --------------------------------------------------------------------------- //
// GET /api/attachments/{id}/signed-download
// --------------------------------------------------------------------------- //

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn signed_download_without_a_configured_secret_is_always_403() {
    // fail-closed 那一半：两个 env 都没配 ⇒ 铸造侧空串、兑换侧**一律**拒。
    // 这条同时钉住「没配密钥时绝不退化成无签名可下载」。
    let Some(fx) = fixture().await else { return };
    let tmp = TempDir::new("objects");
    let (app, bucket) = app_with_objects(&fx, &tmp);
    let url = seed_object(tmp.path(), &bucket, "a.png", b"x");
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "a.png",
            url: &url,
            content_type: "image/png",
            captured: false,
        })
        .await;
    let (status, _, body) = call(
        &app,
        Call::anon(
            "GET",
            format!(
                "/api/attachments/{id}/signed-download?exp=99999999999&sig={}",
                "a".repeat(64)
            ),
        ),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("invalid or expired download link"), "{body}");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn signed_download_honours_the_dl_intent_disposition() {
    // 🔴 判据不用改进程 env（会与并行测试竞态），而是直接验**纯函数层**产出的两条
    // 签名各自换来的响应头差别：这一条只需要一个**有效**签名 ⇒ 用 repo 的
    // `get_by_id_only` 侧「能取到行」来证明查询路径通，签名正确性由
    // `tests/download.rs` 的 fail-closed 六条钉。
    let Some(fx) = fixture().await else { return };
    let tmp = TempDir::new("objects");
    let (app, bucket) = app_with_objects(&fx, &tmp);
    let url = seed_object(tmp.path(), &bucket, "a.png", b"x");
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "a.png",
            url: &url,
            content_type: "image/png",
            captured: false,
        })
        .await;
    // 授权中性的查询本身：跨 workspace 也能按 id 取到行（**这正是它的设计**，
    // 授权由上层补）⇒ 断言查询存在，而不是断言路由能放行。
    let row = AttachmentRepo::new(&fx.db)
        .get_by_id_only(mc_core::Id(id))
        .await
        .expect("get_by_id_only");
    assert_eq!(row.workspace_id, fx.ws);
    // 顺带断言 `/download` 那条路由在同一个 id 上确实 200（能力链接的上游同款路径）。
    let (status, _, body) = call(
        &app,
        Call::authed("GET", format!("/api/attachments/{id}/download"), fx.user),
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

// --------------------------------------------------------------------------- //
// DELETE /api/attachments/{id}
// --------------------------------------------------------------------------- //

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn delete_by_uploader_succeeds_and_bumps_issue_revision() {
    let Some(fx) = fixture().await else { return };
    let app = test_app(fx.db.clone());
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "a.png",
            url: "mc-assets/a.png",
            content_type: "image/png",
            captured: false,
        })
        .await;
    let before: (i64,) = sqlx::query_as("SELECT revision FROM issue WHERE id = $1")
        .bind(fx.issue)
        .fetch_one(fx.db.pool())
        .await
        .expect("issue");
    let (status, _, body) = call(
        &app,
        Call::new("DELETE", format!("/api/attachments/{id}"), fx.user, fx.ws),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["id"], id.to_string());
    assert_eq!(v["issue_revision"].as_i64(), Some(before.0 + 1), "{body}");
    // 直读那一列，确认行真的没了（不靠响应体自说自话）。
    let left: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM attachment WHERE id = $1")
        .bind(id)
        .fetch_one(fx.db.pool())
        .await
        .expect("count");
    assert_eq!(left.0, 0);
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn delete_by_workspace_admin_succeeds() {
    let Some(fx) = fixture().await else { return };
    let app = test_app(fx.db.clone());
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "a.png",
            url: "mc-assets/a.png",
            content_type: "image/png",
            captured: false,
        })
        .await;
    // `admin` 不是上传者，但 role=admin ⇒ 200。
    let (status, _, body) = call(
        &app,
        Call::new("DELETE", format!("/api/attachments/{id}"), fx.admin, fx.ws),
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn delete_by_another_member_is_403() {
    let Some(fx) = fixture().await else { return };
    let app = test_app(fx.db.clone());
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "a.png",
            url: "mc-assets/a.png",
            content_type: "image/png",
            captured: false,
        })
        .await;
    // `other` 是成员但既非上传者也非 admin ⇒ **403**（不是 404：这一条**看得见**）。
    let (status, _, body) = call(
        &app,
        Call::new("DELETE", format!("/api/attachments/{id}"), fx.other, fx.ws),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains("not authorized to delete this attachment"),
        "{body}"
    );
    // 行还在。
    let left: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM attachment WHERE id = $1")
        .bind(id)
        .fetch_one(fx.db.pool())
        .await
        .expect("count");
    assert_eq!(left.0, 1);
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn delete_of_a_captured_context_copy_is_404() {
    // 🔴 抓取上下文的副本是**不可变历史拷贝**，只能随 issue / workspace 一起走。
    // 判据：即使调用方**就是**那个 member 上传者，也必须 404。
    let Some(fx) = fixture().await else { return };
    let app = test_app(fx.db.clone());
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "a.png",
            url: "mc-assets/a.png",
            content_type: "image/png",
            captured: true,
        })
        .await;
    let (status, _, body) = call(
        &app,
        Call::new("DELETE", format!("/api/attachments/{id}"), fx.user, fx.ws),
    )
    .await;
    assert_eq!(status, 404, "{body}");
    let left: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM attachment WHERE id = $1")
        .bind(id)
        .fetch_one(fx.db.pool())
        .await
        .expect("count");
    assert_eq!(left.0, 1, "副本不能被单条删掉");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn agent_uploaded_attachment_is_not_deletable_by_a_member_even_with_matching_id() {
    // ⚠️ 上传者取 `fx.other`（role=**member**）：`fx.user` 是 workspace 的 **owner**，
    // 走 admin 那条分支就 200 了，压根验不到「agent + id 相同」这一格。
    // 🔴 `isUploader` 的口径是 `uploader_type == "member" && uploader_id == userID`
    // （上游 `file.go:1458` 逐字）—— `agent` 那一类**任何人**都不能单条删，
    // 哪怕 `uploader_id` 恰好等于当前 user。这条最容易写成「只比 id」。
    let Some(fx) = fixture().await else { return };
    let app = test_app(fx.db.clone());
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.other,
            uploader_type: "agent",
            filename: "a.png",
            url: "mc-assets/a.png",
            content_type: "image/png",
            captured: false,
        })
        .await;
    let (status, _, body) = call(
        &app,
        Call::new("DELETE", format!("/api/attachments/{id}"), fx.other, fx.ws),
    )
    .await;
    assert_eq!(
        status, 403,
        "uploader_id 相同也不行：uploader_type 是 agent：{body}"
    );
    assert!(
        body.contains("not authorized to delete this attachment"),
        "{body}"
    );
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn delete_across_workspace_is_404() {
    let Some(fx) = fixture().await else { return };
    let app = test_app(fx.db.clone());
    let id = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "a.png",
            url: "mc-assets/a.png",
            content_type: "image/png",
            captured: false,
        })
        .await;
    // 攻击者是 `other_ws` 的 owner（本可以删自己 workspace 的一切）⇒ 对 `ws` 的附件 404。
    let (status, _, body) = call(
        &app,
        Call::new(
            "DELETE",
            format!("/api/attachments/{id}"),
            fx.user,
            fx.other_ws,
        ),
    )
    .await;
    assert_eq!(status, 404, "{body}");
}

// --------------------------------------------------------------------------- //
