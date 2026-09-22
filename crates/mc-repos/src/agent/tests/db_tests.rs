//! `AgentRepo` 的 PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL` 触发）。
//!
//! 由 `tests.rs` 拆出（R7 单文件 800 行硬上限，门 ⑩）。手感与 `issue_status.rs`
//! 一致：**未设置** `MULTICA_TEST_DATABASE_URL` → 每条打印跳过并 `return`；
//! **已设置但连不上 / 没建表** → panic（不许静默跳过假装绿）。
//!
//! 目标库必须是**上游 schema**（`contracts/upstream-schema.sql`）：
//! `cargo run -p mc-migrate -- run --dir migrations`（见 `docs/26-W0-SCHEMA-SWITCHOVER.md`）。

use super::super::*;
use crate::agent::tasks::is_visible_task_history;
use crate::RepoError;
use pretty_assertions::assert_eq;
use serde_json::json;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

/// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`（测试打印跳过）；**已设置但连不上**
/// → panic（否则库坏了也会报绿）。
async fn setup() -> Option<(Db, Id)> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let db = Db::connect(&url, 4, 1)
        .await
        .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m3b-agent', $1) RETURNING id",
    )
    .bind(format!("itest-m3b-a-{}", Uuid::new_v4()))
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

/// 建一个 `user` 行（`agent.archived_by` 有 FK 指向 `\"user\"`，归档要真 actor）。
async fn seed_user(db: &Db, ws: Id) -> Uuid {
    sqlx::query_scalar("INSERT INTO \"user\" (name, email) VALUES ($1, $2) RETURNING id")
        .bind(format!("u-{}", Uuid::new_v4()))
        .bind(format!("u-{}@example.test", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .unwrap_or_else(|e| panic!("user fixture failed (ws {ws:?}): {e}"))
}

/// 建 runtime（create/update 的 `runtime_id` 绑定校验用）。
async fn seed_runtime(db: &Db, ws: Id) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent_runtime (workspace_id, name, runtime_mode, provider, owner_id, visibility) \
         VALUES ($1, $2, 'local', 'claude', NULL, 'public') RETURNING id",
    )
    .bind(ws.0)
    .bind(format!("rt-{}", Uuid::new_v4()))
    .fetch_one(db.pool())
    .await
    .expect("runtime")
}

fn new_agent(ws: Id, name: &str) -> NewAgent {
    NewAgent {
        workspace_id: ws,
        name: name.into(),
        description: String::new(),
        instructions: String::new(),
        avatar_url: None,
        runtime_mode: "local".into(),
        runtime_id: None,
        runtime_config: None,
        visibility: VISIBILITY_PRIVATE.into(),
        permission_mode: PERMISSION_MODE_PRIVATE.into(),
        max_concurrent_tasks: None,
        owner_id: None,
        custom_env: None,
        custom_args: None,
        mcp_config: None,
        model: None,
        thinking_level: None,
        service_tier: None,
        conversation_starters: None,
        composio_toolkit_allowlist: None,
    }
}

async fn teardown(db: &Db, ws: Id) {
    for sql in [
        "DELETE FROM agent_to_label WHERE agent_id IN (SELECT id FROM agent WHERE workspace_id = $1)",
        "DELETE FROM agent_invocation_target WHERE agent_id IN (SELECT id FROM agent WHERE workspace_id = $1)",
        "DELETE FROM agent_task_queue WHERE agent_id IN (SELECT id FROM agent WHERE workspace_id = $1)",
        "DELETE FROM agent WHERE workspace_id = $1",
        "DELETE FROM issue_label WHERE workspace_id = $1",
        "DELETE FROM agent_runtime WHERE workspace_id = $1",
        "DELETE FROM workspace WHERE id = $1",
    ] {
        let _ = sqlx::query(sql).bind(ws.0).execute(db.pool()).await;
    }
}

