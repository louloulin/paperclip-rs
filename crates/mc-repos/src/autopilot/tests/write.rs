//! `crate::autopilot::write` 的真库语义（M5-2 写面）。
//!
//! 走 HTTP 的 e2e 在 `crates/mc-http/tests/autopilots/crud.rs`；这里只测**只有 SQL 能回答**的
//! 那一半：
//!
//! - `update` 的列语义确实分两档：COALESCE 列 `None` = 保持，直赋值列 `None` = 清空；
//! - `status` 一被写就把 `pause_reason` 清空（`CASE WHEN $6 IS NOT NULL`），不写则原样保留；
//! - `archive` = `status='archived'` + `pause_reason=NULL`（不是 DELETE）；
//! - `autopilot_rule_version` 是 append-only 快照：同一 autopilot 可以有多行，`NULL` 汇总折 `{}`；
//! - `lock_agent_for_autopilot_assignment` 的 `kind='user'` 过滤与 `FOR SHARE` 取到的五列
//!   （assignee 判定与 invoke 门共用同一把锁下的同一个行）；
//! - `add_subscriber` / `add_collaborator` 的幂等形状（`DO NOTHING` vs 刷新 `granted_by`），
//!   以及 `delete_collaborator` 删 0 行也不算错；
//! - `set_trigger_publishers_by_autopilot` 只动审计列。

use sqlx::PgConnection;
use uuid::Uuid;

use super::{setup, teardown};
use crate::autopilot::write::{
    add_collaborator, add_subscriber, archive, create, delete_collaborator,
    delete_subscribers_for_autopilot, get_project_in_workspace, insert_rule_version,
    is_workspace_member, lock_active_member, lock_agent_for_autopilot_assignment,
    lock_autopilot_for_update, lock_squad_for_autopilot_assignment, lock_subscriber_writes,
    set_trigger_publishers_by_autopilot, update, AutopilotWriteRepo, NewAutopilot, UpdateAutopilot,
    PUBLISHED_BY_MEMBER, STATUS_ACTIVE, STATUS_ARCHIVED,
};
use crate::RepoError;

/// 本文件自己的种子（M5-1 的 `tests/mod.rs` 只有 workspace：写面要的 agent/squad/project
/// 都在这里就地铺，免得把公共脚手架撑成万用表）。
struct Seeded {
    user_id: Uuid,
    agent_id: Uuid,
}

async fn seed_user_and_member(pool: &sqlx::PgPool, workspace_id: Uuid) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-w', $1) RETURNING id"#,
    )
    .bind(format!("w-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");
    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'member')")
        .bind(workspace_id)
        .bind(user_id)
        .execute(pool)
        .await
        .expect("insert member");
    user_id
}

/// `kind` 与 `runtime` 都可控：写面的两条判定（`kind='user'` 过滤、`requireRuntime`）都要用。
async fn seed_agent(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    kind: &str,
    runtime_id: Option<Uuid>,
    owner_id: Option<Uuid>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id, \
             permission_mode) \
         VALUES ($1, $2, 'local', 'idle', $3, $4, $5, 'private') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-w-agent-{}", Uuid::new_v4()))
    .bind(kind)
    .bind(runtime_id)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

async fn seed_runtime(pool: &sqlx::PgPool, workspace_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime (workspace_id, daemon_id, name, runtime_mode, provider, status) \
         VALUES ($1, $2, $3, 'local', 'claude', 'online') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("daemon-{}", Uuid::new_v4()))
    .bind(format!("rt-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime")
}

async fn seed_project(pool: &sqlx::PgPool, workspace_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO project(workspace_id, title) VALUES ($1, 'itest-w-proj') RETURNING id",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("insert project")
}

