//! M10-B2 真库那一半（门 ⑥，`--with-db`）：`POST /api/upload-file` 的**落行**那一支。
//!
//! 全部 `#[ignore]` + `MULTICA_TEST_DATABASE_URL`。夹具在 `fx.rs`。
//!
//! 判据表（专属验收）：
//!
//! | 键 | 钉住的东西 |
//! |---|---|
//! | `POST /api/upload-file` | 带 workspace ⇒ 成员校验 → 落 `attachment` 行 → `AttachmentResponse` 逐字段 |
//! | 同上 | 非成员 **403**；跨 workspace 的 `issue_id` **403**；畸形 id **400** |
//! | 同上 | `task_id` / `chat_session_id` fail-closed **403** |
//! | 同上 | 无 workspace ⇒ **不写行**、键落在 `users/<user>/` 下 |
//! | `GET /uploads/*` | 上传完立刻从静态分发面取回**同样字节**（端到端闭环） |

use serde_json::Value;

use super::fx::fixture;
use super::support::{call, call_bytes, test_app, Call, Part, TempDir, PNG_1X1};
use crate::routes::uploads::{ERR_BAD_ISSUE, ERR_NOT_MEMBER, STATIC_PREFIX};

/// 真库那一半的 app：既要库，也要一个真的本地磁盘根（`/uploads/*` 才挂载）。
fn app_with_disk(fx: &super::fx::Fx, dir: &TempDir) -> axum::Router {
    test_app(fx.db.clone(), dir.path())
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn upload_with_workspace_creates_the_attachment_row() {
    let Some(fx) = fixture().await else { return };
    let dir = TempDir::new("db-create");
    let app = app_with_disk(&fx, &dir);
    let (status, _, body) = call(
        &app,
        Call::upload(&[Part::file("shot.png", &PNG_1X1)])
            .with_user(fx.user)
            .with_workspace(fx.ws),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["workspace_id"], fx.ws.to_string(), "{body}");
    assert_eq!(v["uploader_type"], "member", "{body}");
    assert_eq!(v["uploader_id"], fx.user.to_string(), "{body}");
    assert_eq!(v["filename"], "shot.png", "{body}");
    assert_eq!(v["content_type"], "image/png", "{body}");
    assert_eq!(v["size_bytes"], 67, "{body}");
    // 键布局逐字对齐上游 `file.go:441-444` 的**有 workspace** 那一支。
    let url = v["url"].as_str().expect("url");
    assert!(
        url.starts_with(&format!("{STATIC_PREFIX}workspaces/{}/", fx.ws)),
        "键布局偏离上游：{url}"
    );
    // 扩展名比对刻意**大小写敏感**（本仓 `storage_filename` 原样保留客户端给的扩展名）。
    assert!(super::support::has_png_extension(url), "{url}");

    // 行真的在库里（不是只回了个体面的 JSON）。
    let n: (i64,) = sqlx::query_as("SELECT count(*) FROM attachment WHERE id = $1")
        .bind(uuid::Uuid::parse_str(v["id"].as_str().expect("id")).expect("uuid"))
        .fetch_one(fx.db.pool())
        .await
        .expect("count");
    assert_eq!(n.0, 1, "attachment 行必须落库");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn upload_attaches_to_an_issue_in_the_same_workspace() {
    let Some(fx) = fixture().await else { return };
    let dir = TempDir::new("db-issue");
    let app = app_with_disk(&fx, &dir);
    let (status, _, body) = call(
        &app,
        Call::upload(&[
            Part::file("shot.png", &PNG_1X1),
            Part::text("issue_id", &fx.issue.to_string()),
        ])
        .with_user(fx.user)
        .with_workspace(fx.ws),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["issue_id"], fx.issue.to_string(), "{body}");
    // 这条行应该出现在 `GET /api/issues/{id}/attachments` 里（M10-B1 那条读面）。
    let (status, _, body) = call(
        &app,
        Call::new(
            "GET",
            format!("/api/issues/{}/attachments", fx.issue),
            fx.user,
            fx.ws,
        ),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let list: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(list.as_array().map(Vec::len), Some(1), "{body}");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn upload_to_an_issue_in_another_workspace_is_403() {
    let Some(fx) = fixture().await else { return };
    let dir = TempDir::new("db-xws");
    let app = app_with_disk(&fx, &dir);
    let (status, _, body) = call(
        &app,
        Call::upload(&[
            Part::file("shot.png", &PNG_1X1),
            Part::text("issue_id", &fx.issue_other_ws.to_string()),
        ])
        .with_user(fx.user)
        .with_workspace(fx.ws),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(body.contains(ERR_BAD_ISSUE), "{body}");
    assert!(
        !dir.path().join("uploads").exists(),
        "校验失败之前不许落对象"
    );
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn upload_by_a_non_member_is_403_and_writes_nothing() {
    let Some(fx) = fixture().await else { return };
    let dir = TempDir::new("db-nonmember");
    let app = app_with_disk(&fx, &dir);
    // `outsider` 由夹具建好（且**故意不在** `ws` 的 `member` 表里）⇒ 这里直接用。
    let (status, _, body) = call(
        &app,
        Call::upload(&[Part::file("shot.png", &PNG_1X1)])
            .with_user(fx.outsider)
            .with_workspace(fx.ws),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(body.contains(ERR_NOT_MEMBER), "{body}");
    assert!(!dir.path().join("uploads").exists(), "非成员不许留下对象");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn upload_without_workspace_writes_no_row() {
    // 上游 `file.go:618-626` 逐字：无 workspace 上下文（头像那一类）只落对象。
    let Some(fx) = fixture().await else { return };
    let dir = TempDir::new("db-nous");
    let app = app_with_disk(&fx, &dir);
    let before: (i64,) = sqlx::query_as("SELECT count(*) FROM attachment")
        .fetch_one(fx.db.pool())
        .await
        .expect("count");
    let (status, _, body) = call(
        &app,
        Call::upload(&[Part::file("avatar.png", &PNG_1X1)]).with_user(fx.user),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["filename"], "avatar.png", "{body}");
    assert!(
        v["url"]
            .as_str()
            .expect("url")
            .starts_with(&format!("{STATIC_PREFIX}users/{}/", fx.user)),
        "{body}"
    );
    // 三键响应：**没有** `workspace_id`（上游那一支逐字）。
    assert!(v.get("workspace_id").is_none(), "{body}");
    let after: (i64,) = sqlx::query_as("SELECT count(*) FROM attachment")
        .fetch_one(fx.db.pool())
        .await
        .expect("count");
    assert_eq!(after.0, before.0, "无 workspace 分支不许写 attachment 行");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn uploaded_object_is_served_back_by_the_static_route() {
    // 两条路由的**闭环**：上传 ⇒ 响应里的 `url` ⇒ 静态分发面取回同样字节。
    let Some(fx) = fixture().await else { return };
    let dir = TempDir::new("db-roundtrip");
    let app = app_with_disk(&fx, &dir);
    let (status, _, body) = call(
        &app,
        Call::upload(&[Part::file("shot.png", &PNG_1X1)])
            .with_user(fx.user)
            .with_workspace(fx.ws),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    let url = v["url"].as_str().expect("url").to_owned();
    let (status, headers, got) = call_bytes(&app, Call::anon("GET", url)).await;
    assert_eq!(status, 200);
    assert_eq!(got, PNG_1X1.to_vec(), "取回的字节必须与上传的逐字相同");
    assert_eq!(headers["content-type"], "image/png");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn workspace_slug_header_also_resolves_the_workspace() {
    // 上游 `file.go:383` 的 `resolveWorkspaceID` 有 slug 那一支；本仓复用
    // `resolve_workspace`（`docs/62` §9.7 的判例：**不新写** workspace 解析）。
    let Some(fx) = fixture().await else { return };
    let dir = TempDir::new("db-slug");
    let app = app_with_disk(&fx, &dir);
    let (status, _, body) = call(
        &app,
        Call::upload(&[Part::file("shot.png", &PNG_1X1)])
            .with_user(fx.user)
            .with_extra_header("x-workspace-slug", &fx.ws_slug),
    )
    .await;
    assert_eq!(status, 200, "slug 头也要能解析出 workspace：{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["workspace_id"], fx.ws.to_string(), "{body}");
}