// ---------------------------------------------------------------------------
// 建
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_create_applies_upstream_column_defaults() {
    let (db, ws) = fixture!();
    let repo = AgentRepo::new(db.clone());
    let runtime_id = seed_runtime(&db, ws).await;

    let mut input = new_agent(ws, "defaults");
    input.runtime_id = Some(runtime_id);
    let row = repo.create(&input).await.expect("create");

    // 列默认值逐条对齐 contracts/upstream-schema.sql 的 `agent` 定义
    assert_eq!(row.max_concurrent_tasks, DEFAULT_MAX_CONCURRENT_TASKS);
    assert_eq!(row.visibility, VISIBILITY_PRIVATE);
    assert_eq!(row.permission_mode, PERMISSION_MODE_PRIVATE);
    assert_eq!(row.status, "offline");
    assert_eq!(row.kind, "user");
    assert_eq!(row.custom_env, json!({}));
    assert_eq!(row.custom_args, json!([]));
    assert_eq!(row.conversation_starters, json!([]));
    assert_eq!(row.disabled_runtime_skills, json!([]));
    assert_eq!(row.runtime_config, json!({}));
    assert_eq!(row.system_key, None);
    assert_eq!(row.description, "");
    assert_eq!(row.instructions, "");
    assert_eq!(row.archived_at, None);

    // 请求里显式给了值 → 覆盖默认值（`COALESCE(narg, default)` 的另一半）
    let mut explicit = new_agent(ws, "explicit");
    explicit.runtime_id = Some(runtime_id);
    explicit.description = "d".into();
    explicit.instructions = "i".into();
    explicit.visibility = VISIBILITY_WORKSPACE.into();
    explicit.permission_mode = PERMISSION_MODE_PUBLIC_TO.into();
    explicit.max_concurrent_tasks = Some(7);
    explicit.model = Some("claude-sonnet".into());
    explicit.thinking_level = Some("high".into());
    explicit.service_tier = Some("priority".into());
    explicit.custom_env = Some(json!({"A": "1"}));
    explicit.custom_args = Some(json!(["--x"]));
    explicit.mcp_config = Some(json!({"mcpServers": {}}));
    explicit.conversation_starters = Some(json!([{"label": "l", "prompt": "p"}]));
    explicit.composio_toolkit_allowlist = Some(vec!["gmail".into(), "slack".into()]);
    let row = repo.create(&explicit).await.expect("create explicit");
    assert_eq!(row.description, "d");
    assert_eq!(row.instructions, "i");
    assert_eq!(row.visibility, VISIBILITY_WORKSPACE);
    assert_eq!(row.permission_mode, PERMISSION_MODE_PUBLIC_TO);
    assert_eq!(row.max_concurrent_tasks, 7);
    assert_eq!(row.model.as_deref(), Some("claude-sonnet"));
    assert_eq!(row.thinking_level.as_deref(), Some("high"));
    assert_eq!(row.service_tier.as_deref(), Some("priority"));
    assert_eq!(row.custom_env, json!({"A": "1"}));
    assert_eq!(row.custom_args, json!(["--x"]));
    assert_eq!(row.mcp_config, Some(json!({"mcpServers": {}})));
    assert_eq!(
        row.conversation_starters,
        json!([{"label": "l", "prompt": "p"}])
    );
    assert_eq!(
        row.composio_toolkit_allowlist.as_ref().map(Vec::len),
        Some(2)
    );
    assert_eq!(row.custom_env_key_count(), 1);
    assert!(row.has_composio_allowlist());
    assert!(!row.is_archived());
    assert!(!row.is_system());
    // 主键/workspace 访问器
    assert_eq!(row.workspace_id(), ws);
    assert_eq!(repo.get(row.id()).await.expect("get").id(), row.id());

    // 同名冲突 → Conflict（上游 `agent_workspace_name_active` 唯一索引 → 409）
    let dup = repo.create(&new_agent(ws, "defaults")).await;
    assert!(
        matches!(dup, Err(RepoError::Conflict)),
        "duplicate name should conflict: {dup:?}"
    );

    // **列默认值本身**（不只映射层）：直接问 catalog，防止夹具/迁移漂移。
    let defaults: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT column_name, column_default FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'agent' \
           AND column_name IN ('max_concurrent_tasks','visibility','permission_mode','status', \
                               'kind','custom_env','custom_args','conversation_starters', \
                               'disabled_runtime_skills','runtime_config')",
    )
    .fetch_all(db.pool())
    .await
    .expect("catalog");
    let get = |name: &str| {
        defaults
            .iter()
            .find(|(c, _)| c == name)
            .and_then(|(_, d)| d.clone())
            .unwrap_or_default()
    };
    assert_eq!(get("max_concurrent_tasks"), "6");
    assert_eq!(get("visibility"), "'private'::text");
    assert_eq!(get("permission_mode"), "'private'::text");
    assert_eq!(get("status"), "'offline'::text");
    assert_eq!(get("kind"), "'user'::text");
    assert_eq!(get("custom_env"), "'{}'::jsonb");
    assert_eq!(get("custom_args"), "'[]'::jsonb");
    assert_eq!(get("conversation_starters"), "'[]'::jsonb");
    assert_eq!(get("disabled_runtime_skills"), "'[]'::jsonb");
    assert_eq!(get("runtime_config"), "'{}'::jsonb");

    teardown(&db, ws).await;
}

