//! `SquadRepo` 的单测 + PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL`）。
//!
//! 手感与 `crate::agent::tests` 一致：**未设置** `MULTICA_TEST_DATABASE_URL` → 打印跳过
//! 并 `return`；**已设置但连不上 / 缺表** → panic（不许静默跳过假装绿）。
//!
//! 目标库必须是**上游 schema**：`cargo run -p mc-migrate -- run --dir migrations`
//! （`migrations/upstream/084_squad.up.sql` + 085–088 + 096 + 127）。

use super::*;
use pretty_assertions::assert_eq;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// 纯函数
// ---------------------------------------------------------------------------

#[test]
fn member_type_whitelist_matches_check_constraint() {
    assert!(is_valid_member_type("agent"));
    assert!(is_valid_member_type("member"));
    assert!(!is_valid_member_type("squad"));
    assert!(!is_valid_member_type("Agent"));
    assert!(!is_valid_member_type(""));
}

#[test]
fn only_conflict_maps_to_duplicate_member() {
    assert!(is_conflict(&RepoError::Conflict));
    assert!(!is_conflict(&RepoError::NotFound));
    assert!(!is_conflict(&RepoError::Db("x".into())));
}

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

/// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`（打印跳过）；设置了但连不上 → panic。
async fn setup() -> Option<(Db, Id)> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let db = Db::connect(&url, 4, 1)
        .await
        .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m4-2-squad', $1) RETURNING id",
    )
    .bind(format!("itest-m4-2-s-{}", Uuid::new_v4()))
    .fetch_one(db.pool())
    .await
    .unwrap_or_else(|e| {
        panic!("workspace fixture failed (run `mc-migrate run --dir migrations`): {e}")
    });
    Some((db, Id::from(workspace_id)))
}

macro_rules! fixture {
    () => {
        match setup().await {
            Some(v) => v,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}

async fn seed_user(db: &Db) -> Uuid {
    sqlx::query_scalar("INSERT INTO \"user\" (name, email) VALUES ($1, $2) RETURNING id")
        .bind(format!("u-{}", Uuid::new_v4()))
        .bind(format!("u-{}@example.test", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .unwrap_or_else(|e| panic!("user fixture failed: {e}"))
}

/// 落一个 agent；`kind` 决定 `agent_in_workspace` 是否该看得见它（上游不过滤 kind）。
async fn seed_agent(db: &Db, ws: Id, kind: &str, runtime_id: Option<Uuid>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent (workspace_id, name, kind, runtime_mode, runtime_id, visibility, \
         permission_mode) VALUES ($1, $2, $3, 'local', $4, 'private', 'private') RETURNING id",
    )
    .bind(ws.0)
    .bind(format!("ag-{}", Uuid::new_v4()))
    .bind(kind)
    .bind(runtime_id)
    .fetch_one(db.pool())
    .await
    .unwrap_or_else(|e| panic!("agent fixture failed: {e}"))
}

async fn seed_runtime(db: &Db, ws: Id, status: &str, last_seen_minutes_ago: Option<i64>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime (workspace_id, name, runtime_mode, provider, owner_id, \
         visibility, status, last_seen_at) \
         VALUES ($1, $2, 'local', 'claude', NULL, 'public', $3, \
                 CASE WHEN $4::bigint IS NULL THEN NULL \
                      ELSE now() - make_interval(mins => $4::int) END) RETURNING id",
    )
    .bind(ws.0)
    .bind(format!("rt-{}", Uuid::new_v4()))
    .bind(status)
    .bind(last_seen_minutes_ago)
    .fetch_one(db.pool())
    .await
    .unwrap_or_else(|e| panic!("runtime fixture failed: {e}"))
}

/// 落一个 issue，返回 id。
async fn seed_issue(db: &Db, ws: Id, actor: Uuid, assignee: Option<(&str, Uuid)>) -> Uuid {
    let (assignee_type, assignee_id) = match assignee {
        Some((t, id)) => (Some(t.to_string()), Some(id)),
        None => (None, None),
    };
    sqlx::query_scalar(
        "INSERT INTO issue (workspace_id, title, status, creator_type, creator_id, number, \
         assignee_type, assignee_id) \
         VALUES ($1, 'itest-issue', 'todo', 'member', $2, 1, $3, $4) RETURNING id",
    )
    .bind(ws.0)
    .bind(actor)
    .bind(assignee_type)
    .bind(assignee_id)
    .fetch_one(db.pool())
    .await
    .unwrap_or_else(|e| panic!("issue fixture failed: {e}"))
}

/// 落一个指向 squad 的 autopilot（`096_autopilot_squad_assignee` 的 `assignee_type=squad`）。
async fn seed_squad_autopilot(db: &Db, ws: Id, actor: Uuid, squad_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO autopilot (workspace_id, title, assignee_type, assignee_id, created_by_type, \
         created_by_id) VALUES ($1, 'itest-ap', 'squad', $2, 'member', $3) RETURNING id",
    )
    .bind(ws.0)
    .bind(squad_id)
    .bind(actor)
    .fetch_one(db.pool())
    .await
    .unwrap_or_else(|e| panic!("autopilot fixture failed: {e}"))
}

async fn seed_task(db: &Db, ws: Id, agent: Uuid, issue: Uuid, status: &str, runtime: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_task_queue (agent_id, issue_id, status, dispatched_at, runtime_id, squad_id) \
         VALUES ($1, $2, $3, now(), $4, (SELECT squad_id FROM squad_member WHERE member_id = $1 \
          AND member_type = 'agent' LIMIT 1)) RETURNING id",
    )
    .bind(agent)
    .bind(issue)
    .bind(status)
    .bind(runtime)
    .fetch_one(db.pool())
    .await
    .unwrap_or_else(|e| panic!("task fixture failed (ws {ws:?}): {e}"))
}

