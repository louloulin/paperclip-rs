//! `mc_repos::{issue_view,pin,stats}` 的**真库**契约测试（门禁 ⑥ 以 `--ignored` 跑）。
//!
//! 为什么在 `tests/` 而不在三个 `src/*.rs` 里：R7 单文件 800 行硬上限（门禁 ⑩）——
//! 三个模块的文档已经写得比较满，再塞 DB 用例就顶线了。集成测试同样属于
//! `cargo test -p mc-repos`，所以门禁 ⑥ 的覆盖面一点没少（与 `scheduler_lease_db.rs` 同款）。
//!
//! 每个用例自己造 workspace + user 并在末尾清理，互不依赖执行顺序：
//! - `pinned_item` / `activity_log` / `issue` 都有 workspace 外键且 `ON DELETE CASCADE`
//!   ⇒ 删 workspace 就够；
//! - `issue_view` / `issue_view_preference` **故意没有外键**（上游仓库策略）
//!   ⇒ 必须显式删，见 [`cleanup`]。

// `position` 是 `float8`：这里断言的正是「上游写进去的那个字面量原样读回来」
// （`max+1` 与 `reorder` 的赋值都是小整数），`clippy::float_cmp` 在这个场景是误报。
#![allow(clippy::float_cmp)]

use std::collections::HashMap;

use mc_core::Id;
use mc_db::Db;
use mc_repos::issue_view::{IssueViewPatch, IssueViewRepo, NewIssueView};
use mc_repos::pin::{PinRepo, PinnedItemRow};
use mc_repos::stats::{AssigneeChangeCountRow, CreatedIssueAssigneeCountRow, StatsRepo};
use mc_repos::RepoError;
use serde_json::json;
use uuid::Uuid;

/// 建库连接（本文件用例全部 `#[ignore]`，只在显式跑真库时执行 ⇒ 缺变量直接 panic，
/// 门禁 ⑥ 把「静默跳过」变成「红」，防止假绿）。
async fn setup() -> (Db, sqlx::PgPool) {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL")
        .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
    let db = Db::connect(&url, 4, 1).await.expect("连接测试库");
    let pool = db.pool().clone();
    (db, pool)
}

/// 新 workspace + n 个新 user（`pinned_item` / `member` 都需要真实行）。
async fn seed(pool: &sqlx::PgPool, users: usize) -> (Uuid, Vec<Uuid>) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m2a-tail-ws', $1) RETURNING id",
    )
    .bind(format!("itest-m2a-tail-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let mut ids = Vec::with_capacity(users);
    for n in 0..users {
        let user: Uuid =
            sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
                .bind(format!("itest-m2a-tail-{n}"))
                .bind(format!("m2a-tail-{n}-{}@example.com", Uuid::new_v4()))
                .fetch_one(pool)
                .await
                .expect("insert user");
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'owner')")
            .bind(workspace_id)
            .bind(user)
            .execute(pool)
            .await
            .expect("insert member");
        ids.push(user);
    }
    (workspace_id, ids)
}

async fn cleanup(pool: &sqlx::PgPool, workspace_id: Uuid, users: &[Uuid]) {
    // `issue_view` / `issue_view_preference` 没有外键（上游 265/268 的注释写明是策略）
    // ⇒ 级联删不到，必须显式清。
    for table in ["issue_view_preference", "issue_view"] {
        let _ = sqlx::query(&format!("DELETE FROM {table} WHERE workspace_id = $1"))
            .bind(workspace_id)
            .execute(pool)
            .await;
    }
    // 其余三张表都挂在 workspace 的 ON DELETE CASCADE 上。
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    for user in users {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user)
            .execute(pool)
            .await;
    }
}

fn new_view(workspace_id: Uuid, owner_id: Uuid, name: &str, visibility: &str) -> NewIssueView {
    NewIssueView {
        workspace_id: Id::from(workspace_id),
        owner_id: Id::from(owner_id),
        name: name.to_string(),
        scope_type: "workspace".into(),
        scope_id: None,
        scope_variant: None,
        visibility: visibility.to_string(),
        definition_version: 1,
        query: json!({"status": ["todo"]}),
        display: json!({}),
    }
}