// ---------------------------------------------------------------------------
// 列表 / 详情
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_list_and_get_in_workspace_are_kind_and_workspace_scoped() {
    let (db, ws) = fixture!();
    let other_ws = {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m3b-other', $1) RETURNING id",
        )
        .bind(format!("itest-m3b-o-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .expect("other ws");
        Id::from(id)
    };
    let repo = AgentRepo::new(db.clone());

    let first = repo.create(&new_agent(ws, "a-first")).await.expect("a");
    let second = repo.create(&new_agent(ws, "a-second")).await.expect("b");
    let foreign = repo
        .create(&new_agent(other_ws, "a-foreign"))
        .await
        .expect("c");
    // system agent（`kind='system'`，CreateAgent 不产生 → 直接 SQL）
    let system_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent (workspace_id, name, runtime_mode, kind, system_key) \
         VALUES ($1, 'a-system', 'local', 'system', 'mika') RETURNING id",
    )
    .bind(ws.0)
    .fetch_one(db.pool())
    .await
    .expect("system agent");

    // 归档 second（`archived_at` 非空 + `archived_by` 真 actor，有 FK 指向 `"user"`）
    let actor = seed_user(&db, ws).await;
    let archived = repo
        .archive(second.id(), Some(actor))
        .await
        .expect("archive");
    assert!(archived.is_archived());
    assert!(archived.archived_at.is_some());
    assert_eq!(archived.archived_by, Some(actor));

    // `list(ws, true)`：上游 `ListAllAgents` = 只 user kind + 不带 archived 过滤，
    // 按 created_at ASC（workspace 也限定）。
    let all = repo.list(ws, true).await.expect("list all");
    let names: Vec<&str> = all.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["a-first", "a-second"]);
    assert_eq!(all[0].id, first.id().0, "created_at ASC");
    assert!(all.iter().all(|r| r.kind == "user"));
    assert!(all.iter().all(|r| r.workspace_id == ws.0));
    assert!(all.iter().all(|r| r.id != system_id));
    assert!(all.iter().all(|r| r.id != foreign.id().0));

    // `include_archived=false` → 只活跃
    let active = repo.list(ws, false).await.expect("list active");
    assert_eq!(
        active.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["a-first"]
    );

    // `get_in_workspace` 跨 workspace → NotFound（上游 `GetAgentInWorkspace`）
    let miss = repo.get_in_workspace(ws, foreign.id()).await;
    assert!(matches!(miss, Err(RepoError::NotFound)), "{miss:?}");
    // 无 workspace 限定的 `get` 能读到（handler 用前者）
    assert_eq!(
        repo.get(foreign.id())
            .await
            .expect("get foreign")
            .workspace_id(),
        other_ws
    );

    // restore 清掉归档三态
    let restored = repo.restore(second.id()).await.expect("restore");
    assert!(!restored.is_archived());
    assert_eq!(restored.archived_by, None);

    teardown(&db, other_ws).await;
    teardown(&db, ws).await;
}

