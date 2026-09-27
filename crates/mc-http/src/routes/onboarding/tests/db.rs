//! M9-3 真库那一半（门 ⑥）：真库 + 直读列断言（`docs/62` §4.2 / §9.7 的对齐纪律）。
//!
//! 未设 `MULTICA_TEST_DATABASE_URL` ⇒ 打印跳过并 `return`；**设了但连不上 ⇒ panic**
//! （不许静默假装绿，与 `routes/cloud/subscriptions/tests/db.rs` 同款）。
//!
//! 拆出来是门 ⑩（单文件 800 行）的要求，先例 = `routes/cloud/subscriptions/tests/db.rs`。
//!
//! # 本文件的判据纪律（与 M9-2 相反的一点）
//!
//! M9-2 的对齐判据是「出站请求逐字」，本片**没有出站** ⇒ 判据全部落在
//! **直读列**上：
//! - `complete` 的幂等 ⇒ 直读 `"user".onboarded_at`，两次调用**逐字相等**；
//! - waitlist ⇒ 直读 `"user".cloud_waitlist_{email,reason}`（`DoD` 第 2 条逐字要求）；
//! - 两条 shim 的 provision 链 ⇒ 直读 `agent` / `issue` / `starter_content_state`。
//!
//! **任何一处都不得**拿响应体当「写进去了」的证据。

use serde_json::{json, Value};
use uuid::Uuid;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use mc_db::Db;

use super::support::*;

// 本文件是 `tests.rs` 的孙模块 ⇒ `super::super` 是 `profile`；shim 面用**绝对**路径取。
use crate::routes::onboarding::shim::shim_content;
use crate::routes::onboarding::shim::{
    BootstrapOnboardingNoRuntimeResponse, BootstrapOnboardingRuntimeResponse,
};

struct Seed {
    workspace: Uuid,
    owner: Uuid,
    outsider: Uuid,
    runtime_public: Uuid,
    runtime_private_owned: Uuid,
    runtime_private_other: Uuid,
}

/// 一个 workspace + owner + 外人 + 三个 runtime（public / 自己私有的 / 别人私有的）。
async fn seed(db: &Db) -> Seed {
    let tag = Uuid::new_v4().simple().to_string();
    let workspace: Uuid =
        sqlx::query_scalar("INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id")
            .bind(format!("itest-m93-{tag}"))
            .bind(format!("itest-m93-{tag}"))
            .fetch_one(db.pool())
            .await
            .expect("insert workspace");
    let owner = new_user(db, &tag, "owner").await;
    let outsider = new_user(db, &tag, "out").await;
    for (user, role) in [(owner, "owner"), (outsider, "member")] {
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(workspace)
            .bind(user)
            .bind(role)
            .execute(db.pool())
            .await
            .expect("insert member");
    }
    Seed {
        workspace,
        owner,
        outsider,
        runtime_public: new_runtime(db, workspace, "pub", Some(owner), "public").await,
        runtime_private_owned: new_runtime(db, workspace, "own", Some(owner), "private").await,
        runtime_private_other: new_runtime(db, workspace, "other", Some(outsider), "private").await,
    }
}

async fn new_user(db: &Db, tag: &str, role_tag: &str) -> Uuid {
    sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
        .bind(format!("itest-m93-{tag}"))
        .bind(format!("itest-m93-{role_tag}-{tag}@example.com"))
        .fetch_one(db.pool())
        .await
        .expect("insert user")
}

async fn new_runtime(db: &Db, ws: Uuid, name: &str, owner: Option<Uuid>, vis: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime (workspace_id, name, runtime_mode, provider, owner_id, \
             visibility) VALUES ($1, $2, 'local', 'codex', $3, $4) RETURNING id",
    )
    .bind(ws)
    .bind(format!("itest-m93-{name}"))
    .bind(owner)
    .bind(vis)
    .fetch_one(db.pool())
    .await
    .expect("insert runtime")
}

