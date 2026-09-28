//! `/api/dashboard/failures/{daily,by-agent}` 2 条（**写者 M9-4**，`docs/62` §4.1 第 5 行）。
//!
//! | method | path | 上游 | cutoff 口径 |
//! |---|---|---|---|
//! | GET | `/api/dashboard/failures/daily` | `GetDashboardFailuresDaily`（`dashboard.go:541`） | N+1 天（`HeadroomDay`） |
//! | GET | `/api/dashboard/failures/by-agent` | `GetDashboardFailuresByAgent`（`:588`） | **恰好 N 天**（`ExactDays`） |
//!
//! 两条只读 `agent_task_queue ⋈ agent ⋈ issue`（[`mc_repos::dashboard::DashboardRepo`]）。
//!
//! ## 两条都返回**每一个**终态任务，而不只是失败的那些
//!
//! `failure_reason == ''` 那一行携带的是那一天的**成功**数 —— 客户端要靠它渲染
//! **错误率**，而把分子分母放在同一个 payload 里保证它们过的是**同一批**过滤。
//! 从 run-time 端点反推分母会**静默不一致**（上游注释逐字）：那两条要求
//! `started_at IS NOT NULL`，而一个在队列里过期掉的任务（`queued_expired`）
//! 从没 start 过。
//!
//! ## 两条 failures 查询**不**要求 `started_at`
//!
//! 同一个理由：漏掉「在队列里过期」就会低报 Errors 图**存在的意义**就是要呈现的那次故障。
//! 每条失败路径都写 `completed_at`，所以按 `completed_at` 分桶能覆盖全部。
//!
//! `status='failed'` 但 `failure_reason` 为 NULL / 空串的行落进 `'unclassified'` 桶
//! （迁移 `1949` 之前的行，或某条忘了分类的失败路径）⇒ 它**仍然可数**，
//! 而不是冒充一次成功。
//!
//! `failure_reason` 的取值是 `pkg/taskfailure` 的规范分类（21 个），
//! 客户端把它们折成几个展示类；**原始 reason 留在 wire 上**，这样那个映射改了不用动后端。

#![allow(clippy::implicit_hasher)] // `Query<HashMap<String, String>>` 的唯一生产者是
                                   // axum 的 `Query`（它按 `serde_urlencoded` 收查询串）；泛型化只会把 axum 的约束
                                   // 泄进本 slice 的签名里（与 `routes/ws.rs:81` 同款判断）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use mc_core::dashboard::{
    CutoffConvention, DashboardFailureByAgentResponse, DashboardFailureDailyResponse,
    DashboardRoute, RESTRICTED_AGENTS_ROW_ID,
};
use uuid::Uuid;

use super::usage::{
    days_of, param, parse_project_id, resolve_viewing_tz, restricted_agent_ids, since_cutoff,
};
use crate::error::ApiResult;
use crate::routes::agents::{repo_err, AgentScope};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;
use mc_repos::dashboard::DashboardRepo;

/// failures 两条切片（M9-4）：`failure_reason` 计数（空串 = 成功桶）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/dashboard/failures/daily", get(failures_daily))
        .route("/api/dashboard/failures/by-agent", get(failures_by_agent))
}

fn repo(state: &AppState) -> DashboardRepo {
    DashboardRepo::new(state.db.clone())
}

// ---------------------------------------------------------------------------
// GET /api/dashboard/failures/daily
// ---------------------------------------------------------------------------

