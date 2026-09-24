//! `/api/skills` 列表 / 搜索 / 详情 / 创建 / 更新 / 删除的端到端测试（M6-2）。
//!
//! 覆盖四类断言：
//! 1. **形状**（`docs/fixtures/m6-golden/skills/*.json` 的 `json_subset` 只比状态，但形状
//!    在 e2e 里必须钉住）；
//! 2. **尾斜杠双形态**（5 个键，axum 漏注册是 404 而不是 307）；
//! 3. **错误码**（400/403/404/409）；
//! 4. **鉴权口径**（读不查成员、写要 creator/admin —— 上游 `canManageSkill`）。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    call, cleanup, connect, create_skill, error_code, error_message, id_of, seed_user,
    seed_workspace,
};

/// 列表：两条形态都服务、不带 `content`、`labels` 恒存在、`enabled` 不出现。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn list_serves_both_slash_forms_and_omits_content() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);

    let created = create_skill(
        &app,
        ws,
        user,
        json!({
            "name": "alpha",
            "description": "first",
            "content": "# Alpha",
            "config": { "k": 1 },
        }),
    )
    .await;
    assert_eq!(created["name"], "alpha");
    assert_eq!(created["description"], "first");
    assert_eq!(created["content"], "# Alpha");
    assert_eq!(created["config"], json!({ "k": 1 }));
    assert_eq!(created["created_by"], user.to_string());
    assert_eq!(created["workspace_id"], ws.to_string());
    assert_eq!(created["files"], json!([]));

    for uri in ["/api/skills/", "/api/skills"] {
        let (status, body) = call(&app, "GET", uri, ws, user, None).await;
        assert_eq!(status, StatusCode::OK, "{uri} -> {body}");
        let list = body.as_array().expect("array");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], "alpha");
        // 列表响应是 `SkillSummaryResponse`：**没有** content 列
        assert!(list[0].get("content").is_none(), "{uri} must omit content");
        // `labels` 是指针 + omitempty，但 ListSkills 总会取址 ⇒ 空也要在
        assert_eq!(list[0]["labels"], json!([]));
        // `enabled` 只有 agent 面（M6-4）才填
        assert!(list[0].get("enabled").is_none());
    }

    cleanup(&pool, ws, &[user]).await;
}