// ---------------------------------------------------------------------------
// 更新（COALESCE + 显式清空）
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_update_coalesces_and_clears_nullable_columns() {
    let (db, ws) = fixture!();
    let repo = AgentRepo::new(db.clone());
    let runtime_id = seed_runtime(&db, ws).await;

    let mut input = new_agent(ws, "upd");
    input.runtime_id = Some(runtime_id);
    input.mcp_config = Some(json!({"a": 1}));
    input.thinking_level = Some("low".into());
    input.service_tier = Some("default".into());
    input.composio_toolkit_allowlist = Some(vec!["gmail".into()]);
    let row = repo.create(&input).await.expect("create");

    // 只改 name：其余列必须原样保留（`COALESCE(narg, col)`）
    let patch = AgentUpdatePatch {
        id: row.id(),
        name: Some("upd-renamed".into()),
        ..Default::default()
    };
    let updated = repo.update(&patch).await.expect("update name");
    assert_eq!(updated.name, "upd-renamed");
    assert_eq!(updated.mcp_config, Some(json!({"a": 1})));
    assert_eq!(updated.thinking_level.as_deref(), Some("low"));
    assert_eq!(updated.service_tier.as_deref(), Some("default"));
    assert_eq!(
        updated.composio_toolkit_allowlist.as_ref().map(Vec::len),
        Some(1)
    );
    assert_eq!(updated.created_at, row.created_at);
    assert!(updated.updated_at >= row.updated_at);

    // 覆盖可空列（`Some(Some(v))`）
    let patch = AgentUpdatePatch {
        id: row.id(),
        mcp_config: Some(Some(json!({"a": 2}))),
        thinking_level: Some(Some("high".into())),
        ..Default::default()
    };
    let updated = repo.update(&patch).await.expect("update nullable");
    assert_eq!(updated.mcp_config, Some(json!({"a": 2})));
    assert_eq!(updated.thinking_level.as_deref(), Some("high"));

    // 事务性字段一起改（runtime 换绑 + visibility/permission_mode + 并发上限）
    let runtime2 = seed_runtime(&db, ws).await;
    let patch = AgentUpdatePatch {
        id: row.id(),
        runtime_id: Some(runtime2),
        visibility: Some(VISIBILITY_WORKSPACE.into()),
        permission_mode: Some(PERMISSION_MODE_PUBLIC_TO.into()),
        max_concurrent_tasks: Some(9),
        status: Some("working".into()),
        custom_args: Some(json!(["--y"])),
        ..Default::default()
    };
    let updated = repo.update(&patch).await.expect("update txn cols");
    assert_eq!(updated.runtime_id, Some(runtime2));
    assert_eq!(updated.visibility, VISIBILITY_WORKSPACE);
    assert_eq!(updated.permission_mode, PERMISSION_MODE_PUBLIC_TO);
    assert_eq!(updated.max_concurrent_tasks, 9);
    assert_eq!(updated.status, "working");
    assert_eq!(updated.custom_args, json!(["--y"]));

    // 显式清空（`clear_nullable` 的列名静态，不来自请求）
    for field in [
        NullableAgentField::McpConfig,
        NullableAgentField::ThinkingLevel,
        NullableAgentField::ServiceTier,
        NullableAgentField::ComposioToolkitAllowlist,
    ] {
        repo.clear_nullable(row.id(), field).await.expect("clear");
    }
    let cleared = repo.get(row.id()).await.expect("get");
    assert_eq!(cleared.mcp_config, None);
    assert_eq!(cleared.thinking_level, None);
    assert_eq!(cleared.service_tier, None);
    assert_eq!(cleared.composio_toolkit_allowlist, None);
    assert!(!cleared.has_composio_allowlist());

    // `update_custom_env` 是独立路径（env 端点专用，改 `custom_env` 不动别的列）
    let env = json!({"B": "2"});
    let row = repo.update_custom_env(row.id(), &env).await.expect("env");
    assert_eq!(row.custom_env, env);
    assert_eq!(row.name, "upd-renamed");

    // 更新不存在的 id → NotFound
    let patch = AgentUpdatePatch {
        id: Id::from(Uuid::new_v4()),
        name: Some("nope".into()),
        ..Default::default()
    };
    assert!(matches!(
        repo.update(&patch).await,
        Err(RepoError::NotFound)
    ));

    teardown(&db, ws).await;
}