// ---------------------------------------------------------------------------
// ① issue_view CRUD + workspace 隔离 + 乐观并发
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn issue_view_crud_is_workspace_scoped_and_revision_guarded() {
    let (db, pool) = setup().await;
    let (ws_a, users) = seed(&pool, 1).await;
    let (ws_b, _) = seed(&pool, 0).await;
    let owner = users[0];
    let repo = IssueViewRepo::new(db.clone());

    let ws_a = Id::from(ws_a);
    let ws_b = Id::from(ws_b);
    let owner_id = Id::from(owner);

    let created = repo
        .create(&new_view(ws_a.0, owner, "Triage", "private"))
        .await
        .expect("create view");
    assert_eq!(created.name, "Triage");
    assert_eq!(created.revision, 1);
    assert_eq!(created.definition_version, 1);
    assert_eq!(created.query, json!({"status": ["todo"]}));
    assert_eq!(created.visibility, "private");
    assert!(created.scope_id.is_none());
    assert!(created.is_readable_by(owner_id));
    assert!(!created.is_shared());

    // 跨 workspace：同一个 id 在 ws_b 下必须 NotFound（不泄漏「存在但不在你的 workspace」）。
    match repo.get(ws_b, created.id()).await {
        Err(RepoError::NotFound) => {}
        other => panic!("cross-workspace get 应为 NotFound，实得 {other:?}"),
    }
    assert_eq!(repo.get(ws_a, created.id()).await.unwrap().id, created.id);
    assert_eq!(repo.count_by_owner(ws_a, owner_id).await.unwrap(), 1);

    // 列表：自己的 + 共享的；别的成员的私有视图看不到。
    assert_eq!(
        repo.list_for_user(ws_a, "workspace", owner_id, None)
            .await
            .unwrap()
            .len(),
        1
    );

    // 乐观并发：版本对 → 更新且 revision +1；版本旧 → None。
    let updated = repo
        .update(
            ws_a,
            created.id(),
            &IssueViewPatch {
                name: "Triage v2".into(),
                visibility: "workspace".into(),
                scope_variant: None,
                query: json!({"status": ["todo", "in_progress"]}),
                display: json!({"group": "status"}),
                expected_revision: 1,
            },
        )
        .await
        .expect("update");
    let updated = updated.expect("revision 1 应命中");
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.name, "Triage v2");
    assert!(updated.is_shared(), "visibility 被改写为 workspace");
    assert!(
        updated.is_readable_by(Id::from(Uuid::new_v4())),
        "共享后人人可读"
    );

    let stale = repo
        .update(
            ws_a,
            created.id(),
            &IssueViewPatch {
                name: "stale".into(),
                visibility: "workspace".into(),
                scope_variant: None,
                query: json!({}),
                display: json!({}),
                expected_revision: 1,
            },
        )
        .await
        .expect("update 不应报错");
    assert!(stale.is_none(), "旧 revision 必须返回 None（路由层 → 409）");

    repo.delete(ws_a, created.id()).await.expect("delete");
    assert!(matches!(
        repo.get(ws_a, created.id()).await,
        Err(RepoError::NotFound)
    ));
    // 删第二次：行已不在 ⇒ NotFound（与 pin 的幂等删除不同，视图删除是 404）。
    assert!(matches!(
        repo.delete(ws_a, created.id()).await,
        Err(RepoError::NotFound)
    ));

    cleanup(&pool, ws_a.0, &users).await;
    cleanup(&pool, ws_b.0, &[]).await;
}

// ---------------------------------------------------------------------------
// ② preference：无记录 = 空文档，不是 404；upsert 整文档覆盖
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn issue_view_preference_defaults_to_absent_then_round_trips() {
    let (db, pool) = setup().await;
    let (ws, users) = seed(&pool, 2).await;
    let repo = IssueViewRepo::new(db.clone());
    let ws = Id::from(ws);
    let (me, other) = (Id::from(users[0]), Id::from(users[1]));

    // 无记录 ⇒ None（路由层回 200 + prefs={} + updated_at=""，不是 404）。
    assert!(repo
        .get_preference(ws, me, "workspace", ws)
        .await
        .unwrap()
        .is_none());

    let first = repo
        .upsert_preference(ws, me, "workspace", ws, &json!({"hidden": ["builtin:all"]}))
        .await
        .expect("upsert");
    assert_eq!(first.prefs, json!({"hidden": ["builtin:all"]}));
    assert_eq!(first.scope_type, "workspace");
    assert_eq!(first.scope_id(), ws);

    // 整文档覆盖（last-write-wins）：旧键消失。
    let second = repo
        .upsert_preference(ws, me, "workspace", ws, &json!({"order": ["view:x"]}))
        .await
        .expect("upsert 2");
    assert_eq!(second.prefs, json!({"order": ["view:x"]}), "整文档替换");
    let read = repo
        .get_preference(ws, me, "workspace", ws)
        .await
        .unwrap()
        .expect("有行");
    assert_eq!(read.prefs, json!({"order": ["view:x"]}));

    // 偏好是**每用户**的：另一个成员在同 scope 下仍是空。
    assert!(repo
        .get_preference(ws, other, "workspace", ws)
        .await
        .unwrap()
        .is_none());
    // 同一个用户换 scope（my 用 user id 回填）也是独立的一格。
    assert!(repo
        .get_preference(ws, me, "my", me)
        .await
        .unwrap()
        .is_none());

    cleanup(&pool, ws.0, &users).await;
}