/// 创建 → 详情（content/metadata 两种 include）→ 更新（整批替换文件）→ 删除。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 建→读→改→删是一条链，切开就断掉了状态相关性
async fn crud_roundtrip_includes_both_include_modes() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);

    let created = create_skill(
        &app,
        ws,
        user,
        json!({
            "name": "beta",
            "description": "second",
            "content": "BODY",
            "config": { "a": [1, 2] },
            "files": [
                { "path": "scripts/run.sh", "content": "echo hi\n" },
                { "path": "SKILL.md", "content": "this is the primary content" },
                { "path": "docs/a.md", "content": "A" },
            ],
        }),
    )
    .await;
    let skill_id = id_of(&created);
    // `SKILL.md` 是保留路径 ⇒ 请求体里合法、但**不落 skill_file**（剩下 2 个）。
    // 创建响应里的 files 是**请求体顺序**（上游 `createSkillWithFilesInTx` 边 upsert 边 append），
    // 与 `GET .../files` 的 `ORDER BY path ASC` 不同 —— 两条都要钉住。
    let paths: Vec<String> = crate::support::files_of(&created)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(
        paths,
        vec!["scripts/run.sh", "docs/a.md"],
        "创建响应保持请求体顺序（SKILL.md 被跳过）"
    );

    // 详情：两种形态 + 默认 include 带 content
    for uri in [
        format!("/api/skills/{skill_id}"),
        format!("/api/skills/{skill_id}/"),
    ] {
        let (status, body) = call(&app, "GET", &uri, ws, user, None).await;
        assert_eq!(status, StatusCode::OK, "{uri} -> {body}");
        assert_eq!(body["content"], "BODY");
        assert_eq!(body["config"], json!({ "a": [1, 2] }));
        assert_eq!(body["files"].as_array().unwrap().len(), 2);
        assert_eq!(
            body["files"][0]["path"], "docs/a.md",
            "取详情是 ORDER BY path ASC"
        );
        assert_eq!(body["files"][0]["content"], "A");
        assert_eq!(body["files"][1]["path"], "scripts/run.sh");
    }

    // include=metadata：正文换成 size + hash
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/skills/{skill_id}/?include=metadata"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("content").is_none(), "metadata 不带 content");
    assert_eq!(body["content_size"], 4);
    assert_eq!(
        body["content_hash"], "30e7e41efab31a8beaa512ea48aa4d99070496095423d4d653137a7f511eb877",
        "sha256(BODY) 逐字值"
    );
    assert!(body["files"][0].get("content").is_none());
    assert_eq!(body["files"][0]["size"], 1);
    assert!(body["files"][0]["content_hash"].is_string());
    assert!(
        body.get("enabled").is_none(),
        "M6-2 不填 enabled（agent 面才有该字段）"
    );
    assert!(body.get("labels").is_none(), "metadata 分支不查标签");

    // 非法 include ⇒ 400（且在取 skill 之前判）
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/skills/{skill_id}?include=files"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_message(&body),
        r#"invalid include: expected "content" or "metadata""#
    );

    // 更新：改正文 + 整批替换文件
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/skills/{skill_id}/"),
        ws,
        user,
        Some(json!({
            "name": "beta-2",
            "content": "BODY2",
            "files": [{ "path": "g.md", "content": "G" }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "beta-2");
    assert_eq!(body["content"], "BODY2");
    // description 是 `*string` 且本次没给 ⇒ 保持原值
    assert_eq!(body["description"], "second");
    assert_eq!(body["files"].as_array().unwrap().len(), 1);
    assert_eq!(body["files"][0]["path"], "g.md");

    // files 缺席 ⇒ 不动支持文件（nil 与 `[]` 是两条不同分支）
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/skills/{skill_id}"),
        ws,
        user,
        Some(json!({ "description": "third" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["description"], "third");
    assert_eq!(body["files"].as_array().unwrap().len(), 1);

    // `[]` ⇒ 清空
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/skills/{skill_id}"),
        ws,
        user,
        Some(json!({ "files": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["files"], json!([]));

    // 删除：204 空体；再取 404
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/skills/{skill_id}/"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/skills/{skill_id}"),
        ws,
        user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, ws, &[user]).await;
}

/// 创建面校验：必填、路径、保留路径、重名 409、空体/`null` 400。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn create_rejects_missing_name_bad_paths_and_duplicates() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);

    // 字面 `null` ⇒ 上游解码进结构体不报错（留零值）⇒ 落进必填校验；空 body 则是 io.EOF
    let (status, body) = call(&app, "POST", "/api/skills/", ws, user, Some(json!(null))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_message(&body), "name is required");

    let (status, body) = call(&app, "POST", "/api/skills/", ws, user, Some(json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "name is required");

    let (status, body) = call(
        &app,
        "POST",
        "/api/skills/",
        ws,
        user,
        Some(json!({ "name": "bad", "files": [{ "path": "", "content": "x" }] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "invalid file path: ");

    let (status, body) = call(
        &app,
        "POST",
        "/api/skills/",
        ws,
        user,
        Some(json!({ "name": "bad", "files": [{ "path": "../escape", "content": "x" }] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "invalid file path: ../escape");

    // 保留路径**合法**（跳过发生在写库阶段，不是校验阶段）
    let created = create_skill(
        &app,
        ws,
        user,
        json!({ "name": "reserved-ok", "files": [{ "path": "./SKILL.md", "content": "x" }] }),
    )
    .await;
    assert_eq!(created["files"], json!([]));

    // 重名 ⇒ 409（上游 `UNIQUE(workspace_id, name)`）
    let (status, body) = call(
        &app,
        "POST",
        "/api/skills/",
        ws,
        user,
        Some(json!({ "name": "reserved-ok" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "conflict");

    cleanup(&pool, ws, &[user]).await;
}

/// 坏 uuid / 未知 id / 别的 workspace 的 id / 缺 workspace 头。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn unknown_ids_and_bad_uuids_do_not_leak_existence() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let (other_ws, other_user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let created = create_skill(&app, ws, user, json!({ "name": "gamma" })).await;
    let skill_id = id_of(&created);

    // 坏 uuid ⇒ 400（上游 `parseUUIDOrBadRequest(…, "skill id")`）
    let (status, body) = call(&app, "GET", "/api/skills/not-a-uuid/", ws, user, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_message(&body), "skill id must be a valid uuid");

    // 未知 uuid ⇒ 404
    let missing = Uuid::new_v4();
    for method in ["GET", "PUT", "DELETE"] {
        let (status, body) = call(
            &app,
            method,
            &format!("/api/skills/{missing}/"),
            ws,
            user,
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} -> {body}");
        assert_eq!(error_code(&body), "not_found");
    }

    // 同一个 skill id、别的 workspace 的 context ⇒ 404（`WHERE id=$1 AND workspace_id=$2`）
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/skills/{skill_id}"),
        other_ws,
        other_user,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cleanup(&pool, ws, &[user]).await;
    cleanup(&pool, other_ws, &[other_user]).await;
}

/// 读 / 写两套鉴权：读只看 workspace，写要 creator 或 admin，非成员一律 404。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn manage_is_creator_or_admin_only_and_reads_ignore_membership() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, creator) = seed_workspace(&pool, "member").await;
    let plain_member = seed_user(&pool, ws, "member").await;
    let admin = seed_user(&pool, ws, "admin").await;
    // 非成员：另一个 workspace 的人（能过鉴权，但不在本 workspace）
    let (_, outsider) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);
    let created = create_skill(&app, ws, creator, json!({ "name": "delta" })).await;
    let skill_id = id_of(&created);

    // 读：上游 ListSkills / GetSkill 不查成员 ⇒ 非成员也 200（本次不锁紧，逐字照上游）
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/skills/{skill_id}"),
        ws,
        outsider,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 写：同 workspace 的普通成员 ⇒ 403 `only the skill creator can manage this skill`
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/skills/{skill_id}"),
        ws,
        plain_member,
        Some(json!({ "description": "hijack" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        error_message(&body),
        "only the skill creator can manage this skill"
    );

    // 写：非成员 ⇒ 404（上游 `requireWorkspaceRole(..., "skill not found")`）
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/skills/{skill_id}"),
        ws,
        outsider,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "not_found");

    // 写：admin 放行（不需要是创建者）
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/skills/{skill_id}"),
        ws,
        admin,
        Some(json!({ "description": "by admin" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["description"], "by admin");
    assert_eq!(body["created_by"], creator.to_string(), "created_by 不变");

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/skills/{skill_id}"),
        ws,
        creator,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    cleanup(&pool, ws, &[creator, plain_member, admin]).await;
}

/// 搜索：空 query ⇒ 400；非空 query 打 `clawhub.ai`（200 或 502 都算通过，
/// 只把「502 必须是那条扁平体」钉住）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn search_requires_a_query_and_degrades_to_a_flat_502() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
        return;
    };
    let (ws, user) = seed_workspace(&pool, "member").await;
    let app = crate::support::app_with_db(db);

    for uri in [
        "/api/skills/search",
        "/api/skills/search?q=",
        "/api/skills/search?q=%20",
    ] {
        let (status, body) = call(&app, "GET", uri, ws, user, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} -> {body}");
        assert_eq!(error_message(&body), "query is required");
    }

    let (status, body) = call(&app, "GET", "/api/skills/search?q=react", ws, user, None).await;
    match status {
        StatusCode::OK => assert!(body.is_array(), "搜索结果必须是裸数组: {body}"),
        StatusCode::BAD_GATEWAY => {
            // 上游的扁平体（**不是**本仓的 {"error":{...}}）—— 契约就是这条形状
            assert_eq!(body["code"], "upstream_unavailable", "{body}");
            assert!(body["error"].is_string(), "{body}");
        }
        other => panic!("unexpected status {other}: {body}"),
    }

    cleanup(&pool, ws, &[user]).await;
}
