//! M10-B1 真库那一半的**元数据读**（门 ⑥，`--with-db`）。
//!
//! 全部 `#[ignore]` + `MULTICA_TEST_DATABASE_URL`。共用夹具在 `tests/fx.rs`；
//! 取字节三键与 `DELETE` 在 `db_bytes.rs`。
//!
//! 判据：`GET /api/attachments/{id}` 的逐字段响应体 + 跨 workspace / 非成员 **404**；
//! `GET /api/issues/{id}/attachments` 的 `created_at ASC` 顺序 + **不带**能力链接。

use mc_repos::attachment::AttachmentRepo;
use serde_json::Value;
use uuid::Uuid;

use super::fx::{fixture, Att};
use super::support::{call, new_uuid, test_app, Call};

// --------------------------------------------------------------------------- //
// GET /api/attachments/{id}
// --------------------------------------------------------------------------- //

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn get_attachment_returns_every_field() {
    let Some(fx) = fixture().await else { return };
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
    let app = test_app(fx.db.clone());
    let (status, _, body) = call(
        &app,
        Call::new("GET", format!("/api/attachments/{id}"), fx.user, fx.ws),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["id"], id.to_string());
    assert_eq!(v["workspace_id"], fx.ws.to_string());
    assert_eq!(v["issue_id"], fx.issue.to_string());
    assert_eq!(v["uploader_type"], "member");
    assert_eq!(v["uploader_id"], fx.user.to_string());
    assert_eq!(v["filename"], "a.png");
    assert_eq!(v["url"], "mc-assets/a.png");
    assert_eq!(v["content_type"], "image/png");
    assert_eq!(v["markdown_url"], format!("/api/attachments/{id}/download"));
    // 🔴 没配根密钥 ⇒ 铸造侧优雅降级成空串（**不是**可伪造的链接）。
    assert_eq!(v["download_url"], "", "{body}");
    assert!(v.get("attachment_download_url").is_none(), "{body}");
    // 🔴 抓取上下文的行不该出现在响应里（`comment_id` 可空 ⇒ 键不出现）。
    assert!(v.get("comment_id").is_none(), "{body}");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn get_attachment_across_workspace_is_404() {
    let Some(fx) = fixture().await else { return };
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
    let app = test_app(fx.db.clone());
    // `other_ws` 的成员拿 `ws` 的附件 id ⇒ **404**（不是 403）。
    let (status, _, body) = call(
        &app,
        Call::new(
            "GET",
            format!("/api/attachments/{id}"),
            fx.user,
            fx.other_ws,
        ),
    )
    .await;
    assert_eq!(status, 404, "{body}");
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn get_attachment_by_non_member_is_404() {
    let Some(fx) = fixture().await else { return };
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
    let app = test_app(fx.db.clone());
    // `other` 是 `ws` 的**成员**（role=member）⇒ 成员门槛过了，但这条 200。
    // 真正要验的是「**完全不是** `ws` 成员」的人：
    let stranger = new_uuid();
    let (status, _, body) = call(
        &app,
        Call::new("GET", format!("/api/attachments/{id}"), stranger, fx.ws),
    )
    .await;
    assert_eq!(status, 404, "非成员应是 404（不是 403）：{body}");
}

// --------------------------------------------------------------------------- //
// GET /api/issues/{id}/attachments
// --------------------------------------------------------------------------- //

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn list_issue_attachments_is_ordered_and_carries_no_capability() {
    let Some(fx) = fixture().await else { return };
    for (i, name) in ["b.png", "a.png", "c.png"].iter().enumerate() {
        // created_at 显式给出 ⇒ 顺序由 SQL 的 `ORDER BY created_at ASC` 决定，
        // **不是**插入顺序、也不是 id 顺序。
        //
        // ⚠️ `i` 显式取 `i32`（`enumerate()` 给的是 `usize`）：后面要 `as f64`，
        // 而 `usize as f64` 会被门 ③ 的 `cast_precision_loss` 拒。
        let i = i32::try_from(i).expect("i fits in i32");
        let id = fx
            .attach(Att {
                issue: fx.issue,
                ws: fx.ws,
                uploader: fx.user,
                uploader_type: "member",
                filename: name,
                url: "mc-assets/x",
                content_type: "image/png",
                captured: false,
            })
            .await;
        // `make_interval(secs => …)` 而不是 `($2 || ' seconds')::interval`：
        // 后者靠字符串拼，判据会退化成「拼出来的字面量对不对」；前者直接吃秒数。
        sqlx::query(
            "UPDATE attachment SET created_at = now() - make_interval(secs => $2::float8) \
             WHERE id = $1",
        )
        .bind(id)
        .bind(f64::from(10 - i * 3))
        .execute(fx.db.pool())
        .await
        .expect("created_at");
    }
    let app = test_app(fx.db.clone());
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
    let v: Value = serde_json::from_str(&body).expect("json");
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 3, "{body}");
    let names: Vec<&str> = arr
        .iter()
        .map(|x| x["filename"].as_str().unwrap())
        .collect();
    // `b`=10 秒前、`a`=7 秒前、`c`=4 秒前 ⇒ `created_at ASC`（**最旧在前**）=
    // `[b, a, c]`。算式写在这里：改秒数就一定会看到预期跟着变。
    assert_eq!(
        names,
        vec!["b.png", "a.png", "c.png"],
        "created_at ASC（最旧在前）"
    );
    // 🔴 列表响应**绝不带**能力链接（TTL 60s ≪ 列表持有时间）。
    for item in arr {
        assert_eq!(
            item["download_url"],
            format!("/api/attachments/{}/download", item["id"].as_str().unwrap())
        );
        assert!(item.get("attachment_download_url").is_none(), "{item}");
    }
}

#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn list_attachments_across_workspace_is_404() {
    let Some(fx) = fixture().await else { return };
    let app = test_app(fx.db.clone());
    // `other_ws` 的 issue ⇒ 404（不是空列表、不是 403）。
    let (status, _, body) = call(
        &app,
        Call::new(
            "GET",
            format!("/api/issues/{}/attachments", fx.issue_other_ws),
            fx.user,
            fx.ws,
        ),
    )
    .await;
    assert_eq!(status, 404, "{body}");
}

