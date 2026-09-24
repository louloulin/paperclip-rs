//! `POST /api/skills/{id}/refresh` 的端到端测试（M6-3 / LUM-1668）。
//!
//! 与 `import.rs` 同一套 mock（同一个进程级端点覆写 ⇒ 同样吃 `support::MOCK_LOCK`）。
//! 刷新是「按 `config.origin.source_url` 重新取件并原地覆盖」，所以这里重点断言**什么被保留**：
//! `id` / `created_by` / `created_at` / 标签连接行 / 非 `origin` 的 config 键。

use axum::http::StatusCode;
use mc_core::hash::ContentHash;
use serde_json::json;
use uuid::Uuid;

use super::support::{
    app_with_db, call, clawhub_mock, cleanup, connect, create_skill, files_of, id_of,
    linked_label_count, seed_label, seed_user, seed_workspace, serve_mock, set_source_endpoints,
    skill_config, MockClawhub, SourceEndpoints, MOCK_LOCK,
};

fn skill_md(name: &str, version: &str) -> String {
    format!("---\nname: {name}\ndescription: from ClawHub {version}\n---\n\n# ClawHub {version}\n")
}

fn refresh_uri(skill_id: Uuid) -> String {
    format!("/api/skills/{skill_id}/refresh")
}

/// `GET /api/skills/{id}?include=metadata`（正文换成 `size` + `content_hash` 的形态）。
async fn skill_metadata(
    app: &axum::Router,
    skill_id: Uuid,
    workspace_id: Uuid,
    user_id: Uuid,
) -> serde_json::Value {
    let (status, body) = call(
        app,
        "GET",
        &format!("/api/skills/{skill_id}?include=metadata"),
        workspace_id,
        user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "metadata failed: {body}");
    body
}