// ---------------------------------------------------------------------------
// 允许列表
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_invocation_targets_replace_and_batch_load() {
    let (db, ws) = fixture!();
    let repo = AgentRepo::new(db.clone());
    let agent = repo.create(&new_agent(ws, "targets")).await.expect("a");
    let other = repo.create(&new_agent(ws, "targets-2")).await.expect("b");
    let creator = Uuid::new_v4();
    let member1 = Uuid::new_v4();
    let member2 = Uuid::new_v4();

    assert!(repo
        .list_invocation_targets(agent.id())
        .await
        .expect("empty")
        .is_empty());

    // 整表替换：workspace + 两个 member（上游 `replaceInvocationTargets`）
    repo.replace_invocation_targets(
        agent.id(),
        Some(creator),
        &[
            (TARGET_WORKSPACE.into(), ws.0),
            (TARGET_MEMBER.into(), member1),
            (TARGET_MEMBER.into(), member2),
        ],
    )
    .await
    .expect("replace");
    let rows = repo
        .list_invocation_targets(agent.id())
        .await
        .expect("list");
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(AgentInvocationTargetRow::is_known_type));
    assert_eq!(rows[0].target_type, TARGET_WORKSPACE);
    assert_eq!(rows[0].target_id, ws.0);
    assert_eq!(rows[0].created_by, Some(creator));
    assert_eq!(
        rows.iter()
            .filter(|r| r.target_type == TARGET_MEMBER)
            .count(),
        2
    );

    // 再替换一次 = 先删后插（不是追加）
    repo.replace_invocation_targets(agent.id(), None, &[(TARGET_MEMBER.into(), member2)])
        .await
        .expect("replace 2");
    let rows = repo
        .list_invocation_targets(agent.id())
        .await
        .expect("list 2");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].target_id, member2);
    assert_eq!(rows[0].created_by, None);

    // 批量加载（上游 `loadInvocationTargetsByAgent` 喂 `accessibleAgentIDs`）
    assert!(repo
        .list_invocation_targets_for_agents(&[])
        .await
        .expect("empty batch")
        .is_empty());
    repo.replace_invocation_targets(other.id(), None, &[(TARGET_TEAM.into(), Uuid::new_v4())])
        .await
        .expect("replace other");
    let batch = repo
        .list_invocation_targets_for_agents(&[agent.id().0, other.id().0, Uuid::new_v4()])
        .await
        .expect("batch");
    assert_eq!(batch.len(), 2);
    assert_eq!(
        batch.iter().filter(|r| r.agent_id == other.id().0).count(),
        1
    );

    // 清空
    repo.replace_invocation_targets(agent.id(), None, &[])
        .await
        .expect("clear");
    assert!(repo
        .list_invocation_targets(agent.id())
        .await
        .expect("clear list")
        .is_empty());

    teardown(&db, ws).await;
}