// ---------------------------------------------------------------------------
// ③ pin：追加位置、重复 = Conflict、删除幂等
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn pin_append_orders_by_position_and_duplicate_is_a_conflict() {
    let (db, pool) = setup().await;
    let (ws, users) = seed(&pool, 2).await;
    let repo = PinRepo::new(db.clone());
    let ws = Id::from(ws);
    let (me, other) = (Id::from(users[0]), Id::from(users[1]));

    assert_eq!(repo.max_position(ws, me).await.unwrap(), 0.0);
    let issue_a = Id::from(Uuid::new_v4());
    let issue_b = Id::from(Uuid::new_v4());

    let first = repo
        .create(ws, me, "issue", issue_a, 1.0)
        .await
        .expect("pin a");
    let second = repo
        .create(ws, me, "issue", issue_b, 2.0)
        .await
        .expect("pin b");
    assert_eq!(first.position, 1.0);
    assert_eq!(second.position, 2.0);
    assert_eq!(repo.max_position(ws, me).await.unwrap(), 2.0);

    // 重复钉同一项 ⇒ 唯一约束 23505 ⇒ Conflict（上游 409 `item already pinned`）。
    assert!(matches!(
        repo.create(ws, me, "issue", issue_a, 3.0).await,
        Err(RepoError::Conflict)
    ));

    // 行序 = position 升序。
    let rows = repo.list(ws, me).await.expect("list");
    assert_eq!(
        rows.iter().map(PinnedItemRow::item_id).collect::<Vec<_>>(),
        vec![issue_a, issue_b]
    );

    // 别人的 pin 列表是空的（pin 是每用户私有的，不是权限判定）。
    assert!(repo.list(ws, other).await.unwrap().is_empty());

    // 删除幂等：第一次 1 行，第二次 0 行，但都不是错误。
    assert_eq!(repo.delete(ws, me, "issue", issue_b).await.unwrap(), 1);
    assert_eq!(repo.delete(ws, me, "issue", issue_b).await.unwrap(), 0);
    // 别人的行删不动（0 行），也不报错。
    assert_eq!(repo.delete(ws, other, "issue", issue_a).await.unwrap(), 0);
    assert_eq!(repo.list(ws, me).await.unwrap().len(), 1);

    cleanup(&pool, ws.0, &users).await;
}

// ---------------------------------------------------------------------------
// ④ pin reorder：只改 position，不动 created_at
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn pin_reorder_rewrites_position_without_touching_created_at() {
    let (db, pool) = setup().await;
    let (ws, users) = seed(&pool, 1).await;
    let repo = PinRepo::new(db.clone());
    let ws = Id::from(ws);
    let me = Id::from(users[0]);
    let a = Id::from(Uuid::new_v4());
    let b = Id::from(Uuid::new_v4());

    let pa = repo.create(ws, me, "issue", a, 1.0).await.unwrap();
    let pb = repo.create(ws, me, "issue", b, 2.0).await.unwrap();

    // 互换：a→2，b→0.5 ⇒ 行序 b, a。
    assert_eq!(
        repo.set_position(ws, me, pa.id(), 2.0).await.unwrap(),
        1,
        "自己的行应命中 1 行"
    );
    assert_eq!(repo.set_position(ws, me, pb.id(), 0.5).await.unwrap(), 1);
    let rows = repo.list(ws, me).await.unwrap();
    assert_eq!(
        rows.iter().map(PinnedItemRow::item_id).collect::<Vec<_>>(),
        vec![b, a],
        "position 是唯一排序键"
    );
    // `created_at` 不因重排而变（`UPDATE` 只写 position 一列）。
    for row in &rows {
        let before = if row.item_id() == a {
            pa.created_at
        } else {
            pb.created_at
        };
        assert_eq!(row.created_at, before, "reorder 不得重新盖时间戳");
    }
    // 别人的 pin id ⇒ 0 行，不报错（上游同样忽略行数）。
    assert_eq!(repo.set_position(ws, me, pa.id(), 9.0).await.unwrap(), 1);

    cleanup(&pool, ws.0, &users).await;
}

