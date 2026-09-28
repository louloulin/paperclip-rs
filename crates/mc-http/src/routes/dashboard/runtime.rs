//! `/api/dashboard/{agent-runtime,runtime/daily}` 2 条（**写者 M9-4**，`docs/62` §4.1 第 5 行）。
//!
//! | method | path | 上游 | cutoff 口径 |
//! |---|---|---|---|
//! | GET | `/api/dashboard/agent-runtime` | `GetDashboardAgentRunTime`（`dashboard.go:384`） | **恰好 N 天**（`ExactDays`） |
//! | GET | `/api/dashboard/runtime/daily` | `GetDashboardRunTimeDaily`（`:474`） | N+1 天（`HeadroomDay`） |
//!
//! 两条只读 `agent_task_queue ⋈ agent ⋈ issue`（[`mc_repos::dashboard::DashboardRepo`]），
//! **不自建聚合**。
//!
//! ## 只有终态任务且两端时间戳齐全的行计入
//!
//! `status IN ('completed','failed','cancelled')` + `started_at IS NOT NULL` +
//! `completed_at IS NOT NULL` —— 排队中/运行中的任务没有有限时长。
//! `'cancelled'` 在过滤里（上游注释逐字：用户中途停掉的运行**已经烧掉的** agent 时间
//! 与 token 是真的；而 `started_at` 守卫把「还在排队时就被取消」的运行挡掉 ——
//! 它从没占住一个 agent）。少了 `'cancelled'`，Time/Tasks 与 Cost/Tokens 会在同一页上
//! **加的是两批不同的任务**。
//!
//! 按 `completed_at` 分桶（不是 `started_at`）—— 这样日界与 per-agent run-time 卡对齐，
//! 窗口也与 token 成本窗口对齐（后者锚在 `tu.created_at` ≈ 完成时刻）。
//!
//! ## `failed_count` / `cancelled_count` 是 `task_count` 的**不相交子集**
//!
//! 成功数由客户端用**余数**推（`mc_core::dashboard` 的行注释逐字点名「不要在这里再给一个
//! `succeeded_count` 字段，那会造出第二个真相源」）。

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
    CutoffConvention, DashboardAgentRunTimeResponse, DashboardRoute, DashboardRunTimeDailyResponse,
    RESTRICTED_AGENTS_ROW_ID,
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

/// runtime 两条切片（M9-4）：只读 `agent_task_queue` ⋈ `agent` ⋈ `issue`。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/dashboard/agent-runtime", get(agent_runtime))
        .route("/api/dashboard/runtime/daily", get(runtime_daily))
}

fn repo(state: &AppState) -> DashboardRepo {
    DashboardRepo::new(state.db.clone())
}

// ---------------------------------------------------------------------------
// GET /api/dashboard/agent-runtime
// ---------------------------------------------------------------------------

/// 上游 `GetDashboardAgentRunTime`：per-agent 的运行秒数 + 任务计数（**服务端**折叠私有 agent）。
///
/// cutoff 落在**看的人的**时区（这样「最近 N 天」与 per-agent 成本卡是同一个窗口），
/// 且用**恰好 N 天**（上游注释逐字：这份响应没有日期，客户端裁不掉 N+1 多给的那一天；
/// 它同时喂排行榜的 Time/Tasks 两列**和** Run time / Tasks 两块 KPI 磁贴，
/// 挂在 N+1 上会让这两块比旁边的 Cost / Tokens 多覆盖一天 —— MUL-5551）。
pub async fn agent_runtime(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<DashboardAgentRunTimeResponse>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let restricted = restricted_agent_ids(&state, &scope).await?;
    let tz = resolve_viewing_tz(&state, auth.id(), param(&query, "tz")).await;
    let since = since_cutoff(
        days_of(&query),
        CutoffConvention::for_route(DashboardRoute::AgentRunTime),
        &tz,
    );
    let project_id = parse_project_id(&query)?;
    let rows = repo(&state)
        .list_agent_runtime(scope.workspace_id, since, project_id)
        .await
        .map_err(|e| repo_err(e, "dashboard"))?;
    Ok(Json(fold_restricted_agent_runtime(rows, &restricted)))
}

// ---------------------------------------------------------------------------
// GET /api/dashboard/runtime/daily
// ---------------------------------------------------------------------------