async fn onboarded_at(db: &Db, user: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar(r#"SELECT onboarded_at FROM "user" WHERE id = $1"#)
        .bind(user)
        .fetch_one(db.pool())
        .await
        .expect("read onboarded_at")
}

async fn waitlist_columns(db: &Db, user: Uuid) -> (Option<String>, Option<String>) {
    sqlx::query_as(
        r#"SELECT cloud_waitlist_email, cloud_waitlist_reason FROM "user" WHERE id = $1"#,
    )
    .bind(user)
    .fetch_one(db.pool())
    .await
    .expect("read waitlist columns")
}

/// `POST /api/me/onboarding/complete` 的**幂等**（`DoD` 第 1 条）：重复调用**不改状态**。
///
/// 判据 = **直读** `onboarded_at` 那一列，两次调用后**逐字相等**。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn complete_is_idempotent_and_preserves_the_first_timestamp() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone());

    let (status, bytes) = send(
        &app,
        &Call::new("POST", "/api/me/onboarding/complete", seed.owner).no_body(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    let first: DateTime<Utc> = onboarded_at(&db, seed.owner)
        .await
        .expect("first complete wrote onboarded_at");
    // 响应里的 `onboarded_at` 与库里那一列是**同一个时刻**（不是 handler 自说自话；
    // ⚠️ 两边的**格式**不同：库里是 PG 的 text，响应是 RFC3339 ⇒ 比的是时刻不是字符串）。
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(json_body(&bytes)["onboarded_at"].as_str().unwrap())
            .expect("rfc3339")
            .with_timezone(&Utc),
        first
    );

    let (status, bytes) = send(
        &app,
        &Call::new("POST", "/api/me/onboarding/complete", seed.owner)
            .body(json!({"completion_path": "full"}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    let second: DateTime<Utc> = onboarded_at(&db, seed.owner).await.expect("still set");
    assert_eq!(first, second, "重复调用改了 onboarded_at（幂等被破坏）");
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(json_body(&bytes)["onboarded_at"].as_str().unwrap())
            .expect("rfc3339")
            .with_timezone(&Utc),
        second
    );
}

/// `complete` 的体是**可选**的；畸形 `workspace_id` fail fast（400），且**不写**任何东西。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn complete_accepts_an_empty_body_and_rejects_a_malformed_workspace_id() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone());

    // 空体 = 合法 legacy 调用。
    let (status, _) = send(
        &app,
        &Call::new("POST", "/api/me/onboarding/complete", seed.outsider).no_body(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(onboarded_at(&db, seed.outsider).await.is_some());

    // 畸形 `workspace_id` ⇒ 400，且 `onboarded_at` **不动**。
    let (status, bytes) = send(
        &app,
        &Call::new("POST", "/api/me/onboarding/complete", seed.owner)
            .body(json!({"workspace_id": "not-a-uuid"}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bytes:?}");
    assert!(onboarded_at(&db, seed.owner).await.is_none());
}

/// `PATCH /api/me/onboarding`：v2 形状**逐字段**落库；缺 `role` / `use_case` 照样 200；
/// 省略 `questionnaire` **保留**旧值（`DoD` 第 1 条）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_questionnaire_round_trips_field_by_field_and_absent_means_keep() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone());
    let path = "/api/me/onboarding";

    let v2 = json!({
        "source": ["search"],
        "source_other": "",
        "source_skipped": false,
        "role": "engineer",
        "role_other": "",
        "role_skipped": false,
        "use_case": ["ship_code", "manage_team"],
        "use_case_other": "",
        "use_case_skipped": false,
        "version": 2,
    });
    let (status, bytes) = send(
        &app,
        &Call::new("PATCH", path, seed.owner).body(json!({"questionnaire": v2}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    // 🔴 直读那一列，逐字段比对（`094` 的 v2 形状）。
    // 🔴 判据只落在**直读那一列**上，**不**比响应体的 `onboarding_questionnaire`
    // （`MeResponse` 的那个字段目前投影的是本地独有列 `onboarding_state`，见
    // `docs/32` §48 / §9.18 登记的 D-3；上游 `userToResponse` 投影的是真列）。
    let stored = questionnaire(&db, seed.owner).await;
    for (key, value) in [
        ("source", json!(["search"])),
        ("role", json!("engineer")),
        ("use_case", json!(["ship_code", "manage_team"])),
        ("use_case_skipped", json!(false)),
        ("version", json!(2)),
    ] {
        assert_eq!(stored[key], value, "字段 {key}");
    }

    // 缺 `role` / `use_case` ⇒ **仍然 200**，逐字落库（上游不校验；见 `profile.rs` 的登记）。
    let partial = json!({"source": ["search"], "version": 2});
    let (status, bytes) = send(
        &app,
        &Call::new("PATCH", path, seed.owner).body(json!({"questionnaire": partial}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "上游对缺项是 200：{bytes:?}");
    let stored = questionnaire(&db, seed.owner).await;
    assert!(stored.get("role").is_none() && stored.get("use_case").is_none());
    assert_eq!(stored["source"], json!(["search"]));

    // 省略 `questionnaire` ⇒ `COALESCE(NULL, col)` ⇒ **保留**上一次的值。
    let (status, _) = send(
        &app,
        &Call::new("PATCH", path, seed.owner).body(json!({}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        questionnaire(&db, seed.owner).await["source"],
        json!(["search"])
    );
}

/// waitlist：**直读两列**与请求体逐字比对（`DoD` 第 2 条 / `docs/62` §9.7）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_waitlist_columns_are_asserted_by_direct_read_not_by_the_response() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone());
    let path = "/api/me/onboarding/cloud-waitlist";

    // 规范化：邮箱小写 + trim；reason 只 trim。
    let (status, bytes) = send(
        &app,
        &Call::new("POST", path, seed.owner)
            .body(json!({"email": "  User@Example.TEST ", "reason": "  want cloud  "}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    // 🔴 直读：与**请求体**逐字比对（不是与响应体）。
    let (email, reason) = waitlist_columns(&db, seed.owner).await;
    assert_eq!(email.as_deref(), Some("user@example.test"));
    assert_eq!(reason.as_deref(), Some("want cloud"));

    // 加入 waitlist **不**等于完成 onboarding（上游逐字）。
    assert!(onboarded_at(&db, seed.owner).await.is_none());

    // 重复调用 ⇒ **覆盖** email + reason（上游逐字）。
    let (status, _) = send(
        &app,
        &Call::new("POST", path, seed.owner)
            .body(json!({"email": "second@example.test"}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (email, reason) = waitlist_columns(&db, seed.owner).await;
    assert_eq!(email.as_deref(), Some("second@example.test"));
    // 空 reason ⇒ `NULL`（不是空串）。
    assert_eq!(reason, None);

    // 非法邮箱 ⇒ 400 且**两列不动**（上一轮的值仍在）。
    let (status, _) = send(
        &app,
        &Call::new("POST", path, seed.owner).body(json!({"email": "nope"}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (email, _) = waitlist_columns(&db, seed.owner).await;
    assert_eq!(email.as_deref(), Some("second@example.test"));
}

/// `POST /api/me/onboarding/runtime-bootstrap` 的 provision 链**逐行**断言（`DoD` 第 3 条）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_runtime_bootstrap_provision_chain_is_asserted_row_by_row() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone());
    let path = "/api/me/onboarding/runtime-bootstrap";
    let body = json!({
        "workspace_id": seed.workspace.to_string(),
        "runtime_id": seed.runtime_private_owned.to_string(),
        "starter_prompt": "帮我先把项目骨架搭起来",
    })
    .to_string();

    let (status, bytes) = send(
        &app,
        &Call::new("POST", path, seed.owner).body(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    let first: BootstrapOnboardingRuntimeResponse =
        serde_json::from_slice(&bytes).expect("runtime-bootstrap response");
    assert_eq!(first.workspace_id, seed.workspace.to_string());

    // ① Helper agent：名字 / visibility / runtime 绑定 / owner / 并发上限 —— 逐列直读。
    let agent: (String, String, Uuid, Uuid, i32, String) = sqlx::query_as(
        "SELECT name, visibility, runtime_id, owner_id, max_concurrent_tasks, instructions \
         FROM agent WHERE id = $1",
    )
    .bind(Uuid::parse_str(&first.agent_id).expect("agent uuid"))
    .fetch_one(db.pool())
    .await
    .expect("helper agent");
    assert_eq!(agent.0, shim_content::ONBOARDING_ASSISTANT_NAME);
    assert_eq!(agent.1, "workspace");
    assert_eq!(agent.2, seed.runtime_private_owned);
    assert_eq!(agent.3, seed.owner);
    assert_eq!(agent.4, 6);
    assert_eq!(agent.5, shim_content::ONBOARDING_ASSISTANT_INSTRUCTIONS);

    // ② starter issue：标题（去重的键）/ 正文（被 `starter_prompt` 整体替换）/ 指派给 Helper。
    let issue: (
        String,
        Option<String>,
        Option<String>,
        Option<Uuid>,
        String,
        Uuid,
    ) = sqlx::query_as(
        "SELECT title, description, assignee_type, assignee_id, status, creator_id \
         FROM issue WHERE id = $1",
    )
    .bind(Uuid::parse_str(&first.issue_id).expect("issue uuid"))
    .fetch_one(db.pool())
    .await
    .expect("starter issue");
    assert_eq!(issue.0, shim_content::ONBOARDING_ISSUE_TITLE);
    assert_eq!(issue.1.as_deref(), Some("帮我先把项目骨架搭起来"));
    assert_eq!(issue.2.as_deref(), Some("agent"));
    assert_eq!(
        issue.3,
        Some(Uuid::parse_str(&first.agent_id).expect("agent uuid"))
    );
    assert_eq!(issue.4, "todo");
    // `creator_id` **恒**是调用者（与 `assignee_id` 可以不同）。
    assert_eq!(issue.5, seed.owner);

    // ③ 标记完成 + ④ starter content：`NULL → 'imported'`。
    assert!(onboarded_at(&db, seed.owner).await.is_some());
    let starter: Option<String> =
        sqlx::query_scalar(r#"SELECT starter_content_state FROM "user" WHERE id = $1"#)
            .bind(seed.owner)
            .fetch_one(db.pool())
            .await
            .expect("starter content state");
    assert_eq!(starter.as_deref(), Some("imported"));

    // ⑤ 重复调用 ⇒ **复用**同一个 agent + 同一个 issue（`onboardingStarted` 只建一次）。
    let (status, bytes) = send(&app, &Call::new("POST", path, seed.owner).body(body)).await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    let second: BootstrapOnboardingRuntimeResponse =
        serde_json::from_slice(&bytes).expect("second response");
    assert_eq!(second.agent_id, first.agent_id, "Helper agent 被重建了");
    assert_eq!(second.issue_id, first.issue_id, "starter issue 被重建了");
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM agent WHERE workspace_id = $1 AND name = $2 AND visibility = 'workspace'",
    )
    .bind(seed.workspace)
    .bind(shim_content::ONBOARDING_ASSISTANT_NAME)
    .fetch_one(db.pool())
    .await
    .expect("count helpers");
    assert_eq!(count.0, 1);
}

/// `runtime-bootstrap` 的三格前置：非成员 403 / runtime 不属于本 workspace 400 /
/// 别人的 private runtime 403。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_runtime_bootstrap_gate_rejects_non_members_and_foreign_runtimes() {
    let db = fixture!();
    let seed = seed(&db).await;
    // 外人**不在**这个 workspace 里（seed 只把 owner / 另一个 member 放进去）。
    let stranger: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m93-stranger', $1) RETURNING id"#,
    )
    .bind(format!(
        "itest-m93-stranger-{}@example.com",
        Uuid::new_v4().simple()
    ))
    .fetch_one(db.pool())
    .await
    .expect("insert stranger");
    let app = test_app(db.clone());
    let path = "/api/me/onboarding/runtime-bootstrap";

    let bootstrap = |runtime: Uuid| {
        json!({"workspace_id": seed.workspace.to_string(), "runtime_id": runtime.to_string()})
            .to_string()
    };

    // 非成员 ⇒ 403（上游逐字，不是全仓 member 口径的 404）。
    let (status, bytes) = send(
        &app,
        &Call::new("POST", path, stranger).body(bootstrap(seed.runtime_public)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{bytes:?}");
    assert!(message_of(&bytes).ends_with("not a member of this workspace"));

    // runtime 不属于本 workspace ⇒ 400 `invalid runtime_id`。
    let other_ws: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m93-x', $1) RETURNING id",
    )
    .bind(format!("itest-m93-x-{}", Uuid::new_v4().simple()))
    .fetch_one(db.pool())
    .await
    .expect("insert other workspace");
    let foreign_runtime: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime (workspace_id, name, runtime_mode, provider, owner_id, visibility) \
         VALUES ($1, 'itest-m93-foreign', 'local', 'codex', $2, 'public') RETURNING id",
    )
    .bind(other_ws)
    .bind(seed.owner)
    .fetch_one(db.pool())
    .await
    .expect("insert foreign runtime");
    let (status, bytes) = send(
        &app,
        &Call::new("POST", path, seed.owner).body(bootstrap(foreign_runtime)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bytes:?}");
    assert!(message_of(&bytes).ends_with("invalid runtime_id"));

    // 别人的 private runtime ⇒ 403（上游 `canUseRuntimeForAgent`）。
    let (status, bytes) = send(
        &app,
        &Call::new("POST", path, seed.owner).body(bootstrap(seed.runtime_private_other)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{bytes:?}");
    assert!(message_of(&bytes)
        .ends_with("this runtime is private; only its owner can create agents on it"));

    // 缺 `runtime_id` ⇒ 400（**不**是 500）。
    let (status, bytes) = send(
        &app,
        &Call::new("POST", path, seed.owner)
            .body(json!({"workspace_id": seed.workspace.to_string()}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bytes:?}");
    assert!(message_of(&bytes).ends_with("runtime_id is required"));
}

/// `no-runtime-bootstrap` 的 provision 链：不建 agent、建 guide issue、指派给**成员自己**、
/// 正文按 `user.language` 选 EN/ZH（`DoD` 第 3 条）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_no_runtime_bootstrap_chain_seeds_the_guide_issue_for_the_member() {
    let db = fixture!();
    let seed = seed(&db).await;
    // 语言决定 EN/ZH：给 owner 一个 `zh-CN`。
    sqlx::query(r#"UPDATE "user" SET language = 'zh-CN' WHERE id = $1"#)
        .bind(seed.owner)
        .execute(db.pool())
        .await
        .expect("set language");
    let app = test_app(db.clone());
    let path = "/api/me/onboarding/no-runtime-bootstrap";
    let body = json!({"workspace_id": seed.workspace.to_string()}).to_string();

    let (status, bytes) = send(
        &app,
        &Call::new("POST", path, seed.owner).body(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    let first: BootstrapOnboardingNoRuntimeResponse =
        serde_json::from_slice(&bytes).expect("no-runtime response");
    assert_eq!(first.workspace_id, seed.workspace.to_string());

    // guide issue 的四列逐字（标题是跨版本去重的键）。
    let issue: (String, Option<String>, Option<String>, Option<Uuid>, String) = sqlx::query_as(
        "SELECT title, description, assignee_type, assignee_id, status FROM issue WHERE id = $1",
    )
    .bind(Uuid::parse_str(&first.issue_id).expect("issue uuid"))
    .fetch_one(db.pool())
    .await
    .expect("guide issue");
    assert_eq!(issue.0, shim_content::NO_RUNTIME_ISSUE_TITLE);
    assert_eq!(
        issue.1.as_deref(),
        Some(shim_content::NO_RUNTIME_ISSUE_DESCRIPTION_ZH)
    );
    assert_eq!(issue.2.as_deref(), Some("member"));
    assert_eq!(issue.3, Some(seed.owner));
    assert_eq!(issue.4, "todo");

    // 标记完成 + starter content。
    assert!(onboarded_at(&db, seed.owner).await.is_some());
    let starter: Option<String> =
        sqlx::query_scalar(r#"SELECT starter_content_state FROM "user" WHERE id = $1"#)
            .bind(seed.owner)
            .fetch_one(db.pool())
            .await
            .expect("starter content state");
    assert_eq!(starter.as_deref(), Some("imported"));

    // **不**建 Helper agent（上游逐字：这条路径只 seed issue）。
    let helpers: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM agent WHERE workspace_id = $1")
        .bind(seed.workspace)
        .fetch_one(db.pool())
        .await
        .expect("count agents");
    assert_eq!(helpers.0, 0);

    // 重复调用 ⇒ 复用同一条 issue。
    let (status, bytes) = send(&app, &Call::new("POST", path, seed.owner).body(body)).await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    let second: BootstrapOnboardingNoRuntimeResponse =
        serde_json::from_slice(&bytes).expect("second response");
    assert_eq!(second.issue_id, first.issue_id);
}

/// 英文用户在 `no-runtime-bootstrap` 上拿到**英文**正文（另一相，不是同一条断言）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn the_no_runtime_body_is_english_for_a_non_zh_user() {
    let db = fixture!();
    let seed = seed(&db).await;
    let app = test_app(db.clone());
    let (status, bytes) = send(
        &app,
        &Call::new(
            "POST",
            "/api/me/onboarding/no-runtime-bootstrap",
            seed.owner,
        )
        .body(json!({"workspace_id": seed.workspace.to_string()}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bytes:?}");
    let first: BootstrapOnboardingNoRuntimeResponse =
        serde_json::from_slice(&bytes).expect("response");
    let description: Option<String> =
        sqlx::query_scalar("SELECT description FROM issue WHERE id = $1")
            .bind(Uuid::parse_str(&first.issue_id).expect("issue uuid"))
            .fetch_one(db.pool())
            .await
            .expect("guide issue");
    assert_eq!(
        description.as_deref(),
        Some(shim_content::NO_RUNTIME_ISSUE_DESCRIPTION_EN)
    );
}

// ---------------------------------------------------------------------------
// 直读小工具（判据纪律：任何一处都**不**拿响应体当「写进去了」的证据）
// ---------------------------------------------------------------------------

async fn questionnaire(db: &Db, user: Uuid) -> Value {
    sqlx::query_scalar(r#"SELECT onboarding_questionnaire FROM "user" WHERE id = $1"#)
        .bind(user)
        .fetch_one(db.pool())
        .await
        .expect("read questionnaire")
}

fn json_body(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}

/// 错误信封里的 `message`（带类型前缀 ⇒ 用例比尾部）。
fn message_of(bytes: &[u8]) -> String {
    json_body(bytes)["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}
