//! `POST /api/skills/import` 的端到端测试（M6-3 / LUM-1668）。
//!
//! 出网取件被换成本用例自己起的 mock 源站（`support::serve_mock` + `set_source_endpoints`），
//! 这与上游用 `httptest` 替换包级 `clawHubAPIBase` 是同一手法 —— 只是 Rust 侧走进程级
//! `OnceLock<RwLock<..>>`，所以**依赖 mock 的用例必须串行**（`support::MOCK_LOCK`）。
//!
//! 覆盖：JSON+ClawHub 成功 / 结构化与非结构化两种响应形态 / 四策略冲突 / 入参与源错误码 /
//! multipart 归档导入 / GitHub 递归 tree + raw 支持文件 / 取件失败的 413·502·503 映射。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

use super::support::{
    app_with_db, call, call_raw, clawhub_mock, cleanup, connect, create_skill, files_of,
    github_api_mock, github_raw_mock, id_of, multipart_req, seed_user, seed_workspace, serve_mock,
    set_source_endpoints, skill_config, MockClawhub, MockGithub, SourceEndpoints, MOCK_LOCK,
    USER_ID_HEADER, WORKSPACE_HEADER,
};

const IMPORT: &str = "/api/skills/import";

/// 装 mock ClawHub：只覆写 clawhub 端点，GitHub 侧维持默认（本用例不出网）。
fn point_at_clawhub(base: &str) {
    set_source_endpoints(Some(SourceEndpoints {
        clawhub_api: format!("{base}/api/v1"),
        ..SourceEndpoints::default()
    }));
}

fn demo_skill_md() -> &'static str {
    "---\nname: demo-skill\ndescription: A demo skill from ClawHub\n---\n\n# Demo\n"
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn json_import_from_clawhub_returns_the_raw_bundle_when_on_conflict_is_absent() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let _serial = MOCK_LOCK.lock().await;
    let mock = MockClawhub::new("demo-skill", "A demo skill from ClawHub")
        .with_files(&[("SKILL.md", demo_skill_md()), ("notes.md", "extra")]);
    let base = serve_mock("127.0.0.1", clawhub_mock(mock)).await;
    point_at_clawhub(&base);
    let app = app_with_db(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        user_id,
        Some(json!({"url": "clawhub.ai/demo-skill"})),
    )
    .await;
    set_source_endpoints(None);

    assert_eq!(status, StatusCode::CREATED, "import failed: {body}");
    // 没有 `on_conflict` ⇒ 旧客户端契约：裸 `SkillWithFilesResponse`（**没有** `status` 键）。
    assert!(
        body.get("status").is_none(),
        "unexpected structured body: {body}"
    );
    assert_eq!(body["name"], "demo-skill");
    assert_eq!(body["description"], "A demo skill from ClawHub");
    assert_eq!(body["content"], demo_skill_md());
    // `SKILL.md` 进 `skill.content`，**不**进 `skill_file`。
    assert_eq!(
        files_of(&body),
        vec![("notes.md".to_string(), "extra".to_string())]
    );

    let skill_id = id_of(&body);
    let config = skill_config(&pool, skill_id).await;
    assert_eq!(config["origin"]["type"], "clawhub");
    // `detect_import_source` 会把裸 slug 补成规范 URL，落库的是**规范化**后的那个。
    assert_eq!(
        config["origin"]["source_url"],
        "https://clawhub.ai/demo-skill"
    );
    assert_eq!(config["origin"]["slug"], "demo-skill");

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn explicit_on_conflict_switches_to_the_structured_result() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let _serial = MOCK_LOCK.lock().await;
    let mock =
        MockClawhub::new("demo-skill", "summary").with_files(&[("SKILL.md", demo_skill_md())]);
    let base = serve_mock("127.0.0.1", clawhub_mock(mock)).await;
    point_at_clawhub(&base);
    let app = app_with_db(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "member").await;

    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        user_id,
        Some(json!({"url": "clawhub.ai/demo-skill", "on_conflict": "fail"})),
    )
    .await;
    set_source_endpoints(None);

    assert_eq!(status, StatusCode::CREATED, "import failed: {body}");
    // 显式传了 `on_conflict` ⇒ `{"status":"created","skill":{…}}`。
    assert_eq!(body["status"], "created");
    assert_eq!(body["skill"]["name"], "demo-skill");
    assert_eq!(body["skill"]["content"], demo_skill_md());
    // `reason` / `existing_skill` 是 `omitempty`，成功路径不该出现。
    assert!(body.get("reason").is_none());
    assert!(body.get("existing_skill").is_none());

    cleanup(&pool, workspace_id, &[user_id]).await;
}