/// 上游 `GetDashboardRunTimeDaily`：per-`日期` 的运行秒数 + 任务计数。
///
/// 日界在**看的人的**时区里切（`AT TIME ZONE`），这样 Time / Tasks 图表与
/// Cost / Tokens 图表的「一天」是同一个（否则东八区的看的人会看到四个 tab
/// 对「1d」窗口各说各话）。
pub async fn runtime_daily(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<DashboardRunTimeDailyResponse>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let tz = resolve_viewing_tz(&state, auth.id(), param(&query, "tz")).await;
    let since = since_cutoff(
        days_of(&query),
        CutoffConvention::for_route(DashboardRoute::RuntimeDaily),
        &tz,
    );
    let project_id = parse_project_id(&query)?;
    let rows = repo(&state)
        .list_runtime_daily(scope.workspace_id, since, &tz, project_id)
        .await
        .map_err(|e| repo_err(e, "dashboard"))?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// 折叠
// ---------------------------------------------------------------------------

/// 上游 `foldRestrictedAgentRunTime`。
///
/// 这一行除 agent 之外**没有别的维度** ⇒ 每个受限行都并进**同一个**桶
/// （上游注释逐字：「hence the empty merge key」）。
pub(super) fn fold_restricted_agent_runtime(
    rows: Vec<DashboardAgentRunTimeResponse>,
    restricted: &HashSet<Uuid>,
) -> Vec<DashboardAgentRunTimeResponse> {
    if restricted.is_empty() {
        return rows;
    }
    let mut out: Vec<DashboardAgentRunTimeResponse> = Vec::with_capacity(rows.len());
    let mut bucket_at: Option<usize> = None;
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
        row.agent_id = RESTRICTED_AGENTS_ROW_ID.to_string();
        if let Some(at) = bucket_at {
            let dst = &mut out[at];
            dst.total_seconds += row.total_seconds;
            dst.task_count += row.task_count;
            dst.metered_task_count += row.metered_task_count;
            dst.failed_count += row.failed_count;
            dst.cancelled_count += row.cancelled_count;
        } else {
            bucket_at = Some(out.len());
            out.push(row);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(agent: &str, seconds: i64, tasks: i32) -> DashboardAgentRunTimeResponse {
        DashboardAgentRunTimeResponse {
            agent_id: agent.into(),
            total_seconds: seconds,
            task_count: tasks,
            metered_task_count: tasks - 1,
            failed_count: 1,
            cancelled_count: 1,
        }
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 这一面除 agent 外没有维度 ⇒ 受限行全部并进**一个**桶，且每个和都加上。
    #[test]
    fn restricted_runtime_rows_merge_into_a_single_bucket() {
        let restricted: HashSet<Uuid> = [id(1), id(2), id(3)].into_iter().collect();
        let folded = fold_restricted_agent_runtime(
            vec![
                row(&id(1).to_string(), 10, 3),
                row(&id(2).to_string(), 20, 4),
                row(&id(9).to_string(), 7, 2),
                row(&id(3).to_string(), 30, 5),
            ],
            &restricted,
        );
        assert_eq!(folded.len(), 2, "两个受限行 + 一个可见行必须塌成 2 行");
        assert_eq!(folded[0].agent_id, RESTRICTED_AGENTS_ROW_ID);
        assert_eq!(folded[0].total_seconds, 60, "10 + 20 + 30，一个都不能少");
        assert_eq!(folded[0].task_count, 12);
        assert_eq!(folded[0].metered_task_count, 9);
        assert_eq!(folded[0].failed_count, 3);
        assert_eq!(folded[0].cancelled_count, 3);
        assert_eq!(folded[1].agent_id, id(9).to_string(), "可见行原样透传");
    }

    /// `failed_count + cancelled_count` 必须仍然是 `task_count` 的子集 ——
    /// 客户端用**余数**推成功数，两个真相源会直接打架。
    #[test]
    fn the_merged_bucket_keeps_counts_as_disjoint_subsets() {
        let restricted: HashSet<Uuid> = [id(1), id(2)].into_iter().collect();
        let folded = fold_restricted_agent_runtime(
            vec![
                row(&id(1).to_string(), 10, 3),
                row(&id(2).to_string(), 20, 4),
            ],
            &restricted,
        );
        let bucket = &folded[0];
        assert!(bucket.failed_count + bucket.cancelled_count <= bucket.task_count);
        assert!(bucket.metered_task_count <= bucket.task_count);
    }

    /// 空 restricted 集合 ⇒ 早退，原样返回（上游逐字）。
    #[test]
    fn an_empty_restricted_set_short_circuits() {
        let rows = vec![row(&id(1).to_string(), 10, 3)];
        assert_eq!(
            fold_restricted_agent_runtime(rows.clone(), &HashSet::new()),
            rows
        );
    }
}

/// 真库用例（门 ⑥）：两条 runtime 聚合的**逐字段**证据。
#[cfg(test)]
mod db_tests {
    use super::super::failures::test_support::{
        app, fixture, get_json, seed, seed_hourly, seed_metered, seed_task, Seed,
    };
    use super::*;
    use chrono::{DateTime, Duration, Utc};

    fn ago(hours: i64) -> DateTime<Utc> {
        Utc::now() - Duration::hours(hours)
    }

    /// `?project_id=` 用例的 `task_usage_hourly` 桶：23:00Z，落在默认 30 天窗口内。
    fn bucket() -> DateTime<Utc> {
        (Utc::now() - Duration::days(2))
            .date_naive()
            .and_hms_opt(23, 0, 0)
            .expect("valid naive time")
            .and_utc()
    }

    // -- GET /api/dashboard/agent-runtime -------------------------------

    /// 只有**终态且两端时间戳齐全**的任务计入；`metered_task_count` 判的是
    /// `task_usage` 里有没有行（provider 报 0 与什么都没报是**两回事**）。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    #[allow(clippy::too_many_lines)] // 造行表本身就是这个用例的断言清单，拆开反而更难读。
    async fn agent_runtime_counts_only_terminal_tasks_with_both_timestamps() {
        let db = fixture!();
        let seed: Seed = seed(&db).await;

        // 基准时刻：三条终态任务分别在它上面跑 60 / 30 / 10 秒。
        let base = ago(5);
        // 成功：跑了 60 秒，且**计过费**。
        let ok = seed_task(
            &db,
            &seed,
            seed.public_agent,
            Some(seed.project_a),
            "completed",
            Some(base),
            Some(base + Duration::seconds(60)),
            None,
        )
        .await;
        seed_metered(&db, ok).await;
        // 失败：跑了 30 秒，**没**计过费，`failure_reason` 给了。
        seed_task(
            &db,
            &seed,
            seed.public_agent,
            Some(seed.project_a),
            "failed",
            Some(base),
            Some(base + Duration::seconds(30)),
            Some("timeout"),
        )
        .await;
        // 取消：跑了 10 秒（用户中途停掉的运行**烧掉的**时间是真的）。
        seed_task(
            &db,
            &seed,
            seed.public_agent,
            Some(seed.project_b),
            "cancelled",
            Some(base),
            Some(base + Duration::seconds(10)),
            None,
        )
        .await;
        // 三行**不该**计入的：
        //  · 还在排队（没有有限时长）；
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
        //  · 运行中（`completed_at` 为空）；
        let _running = seed_task(
            &db,
            &seed,
            seed.public_agent,
            None,
            "running",
            Some(ago(1)),
            None,
            None,
        )
        .await;
        //  · 排队时就被取消（`started_at` 为空 ⇒ 它从没占住一个 agent）。
        seed_task(
            &db,
            &seed,
            seed.public_agent,
            None,
            "cancelled",
            None,
            Some(ago(1)),
            None,
        )
        .await;

        let app = app(db);
        let (status, body) = get_json(
            &app,
            "/api/dashboard/agent-runtime",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        assert_eq!(rows.len(), 1, "只有 public_agent 有行");
        let row = &rows[0];
        assert_eq!(
            row["agent_id"],
            serde_json::json!(seed.public_agent.to_string())
        );
        assert_eq!(
            row["task_count"], 3,
            "queued / running / 排队时取消 三行都不算"
        );
        assert_eq!(
            row["metered_task_count"], 1,
            "只有 completed 那次有 task_usage 行"
        );
        assert_eq!(row["failed_count"], 1);
        assert_eq!(row["cancelled_count"], 1);
        // 60 + 30 + 10 秒（容差：造行用的 `Utc::now()` 与查询有毫秒级漂移）。
        let seconds = row["total_seconds"]
            .as_i64()
            .expect("total_seconds is an int");
        assert!((95..=105).contains(&seconds), "total_seconds = {seconds}");

        // `?project_id=` 真的在过滤（project 走 `issue.project_id` 的 LEFT JOIN）。
        let path = format!("/api/dashboard/agent-runtime?project_id={}", seed.project_b);
        let (status, body) = get_json(&app, &path, seed.member, seed.workspace).await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["task_count"], 1,
            "只有那条 cancelled 在 project_b 上"
        );
    }

    // -- GET /api/dashboard/agent-runtime 的服务端折叠 -------------------

    /// `agent-runtime` 是 per-agent 的三条之一 ⇒ **必须**服务端折叠。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn agent_runtime_folds_restricted_agents_into_one_bucket() {
        let db = fixture!();
        let seed = seed(&db).await;
        let base = ago(5);
        for (agent, seconds, status) in [
            (seed.public_agent, 60i64, "completed"),
            (seed.private_agent, 1800, "completed"),
            (seed.system_agent, 600, "failed"),
        ] {
            seed_task(
                &db,
                &seed,
                agent,
                None,
                status,
                Some(base),
                Some(base + Duration::seconds(seconds)),
                None,
            )
            .await;
        }
        let app = app(db);
        let (status, body) = get_json(
            &app,
            "/api/dashboard/agent-runtime",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        assert_eq!(rows.len(), 2, "可见一行 + 哨兵桶一行");
        let bucket = rows
            .iter()
            .find(|r| r["agent_id"] == serde_json::json!(RESTRICTED_AGENTS_ROW_ID))
            .expect("private + system 必须折进同一个桶");
        assert_eq!(
            bucket["task_count"], 2,
            "这一面除 agent 外没有维度 ⇒ 一个桶"
        );
        assert_eq!(bucket["failed_count"], 1);
        let seconds = bucket["total_seconds"]
            .as_i64()
            .expect("total_seconds is an int");
        assert!(
            (2390..=2410).contains(&seconds),
            "1800 + 600 两段 = {seconds} 秒"
        );
        assert!(!body.to_string().contains(&seed.private_agent.to_string()));
        assert!(!body.to_string().contains(&seed.system_agent.to_string()));
    }

    // -- GET /api/dashboard/runtime/daily -------------------------------

    /// per-`(日期)` 的运行秒数 + 计数；**日界在调用方的时区**里切。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn runtime_daily_buckets_terminal_tasks_by_the_viewers_tz() {
        let db = fixture!();
        let seed: Seed = seed(&db).await;
        // 23:30Z ⇒ UTC 还在当天，Asia/Shanghai 已经是第二天。
        let at = (Utc::now() - Duration::days(2))
            .date_naive()
            .and_hms_opt(23, 30, 0)
            .expect("valid naive time")
            .and_utc();
        seed_task(
            &db,
            &seed,
            seed.public_agent,
            Some(seed.project_a),
            "completed",
            Some(at - Duration::seconds(90)),
            Some(at),
            None,
        )
        .await;
        seed_task(
            &db,
            &seed,
            seed.public_agent,
            Some(seed.project_a),
            "failed",
            Some(at - Duration::seconds(30)),
            Some(at),
            Some("boom"),
        )
        .await;
        let app = app(db);

        let (status, body) = get_json(
            &app,
            "/api/dashboard/runtime/daily?tz=UTC",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        assert_eq!(rows.len(), 1, "同一天的两次运行并成一个桶");
        assert_eq!(rows[0]["date"], at.format("%Y-%m-%d").to_string());
        assert_eq!(rows[0]["task_count"], 2);
        assert_eq!(rows[0]["failed_count"], 1);
        assert_eq!(rows[0]["cancelled_count"], 0);
        let seconds = rows[0]["total_seconds"]
            .as_i64()
            .expect("total_seconds is an int");
        assert!((115..=125).contains(&seconds), "90 + 30 = {seconds}");

        let (status, body) = get_json(
            &app,
            "/api/dashboard/runtime/daily?tz=Asia/Shanghai",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        let shanghai_date: chrono_tz::Tz = "Asia/Shanghai".parse().unwrap();
        assert_eq!(
            rows[0]["date"],
            serde_json::json!(at
                .with_timezone(&shanghai_date)
                .format("%Y-%m-%d")
                .to_string())
        );
    }

    // -- 跨面：`?project_id=` 过滤 / 访问门 / `?days=` 窗口 ------------------
    //
    // 这三条**不属于** runtime 面的哪一条路由，但断言覆盖 dashboard 全部 6 条
    // ⇒ 与其复制 6 份，不如在这个 db_tests 模块里各写一次。

    /// 非成员 ⇒ 404（不暴露 workspace 是否存在），与本仓既有约定一致。
    /// `?project_id=` 真的在过滤（`$n::uuid IS NULL OR …` 的两个分支都要走到）。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn usage_daily_scopes_to_one_project_and_rejects_a_malformed_id() {
        let db = fixture!();
        let seed = seed(&db).await;
        seed_hourly(
            &db,
            &seed,
            seed.public_agent,
            Some(seed.project_a),
            "claude",
            "claude",
            bucket(),
            10,
        )
        .await;
        seed_hourly(
            &db,
            &seed,
            seed.public_agent,
            Some(seed.project_b),
            "claude",
            "claude",
            bucket(),
            20,
        )
        .await;
        seed_hourly(
            &db,
            &seed,
            seed.public_agent,
            None,
            "claude",
            "claude",
            bucket(),
            30,
        )
        .await;
        let app = app(db);

        let path = format!("/api/dashboard/usage/daily?project_id={}", seed.project_a);
        let (status, body) = get_json(&app, &path, seed.member, seed.workspace).await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        assert_eq!(rows.len(), 1, "只该看到 project_a 那一行");
        assert_eq!(rows[0]["input_tokens"], 10);

        // 不给 `project_id` ⇒ 全 workspace（两个 project + 无 project 那一行）。
        let (status, body) = get_json(
            &app,
            "/api/dashboard/usage/daily",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body.as_array().expect("bare JSON array").len(), 1);
        assert_eq!(body[0]["input_tokens"], 60, "10 + 20 + 30");

        // 非法 uuid ⇒ **400**（上游 `parseProjectIDParam` 唯一的错误分支）。
        let (status, _) = get_json(
            &app,
            "/api/dashboard/usage/daily?project_id=not-a-uuid",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 400);
    }

    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn a_non_member_gets_404_on_every_dashboard_route() {
        let db = fixture!();
        let seed = seed(&db).await;
        let app = app(db);
        for path in [
            "/api/dashboard/usage/daily",
            "/api/dashboard/usage/by-agent",
            "/api/dashboard/agent-runtime",
            "/api/dashboard/runtime/daily",
            "/api/dashboard/failures/daily",
            "/api/dashboard/failures/by-agent",
        ] {
            let (status, _) = get_json(&app, path, seed.outsider, seed.workspace).await;
            assert_eq!(status, 404, "{path}");
        }
    }

    /// 窗口真的按 `?days=` 收：把一行放在 cutoff 之外，`days=1` 就看不到它。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn the_days_window_actually_excludes_rows_older_than_the_cutoff() {
        let db = fixture!();
        let seed = seed(&db).await;
        let recent = (Utc::now() - Duration::hours(6))
            .date_naive()
            .and_hms_opt(3, 0, 0)
            .expect("valid naive time")
            .and_utc();
        let old = (Utc::now() - Duration::days(10))
            .date_naive()
            .and_hms_opt(3, 0, 0)
            .expect("valid naive time")
            .and_utc();
        seed_hourly(
            &db,
            &seed,
            seed.public_agent,
            None,
            "claude",
            "recent",
            recent,
            10,
        )
        .await;
        seed_hourly(
            &db,
            &seed,
            seed.public_agent,
            None,
            "claude",
            "old",
            old,
            20,
        )
        .await;
        let app = app(db);

        let (_, body) = get_json(
            &app,
            "/api/dashboard/usage/daily?days=1&tz=UTC",
            seed.member,
            seed.workspace,
        )
        .await;
        let models: Vec<_> = body
            .as_array()
            .expect("bare JSON array")
            .iter()
            .map(|r| r["model"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(models.contains(&"recent".to_string()), "{models:?}");
        assert!(
            !models.contains(&"old".to_string()),
            "10 天前那行必须被 cutoff 挡住"
        );

        // `days` 非法 ⇒ **静默回落 30**（上游 `parseDaysCutoff` 没有 400 分支），
        // 于是 10 天前那行又回来了。
        let (status, body) = get_json(
            &app,
            "/api/dashboard/usage/daily?days=99999&tz=UTC",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200, "非法 days 不许 400 —— 上游是静默回落");
        let models: Vec<_> = body
            .as_array()
            .expect("bare JSON array")
            .iter()
            .map(|r| r["model"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(models.contains(&"old".to_string()), "{models:?}");
    }
}