/// 上游 `GetDashboardFailuresDaily`：per-`(日期, failure_reason)` 的终态任务计数。
///
/// 与其它每日序列**同一个**看包人时区的日界（这样 Errors tab 与
/// Cost / Tokens / Time / Tasks 对齐）。
pub async fn failures_daily(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<DashboardFailureDailyResponse>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let tz = resolve_viewing_tz(&state, auth.id(), param(&query, "tz")).await;
    let since = since_cutoff(
        days_of(&query),
        CutoffConvention::for_route(DashboardRoute::FailuresDaily),
        &tz,
    );
    let project_id = parse_project_id(&query)?;
    let rows = repo(&state)
        .list_failures_daily(scope.workspace_id, since, &tz, project_id)
        .await
        .map_err(|e| repo_err(e, "dashboard"))?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// GET /api/dashboard/failures/by-agent
// ---------------------------------------------------------------------------

/// 上游 `GetDashboardFailuresByAgent`：per-`(agent, failure_reason)` 的终态任务计数
/// （Usage 页「top offenders」那一半；**服务端**折叠私有 agent）。
///
/// cutoff 收紧到**恰好 `days` 个日历桶**（上游注释逐字：SQL 里没有日期分组，客户端
/// 没法像裁日期序列那样裁它；挂在默认的 N+1 cutoff 上，这个清单会多覆盖一天 ——
/// 于是 `days=1` 时卡片会报出**昨天**的失败，而紧挨着的图表一条都没画）。
pub async fn failures_by_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<DashboardFailureByAgentResponse>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let restricted = restricted_agent_ids(&state, &scope).await?;
    let tz = resolve_viewing_tz(&state, auth.id(), param(&query, "tz")).await;
    let since = since_cutoff(
        days_of(&query),
        CutoffConvention::for_route(DashboardRoute::FailuresByAgent),
        &tz,
    );
    let project_id = parse_project_id(&query)?;
    let rows = repo(&state)
        .list_failures_by_agent(scope.workspace_id, since, project_id)
        .await
        .map_err(|e| repo_err(e, "dashboard"))?;
    Ok(Json(fold_restricted_failures_by_agent(rows, &restricted)))
}

// ---------------------------------------------------------------------------
// 折叠
// ---------------------------------------------------------------------------

