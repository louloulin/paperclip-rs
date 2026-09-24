//! `/api/skills/:id/files` 支持文件的端到端测试（M6-2）。
//!
//! 上游三条：`ListSkillFiles` / `PutSkillFiles`（**单文件 upsert**）/ `DeleteSkillFile`。
//! 这里钉住四件容易写错的事：
//! 1. `PUT` 的请求体是**一个**文件（`{path, content}`），同路径再 PUT 是**覆盖**不是新增；
//! 2. 整批替换只属于 `PUT /api/skills/:id`（见 `crud.rs`），不在本文件；
//! 3. 保留路径 `SKILL.md` 与越界路径各是一条 400，文案不同；
//! 4. 删除要证明文件属于该 skill，否则 404（不是 403）。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{call, cleanup, connect, create_skill, error_message, id_of, seed_workspace};

/// upsert → 列表（两种 include）→ 删除的往返 + 幂等覆盖。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn file_upsert_is_single_file_and_overwrites_the_same_path() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let skill = create_skill(&app, ws, user, json!({ "name": "files" })).await;
    let skill_id = id_of(&skill);
    let path = format!("/api/skills/{skill_id}/files");

    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({ "path": "docs/guide.md", "content": "one" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path"], "docs/guide.md");
    assert_eq!(body["content"], "one");
    assert_eq!(body["skill_id"], skill_id.to_string());
    let file_id = id_of(&body);

    // 同路径再 PUT ⇒ 覆盖同一行（`ON CONFLICT (skill_id, path) DO UPDATE`）
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({ "path": "docs/guide.md", "content": "two" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], file_id.to_string(), "覆盖不换 id");
    assert_eq!(body["content"], "two");

    // 另一条路径共存
    let (status, _) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({ "path": "a/b/c.txt", "content": "deep" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 裸数组、两条路径、按 path 升序
    let (status, body) = call(&app, "GET", &path, ws, user, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = body.as_array().expect("array");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0]["path"], "a/b/c.txt");
    assert_eq!(listed[1]["path"], "docs/guide.md");
    assert_eq!(listed[1]["content"], "two");

    // include=metadata：正文换 size + hash（与 SQL 的 octet_length/sha256 同值）
    let (status, body) = call(
        &app,
        "GET",
        &format!("{path}?include=metadata"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[1]["size"], 3);
    assert_eq!(
        body[1]["content_hash"], "3fc4ccfe745870e2c0d99f71f30ff0656c8dedd41cc1d7d3d376b0dbe685e2f3",
        "sha256(\"two\")"
    );
    assert!(body[1].get("content").is_none());

    // 非法 include 早于取 skill ⇒ 400（且 skill 不存在时也是 400，不是 404）
    let (status, body) = call(&app, "GET", &format!("{path}?include=nope"), ws, user, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_message(&body),
        r#"invalid include: expected "content" or "metadata""#
    );
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/skills/{}/files?include=nope", Uuid::new_v4()),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "include 判定在 load 之前");

    // 删除 ⇒ 204；再删 ⇒ 404
    let (status, _) = call(&app, "DELETE", &format!("{path}/{file_id}"), ws, user, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = call(&app, "DELETE", &format!("{path}/{file_id}"), ws, user, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    // 本仓 404 文案是 `not found: <资源名>`（上游写 `skill file not found`）——
    // 详见 `docs/32` §9.6 的错误体偏离条。
    assert_eq!(error_message(&body), "skill file");

    cleanup(&pool, ws, &[user]).await;
}

/// 路径护栏：保留路径、越界路径、坏 body、坏 uuid、跨 skill 的 file id。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn file_paths_and_ids_are_guarded() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let skill = create_skill(&app, ws, user, json!({ "name": "guards" })).await;
    let skill_id = id_of(&skill);
    let path = format!("/api/skills/{skill_id}/files");

    // 保留路径：可以出现在请求体里，但不作为支持文件写入
    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({ "path": "./SKILL.md", "content": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        error_message(&body),
        "SKILL.md is reserved for the primary skill content"
    );

    // 越界 / 绝对路径 / `..foo`（上游 `HasPrefix(Clean(p), "..")` 的怪癖）
    for bad in ["../oops.txt", "/etc/passwd", "..foo", ""] {
        let (status, body) = call(
            &app,
            "PUT",
            &path,
            ws,
            user,
            Some(json!({ "path": bad, "content": "x" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} -> {body}");
        assert_eq!(error_message(&body), "invalid file path", "{bad}");
    }

    // body 不是对象 ⇒ 400（`null` 在上游是解码成功留零值，随后撞路径校验，同为 400）
    for body in [json!(null), json!([1, 2]), json!("nope")] {
        let (status, _) = call(&app, "PUT", &path, ws, user, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // 坏 file id ⇒ 400；别的 skill 的 file id ⇒ 404
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("{path}/not-a-uuid"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "file id must be a valid uuid");

    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        user,
        Some(json!({ "path": "shared.txt", "content": "mine" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let other_file_id = id_of(&body);

    let other = create_skill(&app, ws, user, json!({ "name": "other" })).await;
    let other_id = id_of(&other);
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/skills/{other_id}/files/{other_file_id}"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    // 本仓 404 文案是 `not found: <资源名>`（上游写 `skill file not found`）——
    // 详见 `docs/32` §9.6 的错误体偏离条。
    assert_eq!(error_message(&body), "skill file");

    // 原文件还在
    let (status, body) = call(&app, "GET", &path, ws, user, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);

    cleanup(&pool, ws, &[user]).await;
}

/// 支持文件的写面与主面同一道鉴权门槛（member 非创建者 ⇒ 403）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn file_writes_require_can_manage() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, creator) = seed_workspace(&pool, "member").await;
    let other = crate::support::seed_user(&pool, ws, "member").await;
    let app = crate::support::app_with_db(db);
    let skill = create_skill(&app, ws, creator, json!({ "name": "perm" })).await;
    let skill_id = id_of(&skill);
    let path = format!("/api/skills/{skill_id}/files");

    // GET 不查成员（上游 ListSkillFiles 没有 canManageSkill）
    let (status, _) = call(&app, "GET", &path, ws, other, None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(
        &app,
        "PUT",
        &path,
        ws,
        other,
        Some(json!({ "path": "x.txt", "content": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        error_message(&body),
        "only the skill creator can manage this skill"
    );

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("{path}/{}", Uuid::new_v4()),
        ws,
        other,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    cleanup(&pool, ws, &[creator, other]).await;
}