// ---------------------------------------------------------------------------
// ⑤ 删视图顺手清扫 view pin（上游 CTE 的语义）
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn deleting_an_issue_view_sweeps_its_sidebar_pins() {
    let (db, pool) = setup().await;
    let (ws, users) = seed(&pool, 2).await;
    let ws = Id::from(ws);
    let (me, other) = (Id::from(users[0]), Id::from(users[1]));
    let views = IssueViewRepo::new(db.clone());
    let pins = PinRepo::new(db.clone());

    let view = views
        .create(&new_view(ws.0, users[0], "Pinned view", "workspace"))
        .await
        .expect("create view");
    // 两个用户都钉了这个视图（清扫必须是**全 workspace 的 view pin**，不只调用者那一条）。
    pins.create(ws, me, "view", view.id(), 1.0).await.unwrap();
    pins.create(ws, other, "view", view.id(), 1.0)
        .await
        .unwrap();
    // 一条指向同名但不同对象的 pin：不得被误扫。
    let untouched = pins
        .create(ws, me, "issue", Id::from(Uuid::new_v4()), 2.0)
        .await
        .unwrap();

    views.delete(ws, view.id()).await.expect("delete view");

    assert_eq!(
        pins.count_for_item(ws, "view", view.id()).await.unwrap(),
        0,
        "视图删了，指向它的 view pin 必须一起消失"
    );
    let rows = pins.list(ws, me).await.unwrap();
    assert_eq!(rows.len(), 1, "只剩那条非 view 的 pin：{rows:?}");
    assert_eq!(rows[0].id(), untouched.id());

    cleanup(&pool, ws.0, &users).await;
}

