//! `/api/squads/:id/members*` 端到端测试（上游 `squad.go` L545–L975）。
//!
//! 覆盖点：成员 CRUD 的 400/403/404/409 分支与 201/204 成功路径、leader 不可移除、
//! DELETE 的 body / query 双入口、`members/role` 的 404、以及成员状态 5 个桶
//! （`working`/`idle`/`unstable`/`offline`/`archived`）+ 人类成员 `status: null`、
//! identifier 前缀的两个来源（`workspace.issue_prefix` 与 slug 派生回退）。

use axum::http::StatusCode;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::support::{
    add_squad_member, app_with_db, archive_agent, call, call_status, cleanup, connect, err_message,
    seed_agent, seed_issue, seed_runtime, seed_squad, seed_task, seed_user, seed_workspace,
    set_issue_prefix,
};

fn find_member(members: &[Value], member_id: Uuid) -> &Value {
    members
        .iter()
        .find(|m| m["member_id"] == json!(member_id.to_string()))
        .unwrap_or_else(|| panic!("member {member_id} missing from {members:?}"))
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 端到端断言按调用顺序平铺，拆函数反而更难读
async fn member_crud_roundtrip() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let plain = seed_user(&pool, workspace_id, "member").await;
    let other_user = seed_user(&pool, workspace_id, "member").await;
    let runtime = seed_runtime(&pool, workspace_id, "online", Some("-1 minutes")).await;
    let leader = seed_agent(&pool, workspace_id, Some(runtime), None, "private").await;
    let own = seed_agent(&pool, workspace_id, Some(runtime), Some(plain), "private").await;
    // plain 名下第二个 agent：squad 的 leader 会自动入队，所以 201 那条得用没入队的这个。
    let own_extra = seed_agent(&pool, workspace_id, Some(runtime), Some(plain), "private").await;
    let foreign = seed_agent(
        &pool,
        workspace_id,
        Some(runtime),
        Some(other_user),
        "private",
    )
    .await;

    // admin 建 squad（leader 自动入队）。
    let (_, squad_body) = call(
        &app,
        "POST",
        "/api/squads/",
        workspace_id,
        admin,
        Some(json!({ "name": "Members", "leader_id": leader.to_string() })),
    )
    .await;
    let squad: Uuid = squad_body["id"].as_str().unwrap().parse().unwrap();

    let members_uri = format!("/api/squads/{squad}/members");
    let role_uri = format!("/api/squads/{squad}/members/role");

    // 列表：leader 是第一个成员（`created_at ASC`）。
    let (status, body) = call(&app, "GET", &members_uri, workspace_id, admin, None).await;
    assert_eq!(status, StatusCode::OK);
    let members = body.as_array().expect("array");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["member_type"], json!("agent"));
    assert_eq!(members[0]["member_id"], json!(leader.to_string()));
    assert_eq!(members[0]["role"], json!("leader"));
    assert_eq!(members[0]["squad_id"], json!(squad.to_string()));
    assert!(members[0]["created_at"].is_string());

    // 入参校验（顺序照上游：member_type → member_id 必填 → uuid）。
    let (bad, body) = call(
        &app,
        "POST",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "human", "member_id": plain.to_string() })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(
        err_message(&body).contains("member_type must be 'agent' or 'member'"),
        "{body}"
    );

    let (bad, body) = call(
        &app,
        "POST",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "agent" })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(
        err_message(&body).contains("member_id is required"),
        "{body}"
    );

    let (bad, body) = call(
        &app,
        "POST",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "agent", "member_id": "nope" })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(
        err_message(&body).contains("member_id must be a valid uuid"),
        "{body}"
    );

    let (bad, body) = call(
        &app,
        "POST",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "agent", "member_id": Uuid::new_v4().to_string() })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(
        err_message(&body).contains("agent not found in this workspace"),
        "{body}"
    );

    // 非 admin、非 owner 的普通成员拿不到别人的 private agent ⇒ 403。
    let (_, own_squad) = call(
        &app,
        "POST",
        "/api/squads/",
        workspace_id,
        plain,
        Some(json!({ "name": "Plain", "leader_id": own.to_string() })),
    )
    .await;
    let own_squad: Uuid = own_squad["id"].as_str().unwrap().parse().unwrap();
    let own_members_uri = format!("/api/squads/{own_squad}/members");
    let (denied, body) = call(
        &app,
        "POST",
        &own_members_uri,
        workspace_id,
        plain,
        Some(json!({ "member_type": "agent", "member_id": foreign.to_string() })),
    )
    .await;
    assert_eq!(denied, StatusCode::FORBIDDEN);
    assert!(
        err_message(&body).contains("you can only add an agent you have access to"),
        "{body}"
    );

    // creator（普通成员）管得了自己的 squad，且自己的 agent 可 @ ⇒ 201。
    let (created, body) = call(
        &app,
        "POST",
        &own_members_uri,
        workspace_id,
        plain,
        Some(
            json!({ "member_type": "agent", "member_id": own_extra.to_string(), "role": "helper" }),
        ),
    )
    .await;
    assert_eq!(created, StatusCode::CREATED, "{body}");
    assert_eq!(body["role"], json!("helper"));

    // 非管理者（不是 creator、也不是 admin）不能改别人的 squad ⇒ 403。
    let (denied, body) = call(
        &app,
        "POST",
        &members_uri,
        workspace_id,
        plain,
        Some(json!({ "member_type": "agent", "member_id": own_extra.to_string() })),
    )
    .await;
    assert_eq!(denied, StatusCode::FORBIDDEN);
    assert!(
        err_message(&body).contains("insufficient permissions"),
        "{body}"
    );

    // admin 加别人的 agent（admin 短路放行）⇒ 201。
    let (created, body) = call(
        &app,
        "POST",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "agent", "member_id": foreign.to_string() })),
    )
    .await;
    assert_eq!(created, StatusCode::CREATED, "{body}");
    assert_eq!(body["role"], json!(""));

    // 重复入队 ⇒ 409（唯一约束 `(squad_id, member_type, member_id)`）。
    let (dup, body) = call(
        &app,
        "POST",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "agent", "member_id": foreign.to_string() })),
    )
    .await;
    assert_eq!(dup, StatusCode::CONFLICT);
    assert!(
        err_message(&body).contains("member already in squad"),
        "{body}"
    );

    // 人类成员：workpace 内 ⇒ 201；不是本 workspace 成员 ⇒ 400。
    let (created, body) = call(
        &app,
        "POST",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "member", "member_id": other_user.to_string() })),
    )
    .await;
    assert_eq!(created, StatusCode::CREATED, "{body}");
    assert_eq!(body["member_type"], json!("member"));

    let outsider = seed_user(&pool, workspace_id, "member").await;
    sqlx::query("DELETE FROM member WHERE workspace_id = $1 AND user_id = $2")
        .bind(workspace_id)
        .bind(outsider)
        .execute(&pool)
        .await
        .expect("drop outsider membership");
    let (bad, body) = call(
        &app,
        "POST",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "member", "member_id": outsider.to_string() })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(
        err_message(&body).contains("member not found in this workspace"),
        "{body}"
    );

    // 改角色：命中 ⇒ 200；没这个成员 ⇒ 404。
    let (ok, body) = call(
        &app,
        "PATCH",
        &role_uri,
        workspace_id,
        admin,
        Some(
            json!({ "member_type": "agent", "member_id": foreign.to_string(), "role": "co-lead" }),
        ),
    )
    .await;
    assert_eq!(ok, StatusCode::OK, "{body}");
    assert_eq!(body["role"], json!("co-lead"));

    let (missing, body) = call(
        &app,
        "PATCH",
        &role_uri,
        workspace_id,
        admin,
        Some(
            json!({ "member_type": "agent", "member_id": Uuid::new_v4().to_string(), "role": "x" }),
        ),
    )
    .await;
    assert_eq!(missing, StatusCode::NOT_FOUND);
    assert!(err_message(&body).contains("squad member"), "{body}");

    // 移除 leader ⇒ 400（本地措辞）。
    let (bad, body) = call(
        &app,
        "DELETE",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "agent", "member_id": leader.to_string() })),
    )
    .await;
    assert_eq!(bad, StatusCode::BAD_REQUEST);
    assert!(
        err_message(&body).contains("cannot remove the squad leader; change leader first"),
        "{body}"
    );

    // DELETE body 形态：204，再删一次 ⇒ 404。
    let removed = call_status(
        &app,
        "DELETE",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "agent", "member_id": foreign.to_string() })),
    )
    .await;
    assert_eq!(removed, StatusCode::NO_CONTENT);
    let (again, body) = call(
        &app,
        "DELETE",
        &members_uri,
        workspace_id,
        admin,
        Some(json!({ "member_type": "agent", "member_id": foreign.to_string() })),
    )
    .await;
    assert_eq!(again, StatusCode::NOT_FOUND);
    assert!(err_message(&body).contains("squad member"), "{body}");

    // DELETE 空 body + query 参数（本仓补充入口）⇒ 204。
    let removed = call_status(
        &app,
        "DELETE",
        &format!("{members_uri}?member_type=member&member_id={other_user}"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(removed, StatusCode::NO_CONTENT);

    // plain 子路由只注册一种形态：`members/role/` 不存在。
    let (trailing, _) = call(
        &app,
        "PATCH",
        &format!("{role_uri}/"),
        workspace_id,
        admin,
        Some(json!({ "member_type": "agent", "member_id": own_extra.to_string(), "role": "x" })),
    )
    .await;
    assert_eq!(trailing, StatusCode::NOT_FOUND);

    // 收尾：workspace 级联删掉两个 squad 与它们的成员。
    cleanup(&pool, workspace_id, &[other_user, outsider]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
#[allow(clippy::too_many_lines)] // 8 个成员的派生桶逐条平铺
async fn member_status_derives_buckets() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    set_issue_prefix(&pool, workspace_id, "SQ").await;

    let rt_online = seed_runtime(&pool, workspace_id, "online", Some("-1 minutes")).await;
    // `online` 恒为 online；想造 unstable 必须用非 online 的 runtime + 新鲜心跳。
    let rt_unstable = seed_runtime(&pool, workspace_id, "offline", Some("-2 minutes")).await;
    let rt_offline = seed_runtime(&pool, workspace_id, "offline", Some("-30 minutes")).await;

    let idle = seed_agent(&pool, workspace_id, Some(rt_online), None, "private").await;
    let working = seed_agent(&pool, workspace_id, Some(rt_online), None, "private").await;
    let unstable = seed_agent(&pool, workspace_id, Some(rt_unstable), None, "private").await;
    let offline = seed_agent(&pool, workspace_id, Some(rt_offline), None, "private").await;
    let no_runtime = seed_agent(&pool, workspace_id, None, None, "private").await;
    let archived = seed_agent(&pool, workspace_id, Some(rt_online), None, "private").await;
    archive_agent(&pool, archived).await;
    let waiting = seed_agent(&pool, workspace_id, Some(rt_online), None, "private").await;
    let human = seed_user(&pool, workspace_id, "member").await;

    let squad = seed_squad(&pool, workspace_id, "Status", idle, admin).await;
    for (member_type, member_id, role) in [
        ("agent", idle, "leader"),
        ("agent", working, ""),
        ("agent", unstable, ""),
        ("agent", offline, ""),
        ("agent", no_runtime, ""),
        ("agent", archived, ""),
        ("agent", waiting, ""),
        ("member", human, ""),
    ] {
        add_squad_member(&pool, squad, member_type, member_id, role).await;
    }

    let issue_working =
        seed_issue(&pool, workspace_id, 42, "in flight", "in_progress", admin).await;
    seed_task(
        &pool,
        working,
        issue_working,
        rt_online,
        "running",
        Some("-2 minutes"),
    )
    .await;
    // archived 优先于 workload：这个 agent 有在飞任务也仍然是 archived。
    let issue_archived = seed_issue(&pool, workspace_id, 43, "archived work", "todo", admin).await;
    seed_task(
        &pool,
        archived,
        issue_archived,
        rt_online,
        "dispatched",
        Some("-1 minutes"),
    )
    .await;
    // waiting_local_directory 让 issue 可见，但不算 working。
    let issue_waiting = seed_issue(&pool, workspace_id, 7, "waiting on dir", "todo", admin).await;
    seed_task(
        &pool,
        waiting,
        issue_waiting,
        rt_online,
        "waiting_local_directory",
        Some("-3 minutes"),
    )
    .await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/squads/{squad}/members/status"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let members = body["members"].as_array().expect("members array");
    assert_eq!(members.len(), 8, "每个成员只出现一次：{body}");

    let m = find_member(members, idle);
    assert_eq!(m["status"], json!("idle"));
    assert_eq!(m["member_type"], json!("agent"));
    assert!(m["last_active_at"].is_string(), "runtime 心跳也算活跃时间");

    let m = find_member(members, working);
    assert_eq!(m["status"], json!("working"));
    assert_eq!(m["active_issues"].as_array().expect("issues").len(), 1);
    assert_eq!(
        m["active_issues"][0]["issue_id"],
        json!(issue_working.to_string())
    );
    assert_eq!(m["active_issues"][0]["identifier"], json!("SQ-42"));
    assert_eq!(m["active_issues"][0]["title"], json!("in flight"));
    assert_eq!(m["active_issues"][0]["issue_status"], json!("in_progress"));
    assert!(m["last_active_at"].is_string());

    assert_eq!(find_member(members, unstable)["status"], json!("unstable"));
    assert_eq!(find_member(members, offline)["status"], json!("offline"));
    assert_eq!(find_member(members, no_runtime)["status"], json!("offline"));
    assert_eq!(
        find_member(members, no_runtime)["last_active_at"],
        json!(null),
        "没有 runtime 行也没有任务 ⇒ 没有活跃时间"
    );
    assert_eq!(find_member(members, archived)["status"], json!("archived"));

    let m = find_member(members, waiting);
    assert_eq!(
        m["status"],
        json!("idle"),
        "waiting_local_directory 不算 working"
    );
    assert_eq!(m["active_issues"][0]["identifier"], json!("SQ-7"));

    let m = find_member(members, human);
    assert_eq!(m["status"], json!(null), "人类成员没有 presence");
    assert_eq!(m["active_issues"], json!([]));
    assert_eq!(m["last_active_at"], json!(null));

    cleanup(&pool, workspace_id, &[]).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn member_status_identifier_prefers_workspace_prefix_then_slug() {
    let Some((pool, db)) = connect().await else {
        return;
    };
    let app = app_with_db(db);
    let (workspace_id, admin) = seed_workspace(&pool, "admin").await;
    let runtime = seed_runtime(&pool, workspace_id, "online", Some("-1 minutes")).await;
    let agent = seed_agent(&pool, workspace_id, Some(runtime), None, "private").await;
    let squad = seed_squad(&pool, workspace_id, "Prefix", agent, admin).await;
    add_squad_member(&pool, squad, "agent", agent, "leader").await;
    let issue = seed_issue(&pool, workspace_id, 7, "prefix probe", "todo", admin).await;
    seed_task(
        &pool,
        agent,
        issue,
        runtime,
        "dispatched",
        Some("-1 minutes"),
    )
    .await;

    // 默认 `workspace.issue_prefix` 为空（0001 里没有该列，020 补的默认值是 ''）⇒ slug 派生。
    let slug: String = sqlx::query_scalar("SELECT slug FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .fetch_one(&pool)
        .await
        .expect("slug");
    let expected = format!("{}-7", mc_repos::issue::issue_prefix_from_slug(&slug));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/squads/{squad}/members/status"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["members"][0]["active_issues"][0]["identifier"],
        json!(expected)
    );

    // 显式前缀优先。
    set_issue_prefix(&pool, workspace_id, "XX").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/squads/{squad}/members/status"),
        workspace_id,
        admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["members"][0]["active_issues"][0]["identifier"],
        json!("XX-7")
    );

    cleanup(&pool, workspace_id, &[]).await;
}