/// 上游 `foldRestrictedFailuresByAgent`。
///
/// 桶**保留** `failure_reason` 拆分（上游注释逐字：客户端像对任何别的 agent 一样，
/// 从这些原始行里推桶的失败率与分类占比；而 `failure_reason == ''` 的成功行是
/// 让 offender 清单仍能与上面那条 workspace 失败总额对账的那个**分母**）。
pub(super) fn fold_restricted_failures_by_agent(
    rows: Vec<DashboardFailureByAgentResponse>,
    restricted: &HashSet<Uuid>,
) -> Vec<DashboardFailureByAgentResponse> {
    if restricted.is_empty() {
        return rows;
    }
    let mut out: Vec<DashboardFailureByAgentResponse> = Vec::with_capacity(rows.len());
    let mut bucket_at: HashMap<String, usize> = HashMap::new();
    for mut row in rows {
        // 不是 uuid 的 `agent_id` 只可能是**已经折叠过的**哨兵 —— 原样透传。
        let Ok(id) = Uuid::parse_str(&row.agent_id) else {
            out.push(row);
            continue;
        };
        if !restricted.contains(&id) {
            out.push(row);
            continue;
        }
        let key = row.failure_reason.clone();
        row.agent_id = RESTRICTED_AGENTS_ROW_ID.to_string();
        if let Some(&at) = bucket_at.get(&key) {
            out[at].task_count += row.task_count;
        } else {
            bucket_at.insert(key, out.len());
            out.push(row);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(agent: &str, reason: &str, count: i32) -> DashboardFailureByAgentResponse {
        DashboardFailureByAgentResponse {
            agent_id: agent.into(),
            failure_reason: reason.into(),
            task_count: count,
        }
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 桶保留 `failure_reason` 拆分，且每个 reason 的计数都**加上**（成功桶那个分母
    /// 也在其中 —— 丢了它，受限桶的失败率就分不出母）。
    #[test]
    fn restricted_failure_rows_merge_per_reason_and_keep_totals() {
        let restricted: HashSet<Uuid> = [id(1), id(2)].into_iter().collect();
        let folded = fold_restricted_failures_by_agent(
            vec![
                row(&id(1).to_string(), "timeout", 3),
                row(&id(2).to_string(), "timeout", 4),
                row(&id(1).to_string(), "", 10),
                row(&id(2).to_string(), "", 5),
                row(&id(9).to_string(), "timeout", 1),
            ],
            &restricted,
        );
        assert_eq!(folded.len(), 3, "两个 reason 桶 + 一个可见行");
        assert_eq!(folded[0].failure_reason, "timeout");
        assert_eq!(folded[0].task_count, 7, "3 + 4");
        assert_eq!(folded[0].agent_id, RESTRICTED_AGENTS_ROW_ID);
        assert_eq!(folded[1].failure_reason, "", "成功桶仍在");
        assert_eq!(folded[1].task_count, 15, "10 + 5");
        assert_eq!(folded[2].agent_id, id(9).to_string());
    }

    /// 空串是**成功**桶而不是「未知原因」⇒ 折叠不得把它改名。
    #[test]
    fn the_empty_reason_bucket_is_the_succeeded_bucket() {
        let restricted: HashSet<Uuid> = [id(1)].into_iter().collect();
        let folded =
            fold_restricted_failures_by_agent(vec![row(&id(1).to_string(), "", 6)], &restricted);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].failure_reason, "");
    }

    /// 空 restricted 集合 ⇒ 早退原样返回。
    #[test]
    fn an_empty_restricted_set_short_circuits() {
        let rows = vec![row(&id(1).to_string(), "timeout", 1)];
        assert_eq!(
            fold_restricted_failures_by_agent(rows.clone(), &HashSet::new()),
            rows
        );
    }
}

// ---------------------------------------------------------------------------
// 真库用例的共用件（门 ⑥）
// ---------------------------------------------------------------------------

/// M9-4 真库用例的共用装置。
///
/// 为什么住在 `usage.rs` 而不是 `mod.rs`：`mod.rs` 由 M9-0 anchor 冻结。
/// 与三片子文件共享的 HTTP 件同一判例。
#[cfg(test)]
pub(crate) mod test_support {
    use axum::body::Body;
    use axum::http::Request;
    use chrono::{DateTime, Utc};
    use http_body_util::BodyExt;
    use mc_core::actor::ActorRegistry;
    use mc_db::Db;
    use mc_realtime::{RealtimeHandle, WsState};
    use serde_json::Value;
    use std::sync::Arc;
    use tower::ServiceExt;
    use uuid::Uuid;

    /// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`（打印跳过并 return）；
    /// **设了却连不上 ⇒ panic**（库坏了必须红，不许静默假装绿）。
    pub(crate) async fn pool() -> Option<Db> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        Some(
            Db::connect(&url, 4, 1).await.unwrap_or_else(|e| {
                panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}")
            }),
        )
    }

    /// `MULTICA_TEST_DATABASE_URL` 缺失 ⇒ 打印跳过并从用例 `return`。
    #[allow(unused_macros)]
    macro_rules! fixture {
        () => {
            match crate::routes::dashboard::failures::test_support::pool().await {
                Some(db) => db,
                None => {
                    eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                    return;
                }
            }
        };
    }
    #[allow(unused_imports)]
    pub(crate) use fixture;

    pub(crate) fn app(db: Db) -> axum::Router {
        use crate::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
        let realtime = RealtimeHandle::start(8);
        let ws = Arc::new(WsState::new(realtime.clone(), "dashboard-test"));
        let state = Arc::new(AppState::new(
            db,
            RuntimeHandles {
                actors: ActorRegistry::new(),
                adapters: Arc::new(AdapterRegistry::default()),
            },
            ConfigSnapshot::default(),
            realtime,
            ws,
        ));
        crate::routes::router(state.clone()).with_state(state)
    }

    /// 一个 workspace + owner + 普通 member + 外人 + 两个 project + 一个 runtime +
    /// 三个 agent（`public_to`+workspace 目标 / owner 的 private / `kind='system'`）。
    #[derive(Debug, Clone)]
    pub(crate) struct Seed {
        pub workspace: Uuid,
        pub owner: Uuid,
        pub member: Uuid,
        pub outsider: Uuid,
        pub project_a: Uuid,
        pub project_b: Uuid,
        pub runtime: Uuid,
        pub public_agent: Uuid,
        pub private_agent: Uuid,
        pub system_agent: Uuid,
    }

    async fn new_user(db: &Db, tag: &str) -> Uuid {
        let email = format!("itest-m94-{tag}-{}@example.com", Uuid::new_v4());
        sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#)
            .bind(format!("itest-m94-{tag}"))
            .bind(email)
            .fetch_one(db.pool())
            .await
            .expect("insert user")
    }

    async fn join(db: &Db, workspace: Uuid, user: Uuid, role: &str) {
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(workspace)
            .bind(user)
            .bind(role)
            .execute(db.pool())
            .await
            .expect("insert member");
    }

    #[allow(clippy::too_many_arguments)]
    async fn new_agent(
        db: &Db,
        workspace: Uuid,
        owner: Uuid,
        name: &str,
        kind: &str,
        mode: &str,
    ) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO agent (workspace_id, name, runtime_mode, owner_id, kind, permission_mode) \
             VALUES ($1, $2, 'local', $3, $4, $5) RETURNING id",
        )
        .bind(workspace)
        .bind(name)
        .bind(owner)
        .bind(kind)
        .bind(mode)
        .fetch_one(db.pool())
        .await
        .expect("insert agent")
    }

    async fn new_project(db: &Db, workspace: Uuid, tag: &str) -> Uuid {
        sqlx::query_scalar("INSERT INTO project(workspace_id, title) VALUES ($1, $2) RETURNING id")
            .bind(workspace)
            .bind(format!("itest-m94-{tag}"))
            .fetch_one(db.pool())
            .await
            .expect("insert project")
    }

    pub(crate) async fn seed(db: &Db) -> Seed {
        let tag = Uuid::new_v4().simple().to_string();
        let workspace: Uuid =
            sqlx::query_scalar("INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id")
                .bind(format!("itest-m94-ws-{tag}"))
                .bind(format!("itest-m94-ws-{tag}"))
                .fetch_one(db.pool())
                .await
                .expect("insert workspace");

        let owner = new_user(db, "owner").await;
        let member = new_user(db, "member").await;
        let outsider = new_user(db, "outsider").await;
        join(db, workspace, owner, "owner").await;
        join(db, workspace, member, "member").await;

        let project_a = new_project(db, workspace, "pa").await;
        let project_b = new_project(db, workspace, "pb").await;

        let runtime: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_runtime (workspace_id, name, runtime_mode, provider, status) \
             VALUES ($1, $2, 'local', 'claude', 'online') RETURNING id",
        )
        .bind(workspace)
        .bind(format!("itest-m94-rt-{tag}"))
        .fetch_one(db.pool())
        .await
        .expect("insert agent_runtime");

        let public_agent = new_agent(db, workspace, owner, "public", "user", "public_to").await;
        sqlx::query(
            "INSERT INTO agent_invocation_target (agent_id, target_type, target_id) \
             VALUES ($1, 'workspace', $2)",
        )
        .bind(public_agent)
        .bind(workspace)
        .execute(db.pool())
        .await
        .expect("insert agent_invocation_target");

        let private_agent = new_agent(db, workspace, owner, "private", "user", "private").await;
        let system_agent = new_agent(db, workspace, owner, "carrier", "system", "private").await;

        Seed {
            workspace,
            owner,
            member,
            outsider,
            project_a,
            project_b,
            runtime,
            public_agent,
            private_agent,
            system_agent,
        }
    }

    /// 一行 `task_usage_hourly`（4 个 token 各给一个可区分的值 + 4 个 uncosted）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn seed_hourly(
        db: &Db,
        seed: &Seed,
        agent: Uuid,
        project: Option<Uuid>,
        provider: &str,
        model: &str,
        bucket_hour: DateTime<Utc>,
        base: i64,
    ) {
        sqlx::query(
            "INSERT INTO task_usage_hourly (bucket_hour, workspace_id, runtime_id, agent_id, project_id, \
                provider, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, \
                cost_usd_ticks, uncosted_input_tokens, uncosted_output_tokens, \
                uncosted_cache_read_tokens, uncosted_cache_write_tokens, task_count, event_count) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,1,1)",
        )
        .bind(bucket_hour)
        .bind(seed.workspace)
        .bind(seed.runtime)
        .bind(agent)
        .bind(project)
        .bind(provider)
        .bind(model)
        .bind(base)
        .bind(base + 1)
        .bind(base + 2)
        .bind(base + 3)
        .bind(base + 10)
        .bind(base + 11)
        .bind(base + 12)
        .bind(base + 13)
        .bind(base + 14)
        .execute(db.pool())
        .await
        .expect("insert task_usage_hourly");
    }

    /// 一行 `agent_task_queue`（`issue_id` 可空 —— 迁移 033 之后它就是可空的）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn seed_task(
        db: &Db,
        seed: &Seed,
        agent: Uuid,
        project: Option<Uuid>,
        status: &str,
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
        failure_reason: Option<&str>,
    ) -> Uuid {
        let issue: Option<Uuid> = match project {
            Some(project) => Some(
                sqlx::query_scalar(
                    "INSERT INTO issue (workspace_id, number, title, status, creator_type, creator_id, project_id) \
                     VALUES ($1, $2, 'itest', 'todo', 'member', $3, $4) RETURNING id",
                )
                .bind(seed.workspace)
                .bind(next_issue_number(db, seed.workspace).await)
                .bind(seed.owner)
                .bind(project)
                .fetch_one(db.pool())
                .await
                .expect("insert issue"),
            ),
            None => None,
        };
        sqlx::query_scalar(
            "INSERT INTO agent_task_queue \
                (agent_id, issue_id, runtime_id, status, started_at, completed_at, failure_reason) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id",
        )
        .bind(agent)
        .bind(issue)
        .bind(seed.runtime)
        .bind(status)
        .bind(started_at)
        .bind(completed_at)
        .bind(failure_reason)
        .fetch_one(db.pool())
        .await
        .expect("insert agent_task_queue")
    }

    /// 一行 `task_usage`（只为 `metered_task_count` 的 `EXISTS` 判定存在）。
    pub(crate) async fn seed_metered(db: &Db, task: Uuid) {
        sqlx::query(
            "INSERT INTO task_usage (task_id, provider, model, input_tokens, output_tokens) \
             VALUES ($1, 'claude', 'claude', 0, 0)",
        )
        .bind(task)
        .execute(db.pool())
        .await
        .expect("insert task_usage");
    }

    async fn next_issue_number(db: &Db, workspace: Uuid) -> i32 {
        let current: Option<i32> =
            sqlx::query_scalar("SELECT MAX(number) FROM issue WHERE workspace_id = $1")
                .bind(workspace)
                .fetch_one(db.pool())
                .await
                .expect("select max issue number");
        current.unwrap_or(0) + 1
    }

    /// 发一个带 `X-Multica-User-Id` + `X-Workspace-ID` 的 GET，返回 `(状态码, JSON)`。
    pub(crate) async fn get_json(
        app: &axum::Router,
        path: &str,
        user: Uuid,
        workspace: Uuid,
    ) -> (axum::http::StatusCode, Value) {
        let request = Request::builder()
            .uri(path)
            .header("x-multica-user-id", user.to_string())
            .header("x-workspace-id", workspace.to_string())
            .body(Body::empty())
            .expect("build request");
        let response = app
            .clone()
            .oneshot(request)
            .await
            .expect("dashboard route must be mounted");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }
}

