//! `/api/squads*` CRUD 端到端测试（上游 `squad.go` `ListSquads` / `CreateSquad` /
//! `GetSquad` / `UpdateSquad` / `DeleteSquad`）。
//!
//! 覆盖点：尾斜杠别名（`/api/squads/` + `/api/squads`）、成员预览的 leader-first 与
//! 3 条上限、`member_count`、重名合法（`087`）、可管理性（admin vs creator、
//! 403 `insufficient permissions`）、换 leader 的副作用（补成员行 + 暂停 autopilot）、
//! 归档的副作用（issue / autopilot 转给 leader、二次删除 400）。

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::support::{
    add_squad_member, app_with_db, assign_issue_to_squad, autopilot_assignee, call,
    call_no_workspace, call_status, cleanup, connect, err_message, issue_assignee, member_count,
    member_preview_len, seed_agent, seed_autopilot, seed_issue, seed_runtime, seed_squad,
    seed_user, seed_workspace, squad_archived,
};

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn list_squads_returns_preview_and_hides_archived() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let (other_workspace, _) = seed_workspace(&pool, "admin").await;
    let runtime = seed_runtime(&pool, workspace_id, "online", Some("-1 minutes")).await;
    let leader = seed_agent(&pool, workspace_id, Some(runtime), None, "private").await;
    let other_agent = seed_agent(&pool, workspace_id, Some(runtime), None, "private").await;

    let squad = seed_squad(&pool, workspace_id, "Alpha", leader, admin).await;
    let doomed = seed_squad(&pool, workspace_id, "Beta", other_agent, admin).await;
    // 别的 workspace 的 squad 不能出现在本 workspace 的列表里。
    seed_squad(&pool, other_workspace, "Gamma", other_agent, admin).await;

    // leader 先入队 + 3 个人类成员 ⇒ member_count = 4、preview 只留前 3 条（leader 在首位）。
    add_squad_member(&pool, squad, "agent", leader, "leader").await;
    for _ in 0..3 {
        let human = seed_user(&pool, workspace_id, "member").await;
        add_squad_member(&pool, squad, "member", human, "").await;
    }

    let (status, body) = call(&app, "GET", "/api/squads/", workspace_id, admin, None).await;
    assert_eq!(status, StatusCode::OK);
    let list = body.as_array().expect("array");
    assert_eq!(list.len(), 2, "本 workspace 未归档 squad 两条：{body}");
    let alpha = list
        .iter()
        .find(|s| s["id"] == json!(squad.to_string()))
        .expect("alpha in list");
    assert_eq!(member_count(alpha), 4);
    assert_eq!(member_preview_len(alpha), 3, "preview 上限 3 条");
    assert_eq!(alpha["member_preview"][0]["member_type"], json!("agent"));
    assert_eq!(
        alpha["member_preview"][0]["member_id"],
        json!(leader.to_string())
    );
    assert_eq!(alpha["member_preview"][0]["role"], json!("leader"));
    assert_eq!(alpha["name"], json!("Alpha"));
    assert_eq!(alpha["archived_at"], json!(null));

    // 无尾斜杠别名必须返回同一份数据（axum 不做归一化，两种形态各自注册）。
    let (alias_status, alias_body) =
        call(&app, "GET", "/api/squads", workspace_id, admin, None).await;
    assert_eq!(alias_status, StatusCode::OK);
    assert_eq!(alias_body, body);

    // 归档 → 列表里消失，但按 id 仍可读到（上游 `GetSquadInWorkspace` 不过滤 archived_at）。
    let archived = call_status(
        &app,
        "DELETE",
        &format!("/api/squads/{doomed}/"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(archived, StatusCode::NO_CONTENT);
    let (_, after) = call(&app, "GET", "/api/squads/", workspace_id, admin, None).await;
    assert_eq!(after.as_array().expect("array").len(), 1);
    let (by_id, one) = call(
        &app,
        "GET",
        &format!("/api/squads/{doomed}/"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(by_id, StatusCode::OK);
    assert!(one["archived_at"].is_string(), "归档后 archived_at 有值");
    assert_eq!(one["archived_by"], json!(admin.to_string()));

    cleanup(&pool, workspace_id, &[]).await;
    cleanup(&pool, other_workspace, &[]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn create_squad_validates_leader_access_and_allows_duplicate_names() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, member) = seed_workspace(&pool, "member").await;
    let admin = seed_user(&pool, workspace_id, "admin").await;
    let other_user = seed_user(&pool, workspace_id, "member").await;
    // 另一个 workspace 的成员：在本 workspace 里不是成员。
    let (elsewhere, outsider) = seed_workspace(&pool, "member").await;
    let runtime = seed_runtime(&pool, workspace_id, "online", Some("-1 minutes")).await;
    // 别人的 private agent：普通成员用不了，admin 可以。
    let foreign = seed_agent(
        &pool,
        workspace_id,
        Some(runtime),
        Some(other_user),
        "private",
    )
    .await;

    let (bad, body) = call(
        &app,
        "POST",
        "/api/squads/",
        workspace_id,
        member,
        Some(json!({ "leader_id": foreign.to_string() })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(err_message(&body).contains("name is required"), "{body}");

    let (bad, body) = call(
        &app,
        "POST",
        "/api/squads/",
        workspace_id,
        member,
        Some(json!({ "name": "NoLeader" })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(
        err_message(&body).contains("leader_id is required"),
        "{body}"
    );

    let (bad, body) = call(
        &app,
        "POST",
        "/api/squads/",
        workspace_id,
        member,
        Some(json!({ "name": "BadId", "leader_id": "not-a-uuid" })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(err_message(&body).contains("valid uuid"), "{body}");

    let (bad, body) = call(
        &app,
        "POST",
        "/api/squads/",
        workspace_id,
        member,
        Some(json!({ "name": "Unknown", "leader_id": Uuid::new_v4().to_string() })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(
        err_message(&body).contains("leader must be a valid agent in this workspace"),
        "{body}"
    );

    // invoke 门：普通成员拿不到别人的 private agent ⇒ 403（上游 `memberCanWireAgent`）。
    let (denied, body) = call(
        &app,
        "POST",
        "/api/squads/",
        workspace_id,
        member,
        Some(json!({ "name": "Foreign", "leader_id": foreign.to_string() })),
    )
    .await;
    assert_eq!(denied, StatusCode::FORBIDDEN);
    assert!(
        err_message(&body).contains("you can only use an agent you have access to as leader"),
        "{body}"
    );

    // 空 JSON body（Go 的 `null` 语义）：两个字段都缺失 ⇒ 先撞 `name is required`。
    let (bad, _) = call(
        &app,
        "POST",
        "/api/squads/",
        workspace_id,
        admin,
        Some(json!(null)),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);

    // admin 建 squad，leader 用别人的 private agent（admin 短路放行），并带上 avatar_url。
    let (created, body) = call(
        &app,
        "POST",
        "/api/squads/",
        workspace_id,
        admin,
        Some(json!({
            "name": "Research",
            "description": "desc",
            "leader_id": foreign.to_string(),
            "avatar_url": "https://cdn.example.com/a.png",
        })),
    )
    .await;
    assert_eq!(created, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], json!("Research"));
    assert_eq!(body["description"], json!("desc"));
    // 上游 `acceptAvatarURL` 会签名成存储 URL；本仓没有对象存储接线 ⇒ 原样存（有意偏离）。
    assert_eq!(body["avatar_url"], json!("https://cdn.example.com/a.png"));
    assert_eq!(body["leader_id"], json!(foreign.to_string()));
    assert_eq!(body["creator_id"], json!(admin.to_string()));
    assert_eq!(body["instructions"], json!(""));
    assert_eq!(member_count(&body), 1, "leader 自动入队");
    assert_eq!(body["member_preview"][0]["role"], json!("leader"));

    // 重名合法（`087_squad_name_not_unique` 删掉了 UNIQUE(workspace_id, name)）。
    let (duplicate, dup_body) = call(
        &app,
        "POST",
        "/api/squads/",
        workspace_id,
        admin,
        Some(json!({ "name": "Research", "leader_id": foreign.to_string() })),
    )
    .await;
    assert_eq!(duplicate, StatusCode::CREATED, "{dup_body}");
    assert_ne!(dup_body["id"], body["id"]);

    // 非成员（不在 workspace 里的用户）看不到这个 workspace。
    let (anon, _) = call(&app, "GET", "/api/squads/", workspace_id, outsider, None).await;
    assert_eq!(anon, StatusCode::NOT_FOUND);

    // 缺 workspace 头 ⇒ 400 `invalid workspace id`。
    let missing_header = call_no_workspace(&app, "GET", "/api/squads/", admin).await;
    assert_eq!(missing_header, StatusCode::BAD_REQUEST);

    cleanup(&pool, workspace_id, &[other_user]).await;
    cleanup(&pool, elsewhere, &[]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn get_squad_scopes_to_workspace_and_validates_id() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let (other_workspace, _) = seed_workspace(&pool, "admin").await;
    let runtime = seed_runtime(&pool, workspace_id, "online", Some("-1 minutes")).await;
    let leader = seed_agent(&pool, workspace_id, Some(runtime), None, "private").await;
    let squad = seed_squad(&pool, workspace_id, "Scoped", leader, admin).await;
    let foreign = seed_squad(&pool, other_workspace, "Foreign", leader, admin).await;

    // 两种形态都是 200。
    for path in [
        format!("/api/squads/{squad}"),
        format!("/api/squads/{squad}/"),
    ] {
        let (status, body) = call(&app, "GET", &path, workspace_id, admin, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(body["id"], json!(squad.to_string()));
    }

    let (bad, body) = call(&app, "GET", "/api/squads/nope", workspace_id, admin, None).await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(
        err_message(&body).contains("squad id must be a valid uuid"),
        "{body}"
    );

    let (missing, body) = call(
        &app,
        "GET",
        &format!("/api/squads/{}", Uuid::new_v4()),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(missing, StatusCode::NOT_FOUND);
    assert!(err_message(&body).contains("squad"), "{body}");

    // 别的 workspace 的 squad：即便 id 存在也是 404（`GetSquadInWorkspace` 双条件）。
    let (cross, _) = call(
        &app,
        "GET",
        &format!("/api/squads/{foreign}"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(cross, StatusCode::NOT_FOUND);

    // 成员列表：plain 子路由只有一种形态（`members/` 不注册 ⇒ 404）。
    let (ok, _) = call(
        &app,
        "GET",
        &format!("/api/squads/{squad}/members"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(ok, StatusCode::OK);
    let (trailing, _) = call(
        &app,
        "GET",
        &format!("/api/squads/{squad}/members/"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(trailing, StatusCode::NOT_FOUND, "plain 子路由不注册尾斜杠");

    cleanup(&pool, workspace_id, &[]).await;
    cleanup(&pool, other_workspace, &[]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn update_squad_coalesces_and_switches_leader() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let plain = seed_user(&pool, workspace_id, "member").await;
    let runtime = seed_runtime(&pool, workspace_id, "online", Some("-1 minutes")).await;
    let bound_leader = seed_agent(&pool, workspace_id, Some(runtime), None, "private").await;
    // 未绑 runtime 的新 leader：换来之后 autopilot 应被暂停。
    let unbound_leader = seed_agent(&pool, workspace_id, None, None, "private").await;
    let squad = seed_squad(&pool, workspace_id, "Original", bound_leader, admin).await;
    add_squad_member(&pool, squad, "agent", bound_leader, "leader").await;
    let autopilot = seed_autopilot(&pool, workspace_id, "squad", squad, admin).await;

    // 只传 name ⇒ 其他列不动（`COALESCE` 语义）。
    let (ok, body) = call(
        &app,
        "PUT",
        &format!("/api/squads/{squad}/"),
        workspace_id,
        admin,
        Some(json!({ "name": "Renamed" })),
    )
    .await;
    assert_eq!(ok, StatusCode::OK, "{body}");
    assert_eq!(body["name"], json!("Renamed"));
    assert_eq!(body["leader_id"], json!(bound_leader.to_string()));

    // 显式空串是「改成空」，不是「不改」。
    sqlx::query("UPDATE squad SET description = 'keep-me' WHERE id = $1")
        .bind(squad)
        .execute(&pool)
        .await
        .expect("set description");
    let (ok, body) = call(
        &app,
        "PUT",
        &format!("/api/squads/{squad}"),
        workspace_id,
        admin,
        Some(json!({ "description": "", "instructions": "run the checklist" })),
    )
    .await;
    assert_eq!(ok, StatusCode::OK, "{body}");
    assert_eq!(body["description"], json!(""));
    assert_eq!(body["instructions"], json!("run the checklist"));

    // 换 leader：新 leader 未绑 runtime ⇒ 补成员行 + 暂停该 squad 的 autopilot。
    let (ok, body) = call(
        &app,
        "PUT",
        &format!("/api/squads/{squad}/"),
        workspace_id,
        admin,
        Some(json!({ "leader_id": unbound_leader.to_string() })),
    )
    .await;
    assert_eq!(ok, StatusCode::OK, "{body}");
    assert_eq!(body["leader_id"], json!(unbound_leader.to_string()));
    assert_eq!(member_count(&body), 2, "新 leader 自动补成员行");
    let is_member: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM squad_member \
         WHERE squad_id = $1 AND member_type = 'agent' AND member_id = $2 AND role = 'leader')",
    )
    .bind(squad)
    .bind(unbound_leader)
    .fetch_one(&pool)
    .await
    .expect("member exists");
    assert!(is_member);
    let (assignee_type, assignee_id, status) = autopilot_assignee(&pool, autopilot).await;
    assert_eq!(assignee_type, "squad", "暂停不等于改指派对象");
    assert_eq!(assignee_id, squad);
    assert_eq!(status, "paused");
    let reason: Option<String> =
        sqlx::query_scalar("SELECT pause_reason FROM autopilot WHERE id = $1")
            .bind(autopilot)
            .fetch_one(&pool)
            .await
            .expect("pause_reason");
    assert_eq!(reason.as_deref(), Some("agent_runtime_required"));

    // 非 creator 的普通成员 ⇒ 403。
    let (denied, body) = call(
        &app,
        "PUT",
        &format!("/api/squads/{squad}/"),
        workspace_id,
        plain,
        Some(json!({ "name": "Hijack" })),
    )
    .await;
    assert_eq!(denied, StatusCode::FORBIDDEN);
    assert!(
        err_message(&body).contains("insufficient permissions"),
        "{body}"
    );

    // 换到不在本 workspace 的 agent ⇒ 400。
    let (bad, body) = call(
        &app,
        "PUT",
        &format!("/api/squads/{squad}/"),
        workspace_id,
        admin,
        Some(json!({ "leader_id": Uuid::new_v4().to_string() })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(
        err_message(&body).contains("leader must be a valid agent in this workspace"),
        "{body}"
    );

    cleanup(&pool, workspace_id, &[]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn delete_squad_archives_and_transfers_work() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let plain = seed_user(&pool, workspace_id, "member").await;
    let runtime = seed_runtime(&pool, workspace_id, "online", Some("-1 minutes")).await;
    let leader = seed_agent(&pool, workspace_id, Some(runtime), None, "private").await;
    let squad = seed_squad(&pool, workspace_id, "Doomed", leader, admin).await;
    let issue = seed_issue(&pool, workspace_id, 7, "assigned to squad", "todo", admin).await;
    assign_issue_to_squad(&pool, issue, squad).await;
    let autopilot = seed_autopilot(&pool, workspace_id, "squad", squad, admin).await;

    // 非 creator 的普通成员 ⇒ 403（归档前不许动）。
    let denied = call_status(
        &app,
        "DELETE",
        &format!("/api/squads/{squad}"),
        workspace_id,
        plain,
        None,
    )
    .await;
    assert_eq!(denied, StatusCode::FORBIDDEN);

    let status = call_status(
        &app,
        "DELETE",
        &format!("/api/squads/{squad}/"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (archived, by) = squad_archived(&pool, squad).await;
    assert!(archived, "软归档（不是物理删除）");
    assert_eq!(by, Some(admin));

    // issue 转给 leader（`assignee_type` 从 squad 变 agent）。
    let (assignee_type, assignee_id) = issue_assignee(&pool, issue).await;
    assert_eq!(assignee_type.as_deref(), Some("agent"));
    assert_eq!(assignee_id, Some(leader));

    // autopilot 也转给 leader，且状态不变（转派 ≠ 暂停）。
    let (ap_type, ap_id, ap_status) = autopilot_assignee(&pool, autopilot).await;
    assert_eq!(ap_type, "agent");
    assert_eq!(ap_id, leader);
    assert_eq!(ap_status, "active");

    // 二次删除 ⇒ 400 `squad is already archived`。
    let (again, body) = call(
        &app,
        "DELETE",
        &format!("/api/squads/{squad}"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(again, StatusCode::BAD_REQUEST);
    assert!(err_message(&body).contains("already archived"), "{body}");

    cleanup(&pool, workspace_id, &[]).await;
}
