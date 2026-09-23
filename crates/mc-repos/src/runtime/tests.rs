//! `mc-repos::runtime` 的 PG 集成测试（M3-4 / LUM-1427）。
//!
//! 从 `profiles.rs` 拆出并集中到一处：四块实现（profiles / ledger / teardown / usage）
//! 共用同一套 fixture，避免每处各写一份 workspace/agent/task 建表逻辑。
//!
//! 运行：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc:mc@127.0.0.1:5432/multica_lum1427 \
//!   cargo test -p mc-repos --lib -- --ignored runtime
//! ```
//! 没有该 env 时静默 skip（与 `crate::issue_table::tests` 一致）。表由门禁 ⑥ 的
//! `mc-migrate run --dir migrations` 预先建好，测试自己不做迁移（重复迁移会
//! `already exists`）。

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use mc_core::Id;

mod scope;

use super::ledger::{BlockingAgentRow, NewAgentRuntime, RuntimeListFilter};
use super::profiles::{NewRuntimeProfile, UpdateRuntimeProfile};
use super::teardown::DeleteRuntimeError;
use super::{AgentRuntimeRepo, ProfileDeleteError, RuntimeProfileRepo};
use crate::RepoError;

const TZ_SHANGHAI: &str = "Asia/Shanghai";

/// 一套隔离 fixture：自己的 workspace + owner + 两个 repo。
///
/// 每个测试用独立 workspace（随机 UUID），所以断言可以直接按 workspace 过滤，
/// 并行跑也不会互相看见。
struct Fixture {
    pool: PgPool,
    profiles: RuntimeProfileRepo,
    runtimes: AgentRuntimeRepo,
    ws: Id,
    owner: Id,
}

impl Fixture {
    async fn setup() -> Option<Self> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let pool = PgPool::connect(&url).await.ok()?;
        let owner = Uuid::new_v4();
        let ws = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "user"(id, name, email) VALUES ($1, 'lum1427', $2)"#)
            .bind(owner)
            .bind(format!("lum1427-{owner}@example.com"))
            .execute(&pool)
            .await
            .ok()?;
        sqlx::query("INSERT INTO workspace(id, name, slug) VALUES ($1, 'lum1427', $2)")
            .bind(ws)
            .bind(format!("lum1427-{}", ws.simple()))
            .execute(&pool)
            .await
            .ok()?;
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'owner')")
            .bind(ws)
            .bind(owner)
            .execute(&pool)
            .await
            .ok()?;
        let db = pool.clone();
        Some(Self {
            pool,
            profiles: RuntimeProfileRepo::from_pool(db.clone()),
            runtimes: AgentRuntimeRepo::from_pool(db),
            ws: Id::from(ws),
            owner: Id::from(owner),
        })
    }

    fn profile(&self, name: &str) -> NewRuntimeProfile {
        NewRuntimeProfile {
            workspace_id: self.ws,
            display_name: name.to_owned(),
            protocol_family: "codex".to_owned(),
            command_name: "codex".to_owned(),
            description: Some("house wrapper".to_owned()),
            fixed_args: vec!["--json".to_owned()],
            created_by: Some(self.owner),
            enabled: true,
            runtime_type: "codex".to_owned(),
        }
    }

    fn runtime(&self, name: &str, provider: &str, profile_id: Option<Id>) -> NewAgentRuntime {
        NewAgentRuntime {
            workspace_id: self.ws,
            daemon_id: Some(format!("daemon-{}", Uuid::new_v4().simple())),
            name: name.to_owned(),
            runtime_mode: "local".to_owned(),
            provider: provider.to_owned(),
            owner_id: Some(self.owner),
            profile_id,
            custom_name: None,
        }
    }

    /// 再建一个真实 user（`agent_runtime.owner_id` 有 FK，不能拿裸 UUID 当 owner）。
    async fn add_user(&self) -> Id {
        let id = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "user"(id, name, email) VALUES ($1, 'other', $2)"#)
            .bind(id)
            .bind(format!("lum1427-other-{id}@example.com"))
            .execute(&self.pool)
            .await
            .unwrap();
        Id::from(id)
    }

    /// 建 agent；`kind = 'system'` 用 `system_key` 标记（MUL-5559 的分类口径）。
    async fn add_agent(&self, runtime_id: Id, name: &str, kind: &str) -> Id {
        let system_key = (kind == "system").then(|| "mika".to_owned());
        Id::from(
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, kind, system_key) \
                 VALUES ($1, $2, 'local', $3, $4, $5) RETURNING id",
            )
            .bind(self.ws.as_uuid())
            .bind(name)
            .bind(runtime_id.as_uuid())
            .bind(kind)
            .bind(system_key)
            .fetch_one(&self.pool)
            .await
            .unwrap(),
        )
    }

    async fn add_task(&self, agent_id: Id, runtime_id: Id, status: &str) -> Id {
        let terminal = matches!(status, "completed" | "failed" | "cancelled");
        Id::from(
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO agent_task_queue(agent_id, runtime_id, status, completed_at) \
                 VALUES ($1, $2, $3, CASE WHEN $4 THEN now() ELSE NULL END) RETURNING id",
            )
            .bind(agent_id.as_uuid())
            .bind(runtime_id.as_uuid())
            .bind(status)
            .bind(terminal)
            .fetch_one(&self.pool)
            .await
            .unwrap(),
        )
    }

    async fn archive_agent(&self, agent_id: Id) {
        sqlx::query("UPDATE agent SET archived_at = now() WHERE id = $1")
            .bind(agent_id.as_uuid())
            .execute(&self.pool)
            .await
            .unwrap();
    }

    /// 尽力清理（workspace 级联够用；失败不影响断言结果）。
    async fn cleanup(&self) {
        for sql in [
            "DELETE FROM agent_runtime WHERE workspace_id = $1",
            "DELETE FROM runtime_profile WHERE workspace_id = $1",
            "DELETE FROM workspace WHERE id = $1",
        ] {
            let _ = sqlx::query(sql)
                .bind(self.ws.as_uuid())
                .execute(&self.pool)
                .await;
        }
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(self.owner.as_uuid())
            .execute(&self.pool)
            .await;
    }
}