/// 铺一个 workspace + 一个成员 + 一个已绑 runtime 的 `kind='user'` agent。
async fn seed(fixture: &super::Fixture) -> Seeded {
    let pool = fixture.db.pool();
    let user_id = seed_user_and_member(pool, fixture.workspace_id).await;
    let runtime_id = seed_runtime(pool, fixture.workspace_id).await;
    let agent_id = seed_agent(
        pool,
        fixture.workspace_id,
        "user",
        Some(runtime_id),
        Some(user_id),
    )
    .await;
    Seeded { user_id, agent_id }
}

async fn cleanup(fixture: &super::Fixture) {
    let pool = fixture.db.pool();
    let ws = fixture.workspace_id;
    // 顺序：autopilot 的子行随 autopilot 走，其余按外键依赖从叶子往根删。
    let _ = sqlx::query("DELETE FROM autopilot WHERE workspace_id = $1")
        .bind(ws)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM squad WHERE workspace_id = $1")
        .bind(ws)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agent WHERE workspace_id = $1")
        .bind(ws)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agent_runtime WHERE workspace_id = $1")
        .bind(ws)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM project WHERE workspace_id = $1")
        .bind(ws)
        .execute(pool)
        .await;
    // 先记住成员（member 删掉之后就查不到 user_id 了），再按依赖方向删。
    let users: Vec<Uuid> = sqlx::query_scalar("SELECT user_id FROM member WHERE workspace_id = $1")
        .bind(ws)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let _ = sqlx::query("DELETE FROM member WHERE workspace_id = $1")
        .bind(ws)
        .execute(pool)
        .await;
    for user_id in users {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user_id)
            .execute(pool)
            .await;
    }
    teardown(fixture).await;
}

async fn new_autopilot(
    conn: &mut PgConnection,
    seeded: &Seeded,
    workspace_id: Uuid,
) -> crate::autopilot::AutopilotRow {
    create(
        conn,
        &NewAutopilot {
            workspace_id,
            title: "itest-w".into(),
            description: Some("d".into()),
            assignee_type: "agent".into(),
            assignee_id: seeded.agent_id,
            status: STATUS_ACTIVE.into(),
            execution_mode: "run_only".into(),
            issue_title_template: Some("{{date}}".into()),
            project_id: None,
            created_by_type: PUBLISHED_BY_MEMBER.into(),
            created_by_id: seeded.user_id,
        },
    )
    .await
    .expect("create autopilot")
}