/// 挂一条标签连接行 + 一个用户自定义 `config` 键（两者都必须活过刷新）。
async fn seed_label_and_config(pool: &sqlx::PgPool, skill_id: Uuid, workspace_id: Uuid) {
    let label_id = seed_label(pool, workspace_id, "skill").await;
    sqlx::query("INSERT INTO skill_to_label(skill_id, label_id) VALUES ($1, $2)")
        .bind(skill_id)
        .bind(label_id)
        .execute(pool)
        .await
        .expect("insert skill_to_label");
    sqlx::query(
        "UPDATE skill SET config = config || '{\"temperature\": 0.5}'::jsonb WHERE id = $1",
    )
    .bind(skill_id)
    .execute(pool)
    .await
    .expect("patch config");
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn refresh_replaces_content_from_the_origin_and_preserves_identity_and_labels() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let _serial = MOCK_LOCK.lock().await;
    let mock = MockClawhub::new("demo-skill", "summary").with_files(&[
        ("SKILL.md", &skill_md("demo-skill", "v1")),
        ("notes.md", "v1"),
    ]);
    let base = serve_mock("127.0.0.1", clawhub_mock(mock.clone())).await;
    set_source_endpoints(Some(SourceEndpoints {
        clawhub_api: format!("{base}/api/v1"),
        ..SourceEndpoints::default()
    }));

    let app = app_with_db(db);
    let (workspace_id, creator) = seed_workspace(&pool, "member").await;
    let (status, imported) = call(
        &app,
        "POST",
        "/api/skills/import",
        workspace_id,
        creator,
        Some(json!({"url": "clawhub.ai/demo-skill", "on_conflict": "fail"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "import failed: {imported}");
    let skill_id = id_of(&imported["skill"]);
    let created_at = imported["skill"]["created_at"].clone();

    // 刷新前后的 `content_hash`（metadata 形态，SQL 现场算的裸 hex `sha256`）。
    let meta_before = skill_metadata(&app, skill_id, workspace_id, creator).await;
    let hash_before = meta_before["content_hash"].clone();
    assert_eq!(
        hash_before,
        json!(ContentHash::sha256(skill_md("demo-skill", "v1").as_bytes()).as_str())
    );

    // 刷新必须保留的两类旁挂数据：标签连接行 + 用户自己写的 config 键。
    seed_label_and_config(&pool, skill_id, workspace_id).await;

    // 上游改了内容**并**改了名字：刷新采纳改名（唯一会写 `name` 的刷新分支）。
    mock.set_metadata("demo-skill-v2", "from ClawHub v2");
    mock.replace_file("SKILL.md", &skill_md("demo-skill-v2", "v2"));
    mock.replace_file("notes.md", "v2");

    let (status, body) = call(
        &app,
        "POST",
        &refresh_uri(skill_id),
        workspace_id,
        creator,
        None,
    )
    .await;
    set_source_endpoints(None);

    assert_eq!(status, StatusCode::OK, "refresh failed: {body}");
    // 刷新响应是**裸** `SkillWithFilesResponse`（没有 `status` 键）。
    assert!(body.get("status").is_none(), "unexpected body: {body}");
    assert_eq!(body["id"], skill_id.to_string());
    assert_eq!(body["name"], "demo-skill-v2");
    assert_eq!(body["description"], "from ClawHub v2");
    assert_eq!(body["created_at"], created_at, "created_at 必须保留");
    assert_eq!(body["created_by"], creator.to_string());
    assert_eq!(
        files_of(&body),
        vec![("notes.md".to_string(), "v2".to_string())]
    );

    // `content_hash` 跟着正文走（且与 SQL 侧同一口径：裸 hex，无 `sha256:` 前缀）。
    let next_content = skill_md("demo-skill-v2", "v2");
    let meta_after = skill_metadata(&app, skill_id, workspace_id, creator).await;
    assert_ne!(meta_after["content_hash"], hash_before, "哈希必须变");
    assert_eq!(
        meta_after["content_hash"],
        json!(ContentHash::sha256(next_content.as_bytes()).as_str())
    );
    assert_eq!(
        meta_after["content_size"],
        json!(i64::try_from(next_content.len()).expect("content length fits in i64"))
    );

    let config = skill_config(&pool, skill_id).await;
    assert_eq!(config["origin"]["type"], "clawhub");
    assert_eq!(
        config["origin"]["source_url"],
        "https://clawhub.ai/demo-skill"
    );
    assert_eq!(config["temperature"], json!(0.5), "非 origin 键不该被抹掉");

    assert_eq!(
        linked_label_count(&pool, skill_id).await,
        1,
        "标签连接行必须保留"
    );
    // 没有产生第二行。
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM skill WHERE workspace_id = $1")
        .bind(workspace_id)
        .fetch_one(&pool)
        .await
        .expect("count skills");
    assert_eq!(total, 1);

    cleanup(&pool, workspace_id, &[creator]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn refresh_is_rejected_for_non_creators_and_for_foreign_workspaces() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let _serial = MOCK_LOCK.lock().await;
    let mock = MockClawhub::new("demo-skill", "summary")
        .with_files(&[("SKILL.md", &skill_md("demo-skill", "v1"))]);
    let base = serve_mock("127.0.0.1", clawhub_mock(mock)).await;
    set_source_endpoints(Some(SourceEndpoints {
        clawhub_api: format!("{base}/api/v1"),
        ..SourceEndpoints::default()
    }));

    let app = app_with_db(db);
    let (workspace_id, creator) = seed_workspace(&pool, "member").await;
    let (status, imported) = call(
        &app,
        "POST",
        "/api/skills/import",
        workspace_id,
        creator,
        Some(json!({"url": "clawhub.ai/demo-skill", "on_conflict": "fail"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "import failed: {imported}");
    let skill_id = id_of(&imported["skill"]);

    // 同工作区的普通成员（既非创建者也非 admin）⇒ 403；文案与导入覆盖那句**不同**。
    let member = seed_user(&pool, workspace_id, "member").await;
    let (status, body) = call(
        &app,
        "POST",
        &refresh_uri(skill_id),
        workspace_id,
        member,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body["error"],
        "only the skill creator or a workspace admin can update this skill from its source"
    );

    // workspace admin 放行（上游是 `isAdmin || isCreator`；比导入覆盖的「仅创建者」宽）。
    let admin = seed_user(&pool, workspace_id, "admin").await;
    let (status, body) = call(
        &app,
        "POST",
        &refresh_uri(skill_id),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin refresh failed: {body}");
    assert_eq!(body["id"], skill_id.to_string());

    set_source_endpoints(None);

    // 非成员（另一个 workspace 的用户）：skill 不在他的工作区 ⇒ 404（不泄露存在性）。
    let (other_workspace, outsider) = seed_workspace(&pool, "owner").await;
    let (status, body) = call(
        &app,
        "POST",
        &refresh_uri(skill_id),
        other_workspace,
        outsider,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    // 扁平体里的文案来自 `mc_errors` 的 `Display`（带内部前缀）—— 与 M6-2 的
    // `crud/files/labels` 同一条已登记偏离（`docs/32` §9.6）。
    assert_eq!(body["error"], "not found: skill");

    cleanup(&pool, workspace_id, &[creator, member, admin]).await;
    cleanup(&pool, other_workspace, &[outsider]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn refresh_rejects_skills_without_a_refreshable_origin() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;
    let not_refreshable =
        "this skill was not imported from a refreshable source (GitHub, skills.sh, or ClawHub)";

    // ① 手写 skill（`config` 是 `{}`）⇒ 422（**不是** 400：桩注释曾写错，已按上游更正）。
    let hand_written = create_skill(
        &app,
        workspace_id,
        user_id,
        json!({"name": "hand-written", "description": "d", "content": "c"}),
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        &refresh_uri(id_of(&hand_written)),
        workspace_id,
        user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(body["error"], not_refreshable);

    // ② 归档导入（有 skill 但没有来源 URL）⇒ 422。
    let archived = create_skill(
        &app,
        workspace_id,
        user_id,
        json!({"name": "archived", "description": "d", "content": "c"}),
    )
    .await;
    let archived_id = id_of(&archived);

    // ③ origin 类型可刷新、但 `source_url` 指向**另一个**源 ⇒ 422（防手改 config 越源）。
    sqlx::query("UPDATE skill SET config = $2::jsonb WHERE id = $1")
        .bind(archived_id)
        .bind(
            json!({"origin": {"type": "github", "source_url": "clawhub.ai/demo-skill"}})
                .to_string(),
        )
        .execute(&pool)
        .await
        .expect("patch config");
    let (status, body) = call(
        &app,
        "POST",
        &refresh_uri(archived_id),
        workspace_id,
        user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(body["error"], not_refreshable);

    // ④ origin 类型根本不可刷新（`archive`）⇒ 422。
    sqlx::query("UPDATE skill SET config = $2::jsonb WHERE id = $1")
        .bind(archived_id)
        .bind(
            json!({"origin": {"type": "archive", "source_url": "clawhub.ai/demo-skill"}})
                .to_string(),
        )
        .execute(&pool)
        .await
        .expect("patch config");
    let (status, body) = call(
        &app,
        "POST",
        &refresh_uri(archived_id),
        workspace_id,
        user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(body["error"], not_refreshable);

    // ⑤ 空 `source_url` ⇒ 422（上游把这条也折进同一句）。
    sqlx::query("UPDATE skill SET config = $2::jsonb WHERE id = $1")
        .bind(archived_id)
        .bind(json!({"origin": {"type": "clawhub", "source_url": "  "}}).to_string())
        .execute(&pool)
        .await
        .expect("patch config");
    let (status, body) = call(
        &app,
        "POST",
        &refresh_uri(archived_id),
        workspace_id,
        user_id,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(body["error"], not_refreshable);

    cleanup(&pool, workspace_id, &[user_id]).await;
}