async fn setup() -> Option<Fixture> {
    let fx = Fixture::setup().await;
    if fx.is_none() {
        eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
    }
    fx
}

fn ids(rows: &[BlockingAgentRow]) -> Vec<String> {
    let mut names: Vec<String> = rows.iter().map(|a| a.name.clone()).collect();
    names.sort();
    names
}

fn at(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn update_patch_distinguishes_untouched_from_cleared() {
    let noop = UpdateRuntimeProfile::default();
    assert!(noop.description.is_none(), "None = 该列不动");
    let clear = UpdateRuntimeProfile {
        description: Some(None),
        ..UpdateRuntimeProfile::default()
    };
    assert_eq!(clear.description, Some(None), "Some(None) = 清空");
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn profile_crud_roundtrip_and_conflict() {
    let Some(fx) = setup().await else {
        return;
    };

    let created = fx.profiles.create(fx.profile("House Codex")).await.unwrap();
    assert_eq!(created.display_name, "House Codex");
    // visibility 由服务端强制 workspace（upstream `runtimeProfileDefaultVisibility`）。
    assert_eq!(created.visibility, "workspace");
    assert_eq!(created.fixed_args, serde_json::json!(["--json"]));
    assert_eq!(created.created_by, Some(fx.owner));

    assert_eq!(fx.profiles.list(fx.ws).await.unwrap().len(), 1);
    assert!(fx.profiles.get(fx.ws, created.id).await.unwrap().is_some());
    assert!(fx
        .profiles
        .get(Id::new(), created.id)
        .await
        .unwrap()
        .is_none());

    let updated = fx
        .profiles
        .update(
            fx.ws,
            created.id,
            UpdateRuntimeProfile {
                display_name: Some("Renamed".to_owned()),
                enabled: Some(false),
                description: Some(None),
                fixed_args: Some(vec![]),
                command_name: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.display_name, "Renamed");
    assert!(!updated.enabled);
    assert!(updated.description.is_none(), "Some(None) 清空 description");
    assert_eq!(updated.fixed_args, serde_json::json!([]));
    assert_eq!(updated.command_name, "codex", "未给的列不动");

    let missing = fx
        .profiles
        .update(fx.ws, Id::new(), UpdateRuntimeProfile::default())
        .await;
    assert!(
        matches!(missing, Err(RepoError::NotFound)),
        "got {missing:?}"
    );

    let dup = fx
        .profiles
        .create(fx.profile("Renamed"))
        .await
        .expect_err("UNIQUE(workspace_id, display_name) 必须真实触发");
    assert!(matches!(dup, RepoError::Conflict), "got {dup:?}");

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn profile_delete_is_blocked_by_active_agents_then_cascades() {
    let Some(fx) = setup().await else {
        return;
    };
    let profile = fx.profiles.create(fx.profile("Cascade")).await.unwrap();
    let rt = fx
        .runtimes
        .create(fx.runtime("codex (host)", "codex", Some(profile.id)))
        .await
        .unwrap();
    let bob = fx.add_agent(rt.id, "bob", "user").await;

    let blocked = fx
        .profiles
        .delete_cascade(fx.ws, profile.id)
        .await
        .expect_err("活跃 agent 在绑时必须 409");
    match blocked {
        ProfileDeleteError::Blocked {
            agents,
            active_agent_count,
            ..
        } => {
            assert_eq!(active_agent_count, 1);
            assert_eq!(ids(&agents), vec!["bob".to_owned()]);
            assert_eq!(agents[0].runtime_status, "offline");
            assert_eq!(agents[0].blocker_class, "user");
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
    assert!(fx.profiles.get(fx.ws, profile.id).await.unwrap().is_some());

    // 归档阻塞者即可级联：runtime 与 profile 消失，agent 行保留（MUL-5559）。
    fx.archive_agent(bob).await;
    let outcome = fx.profiles.delete_cascade(fx.ws, profile.id).await.unwrap();
    assert_eq!(outcome.deleted_runtime_ids, vec![rt.id]);
    assert_eq!(outcome.agents_unbound, 1);
    assert!(fx.profiles.get(fx.ws, profile.id).await.unwrap().is_none());
    assert!(fx.runtimes.get(rt.id).await.unwrap().is_none());
    let kept: Option<(Option<Uuid>,)> =
        sqlx::query_as("SELECT runtime_id FROM agent WHERE id = $1")
            .bind(bob.as_uuid())
            .fetch_optional(&fx.pool)
            .await
            .unwrap();
    assert_eq!(kept, Some((None,)), "agent 行保留且已解绑");

    let again = fx.profiles.delete_cascade(fx.ws, profile.id).await;
    assert!(
        matches!(again, Err(ProfileDeleteError::NotFound)),
        "got {again:?}"
    );
    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn list_filters_by_role_and_owner() {
    let Some(fx) = setup().await else {
        return;
    };
    let mine = fx
        .runtimes
        .create(fx.runtime("mine", "codex", None))
        .await
        .unwrap();
    // 别人的机器：owner 是 stranger，默认 private；改成 public 后普通成员才看得见。
    let stranger = fx.add_user().await;
    let mut theirs = fx.runtime("theirs", "claude", None);
    theirs.owner_id = Some(stranger);
    let other = fx.runtimes.create(theirs).await.unwrap();
    fx.runtimes
        .set_visibility(other.id, "public")
        .await
        .unwrap();

    let all = fx
        .runtimes
        .list(fx.ws, RuntimeListFilter::All)
        .await
        .unwrap();
    assert_eq!(all.len(), 2, "owner/admin 看全量（含别人的 private）");
    let owned = fx
        .runtimes
        .list(fx.ws, RuntimeListFilter::Owner(fx.owner))
        .await
        .unwrap();
    assert_eq!(owned.len(), 1, "?owner=me 只要自己名下的");
    assert_eq!(owned[0].id, mine.id);
    let visible = fx
        .runtimes
        .list(fx.ws, RuntimeListFilter::Visible(stranger))
        .await
        .unwrap();
    assert_eq!(visible.len(), 1, "成员可见 = owner 匹配 ∪ public");
    assert_eq!(visible[0].id, other.id);
    let as_owner = fx
        .runtimes
        .list(fx.ws, RuntimeListFilter::Visible(fx.owner))
        .await
        .unwrap();
    assert_eq!(as_owner.len(), 2, "自己的 + 别人的 public");
    assert!(visible[0].usable_by(stranger), "owner 可用");
    assert!(mine.usable_by(fx.owner), "private 的 owner 可用");
    assert!(!mine.usable_by(stranger), "private 对别人不可用");

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn rename_covers_custom_name_and_machine_wide_apply() {
    let Some(fx) = setup().await else {
        return;
    };
    let a = fx
        .runtimes
        .create(fx.runtime("codex (a)", "codex", None))
        .await
        .unwrap();
    let mut second = fx.runtime("codex (b)", "omp", None);
    second.daemon_id = a.daemon_id.clone();
    let b = fx.runtimes.create(second).await.unwrap();
    assert_ne!(a.id, b.id, "同 daemon 不同 provider 各一行");

    let renamed = fx
        .runtimes
        .set_custom_name(a.id, Some("我的机器"))
        .await
        .unwrap();
    assert_eq!(renamed.display_name(), "我的机器");
    let cleared = fx.runtimes.set_custom_name(a.id, None).await.unwrap();
    assert!(cleared.custom_name.is_none());
    assert_eq!(cleared.display_name(), "codex (a)", "清空回落 daemon 名");

    let touched = fx
        .runtimes
        .set_custom_name_by_daemon(
            fx.ws,
            a.daemon_id.as_deref().unwrap(),
            Some(fx.owner),
            Some("整机名"),
        )
        .await
        .unwrap();
    assert_eq!(touched.len(), 2, "同机两个 provider 一起改");
    let untouched = fx
        .runtimes
        .set_custom_name_by_daemon(
            fx.ws,
            a.daemon_id.as_deref().unwrap(),
            Some(Id::new()),
            Some("不该生效"),
        )
        .await
        .unwrap();
    assert!(untouched.is_empty(), "owner 过滤挡住别人的机器");

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 端到端：夹具 + 拆除断言按步骤平铺
async fn delete_strict_tears_down_everything_and_keeps_user_agents() {
    let Some(fx) = setup().await else {
        return;
    };
    let rt = fx
        .runtimes
        .create(fx.runtime("host", "codex", None))
        .await
        .unwrap();
    let bob = fx.add_agent(rt.id, "bob", "user").await;
    let mika = fx.add_agent(rt.id, "mika", "system").await;
    sqlx::query(
        "INSERT INTO agent_invocation_target(agent_id, target_type, target_id) \
         VALUES ($1, 'workspace', $2)",
    )
    .bind(mika.as_uuid())
    .bind(fx.ws.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();
    let task = fx.add_task(bob, rt.id, "deferred").await;
    let ap: Uuid = sqlx::query_scalar(
        "INSERT INTO autopilot(workspace_id, title, assignee_id, created_by_type, created_by_id) \
         VALUES ($1, 'nightly', $2, 'member', $3) RETURNING id",
    )
    .bind(fx.ws.as_uuid())
    .bind(bob.as_uuid())
    .bind(fx.owner.as_uuid())
    .fetch_one(&fx.pool)
    .await
    .unwrap();

    let blocked = fx
        .runtimes
        .delete_strict(rt.id)
        .await
        .expect_err("活跃 agent 挡住严格删除");
    match blocked {
        DeleteRuntimeError::HasActiveAgents(agents) => {
            assert_eq!(ids(&agents), vec!["bob".to_owned()], "系统 agent 不算阻塞");
        }
        other => panic!("expected HasActiveAgents, got {other:?}"),
    }

    fx.archive_agent(bob).await;
    let outcome = fx.runtimes.delete_strict(rt.id).await.unwrap();
    assert_eq!(
        outcome,
        super::TeardownOutcome {
            agents_unbound: 1,
            tasks_cancelled: 1,
            autopilots_paused: 1,
        }
    );

    assert!(fx.runtimes.get(rt.id).await.unwrap().is_none());
    let agent_row: Option<(Option<Uuid>,)> =
        sqlx::query_as("SELECT runtime_id FROM agent WHERE id = $1")
            .bind(bob.as_uuid())
            .fetch_optional(&fx.pool)
            .await
            .unwrap();
    assert_eq!(agent_row, Some((None,)), "用户 agent 保留并解绑");
    let system_gone: Option<Uuid> = sqlx::query_scalar("SELECT id FROM agent WHERE id = $1")
        .bind(mika.as_uuid())
        .fetch_optional(&fx.pool)
        .await
        .unwrap();
    assert!(system_gone.is_none(), "系统 agent 被销毁");
    let targets: i64 =
        sqlx::query_scalar("SELECT count(*) FROM agent_invocation_target WHERE agent_id = $1")
            .bind(mika.as_uuid())
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(targets, 0, "非 FK 依赖必须在删 agent 前清掉");

    let (status, runtime_id): (String, Option<Uuid>) =
        sqlx::query_as("SELECT status, runtime_id FROM agent_task_queue WHERE id = $1")
            .bind(task.as_uuid())
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(status, "cancelled", "deferred 也必须取消");
    assert_eq!(runtime_id, None, "终态任务摘掉 runtime_id");
    let (ap_status, pause_reason): (String, Option<String>) =
        sqlx::query_as("SELECT status, pause_reason FROM autopilot WHERE id = $1")
            .bind(ap)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(ap_status, "paused");
    assert_eq!(pause_reason.as_deref(), Some("agent_runtime_required"));

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn unbind_rejects_a_drifted_plan_then_deletes() {
    let Some(fx) = setup().await else {
        return;
    };
    let rt = fx
        .runtimes
        .create(fx.runtime("host", "codex", None))
        .await
        .unwrap();
    let bob = fx.add_agent(rt.id, "bob", "user").await;

    let drifted = fx
        .runtimes
        .unbind_agents_and_delete(rt.id, &[])
        .await
        .expect_err("确认集合与现状不一致必须 409");
    match drifted {
        DeleteRuntimeError::PlanChanged(agents) => assert_eq!(ids(&agents), vec!["bob".to_owned()]),
        other => panic!("expected PlanChanged, got {other:?}"),
    }
    assert!(fx.runtimes.get(rt.id).await.unwrap().is_some());

    // 重复 id 要去重后再比对（upstream `parseExpectedActiveAgentIDs`）。
    let outcome = fx
        .runtimes
        .unbind_agents_and_delete(rt.id, &[bob, bob])
        .await
        .unwrap();
    assert_eq!(outcome.agents_unbound, 1);
    assert!(fx.runtimes.get(rt.id).await.unwrap().is_none());

    let missing = fx.runtimes.delete_strict(Id::new()).await;
    assert!(matches!(missing, Err(DeleteRuntimeError::NotFound)));

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn teardown_fails_closed_on_a_task_it_cannot_cancel() {
    let Some(fx) = setup().await else {
        return;
    };
    let rt = fx
        .runtimes
        .create(fx.runtime("host", "codex", None))
        .await
        .unwrap();
    let bob = fx.add_agent(rt.id, "bob", "user").await;
    // `completed` + `completed_at IS NULL`：状态不在取消清单里，但行确实没完成
    // —— 这时必须 abort（不能靠级联把行吞掉）。走裸 SQL，绕开 `add_task` 的推导。
    sqlx::query(
        "INSERT INTO agent_task_queue(agent_id, runtime_id, status) VALUES ($1, $2, 'completed')",
    )
    .bind(bob.as_uuid())
    .bind(rt.id.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();

    let err = fx
        .runtimes
        .unbind_agents_and_delete(rt.id, &[bob])
        .await
        .expect_err("未 drain 必须 fail-closed");
    assert!(matches!(err, DeleteRuntimeError::NotDrained), "got {err:?}");
    assert!(fx.runtimes.get(rt.id).await.unwrap().is_some(), "事务回滚");
    let still_bound: Option<Uuid> =
        sqlx::query_scalar("SELECT runtime_id FROM agent WHERE id = $1")
            .bind(bob.as_uuid())
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(still_bound, Some(rt.id.as_uuid()), "agent 未被解绑");

    fx.cleanup().await;
}

#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // 四条读数一次铺开，共享同一批 fixture
async fn usage_reads_group_by_day_agent_hour_and_tz() {
    let Some(fx) = setup().await else {
        return;
    };
    let rt = fx
        .runtimes
        .create(fx.runtime("host", "codex", None))
        .await
        .unwrap();
    let bob = fx.add_agent(rt.id, "bob", "user").await;
    let since = at("2020-01-01T00:00:00Z");

    // 20:00 UTC = 次日 04:00 (UTC+8)：同一行在 UTC 与上海落到不同日历日/小时。
    sqlx::query(
        "INSERT INTO task_usage_hourly(bucket_hour, workspace_id, runtime_id, agent_id, \
             provider, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, \
             cost_usd_ticks, uncosted_input_tokens) \
         VALUES ('2026-09-20T20:00:00Z', $1, $2, $3, 'OpenAI', 'gpt-5', 100, 20, 5, 1, 7, 30)",
    )
    .bind(fx.ws.as_uuid())
    .bind(rt.id.as_uuid())
    .bind(bob.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();

    let daily = fx
        .runtimes
        .list_runtime_usage(rt.id, since, TZ_SHANGHAI)
        .await
        .unwrap();
    assert_eq!(daily.len(), 1);
    assert_eq!(daily[0].date, NaiveDate::from_ymd_opt(2026, 9, 21).unwrap());
    assert_eq!(daily[0].provider, "openai", "provider 归一成小写");
    assert_eq!(daily[0].input_tokens, 100);
    assert_eq!(daily[0].cost_usd_ticks, 7);
    assert_eq!(daily[0].uncosted_input_tokens, 30);
    assert_eq!(
        daily[0].uncosted_output_tokens, 20,
        "uncosted_* 为空时回落到总量"
    );

    let utc = fx
        .runtimes
        .list_runtime_usage(rt.id, since, "UTC")
        .await
        .unwrap();
    assert_eq!(utc[0].date, NaiveDate::from_ymd_opt(2026, 9, 20).unwrap());

    let task = fx.add_task(bob, rt.id, "completed").await;
    sqlx::query("UPDATE agent_task_queue SET started_at = '2026-09-20T20:00:00Z' WHERE id = $1")
        .bind(task.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();
    // 已定价与未定价各一行：成本走真实值，token 走 FILTER 出来的未定价部分。
    for (provider, model, ticks) in [
        (Some("OpenAI"), "gpt-5", Some(7_i64)),
        (None, "gpt-5-mini", None),
    ] {
        sqlx::query(
            "INSERT INTO task_usage(task_id, provider, model, input_tokens, output_tokens, \
                 created_at, cost_usd_ticks) \
             VALUES ($1, COALESCE($2, ''), $3, 10, 5, '2026-09-20T20:30:00Z', $4)",
        )
        .bind(task.as_uuid())
        .bind(provider)
        .bind(model)
        .bind(ticks)
        .execute(&fx.pool)
        .await
        .unwrap();
    }

    let by_agent = fx
        .runtimes
        .list_runtime_usage_by_agent(rt.id, since)
        .await
        .unwrap();
    assert_eq!(by_agent.len(), 2);
    let priced = by_agent.iter().find(|r| r.model == "gpt-5").unwrap();
    assert_eq!(priced.agent_id, bob);
    assert_eq!(priced.cost_usd_ticks, 7);
    assert_eq!(priced.uncosted_input_tokens, 0, "有价格的行不进 uncosted");
    assert_eq!(priced.task_count, 1);
    let unpriced = by_agent.iter().find(|r| r.model == "gpt-5-mini").unwrap();
    assert_eq!(unpriced.cost_usd_ticks, 0);
    assert_eq!(unpriced.uncosted_input_tokens, 10, "未定价 token 单独报");

    let by_hour = fx
        .runtimes
        .get_runtime_usage_by_hour(rt.id, since, TZ_SHANGHAI)
        .await
        .unwrap();
    assert!(
        by_hour.iter().all(|r| r.hour == 4),
        "20:30Z 在上海是 04 点，got {by_hour:?}"
    );

    let activity = fx
        .runtimes
        .get_runtime_task_activity(rt.id, TZ_SHANGHAI)
        .await
        .unwrap();
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].hour, 4);
    assert_eq!(activity[0].count, 1);

    fx.cleanup().await;
}