/// 真库用例（门 ⑥）：两条 failures 聚合的**逐字段**证据。
#[cfg(test)]
mod db_tests {
    use super::test_support::{app, fixture, get_json, seed, seed_task, Seed};
    use super::*;
    use chrono::{Duration, Utc};
    use serde_json::Value;

    // -- GET /api/dashboard/failures/daily ------------------------------

    /// 两条 failures 查询**不**要求 `started_at`：在队列里过期掉的任务从没 start 过，
    /// 但它无可争议是一次失败 —— 漏掉它就会低报 Errors 图要呈现的那次故障。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    #[allow(clippy::too_many_lines)] // 造行表本身就是这个用例的断言清单，拆开反而更难读。
    async fn failures_daily_counts_never_started_tasks_and_keeps_the_succeeded_bucket() {
        let db = fixture!();
        let seed: Seed = seed(&db).await;
        let at = (Utc::now() - Duration::days(1))
            .date_naive()
            .and_hms_opt(9, 0, 0)
            .expect("valid naive time")
            .and_utc();

        // 成功 ×2（空串 = 成功桶，客户端的错误率分母）。
        seed_task(
            &db,
            &seed,
            seed.public_agent,
            None,
            "completed",
            Some(at - Duration::hours(1)),
            Some(at),
            None,
        )
        .await;
        seed_task(
            &db,
            &seed,
            seed.public_agent,
            None,
            "completed",
            Some(at - Duration::hours(2)),
            Some(at),
            None,
        )
        .await;
        // 失败，原因已分类。
        seed_task(
            &db,
            &seed,
            seed.public_agent,
            None,
            "failed",
            Some(at - Duration::hours(1)),
            Some(at),
            Some("timeout"),
        )
        .await;
        // 失败，但 `failure_reason` 为 NULL ⇒ 落 `'unclassified'`，**仍然可数**
        // （而不是冒充一次成功 —— 那会让错误率凭空变好）。
        seed_task(
            &db,
            &seed,
            seed.public_agent,
            None,
            "failed",
            None,
            Some(at),
            None,
        )
        .await;
        // 在队列里过期掉：**从没 start 过**，但确实失败了。
        seed_task(
            &db,
            &seed,
            seed.public_agent,
            None,
            "failed",
            None,
            Some(at),
            Some("queued_expired"),
        )
        .await;
        // 还在排队 ⇒ 不是终态，不计。
        let _queued = seed_task(
            &db,
            &seed,
            seed.public_agent,
            None,
            "queued",
            None,
            None,
            None,
        )
        .await;

        let app = app(db);
        let (status, body) = get_json(
            &app,
            "/api/dashboard/failures/daily?tz=UTC",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        let date = at.format("%Y-%m-%d").to_string();
        let find = |reason: &str| -> Value {
            rows.iter()
                .find(|r| {
                    r["date"] == serde_json::json!(date)
                        && r["failure_reason"] == serde_json::json!(reason)
                })
                .cloned()
                .unwrap_or_else(|| panic!("no row for {reason:?} in {rows:?}"))
        };
        assert_eq!(find("")["task_count"], 2, "空串是成功桶");
        assert_eq!(find("timeout")["task_count"], 1);
        assert_eq!(
            find("unclassified")["task_count"],
            1,
            "failed + NULL reason ⇒ unclassified，不是成功"
        );
        assert_eq!(
            find("queued_expired")["task_count"],
            1,
            "从没 start 过的失败也必须计入（这条查询不要求 started_at）"
        );
    }

    // -- GET /api/dashboard/failures/by-agent ---------------------------

    /// per-agent 的「top offenders」：服务端折叠 + **保留 `failure_reason` 拆分**
    /// （成功桶那个分母也在其中）。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn failures_by_agent_folds_restricted_agents_per_failure_reason() {
        let db = fixture!();
        let seed: Seed = seed(&db).await;
        let at = (Utc::now() - Duration::hours(3))
            .date_naive()
            .and_hms_opt(9, 0, 0)
            .expect("valid naive time")
            .and_utc();

        for (agent, status, reason) in [
            (seed.public_agent, "failed", Some("timeout")),
            (seed.public_agent, "completed", None),
            (seed.private_agent, "failed", Some("timeout")),
            (seed.private_agent, "failed", Some("overflow")),
            (seed.private_agent, "completed", None),
            (seed.system_agent, "failed", Some("timeout")),
        ] {
            seed_task(
                &db,
                &seed,
                agent,
                None,
                status,
                Some(at - Duration::hours(1)),
                Some(at),
                reason,
            )
            .await;
        }

        let app = app(db);
        let (status, body) = get_json(
            &app,
            "/api/dashboard/failures/by-agent",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        let bucket = |reason: &str| -> Value {
            rows.iter()
                .find(|r| {
                    r["agent_id"] == serde_json::json!(RESTRICTED_AGENTS_ROW_ID)
                        && r["failure_reason"] == serde_json::json!(reason)
                })
                .cloned()
                .unwrap_or_else(|| panic!("no bucket row for {reason:?} in {rows:?}"))
        };
        // 可见 agent 的行原样透传。
        assert!(rows.iter().any(|r| {
            r["agent_id"] == serde_json::json!(seed.public_agent.to_string())
                && r["failure_reason"] == serde_json::json!("timeout")
        }));
        // private(1 timeout) + private(1 overflow) + system(1 timeout) ⇒ timeout 桶 = 2。
        assert_eq!(bucket("timeout")["task_count"], 2, "合并后总额不丢");
        assert_eq!(bucket("overflow")["task_count"], 1);
        assert_eq!(
            bucket("")["task_count"],
            1,
            "成功桶是失败率的分母，折叠不得改名或丢行"
        );
        let raw = body.to_string();
        assert!(!raw.contains(&seed.private_agent.to_string()));
        assert!(!raw.contains(&seed.system_agent.to_string()));
    }
}