/// 列语义分两档：COALESCE 列 `None` = 保持；`issue_title_template` / `project_id` 直赋值
/// ⇒ `None` = 清空（handler 必须在请求缺键时回填 `prev`，这条测试是该契约的 SQL 依据）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn update_patch_semantics_are_per_column() {
    let Some(fixture) = setup().await else {
        println!("skip update_patch_semantics_are_per_column: no env");
        return;
    };
    let seeded = seed(&fixture).await;
    let ws = fixture.workspace_id;
    let mut conn = fixture.db.pool().acquire().await.unwrap();
    let before = new_autopilot(&mut conn, &seeded, ws).await;
    let project_id = seed_project(fixture.db.pool(), ws).await;
    // 先把两个直赋值列填上非空值，才能观察「不传 = 清空」。
    let filled = update(
        &mut conn,
        &UpdateAutopilot {
            id: before.id,
            project_id: Some(project_id),
            ..UpdateAutopilot::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(filled.issue_title_template.as_deref(), Some("{{date}}"));
    assert_eq!(filled.project_id, Some(project_id));

    // 全 `None`：COALESCE 列保持，直赋值列清空。
    let cleared = update(
        &mut conn,
        &UpdateAutopilot {
            id: before.id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(cleared.title, before.title);
    assert_eq!(cleared.execution_mode, before.execution_mode);
    assert_eq!(cleared.assignee_id, before.assignee_id);
    assert!(
        cleared.issue_title_template.is_none(),
        "直赋值列 None = 清空"
    );
    assert!(cleared.project_id.is_none(), "直赋值列 None = 清空");

    // 有值：逐列落库。
    let patched = update(
        &mut conn,
        &UpdateAutopilot {
            id: before.id,
            title: Some("edited".into()),
            description: Some("d2".into()),
            execution_mode: Some("create_issue".into()),
            issue_title_template: Some("x-{{date}}".into()),
            project_id: Some(project_id),
            ..UpdateAutopilot::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(patched.title, "edited");
    assert_eq!(patched.description.as_deref(), Some("d2"));
    assert_eq!(patched.execution_mode, "create_issue");
    assert_eq!(patched.issue_title_template.as_deref(), Some("x-{{date}}"));
    assert_eq!(patched.project_id, Some(project_id));

    // 行不存在 ⇒ `RepoError::NotFound`（handler 把它折成 404）。
    let missing = update(
        &mut conn,
        &UpdateAutopilot {
            id: Uuid::new_v4(),
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(missing, Err(RepoError::NotFound)));

    cleanup(&fixture).await;
}

/// 写 `status` 就清 `pause_reason`；`archive` = 归档 + 清暂停原因；规则版本是 append-only 快照。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn archive_clears_pause_reason_and_rule_versions_are_append_only() {
    let Some(fixture) = setup().await else {
        println!("skip archive_clears_pause_reason_and_rule_versions_are_append_only: no env");
        return;
    };
    let seeded = seed(&fixture).await;
    let ws = fixture.workspace_id;
    let mut conn = fixture.db.pool().acquire().await.unwrap();
    let row = new_autopilot(&mut conn, &seeded, ws).await;

    insert_rule_version(
        &mut conn,
        row.id,
        ws,
        PUBLISHED_BY_MEMBER,
        Some(seeded.user_id),
        None,
    )
    .await
    .unwrap();
    // 暂停时写原因（写面外直接铺，模拟 daemon 因配额/离线暂停）。
    sqlx::query("UPDATE autopilot SET status = 'paused', pause_reason = 'quota' WHERE id = $1")
        .bind(row.id)
        .execute(&mut *conn)
        .await
        .unwrap();

    // 只改 title ⇒ 状态与暂停原因都不动。
    let titled = update(
        &mut conn,
        &UpdateAutopilot {
            id: row.id,
            title: Some("t2".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(titled.status, "paused");
    assert_eq!(titled.pause_reason.as_deref(), Some("quota"));

    // 写 status ⇒ `pause_reason` 被清空（即使是同一个值也会清）。
    let resumed = update(
        &mut conn,
        &UpdateAutopilot {
            id: row.id,
            status: Some(STATUS_ACTIVE.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(resumed.pause_reason.is_none(), "写 status 必然清暂停原因");

    archive(&mut conn, row.id).await.unwrap();
    let archived = lock_autopilot_for_update(&mut conn, row.id, ws)
        .await
        .unwrap();
    assert_eq!(archived.status, STATUS_ARCHIVED);
    assert!(archived.pause_reason.is_none());

    // 归档后再发一次版本（上游 DeleteAutopilot 就是这么做的）：行数 +1，不是覆盖。
    insert_rule_version(
        &mut conn,
        row.id,
        ws,
        PUBLISHED_BY_MEMBER,
        Some(seeded.user_id),
        Some(&serde_json::json!({"status": "archived"})),
    )
    .await
    .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM autopilot_rule_version WHERE autopilot_id = $1")
            .bind(row.id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(count, 2, "append-only：每次发布一行快照");
    // `config_summary = NULL` 折 `{}`（不是 NULL），客户端不必区分两种空。
    let empty: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM autopilot_rule_version \
         WHERE autopilot_id = $1 AND config_summary = '{}'::jsonb",
    )
    .bind(row.id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(empty, 1);

    // 跨工作区取不到（锁查询带 workspace 谓词 ⇒ 并发删除与越权都折 NotFound）。
    let other = lock_autopilot_for_update(&mut conn, row.id, Uuid::new_v4()).await;
    assert!(matches!(other, Err(RepoError::NotFound)));

    cleanup(&fixture).await;
}

/// assignee 行锁：`kind='user'` 过滤 + 取到 invoke 门要的三列；squad 侧取队长与归档位。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn assignee_locks_enforce_kind_and_archival() {
    let Some(fixture) = setup().await else {
        println!("skip assignee_locks_enforce_kind_and_archival: no env");
        return;
    };
    let seeded = seed(&fixture).await;
    let ws = fixture.workspace_id;
    let pool = fixture.db.pool();
    let mut conn = pool.acquire().await.unwrap();

    let agent = lock_agent_for_autopilot_assignment(&mut conn, seeded.agent_id, ws)
        .await
        .unwrap()
        .expect("kind='user' 的 agent 可取");
    assert_eq!(agent.id, seeded.agent_id);
    assert!(agent.runtime_id.is_some());
    assert_eq!(agent.owner_id, Some(seeded.user_id));
    assert_eq!(agent.permission_mode, "private");
    assert!(agent.archived_at.is_none());

    // 非 `user` 型（daemon/system agent）不是合法 autopilot assignee：上游把它们排除在选择面外。
    let system = seed_agent(pool, ws, "system", None, None).await;
    assert!(lock_agent_for_autopilot_assignment(&mut conn, system, ws)
        .await
        .unwrap()
        .is_none());

    // 归档的 agent 取得到行（判定留给 handler 的 422），跨工作区取不到（400）。
    sqlx::query("UPDATE agent SET archived_at = now() WHERE id = $1")
        .bind(seeded.agent_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    let archived = lock_agent_for_autopilot_assignment(&mut conn, seeded.agent_id, ws)
        .await
        .unwrap()
        .expect("归档的 agent 仍要取到行，才能区分 400 与 422");
    assert!(archived.archived_at.is_some());
    assert!(
        lock_agent_for_autopilot_assignment(&mut conn, seeded.agent_id, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );

    let squad_id: Uuid = sqlx::query_scalar(
        "INSERT INTO squad(workspace_id, name, description, leader_id, creator_id) \
         VALUES ($1, 'itest-w-squad', '', $2, $3) RETURNING id",
    )
    .bind(ws)
    .bind(seeded.agent_id)
    .bind(seeded.user_id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let squad = lock_squad_for_autopilot_assignment(&mut conn, squad_id, ws)
        .await
        .unwrap()
        .expect("squad 可取");
    assert_eq!(squad.leader_id, seeded.agent_id);
    assert!(squad.archived_at.is_none());

    let project_id = seed_project(pool, ws).await;
    assert!(get_project_in_workspace(pool, project_id, ws)
        .await
        .unwrap());
    assert!(!get_project_in_workspace(pool, project_id, Uuid::new_v4())
        .await
        .unwrap());
    assert!(!get_project_in_workspace(pool, Uuid::new_v4(), ws)
        .await
        .unwrap());

    cleanup(&fixture).await;
}

/// 订阅者/协作者的幂等形状，以及两个「只查不锁」的成员探针。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn subscriber_and_collaborator_writes_are_idempotent() {
    let Some(fixture) = setup().await else {
        println!("skip subscriber_and_collaborator_writes_are_idempotent: no env");
        return;
    };
    let seeded = seed(&fixture).await;
    let other_member = seed_user_and_member(fixture.db.pool(), fixture.workspace_id).await;
    let ws = fixture.workspace_id;
    let write_repo = AutopilotWriteRepo::new(fixture.db.clone());
    let mut tx = write_repo.begin().await.unwrap();
    let conn = &mut *tx;
    let row = new_autopilot(conn, &seeded, ws).await;

    // 订阅：`DO NOTHING` ⇒ 重复订阅不报错、也不产生第二行。
    add_subscriber(conn, row.id, seeded.user_id).await.unwrap();
    add_subscriber(conn, row.id, seeded.user_id).await.unwrap();
    add_subscriber(conn, row.id, other_member).await.unwrap();
    let subs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM autopilot_subscriber WHERE autopilot_id = $1 AND user_type = 'member'",
    )
    .bind(row.id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(subs, 2);

    // 授权：`ON CONFLICT DO UPDATE` ⇒ 重复授权刷新 `granted_by`（审计谁最后扩的权）。
    add_collaborator(conn, row.id, other_member, seeded.user_id)
        .await
        .unwrap();
    add_collaborator(conn, row.id, other_member, other_member)
        .await
        .unwrap();
    let granted_by: Uuid = sqlx::query_scalar(
        "SELECT granted_by FROM autopilot_collaborator \
         WHERE autopilot_id = $1 AND user_type = 'member' AND user_id = $2",
    )
    .bind(row.id)
    .bind(other_member)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(granted_by, other_member, "再次授权刷新授权人，而不是报冲突");

    // 撤销不存在的行 = 成功（幂等），且不影响别的授权行。
    delete_collaborator(conn, row.id, seeded.user_id)
        .await
        .unwrap();
    delete_collaborator(conn, row.id, other_member)
        .await
        .unwrap();
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM autopilot_collaborator WHERE autopilot_id = $1")
            .bind(row.id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(left, 0);

    // 全量替换的前半步：清空指定 autopilot 的订阅行。
    delete_subscribers_for_autopilot(conn, row.id)
        .await
        .unwrap();
    let after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM autopilot_subscriber WHERE autopilot_id = $1")
            .bind(row.id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(after, 0);

    // 两个成员探针：`lock_active_member` 在事务里加 `FOR SHARE`（成员撤销与之互斥）。
    assert!(lock_active_member(conn, ws, seeded.user_id).await.unwrap());
    assert!(!lock_active_member(conn, ws, Uuid::new_v4()).await.unwrap());
    assert!(is_workspace_member(conn, ws, seeded.user_id).await.unwrap());
    assert!(!is_workspace_member(conn, Uuid::new_v4(), seeded.user_id)
        .await
        .unwrap());
    // 同一把 advisory 锁可重复取（xact 级：同事务重入是空操作）。
    lock_subscriber_writes(conn, ws, seeded.user_id)
        .await
        .unwrap();
    lock_subscriber_writes(conn, ws, seeded.user_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    cleanup(&fixture).await;
}

/// 触发器责任人转移只动审计列（`published_by_*`），不动触发器本体与 run 归属。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn trigger_publishers_follow_substantive_edits() {
    let Some(fixture) = setup().await else {
        println!("skip trigger_publishers_follow_substantive_edits: no env");
        return;
    };
    let seeded = seed(&fixture).await;
    let editor = seed_user_and_member(fixture.db.pool(), fixture.workspace_id).await;
    let ws = fixture.workspace_id;
    let mut conn = fixture.db.pool().acquire().await.unwrap();
    let row = new_autopilot(&mut conn, &seeded, ws).await;
    let trigger_id: Uuid = sqlx::query_scalar(
        "INSERT INTO autopilot_trigger (autopilot_id, kind, enabled, published_by_type, \
             published_by_id) \
         VALUES ($1, 'api', true, 'member', $2) RETURNING id",
    )
    .bind(row.id)
    .bind(seeded.user_id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();

    set_trigger_publishers_by_autopilot(&mut conn, row.id, editor)
        .await
        .unwrap();
    let (kind, enabled, publisher): (String, bool, Uuid) = sqlx::query_as(
        "SELECT kind, enabled, published_by_id FROM autopilot_trigger WHERE id = $1",
    )
    .bind(trigger_id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(kind, "api");
    assert!(enabled, "转移责任人不会停用触发器");
    assert_eq!(publisher, editor);

    cleanup(&fixture).await;
}