// ---------------------------------------------------------------------------
// label
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_labels_attach_detach_are_resource_type_guarded() {
    let (db, ws) = fixture!();
    let repo = AgentRepo::new(db.clone());
    let agent = repo.create(&new_agent(ws, "labelled")).await.expect("a");

    let seed_label = |name: &'static str, resource_type: &'static str| {
        let pool = db.pool().clone();
        async move {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO issue_label (workspace_id, name, color, resource_type) \
                 VALUES ($1, $2, 'blue', $3) RETURNING id",
            )
            .bind(ws.0)
            .bind(name)
            .bind(resource_type)
            .fetch_one(&pool)
            .await
            .expect("label");
            Id::from(id)
        }
    };
    let zeta = seed_label("zeta", "agent").await;
    let alpha = seed_label("alpha", "agent").await;
    let issue_label = seed_label("issue-only", "issue").await;

    // 挂载 + 幂等（`ON CONFLICT DO NOTHING` → rows_affected = 0）
    assert_eq!(
        repo.attach_label(agent.id(), zeta, ws)
            .await
            .expect("attach"),
        1
    );
    assert_eq!(
        repo.attach_label(agent.id(), zeta, ws)
            .await
            .expect("attach dup"),
        0
    );
    assert_eq!(
        repo.attach_label(agent.id(), alpha, ws)
            .await
            .expect("attach 2"),
        1
    );

    // resource_type guard（`issue_label` 的 label 挂不上）
    assert_eq!(
        repo.attach_label(agent.id(), issue_label, ws)
            .await
            .expect("attach issue label"),
        0
    );

    // 列表：只 `resource_type='agent'`，按 `LOWER(name)` 升序
    let labels = repo.list_labels(agent.id()).await.expect("labels");
    assert_eq!(
        labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        vec!["alpha", "zeta"]
    );
    assert_eq!(labels[0].workspace_id(), ws);
    assert_eq!(labels[0].resource_type, "agent");

    // `get_label` 让 handler 能判 404（issue label 存在 → 但 resource_type != agent）
    let fetched = repo.get_label(ws, issue_label).await.expect("get label");
    assert_eq!(fetched.resource_type, "issue");
    assert!(repo.get_label(ws, zeta).await.is_ok());
    assert!(matches!(
        repo.get_label(ws, Id::from(Uuid::new_v4())).await,
        Err(RepoError::NotFound)
    ));

    // 跨 workspace 的 agent → EXISTS 守卫挡住（rows_affected = 0）
    let other_ws = Id::from(Uuid::new_v4());
    assert_eq!(
        repo.attach_label(agent.id(), zeta, other_ws)
            .await
            .expect("wrong ws attach"),
        0
    );

    // 卸载
    assert_eq!(
        repo.detach_label(agent.id(), alpha, ws)
            .await
            .expect("detach"),
        1
    );
    assert_eq!(
        repo.detach_label(agent.id(), alpha, ws)
            .await
            .expect("detach again"),
        0
    );
    let labels = repo.list_labels(agent.id()).await.expect("labels 2");
    assert_eq!(
        labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        vec!["zeta"]
    );

    teardown(&db, ws).await;
}

#[cfg(test)]
mod tasks;
// ---------------------------------------------------------------------------
// runtime 绑定
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_runtime_binding_requires_same_workspace() {
    let (db, ws) = fixture!();
    let repo = AgentRepo::new(db.clone());
    let runtime_id = seed_runtime(&db, ws).await;

    let binding = repo
        .runtime_binding(ws, runtime_id)
        .await
        .expect("same ws binding");
    assert_eq!(binding.id, runtime_id);
    assert_eq!(binding.workspace_id, ws.0);
    assert_eq!(binding.runtime_mode, "local");
    assert_eq!(binding.visibility, "public");
    assert_eq!(binding.provider, "claude");

    // 跨 workspace → NotFound（handler 翻成 400 `invalid runtime_id`）
    let miss = repo
        .runtime_binding(Id::from(Uuid::new_v4()), runtime_id)
        .await;
    assert!(matches!(miss, Err(RepoError::NotFound)), "{miss:?}");
    assert!(matches!(
        repo.runtime_binding(ws, Uuid::new_v4()).await,
        Err(RepoError::NotFound)
    ));

    teardown(&db, ws).await;
}