// ---------------------------------------------------------------------------
// ⑥ assignee-frequency：两路 SQL 的窗口条件与 GROUP BY 口径
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "需要 MULTICA_TEST_DATABASE_URL"]
async fn assignee_frequency_aggregates_both_sources_with_upstream_windows() {
    let (db, pool) = setup().await;
    let (ws, users) = seed(&pool, 3).await;
    let ws = Id::from(ws);
    let (me, member_target, agent_target) =
        (Id::from(users[0]), Id::from(users[1]), Id::from(users[2]));

    // ---- 源 1：activity_log 的 assignee_changed ----
    // me 改派到 member_target 两次、到 agent_target 一次 ⇒ member:×2 / agent:×1。
    for _ in 0..2 {
        insert_assignee_change(&pool, ws.0, me.0, "member", member_target.0).await;
    }
    insert_assignee_change(&pool, ws.0, me.0, "agent", agent_target.0).await;
    // 反例①：**别人**的动作不算在 me 头上。
    insert_assignee_change(&pool, ws.0, member_target.0, "member", agent_target.0).await;
    // 反例②：非 `assignee_changed` 的动作不算。
    sqlx::query(
        "INSERT INTO activity_log (workspace_id, actor_type, actor_id, action, details) \
         VALUES ($1, 'member', $2, 'status_changed', '{\"to_type\":\"agent\",\"to_id\":\"x\"}')",
    )
    .bind(ws.0)
    .bind(me.0)
    .execute(&pool)
    .await
    .expect("insert status_changed");
    // 反例③：`actor_type <> 'member'` 不算（同一个 actor id 也不行）。
    sqlx::query(
        "INSERT INTO activity_log (workspace_id, actor_type, actor_id, action, details) \
         VALUES ($1, 'agent', $2, 'assignee_changed', $3::jsonb)",
    )
    .bind(ws.0)
    .bind(me.0)
    .bind(json!({"to_type": "member", "to_id": agent_target.to_string()}).to_string())
    .execute(&pool)
    .await
    .expect("insert agent-actor row");
    // 反例④：`details` 缺 `to_id` 不算。
    sqlx::query(
        "INSERT INTO activity_log (workspace_id, actor_type, actor_id, action, details) \
         VALUES ($1, 'member', $2, 'assignee_changed', '{\"to_type\":\"member\"}')",
    )
    .bind(ws.0)
    .bind(me.0)
    .execute(&pool)
    .await
    .expect("insert incomplete details");

    // ---- 源 2：我建单时已带指派人 ----
    // member_target 两次 ⇒ +2；agent_target 一次 ⇒ +1；
    // 反例⑤：别人建的单不算；反例⑥：没指派人的单不算。
    for _ in 0..2 {
        insert_issue(&pool, ws.0, me.0, Some(("member", member_target.0))).await;
    }
    insert_issue(&pool, ws.0, me.0, Some(("agent", agent_target.0))).await;
    insert_issue(
        &pool,
        ws.0,
        member_target.0,
        Some(("member", agent_target.0)),
    )
    .await;
    insert_issue(&pool, ws.0, me.0, None).await;

    let repo = StatsRepo::new(db.clone());
    let entries = repo.assignee_frequency(ws, me).await.expect("frequency");

    // member_target：源 1 的 2 + 源 2 的 2 = 4；agent_target：1 + 1 = 2。
    let got: HashMap<(String, String), i64> = entries
        .iter()
        .map(|e| {
            (
                (e.assignee_type.clone(), e.assignee_id.clone()),
                e.frequency,
            )
        })
        .collect();
    assert_eq!(
        got.get(&("member".into(), member_target.to_string())),
        Some(&4),
        "两路必须相加：{entries:?}"
    );
    assert_eq!(
        got.get(&("agent".into(), agent_target.to_string())),
        Some(&2),
        "agent 目标只该拿源 1 的 1 + 源 2 的 1；反例行集若被算进来会变大：{entries:?}"
    );
    assert_eq!(entries.len(), 2, "反例行不得各自成一条：{entries:?}");
    assert_eq!(entries[0].assignee_id, member_target.to_string());
    assert_eq!(entries[0].frequency, 4, "频次降序");
    assert!(entries[1].frequency <= entries[0].frequency);

    // 分路读也要各自对（把两半的门分开断言，红了能指出是哪一半错了）。
    let activity: Vec<AssigneeChangeCountRow> =
        repo.count_assignee_changes_by_actor(ws, me).await.unwrap();
    let activity_sum: i64 = activity.iter().map(|r| r.frequency).sum();
    assert_eq!(activity_sum, 3, "源 1 只认 3 条：{activity:?}");
    let created: Vec<CreatedIssueAssigneeCountRow> =
        repo.count_created_issue_assignees(ws, me).await.unwrap();
    let created_sum: i64 = created.iter().map(|r| r.frequency).sum();
    assert_eq!(created_sum, 3, "源 2 只认 3 条：{created:?}");

    // 别人（没建过单、没改派过）得到空列表。
    assert!(repo
        .assignee_frequency(ws, agent_target)
        .await
        .unwrap()
        .is_empty());

    cleanup(&pool, ws.0, &users).await;
}

/// 以 `assignee_changed` 记一条改派活动（`details` 形状与上游一致）。
async fn insert_assignee_change(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    actor_id: Uuid,
    to_type: &str,
    to_id: Uuid,
) {
    sqlx::query(
        "INSERT INTO activity_log (workspace_id, actor_type, actor_id, action, details) \
         VALUES ($1, 'member', $2, 'assignee_changed', $3::jsonb)",
    )
    .bind(workspace_id)
    .bind(actor_id)
    .bind(json!({"to_type": to_type, "to_id": to_id.to_string()}).to_string())
    .execute(pool)
    .await
    .expect("insert assignee_changed");
}

/// 建一条 issue（`assignee` 为 `None` 时不带指派人）。
async fn insert_issue(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    creator_id: Uuid,
    assignee: Option<(&str, Uuid)>,
) {
    let (assignee_type, assignee_id) = match assignee {
        Some((t, id)) => (Some(t.to_string()), Some(id)),
        None => (None, None),
    };
    sqlx::query(
        // `issue` 有 UNIQUE (workspace_id, number) 而 `number` 默认 0 ⇒ 同 workspace 连建多
        // 条必须自己发号（生产路径由 handler 发号，这里直插表所以要显式补）。
        "INSERT INTO issue (workspace_id, title, creator_type, creator_id, assignee_type, \
                            assignee_id, number) \
         VALUES ($1, 'itest assignee frequency', 'member', $2, $3::text, $4::uuid, \
                 (SELECT COALESCE(MAX(i.number), 0) + 1 FROM issue i WHERE i.workspace_id = $1))",
    )
    .bind(workspace_id)
    .bind(creator_id)
    .bind(assignee_type)
    .bind(assignee_id)
    .execute(pool)
    .await
    .expect("insert issue");
}