// --------------------------------------------------------------------------- //
// repo 层（`crates/mc-repos/src/attachment.rs` 的四个查询）
// --------------------------------------------------------------------------- //

/// 🔴 `get` 带 workspace 条件、`get_by_id_only` **不带** —— 这条是 `/download`
/// 能自解析 workspace 的机制基础（上游 `attachment.sql:42-50` 的注释逐字：
/// "access-neutral on purpose"）。判据不能只测「能取到」，要测**两个查询的差别**。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn repo_get_is_workspace_scoped_but_get_by_id_only_is_not() {
    let Some(fx) = fixture().await else { return };
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
    let repo = AttachmentRepo::new(&fx.db);
    assert!(repo.get(mc_core::Id(fx.ws), mc_core::Id(id)).await.is_ok());
    assert!(
        repo.get(mc_core::Id(fx.other_ws), mc_core::Id(id))
            .await
            .is_err(),
        "跨 workspace 必须取不到"
    );
    // 授权中性那一半：按 id 单查**能**取到（`/download` 靠它自解析 workspace）。
    assert!(repo.get_by_id_only(mc_core::Id(id)).await.is_ok());
}

/// 🔴 `DeleteAttachment` 的 `source_context_id IS NULL` 那一格必须真的挡住副本
/// ⇒ `changed == false` 且两个 revision 都是 0。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn repo_delete_reports_changed_false_for_a_captured_copy() {
    let Some(fx) = fixture().await else { return };
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
    let out = AttachmentRepo::new(&fx.db)
        .delete(mc_core::Id(fx.ws), mc_core::Id(id))
        .await
        .expect("delete");
    assert!(!out.changed, "`source_context_id IS NULL` 那一格必须挡住它");
    assert_eq!(out.issue_revision, 0);
    assert_eq!(out.comment_revision, 0);
}

/// `DeleteAttachment` 的 CTE 正常路径：`changed == true` + `issue.revision` **真的 +1**。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn repo_delete_bumps_issue_revision_when_it_really_deletes() {
    let Some(fx) = fixture().await else { return };
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
    let out = AttachmentRepo::new(&fx.db)
        .delete(mc_core::Id(fx.ws), mc_core::Id(id))
        .await
        .expect("delete");
    assert!(out.changed);
    assert_eq!(out.issue_revision, before.0 + 1);
    // 第二次删 ⇒ `changed == false`（并发双删里后到那条走的就是这一格）。
    let again = AttachmentRepo::new(&fx.db)
        .delete(mc_core::Id(fx.ws), mc_core::Id(id))
        .await
        .expect("delete again");
    assert!(!again.changed);
}

/// `list_by_issue` 只列**本 issue + 本 workspace** 的行（两个条件都在）。
#[tokio::test]
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
async fn repo_list_is_scoped_to_issue_and_workspace() {
    let Some(fx) = fixture().await else { return };
    let mine = fx
        .attach(Att {
            issue: fx.issue,
            ws: fx.ws,
            uploader: fx.user,
            uploader_type: "member",
            filename: "mine.png",
            url: "mc-assets/mine.png",
            content_type: "image/png",
            captured: false,
        })
        .await;
    // 同一个 workspace、**另一个** issue 的附件。
    let other_issue = new_uuid();
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, number, identifier, title, status, \
                            creator_type, creator_id) \
         VALUES ($1,$2,2,$3,'t2','todo','user',$4)",
    )
    .bind(other_issue)
    .bind(fx.ws)
    .bind(format!("M10B1-{}", other_issue.simple()))
    .bind(fx.user)
    .execute(fx.db.pool())
    .await
    .expect("issue");
    fx.attach(Att {
        issue: other_issue,
        ws: fx.ws,
        uploader: fx.user,
        uploader_type: "member",
        filename: "theirs.png",
        url: "mc-assets/theirs.png",
        content_type: "image/png",
        captured: false,
    })
    .await;

    let repo = AttachmentRepo::new(&fx.db);
    let rows = repo
        .list_by_issue(mc_core::Id(fx.issue), mc_core::Id(fx.ws))
        .await
        .expect("list");
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![mine], "只该列本 issue 的那一行");
}