/// 建一个「库里已经有同名手写 skill」的场景，并返回 `(app, workspace, 创建者, 已有行 id)`。
///
/// 调用方必须已经持有 `MOCK_LOCK` 并把端点指向 `base`。
async fn duplicate_scene(
    pool: &sqlx::PgPool,
    db: mc_db::Db,
    base: &str,
    mock: &Arc<MockClawhub>,
) -> (Router, Uuid, Uuid, Uuid) {
    let _ = mock;
    point_at_clawhub(base);
    let app = app_with_db(db);
    let (workspace_id, creator) = seed_workspace(pool, "member").await;
    let first = create_skill(
        &app,
        workspace_id,
        creator,
        json!({"name": "demo-skill", "description": "hand written", "content": "local"}),
    )
    .await;
    (app, workspace_id, creator, id_of(&first))
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn duplicate_import_reports_the_existing_skill_in_both_shapes() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let _serial = MOCK_LOCK.lock().await;
    let mock = MockClawhub::new("demo-skill", "summary")
        .with_files(&[("SKILL.md", demo_skill_md()), ("notes.md", "v1")]);
    let base = serve_mock("127.0.0.1", clawhub_mock(mock.clone())).await;
    let (app, workspace_id, creator, first_id) = duplicate_scene(&pool, db, &base, &mock).await;

    // ① unstructured + 重名 ⇒ 上游那个「带 existing_skill 的扁平 409」。
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        creator,
        Some(json!({"url": "clawhub.ai/demo-skill"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "a skill with this name already exists");
    assert_eq!(body["existing_skill"]["id"], first_id.to_string());
    // 旧形态这条分支**不知道调用者是谁**（上游 `existingSkillIdentityByName` 传空 userID）
    // ⇒ `can_overwrite` 恒为 false，`omitempty` 下整个键都不出现。
    assert!(body["existing_skill"].get("can_overwrite").is_none());

    // ② `fail` ⇒ 409 + `status: conflict` + 上游原因串（逐字）。
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        creator,
        Some(json!({"url": "clawhub.ai/demo-skill", "on_conflict": "fail"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["status"], "conflict");
    assert_eq!(
        body["reason"],
        "a skill with this name already exists; use --on-conflict overwrite to replace it or --on-conflict rename to import a copy"
    );
    assert_eq!(body["existing_skill"]["can_overwrite"], true);

    set_source_endpoints(None);
    cleanup(&pool, workspace_id, &[creator]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn skip_overwrite_and_rename_follow_the_conflict_matrix() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let _serial = MOCK_LOCK.lock().await;
    let mock = MockClawhub::new("demo-skill", "summary")
        .with_files(&[("SKILL.md", demo_skill_md()), ("notes.md", "v1")]);
    let base = serve_mock("127.0.0.1", clawhub_mock(mock.clone())).await;
    let (app, workspace_id, creator, first_id) = duplicate_scene(&pool, db, &base, &mock).await;
    let outsider = seed_user(&pool, workspace_id, "member").await;

    // ③ `skip` ⇒ 200 + 已存在的 id（**不**写库）。
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        creator,
        Some(json!({"url": "clawhub.ai/demo-skill", "on_conflict": "skip"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "skipped");
    assert_eq!(body["reason"], "a skill with this name already exists");

    // ④ 非创建者 `overwrite` ⇒ 403（导入覆盖是**仅创建者**，比刷新那条判权窄）。
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        outsider,
        Some(json!({"url": "clawhub.ai/demo-skill", "on_conflict": "overwrite"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["status"], "failed");
    assert_eq!(
        body["reason"],
        "only the skill creator can overwrite this skill"
    );

    // ⑤ 创建者 `overwrite` ⇒ 200 + `status: updated`，并**原地**替换内容与来源。
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        creator,
        Some(json!({"url": "clawhub.ai/demo-skill", "on_conflict": "overwrite"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "overwrite failed: {body}");
    assert_eq!(body["status"], "updated");
    let updated_id = id_of(&body["skill"]);
    assert_eq!(updated_id, first_id, "overwrite must keep the same row");
    assert_eq!(body["skill"]["content"], demo_skill_md());
    assert_eq!(
        files_of(&body["skill"]),
        vec![("notes.md".to_string(), "v1".to_string())]
    );
    assert_eq!(
        skill_config(&pool, updated_id).await["origin"]["source_url"],
        "https://clawhub.ai/demo-skill"
    );

    // ⑥ `rename` ⇒ 201 + `-2` 后缀 + 上游原因串，原行不动。
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        creator,
        Some(json!({"url": "clawhub.ai/demo-skill", "on_conflict": "rename"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "rename failed: {body}");
    assert_eq!(body["status"], "created");
    assert_eq!(body["reason"], "renamed to avoid an existing skill");
    assert_eq!(body["skill"]["name"], "demo-skill-2");
    assert_ne!(id_of(&body["skill"]), first_id);
    assert_eq!(body["existing_skill"]["id"], first_id.to_string());

    set_source_endpoints(None);
    cleanup(&pool, workspace_id, &[creator, outsider]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn request_validation_matches_the_upstream_status_codes() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    // 字面 `null` 是**另一条**分支：上游 `json.NewDecoder` 把它解成零值结构体，于是走到
    // 「url 为空」的源判定（400 但文案不同）。
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        user_id,
        Some(json!(null)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!body["error"].as_str().unwrap_or("").is_empty());

    // 数组 / 字段类型不对：`json.NewDecoder` 只接受对象。
    for body in [Some(json!([1, 2])), Some(json!({"url": 7}))] {
        let (status, json_body) = call(&app, "POST", IMPORT, workspace_id, user_id, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {json_body}");
        assert_eq!(json_body["error"], "invalid request body");
    }
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(IMPORT)
                .header(USER_ID_HEADER, user_id.to_string())
                .header(WORKSPACE_HEADER, workspace_id.to_string())
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .expect("request"),
        )
        .await
        .expect("router call");
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // `on_conflict` 不在四值内 ⇒ 400（文案逐字）。
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        user_id,
        Some(json!({"url": "clawhub.ai/demo-skill", "on_conflict": "merge"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"],
        "on_conflict must be one of: fail, overwrite, rename, skip"
    );

    // 源不认（不支持的主机 / 空 url）⇒ 400，且**早于**任何出网。
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        user_id,
        Some(json!({"url": "https://example.com/bundle.zip"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"],
        "unsupported source: example.com (supported: clawhub.ai, skills.sh, github.com)"
    );

    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        user_id,
        Some(json!({"url": ""})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!body["error"].as_str().unwrap_or("").is_empty());

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn archive_upload_imports_a_local_zip_and_is_always_structured() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    let archive = super::zipfixture::zip_store(&[
        (
            "bundle/SKILL.md",
            "---\nname: archive-skill\ndescription: from a zip\n---\n\n# Archive\n",
        ),
        ("bundle/refs/guide.md", "guide"),
    ]);
    let request = multipart_req(
        IMPORT,
        workspace_id,
        user_id,
        "fail",
        "bundle.zip",
        &archive,
    );
    let (status, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::CREATED, "archive import failed: {body}");
    // 归档路径**永远**结构化（上游注释：没有需要兼容的旧客户端）。
    assert_eq!(body["status"], "created");
    assert_eq!(body["skill"]["name"], "archive-skill");
    assert_eq!(body["skill"]["description"], "from a zip");
    assert_eq!(
        files_of(&body["skill"]),
        vec![("refs/guide.md".to_string(), "guide".to_string())]
    );
    // 归档导入**没有**可回溯的 URL ⇒ `config` 是 `{}`（刷新这类 skill 会 422）。
    assert_eq!(skill_config(&pool, id_of(&body["skill"])).await, json!({}));

    // 归档路径的上限违例是 **400**（上游 `writeError(400, err.Error())`），不是 413。
    // frontmatter 没写 name 也没有包装目录时，回落上传文件名（去掉扩展名）。
    let unnamed = super::zipfixture::zip_store(&[("SKILL.md", "# no frontmatter name")]);
    let request = multipart_req(
        IMPORT,
        workspace_id,
        user_id,
        "fail",
        "local-bundle.zip",
        &unnamed,
    );
    let (status, body) = call_raw(&app, request).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "unnamed archive should fall back: {body}"
    );
    assert_eq!(body["skill"]["name"], "local-bundle");

    // 不是 zip ⇒ 400（包本身不合法）。
    let request = multipart_req(
        IMPORT,
        workspace_id,
        user_id,
        "fail",
        "bundle.zip",
        b"not a zip",
    );
    let (status, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(!body["error"].as_str().unwrap_or("").is_empty());

    // 缺 `file` 字段 ⇒ 400。
    let request = Request::builder()
        .method("POST")
        .uri(IMPORT)
        .header(USER_ID_HEADER, user_id.to_string())
        .header(WORKSPACE_HEADER, workspace_id.to_string())
        .header("content-type", "multipart/form-data; boundary=----empty")
        .body(Body::from("------empty--\r\n"))
        .expect("request");
    let (status, body) = call_raw(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(
        body["error"],
        "a skill archive file is required (form field \"file\")"
    );

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn github_import_pulls_the_tree_and_the_raw_supporting_files() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let _serial = MOCK_LOCK.lock().await;
    // api 与 raw 用**两个主机名**（127.0.0.1 / localhost）：`GITHUB_TOKEN` 出站闸门比较的正是
    // 主机名，这是唯一能在本机同时验证「向 raw 带 token」与「不向别的站带 token」的形态。
    let github = Arc::new(
        MockGithub::new()
            .with_file(
                "skills/demo/SKILL.md",
                "---\nname: github-skill\ndescription: from GitHub\n---\n\n# GitHub\n",
            )
            .with_file("skills/demo/reference.md", "reference"),
    );
    let api_base = serve_mock("127.0.0.1", github_api_mock(github.clone())).await;
    let raw_base = serve_mock("localhost", github_raw_mock(github)).await;
    set_source_endpoints(Some(SourceEndpoints {
        github_api: api_base,
        github_raw: raw_base,
        ..SourceEndpoints::default()
    }));

    let app = app_with_db(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        user_id,
        Some(json!({"url": "https://github.com/acme/skills/tree/main/skills/demo"})),
    )
    .await;
    set_source_endpoints(None);

    assert_eq!(status, StatusCode::CREATED, "github import failed: {body}");
    assert_eq!(body["name"], "github-skill");
    assert_eq!(body["description"], "from GitHub");
    assert_eq!(
        files_of(&body),
        vec![("reference.md".to_string(), "reference".to_string())]
    );
    let config = skill_config(&pool, id_of(&body)).await;
    assert_eq!(config["origin"]["type"], "github");
    assert_eq!(config["origin"]["owner"], "acme");
    assert_eq!(config["origin"]["repo"], "skills");
    assert_eq!(config["origin"]["ref"], "main");
    assert_eq!(config["origin"]["path"], "skills/demo");

    cleanup(&pool, workspace_id, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn fetch_failures_map_to_the_upstream_status_codes() {
    let Some((pool, db)) = connect().await else {
        eprintln!("skipped: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let _serial = MOCK_LOCK.lock().await;
    let app = app_with_db(db);
    let (workspace_id, user_id) = seed_workspace(&pool, "owner").await;

    // ① ClawHub 404 ⇒ 502（上游把「skill not found」当普通取件错误）。
    let missing = serve_mock(
        "127.0.0.1",
        Router::new().fallback(|| async { (StatusCode::NOT_FOUND, "nope").into_response() }),
    )
    .await;
    point_at_clawhub(&missing);
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        user_id,
        Some(json!({"url": "clawhub.ai/missing-skill"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "body: {body}");
    assert_eq!(body["error"], "skill not found on ClawHub: missing-skill");

    // ② 单文件超过 1 MiB ⇒ 413，且**整包失败**（不落一个残缺的包）。
    let huge = MockClawhub::new("demo-skill", "summary").with_files(&[
        ("SKILL.md", demo_skill_md()),
        ("big.txt", &"x".repeat(1024 * 1024 + 1)),
    ]);
    let big_base = serve_mock("127.0.0.1", clawhub_mock(huge)).await;
    point_at_clawhub(&big_base);
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        user_id,
        Some(json!({"url": "clawhub.ai/demo-skill"})),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "body: {body}");
    assert!(body["error"].as_str().unwrap_or("").contains("big.txt"));

    // ③ tree 被 GitHub 截断 ⇒ 503 可重试（本仓不移植上游的逐目录爬取回落）。
    let github = Arc::new(
        MockGithub::new()
            .with_file("SKILL.md", demo_skill_md())
            .truncated(true),
    );
    let api_base = serve_mock("127.0.0.1", github_api_mock(github.clone())).await;
    let raw_base = serve_mock("localhost", github_raw_mock(github)).await;
    set_source_endpoints(Some(SourceEndpoints {
        github_api: api_base,
        github_raw: raw_base,
        ..SourceEndpoints::default()
    }));
    let (status, body) = call(
        &app,
        "POST",
        IMPORT,
        workspace_id,
        user_id,
        Some(json!({"url": "https://github.com/acme/skills/tree/main"})),
    )
    .await;
    set_source_endpoints(None);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
    assert!(body["error"].as_str().unwrap_or("").contains("too large"));

    cleanup(&pool, workspace_id, &[user_id]).await;
}