async fn teardown(db: &Db, ws: Id) {
    for sql in [
        "DELETE FROM autopilot WHERE workspace_id = $1",
        "DELETE FROM agent_task_queue WHERE agent_id IN (SELECT id FROM agent WHERE workspace_id = $1)",
        "DELETE FROM issue WHERE workspace_id = $1",
        "DELETE FROM squad_member WHERE squad_id IN (SELECT id FROM squad WHERE workspace_id = $1)",
        "DELETE FROM squad WHERE workspace_id = $1",
        "DELETE FROM agent WHERE workspace_id = $1",
        "DELETE FROM agent_runtime WHERE workspace_id = $1",
        "DELETE FROM member WHERE workspace_id = $1",
        "DELETE FROM workspace WHERE id = $1",
    ] {
        let _ = sqlx::query(sql).bind(ws.0).execute(db.pool()).await;
    }
}

fn new_squad(ws: Id, actor: Id, leader: Id, name: &str) -> NewSquad {
    NewSquad {
        workspace_id: ws,
        name: name.into(),
        description: format!("{name} desc"),
        leader_id: leader,
        creator_id: actor,
        avatar_url: None,
    }
}

// ---------------------------------------------------------------------------
// squad CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_create_allows_duplicate_name_and_list_hides_archived() {
    let (db, ws) = fixture!();
    let repo = SquadRepo::new(db.clone());
    let actor = seed_user(&db).await;
    let leader = seed_agent(&db, ws, "user", None).await;

    let first = repo
        .create(&new_squad(ws, Id(actor), Id(leader), "Research"))
        .await
        .expect("create first");
    // 087_squad_name_not_unique：重名合法（没有 409）。
    let second = repo
        .create(&new_squad(ws, Id(actor), Id(leader), "Research"))
        .await
        .expect("create duplicate name");
    assert_ne!(first.id(), second.id());
    assert_eq!(first.description, "Research desc");
    assert_eq!(first.instructions, "");
    assert_eq!(first.avatar_url, None);
    assert_eq!(first.creator_id(), Id(actor));
    assert!(first.archived_at.is_none());
    assert!(!first.is_archived());

    let listed = repo.list(ws).await.expect("list");
    assert_eq!(listed.len(), 2);
    // created_at ASC：先建的在前（同事务内 created_at 可能相等，只断言集合包含）。
    assert!(listed.iter().any(|s| s.id() == first.id()));
    assert!(listed.iter().any(|s| s.id() == second.id()));

    // workspace 作用域：别的 workspace 查不到。
    let other_ws: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m4-2-other', $1) RETURNING id",
    )
    .bind(format!("itest-m4-2-o-{}", Uuid::new_v4()))
    .fetch_one(db.pool())
    .await
    .expect("other workspace");
    assert!(repo
        .find_in_workspace(Id(other_ws), first.id())
        .await
        .expect("find other ws")
        .is_none());

    let archived = repo.archive(first.id(), Id(actor)).await.expect("archive");
    assert!(archived.is_archived());
    assert_eq!(archived.archived_by, Some(actor));
    let listed = repo.list(ws).await.expect("list after archive");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id(), second.id());

    teardown(&db, ws).await;
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(other_ws)
        .execute(db.pool())
        .await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_update_coalesces_adds_leader_member_and_pauses_autopilots() {
    let (db, ws) = fixture!();
    let repo = SquadRepo::new(db.clone());
    let actor = seed_user(&db).await;
    let old_leader = seed_agent(&db, ws, "user", None).await;
    let runtime = seed_runtime(&db, ws, "online", Some(0)).await;
    // 新 leader 绑了 runtime ⇒ 不该暂停。
    let bound_leader = seed_agent(&db, ws, "user", Some(runtime)).await;
    let unbound_leader = seed_agent(&db, ws, "user", None).await;

    let squad = repo
        .create(&new_squad(ws, Id(actor), Id(old_leader), "Sq"))
        .await
        .expect("create");
    let ap = seed_squad_autopilot(&db, ws, actor, squad.id().0).await;

    // 只改 name：其余列保持。
    let renamed = repo
        .update(
            ws,
            squad.id(),
            &SquadUpdate {
                name: Some("Sq renamed".into()),
                ..SquadUpdate::default()
            },
        )
        .await
        .expect("rename");
    assert_eq!(renamed.name, "Sq renamed");
    assert_eq!(renamed.description, "Sq desc");
    assert_eq!(renamed.leader_id(), Id(old_leader));

    // 换 leader（已绑 runtime）+ 自动补成员行 + 不暂停 autopilot。
    let swapped = repo
        .update(
            ws,
            squad.id(),
            &SquadUpdate {
                leader_id: Some(bound_leader),
                pause_autopilots: false,
                ..SquadUpdate::default()
            },
        )
        .await
        .expect("swap leader");
    assert_eq!(swapped.leader_id(), Id(bound_leader));
    assert!(repo
        .is_member(squad.id(), MEMBER_TYPE_AGENT, Id(bound_leader))
        .await
        .expect("is member"));
    let role: String =
        sqlx::query_scalar("SELECT role FROM squad_member WHERE squad_id = $1 AND member_id = $2")
            .bind(squad.id().0)
            .bind(bound_leader)
            .fetch_one(db.pool())
            .await
            .expect("leader member row");
    assert_eq!(role, ROLE_LEADER);
    let status: String = sqlx::query_scalar("SELECT status FROM autopilot WHERE id = $1")
        .bind(ap)
        .fetch_one(db.pool())
        .await
        .expect("autopilot status");
    assert_eq!(status, "active");

    // 换到未绑 runtime 的 leader ⇒ 暂停该 squad 的 autopilot。
    let swapped = repo
        .update(
            ws,
            squad.id(),
            &SquadUpdate {
                leader_id: Some(unbound_leader),
                pause_autopilots: true,
                ..SquadUpdate::default()
            },
        )
        .await
        .expect("swap to unbound leader");
    assert_eq!(swapped.leader_id(), Id(unbound_leader));
    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status, pause_reason FROM autopilot WHERE id = $1")
            .bind(ap)
            .fetch_one(db.pool())
            .await
            .expect("autopilot paused");
    assert_eq!(status, "paused");
    assert_eq!(reason.as_deref(), Some("agent_runtime_required"));

    // 不存在的 squad / 不属于本 workspace 的 agent ⇒ NotFound。
    let err = repo
        .update(ws, Id(Uuid::new_v4()), &SquadUpdate::default())
        .await
        .expect_err("missing squad");
    assert!(matches!(err, RepoError::NotFound), "{err:?}");
    let err = repo
        .update(
            ws,
            squad.id(),
            &SquadUpdate {
                leader_id: Some(Uuid::new_v4()),
                ..SquadUpdate::default()
            },
        )
        .await
        .expect_err("missing leader agent");
    assert!(matches!(err, RepoError::NotFound), "{err:?}");

    teardown(&db, ws).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_delete_transfers_issues_and_autopilots_to_leader() {
    let (db, ws) = fixture!();
    let repo = SquadRepo::new(db.clone());
    let actor = seed_user(&db).await;
    let leader = seed_agent(&db, ws, "user", None).await;
    let squad = repo
        .create(&new_squad(ws, Id(actor), Id(leader), "Sq"))
        .await
        .expect("create");
    let issue = seed_issue(&db, ws, actor, Some(("squad", squad.id().0))).await;
    let ap = seed_squad_autopilot(&db, ws, actor, squad.id().0).await;

    let moved = repo
        .transfer_assignees(squad.id(), Id(leader))
        .await
        .expect("transfer issues");
    assert_eq!(moved, 1);
    let (kind, assignee, revision): (Option<String>, Option<Uuid>, i64) =
        sqlx::query_as("SELECT assignee_type, assignee_id, revision FROM issue WHERE id = $1")
            .bind(issue)
            .fetch_one(db.pool())
            .await
            .expect("issue after transfer");
    assert_eq!(kind.as_deref(), Some("agent"));
    assert_eq!(assignee, Some(leader));
    assert_eq!(revision, 2, "转移要顺带把 revision +1");

    let moved = repo
        .transfer_autopilots(squad.id(), Id(leader))
        .await
        .expect("transfer autopilots");
    assert_eq!(moved, 1);
    let (kind, assignee): (String, Uuid) =
        sqlx::query_as("SELECT assignee_type, assignee_id FROM autopilot WHERE id = $1")
            .bind(ap)
            .fetch_one(db.pool())
            .await
            .expect("autopilot after transfer");
    assert_eq!(kind, "agent");
    assert_eq!(assignee, leader);

    teardown(&db, ws).await;
}

// ---------------------------------------------------------------------------
// squad member
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_member_crud_and_duplicate_conflict() {
    let (db, ws) = fixture!();
    let repo = SquadRepo::new(db.clone());
    let actor = seed_user(&db).await;
    let leader = seed_agent(&db, ws, "user", None).await;
    let squad = repo
        .create(&new_squad(ws, Id(actor), Id(leader), "Sq"))
        .await
        .expect("create");

    let agent_member = repo
        .add_member(squad.id(), MEMBER_TYPE_AGENT, Id(leader), "contributor")
        .await
        .expect("add agent member");
    assert_eq!(agent_member.member_type, MEMBER_TYPE_AGENT);
    assert_eq!(agent_member.role, "contributor");
    assert_eq!(agent_member.squad_id(), squad.id());

    // UNIQUE(squad_id, member_type, member_id) ⇒ 409 那一支。
    let dup = repo
        .add_member(squad.id(), MEMBER_TYPE_AGENT, Id(leader), "contributor")
        .await
        .expect_err("duplicate member");
    assert!(is_conflict(&dup), "期望 Conflict，实得 {dup:?}");

    // 同一个 id 换 member_type 是另一行（上游 member_type 参与唯一键）。
    let human = repo
        .add_member(squad.id(), MEMBER_TYPE_MEMBER, Id(actor), "")
        .await
        .expect("add human member");
    assert_eq!(human.member_type, MEMBER_TYPE_MEMBER);

    let members = repo.list_members(squad.id()).await.expect("list members");
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].id(), agent_member.id(), "created_at ASC");

    let patched = repo
        .update_member_role(squad.id(), MEMBER_TYPE_AGENT, Id(leader), ROLE_LEADER)
        .await
        .expect("patch role")
        .expect("member exists");
    assert_eq!(patched.role, ROLE_LEADER);
    assert!(repo
        .update_member_role(squad.id(), MEMBER_TYPE_AGENT, Id(Uuid::new_v4()), "x")
        .await
        .expect("patch missing")
        .is_none());

    assert!(repo
        .is_member(squad.id(), MEMBER_TYPE_MEMBER, Id(actor))
        .await
        .expect("is member"));
    assert!(!repo
        .is_member(squad.id(), MEMBER_TYPE_AGENT, Id(Uuid::new_v4()))
        .await
        .expect("is not member"));

    assert_eq!(
        repo.remove_member(squad.id(), MEMBER_TYPE_MEMBER, Id(actor))
            .await
            .expect("remove"),
        1
    );
    assert_eq!(
        repo.remove_member(squad.id(), MEMBER_TYPE_MEMBER, Id(actor))
            .await
            .expect("remove again"),
        0
    );
    assert_eq!(
        repo.list_members(squad.id())
            .await
            .expect("list after remove")
            .len(),
        1
    );

    teardown(&db, ws).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_preview_rows_put_leader_first_and_skip_archived_squads() {
    let (db, ws) = fixture!();
    let repo = SquadRepo::new(db.clone());
    let actor = seed_user(&db).await;
    let leader = seed_agent(&db, ws, "user", None).await;
    let other_agent = seed_agent(&db, ws, "user", None).await;
    let squad = repo
        .create(&new_squad(ws, Id(actor), Id(leader), "Sq"))
        .await
        .expect("create");
    // 先加普通成员、后加 leader：排序仍要 leader 在前（上游 leader-first 表达式）。
    repo.add_member(squad.id(), MEMBER_TYPE_AGENT, Id(other_agent), "")
        .await
        .expect("add other");
    repo.add_member(squad.id(), MEMBER_TYPE_AGENT, Id(leader), ROLE_LEADER)
        .await
        .expect("add leader");

    let preview = repo
        .member_preview_by_squad(squad.id())
        .await
        .expect("preview by squad");
    assert_eq!(preview.len(), 2);
    assert_eq!(preview[0].member_id, leader);
    assert_eq!(preview[1].member_id, other_agent);

    let by_ws = repo
        .member_preview_by_workspace(ws)
        .await
        .expect("preview by workspace");
    assert_eq!(by_ws.len(), 2);

    repo.archive(squad.id(), Id(actor)).await.expect("archive");
    assert!(repo
        .member_preview_by_workspace(ws)
        .await
        .expect("preview after archive")
        .is_empty());
    // BySquad 没有 workspace/archived 条件（与上游一致）。
    assert_eq!(
        repo.member_preview_by_squad(squad.id())
            .await
            .expect("preview by squad after archive")
            .len(),
        2
    );

    teardown(&db, ws).await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_member_status_rows_join_runtime_and_in_flight_tasks() {
    let (db, ws) = fixture!();
    let repo = SquadRepo::new(db.clone());
    let actor = seed_user(&db).await;
    let runtime = seed_runtime(&db, ws, "online", Some(0)).await;
    let leader = seed_agent(&db, ws, "user", Some(runtime)).await;
    // 掉线 1 分钟的 worker：unstable。
    let stale_runtime = seed_runtime(&db, ws, "offline", Some(1)).await;
    let worker = seed_agent(&db, ws, "user", Some(stale_runtime)).await;
    // 没有 runtime 行的 agent：offline。
    let loner = seed_agent(&db, ws, "user", None).await;
    let squad = repo
        .create(&new_squad(ws, Id(actor), Id(leader), "Sq"))
        .await
        .expect("create");
    for (agent, role) in [(leader, ROLE_LEADER), (worker, "contributor"), (loner, "")] {
        repo.add_member(squad.id(), MEMBER_TYPE_AGENT, Id(agent), role)
            .await
            .expect("add member");
    }
    repo.add_member(squad.id(), MEMBER_TYPE_MEMBER, Id(actor), "")
        .await
        .expect("add human");

    let issue = seed_issue(&db, ws, actor, None).await;
    // `251_agent_runtime_unbind` 的 CHECK：活着的任务必须有 runtime_id。
    let running = seed_task(&db, ws, worker, issue, "running", stale_runtime).await;
    let waiting = seed_task(
        &db,
        ws,
        worker,
        issue,
        "waiting_local_directory",
        stale_runtime,
    )
    .await;

    let rows = repo
        .member_status_rows(squad.id())
        .await
        .expect("status rows");
    // worker 两条在飞任务 ⇒ 两行；其余成员各一行。
    assert_eq!(rows.len(), 5);

    let worker_rows: Vec<_> = rows.iter().filter(|r| r.member_id == worker).collect();
    assert_eq!(worker_rows.len(), 2);
    for row in &worker_rows {
        assert_eq!(row.runtime_status.as_deref(), Some("offline"));
        assert!(row.runtime_last_seen_at.is_some());
        assert!(row.issue_number.is_some());
        assert_eq!(row.issue_title.as_deref(), Some("itest-issue"));
        assert_eq!(row.issue_status.as_deref(), Some("todo"));
        assert_eq!(row.task_issue_id, Some(issue));
    }
    // dispatched_at DESC：两条任务同 now()，只断言 running 与 waiting 都出现。
    let task_ids: Vec<Option<Uuid>> = worker_rows.iter().map(|r| r.task_id).collect();
    assert!(task_ids.contains(&Some(running)));
    assert!(task_ids.contains(&Some(waiting)));

    let leader_row = rows
        .iter()
        .find(|r| r.member_id == leader)
        .expect("leader row");
    assert_eq!(leader_row.runtime_status.as_deref(), Some("online"));
    assert!(leader_row.task_id.is_none());
    assert!(leader_row.issue_number.is_none());

    let human_row = rows
        .iter()
        .find(|r| r.member_type == MEMBER_TYPE_MEMBER)
        .expect("human row");
    // 人类成员没有 agent/runtime 行 ⇒ 全 NULL（handler 据此返回 status: null）。
    assert!(human_row.agent_archived_at.is_none());
    assert!(human_row.runtime_status.is_none());
    assert!(human_row.runtime_last_seen_at.is_none());
    assert!(human_row.task_id.is_none());

    // 派生桶（`mc_squad::status` 的纯函数）不在此断言：那是 `mc-repos` 之外的 crate，
    // 且分支组合已由 `mc-squad` 自己的单测穷举。这里只保证 SQL 把三台信号取全了。
    let statuses: Vec<&str> = worker_rows
        .iter()
        .filter_map(|r| r.task_status.as_deref())
        .collect();
    assert!(statuses.contains(&"running"));
    assert!(statuses.contains(&"waiting_local_directory"));
    assert_eq!(worker_rows[0].agent_archived_at, None);

    teardown(&db, ws).await;
}

// ---------------------------------------------------------------------------
// 跨域只读
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_agent_lookup_is_kind_agnostic_and_workspace_scoped() {
    let (db, ws) = fixture!();
    let repo = SquadRepo::new(db.clone());
    let runtime = seed_runtime(&db, ws, "online", Some(0)).await;

    // 上游 GetAgentInWorkspace 不过滤 kind：system agent 也能当 squad leader。
    let system_agent = seed_agent(&db, ws, "system", Some(runtime)).await;
    let row = repo
        .agent_in_workspace(ws, Id(system_agent))
        .await
        .expect("lookup")
        .expect("system agent visible");
    assert_eq!(row.kind, "system");
    assert!(row.runtime_bound());
    assert_eq!(row.runtime_id, Some(runtime));
    assert!(row.archived_at.is_none());

    // workspace 作用域：换 workspace 就查不到（且不报错）。
    let other_ws: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m4-2-lookup', $1) RETURNING id",
    )
    .bind(format!("itest-m4-2-l-{}", Uuid::new_v4()))
    .fetch_one(db.pool())
    .await
    .expect("other workspace");
    assert!(repo
        .agent_in_workspace(Id(other_ws), Id(system_agent))
        .await
        .expect("cross-workspace lookup")
        .is_none());

    let actor = seed_user(&db).await;
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ($1, $2, 'member')")
        .bind(ws.0)
        .bind(actor)
        .execute(db.pool())
        .await
        .expect("member fixture");
    assert!(repo
        .workspace_member_exists(ws, Id(actor))
        .await
        .expect("member exists"));
    assert!(!repo
        .workspace_member_exists(ws, Id(Uuid::new_v4()))
        .await
        .expect("member missing"));

    teardown(&db, ws).await;
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(other_ws)
        .execute(db.pool())
        .await;
}
