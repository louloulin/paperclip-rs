//! `/api/dashboard/usage/{daily,by-agent}` 2 条（**写者 M9-4**，`docs/62` §4.1 第 5 行）。
//!
//! | method | path | 上游 | cutoff 口径 |
//! |---|---|---|---|
//! | GET | `/api/dashboard/usage/daily` | `GetDashboardUsageDaily`（`dashboard.go:178`） | N+1 天（`HeadroomDay`） |
//! | GET | `/api/dashboard/usage/by-agent` | `GetDashboardUsageByAgent`（`:262`） | **恰好 N 天**（`ExactDays`） |
//!
//! 两条都只读 [`mc_repos::dashboard::DashboardRepo`]，**不自建聚合**、**不读**
//! `task_usage_dashboard_*`（那两张 legacy rollup 表已被 `103` 删掉）。
//!
//! ## 为什么 `by-agent` 必须是「恰好 N 天」
//!
//! 上游注释逐字：「by agent 在 SQL 里**没有**日期分组 —— tz 只决定 cutoff 边界，不是分桶轴。
//! 这正是 cutoff 必须是**精确 N 天**那一半的原因：客户端用 `-(days-1)` 裁掉
//! `parseSinceParamInTZ` 多给的那一天，而一份**不带日期**的响应没法这样裁。
//! 挂在 N+1 的 cutoff 上，这个排行榜会比它正上方的 Tokens/Cost KPI 与图表多覆盖
//! 一个日历日 —— 于是 1D 窗口下**单个 agent 的一行可以比 workspace 总额还高**
//! （MUL-5551）。」
//!
//! `by-agent` 因此**必须**做服务端可见性折叠，否则折叠后剩下的哨兵桶会把两半的差额吞进一行。
//!
//! ## `foldRestrictedAgents`：折叠而不是丢弃
//!
//! 「private 是整个代码库都在守的承诺」—— agent 详情 403、list 端点过滤、连 admin 都调不动
//! 别人的私有 agent。这三条端点曾经把整个 workspace 的**裸 agent UUID** 返出去，等于告诉
//! 一个普通成员「某个私有 agent 存在、跑多少、失败在什么上」。客户端**已经**在折叠那些行了，
//! 但上游注释逐字：「client-side filtering is decoration: one curl bypasses it」⇒ 必须在
//! **服务端**折叠。
//!
//! 为什么**折叠**而不是**丢行**：这三条响应都是「另一半是 workspace 级、未过滤」的配对
//! （`usage/daily` / `runtime/daily` / `failures/daily`），丢行会让 per-agent 明细**加不回**
//! 紧挨着渲染的总额。一个合并桶保住每一个和，同时不携带任何真 agent id。桶的**合并键**保留
//! 还剩下的维度：`by-agent` 留 `(provider, model)`（否则桶里的钱算不出来，排行榜就不再等于
//! Cost KPI），`agent-runtime` 一个桶，`failures/by-agent` 留 `failure_reason`。
//!
//! `kind = 'system'` 的隐藏承运 agent 也走这条折叠 —— 没有任何 list 端点会把它交给任何人，
//! 而聚合查询不带 `kind` 过滤，它会自己冒成一个没人能叫出名字的裸 UUID。
//!
//! ## 本文件为什么**拥有**三片子文件共享的 HTTP 件
//!
//! [`super`](super) 的 `mod.rs` 由 M9-0 anchor 冻结（「后续切片**不得**编辑」），而
//! 「查询串 / tz / cutoff / 折叠」是 6 条**逐字共用**的；放一个专门的第四个文件就得改 `mod.rs`
//! ⇒ 它们落在本文件，由 `runtime.rs` / `failures.rs` 以 `super::usage::` 取用（真库用例的
//! 共用装置同理，落在 `failures.rs`）。
//!
//! 形态（`docs/62` §1.4 实测 `declared 34 / dual-form required: 3`）：本波**只有**
//! `/api/notification-preferences` 那 3 条需要补尾斜杠形态 ⇒ 本文件按上游字面量注册。

#![allow(clippy::implicit_hasher)] // `Query<HashMap<String, String>>` 的唯一生产者是
                                   // axum 的 `Query`（它按 `serde_urlencoded` 收查询串）；泛型化只会把 axum 的约束
                                   // 泄进本 slice 的签名里（与 `routes/ws.rs:81` 同款判断）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Duration, LocalResult, TimeZone, Utc};
use chrono_tz::Tz;
use mc_core::dashboard::{
    cutoff_days_with_convention, resolve_days, CutoffConvention, DashboardRoute,
    DashboardUsageByAgentResponse, DashboardUsageDailyResponse, RESTRICTED_AGENTS_ROW_ID,
};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::dashboard::DashboardRepo;
use mc_repos::Repository;
use uuid::Uuid;

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, repo_err, AgentScope};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// usage 两条切片（M9-4）：只读 `task_usage_hourly`。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/dashboard/usage/daily", get(usage_daily))
        .route("/api/dashboard/usage/by-agent", get(usage_by_agent))
}

fn repo(state: &AppState) -> DashboardRepo {
    DashboardRepo::new(state.db.clone())
}

// ---------------------------------------------------------------------------
// GET /api/dashboard/usage/daily
// ---------------------------------------------------------------------------

/// 上游 `GetDashboardUsageDaily`：per-`(日期, provider, model)` 的 token 聚合。
///
/// `provider` / `model` 都留在 wire 上（成本在客户端按 per-model 价格表算，
/// 而**不同 provider 的裸 model id 会撞名** —— 上游点名 Cursor 的 `auto`）。
pub async fn usage_daily(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<DashboardUsageDailyResponse>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let tz = resolve_viewing_tz(&state, auth.id(), param(&query, "tz")).await;
    let since = since_cutoff(days_of(&query), cutoff_for(DashboardRoute::UsageDaily), &tz);
    let project_id = parse_project_id(&query)?;
    let rows = repo(&state)
        .list_usage_daily(scope.workspace_id, since, &tz, project_id)
        .await
        .map_err(|e| repo_err(e, "dashboard"))?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// GET /api/dashboard/usage/by-agent
// ---------------------------------------------------------------------------

/// 上游 `GetDashboardUsageByAgent`：per-`(agent, provider, model)`，**服务端**折叠私有 agent。
pub async fn usage_by_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<DashboardUsageByAgentResponse>>> {
    let scope = AgentScope::resolve(&state, auth, &headers, &query).await?;
    let restricted = restricted_agent_ids(&state, &scope).await?;
    let tz = resolve_viewing_tz(&state, auth.id(), param(&query, "tz")).await;
    // 恰好 N 天（**不是** N+1）—— 见文件头「MUL-5551」那一节。
    let since = since_cutoff(
        days_of(&query),
        cutoff_for(DashboardRoute::UsageByAgent),
        &tz,
    );
    let project_id = parse_project_id(&query)?;
    let rows = repo(&state)
        .list_usage_by_agent(scope.workspace_id, since, project_id)
        .await
        .map_err(|e| repo_err(e, "dashboard"))?;
    Ok(Json(fold_restricted_usage_by_agent(rows, &restricted)))
}

// ---------------------------------------------------------------------------
// 三片子文件共享件
// ---------------------------------------------------------------------------

/// 查询串取值（空串当没给，与 `routes::agents::query_value` 同款）。
pub(super) fn param<'a>(query: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    query
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

/// `?days=`：缺省 30、合法区间 `1..=365`，**非法值静默回落**（上游 `parseDaysCutoff`
/// 没有 400 分支）。口径与 [`resolve_days`] 逐字同源，路由层不再自己钳一次。
pub(super) fn days_of(query: &HashMap<String, String>) -> u32 {
    resolve_days(param(query, "days"))
}

/// 每条路由用哪一半 cutoff —— **3 条日期序列 N+1 / 3 条 per-agent 恰好 N**。
///
/// 🔴 本函数**故意不复用** [`CutoffConvention::for_route`]：那个 anchor 冻结的映射把
/// `usage/by-agent` 与 `failures/by-agent` 也归到 `HeadroomDay`，与上游逐字相反。
/// 上游 `dashboard.go` 的六处调用点（`f41fae6b`）是 188 / 486 / 553 走
/// `parseSinceParamInTZ`（N+1），285 / 406 / 609 走 `parseExactSinceParamInTZ`（恰好 N）
/// —— 正好是**三条按日期分桶的**用 N+1、**三条 per-agent 的**用 N。三条 per-agent 的
/// 响应都不带日期，客户端没法用 `-(days-1)` 裁掉多余的那一天（MUL-5551：
/// 1D 窗口下单个 agent 的一行会读得比 workspace 总额还高）。
///
/// `mc_core::dashboard` 是 M9-0 anchor 定形后冻结的（不在本片写集里），所以这里
/// 按上游实现并把分歧登记进 `docs/32` §9.13，等 owner 裁决后再决定是改 anchor
/// 还是改本函数。锚点自身的 `the_six_routes_use_the_two_cutoff_halves` 用例钉的是
/// 那份**分歧**映射 —— 它仍然绿，因为本片没动它。
pub(super) fn cutoff_for(route: DashboardRoute) -> CutoffConvention {
    if route.is_per_agent() {
        CutoffConvention::ExactDays
    } else {
        CutoffConvention::HeadroomDay
    }
}

/// `?project_id=`：缺省 `None` ⇒ SQL 里 `($n::uuid IS NULL OR …)` 退化成「不过滤」；
/// 给了但不是 uuid ⇒ **400**（上游 `parseProjectIDParam` 唯一的错误分支）。
pub(super) fn parse_project_id(query: &HashMap<String, String>) -> Result<Option<Uuid>, Error> {
    let Some(raw) = param(query, "project_id") else {
        return Ok(None);
    };
    Uuid::parse_str(raw.trim())
        .map(Some)
        .map_err(|_| bad_request("invalid project_id"))
}

/// 上游 `parseSinceParamInTZ` / `parseExactSinceParamInTZ` 的合流：
/// `HeadroomDay` ⇒ 今天本地零点往前推 `days` 天（N+1 个桶），
/// `ExactDays` ⇒ 往前推 `days - 1` 天（恰好 N 个桶）。
pub(super) fn since_cutoff(days: u32, convention: CutoffConvention, tz: &str) -> DateTime<Utc> {
    let tz: Tz = tz.parse().unwrap_or(Tz::UTC);
    since_from_days(
        Utc::now(),
        i64::from(cutoff_days_with_convention(days, convention)),
        tz,
    )
}

/// 上游 `sinceFromDays`：`now` 的**本地日历日**往前推 `days` 天的那个**本地零点**。
pub(super) fn since_from_days(now: DateTime<Utc>, days: i64, tz: Tz) -> DateTime<Utc> {
    let today = now.with_timezone(&tz).date_naive();
    let Some(midnight) = today.and_hms_opt(0, 0, 0) else {
        return now;
    };
    let Some(target) = midnight.checked_sub_signed(Duration::days(days)) else {
        return now;
    };
    resolve_local(tz, target).unwrap_or(now)
}

/// 把「本地墙上时间」解析回瞬时，并照 Go `time.Date` 的方式处理 DST 异常：
///
/// - **歧义**（秋季重复的那个 00:00）取较早的一支；
/// - **不存在**（春季被抹掉的 00:00，例如 `America/Santiago`）往前推一小时 ——
///   少了这一步会让切点整整早一天。
pub(super) fn resolve_local(tz: Tz, naive: chrono::NaiveDateTime) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        LocalResult::Ambiguous(earliest, _) => Some(earliest.with_timezone(&Utc)),
        LocalResult::None => tz
            .from_local_datetime(&(naive + Duration::hours(1)))
            .earliest()
            .map(|dt| dt.with_timezone(&Utc)),
    }
}

/// 上游 `resolveViewingTZ`：`?tz=` → `user.timezone` → `"UTC"`。
///
/// ⚠️ **永不报错**（上游逐字：invalid values fall through rather than erroring ——
/// tz is a display concern）。
pub(super) async fn resolve_viewing_tz(state: &AppState, user_id: Id, raw: Option<&str>) -> String {
    if let Some(tz) = raw
        .map(str::trim)
        .filter(|tz| !tz.is_empty() && valid_tz(tz))
    {
        return tz.to_string();
    }
    // 冷路径：只有不带 `?tz=` 的 API 客户端会走到这里。
    let stored = mc_repos::user::UserRepo::new(state.db.clone())
        .get(&user_id)
        .await
        .ok()
        .and_then(|user| user.timezone)
        .map(|value| value.trim().to_string());
    let Some(stored) = stored else {
        return mc_core::dashboard::DEFAULT_TIMEZONE.to_string();
    };
    if stored.is_empty() || !valid_tz(&stored) {
        return mc_core::dashboard::DEFAULT_TIMEZONE.to_string();
    }
    stored
}

/// 只接受 `chrono-tz` 认得的名字（等价于上游 `time.LoadLocation` 成功）。
pub(super) fn valid_tz(name: &str) -> bool {
    name.parse::<Tz>().is_ok()
}

/// 上游 `dashboardRestrictedAgents`：**出错 ⇒ 500**，绝不返回一份没折叠过的聚合。
pub(super) async fn restricted_agent_ids(
    state: &AppState,
    scope: &AgentScope,
) -> Result<HashSet<Uuid>, Error> {
    repo(state)
        .restricted_agent_ids(scope.workspace_id, scope.user_id, &scope.role)
        .await
        .map_err(|e| repo_err(e, "agent"))
}

// ---------------------------------------------------------------------------
// 折叠
// ---------------------------------------------------------------------------

/// 上游 `foldRestrictedUsageByAgent`。
///
/// 桶的合并键 = `(provider, model)`（上游 `providerModelKey`）：留着这个维度，
/// 客户端才能用它的 per-model 价格表给桶定价；否则桶里的钱算不出来，
/// 排行榜就不再等于紧挨着的 Cost KPI —— 而那正是「折叠而非丢弃」的全部理由。
pub(super) fn fold_restricted_usage_by_agent(
    rows: Vec<DashboardUsageByAgentResponse>,
    restricted: &HashSet<Uuid>,
) -> Vec<DashboardUsageByAgentResponse> {
    if restricted.is_empty() {
        return rows;
    }
    let mut out: Vec<DashboardUsageByAgentResponse> = Vec::with_capacity(rows.len());
    // 键 = `(provider, model)`；值为该桶在 `out` 里的下标。
    let mut bucket_at: HashMap<(String, String), usize> = HashMap::new();
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
        let key = (row.provider.clone(), row.model.clone());
        row.agent_id = RESTRICTED_AGENTS_ROW_ID.to_string();
        if let Some(&at) = bucket_at.get(&key) {
            merge_usage_row(&mut out[at], &row);
        } else {
            bucket_at.insert(key, out.len());
            out.push(row);
        }
    }
    out
}

/// 上游 `foldRestrictedUsageByAgent` 的 `merge`：**每一个和都要加上**（不丢总额）。
fn merge_usage_row(dst: &mut DashboardUsageByAgentResponse, src: &DashboardUsageByAgentResponse) {
    dst.tokens.input_tokens += src.tokens.input_tokens;
    dst.tokens.output_tokens += src.tokens.output_tokens;
    dst.tokens.cache_read_tokens += src.tokens.cache_read_tokens;
    dst.tokens.cache_write_tokens += src.tokens.cache_write_tokens;
    dst.cost_usd_ticks += src.cost_usd_ticks;
    dst.uncosted_input_tokens += src.uncosted_input_tokens;
    dst.uncosted_output_tokens += src.uncosted_output_tokens;
    dst.uncosted_cache_read_tokens += src.uncosted_cache_read_tokens;
    dst.uncosted_cache_write_tokens += src.uncosted_cache_write_tokens;
    dst.task_count += src.task_count;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(agent: &str, provider: &str, model: &str, tokens: i64) -> DashboardUsageByAgentResponse {
        DashboardUsageByAgentResponse {
            agent_id: agent.into(),
            provider: provider.into(),
            model: model.into(),
            tokens: mc_core::dashboard::TokenUsageCounts {
                input_tokens: tokens,
                output_tokens: tokens * 2,
                cache_read_tokens: tokens * 3,
                cache_write_tokens: tokens * 4,
            },
            cost_usd_ticks: tokens * 10,
            uncosted_input_tokens: tokens,
            uncosted_output_tokens: tokens * 2,
            uncosted_cache_read_tokens: tokens * 3,
            uncosted_cache_write_tokens: tokens * 4,
            task_count: i32::try_from(tokens).expect("fixture token 数远小于 i32::MAX"),
        }
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn set(ids: &[Uuid]) -> HashSet<Uuid> {
        ids.iter().copied().collect()
    }

    /// 两个受限 agent 的同 `(provider, model)` 行合并成**一个**哨兵桶，且**和全部保留**。
    #[test]
    fn restricted_usage_rows_merge_into_one_bucket_without_losing_totals() {
        let folded = fold_restricted_usage_by_agent(
            vec![
                row(&id(1).to_string(), "anthropic", "claude", 10),
                row(&id(2).to_string(), "anthropic", "claude", 5),
            ],
            &set(&[id(1), id(2)]),
        );
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].agent_id, RESTRICTED_AGENTS_ROW_ID);
        // 10 + 5 的**每一个**字段。
        assert_eq!(folded[0].tokens.input_tokens, 15);
        assert_eq!(folded[0].tokens.output_tokens, 30);
        assert_eq!(folded[0].tokens.cache_read_tokens, 45);
        assert_eq!(folded[0].tokens.cache_write_tokens, 60);
        assert_eq!(folded[0].cost_usd_ticks, 150);
        assert_eq!(folded[0].uncosted_input_tokens, 15);
        assert_eq!(folded[0].uncosted_output_tokens, 30);
        assert_eq!(folded[0].uncosted_cache_read_tokens, 45);
        assert_eq!(folded[0].uncosted_cache_write_tokens, 60);
        assert_eq!(folded[0].task_count, 15);
    }

    /// 桶**保留** `(provider, model)` 维度（否则客户端给桶定价的钱算不出来）。
    #[test]
    fn the_restricted_bucket_keeps_its_provider_model_split() {
        let folded = fold_restricted_usage_by_agent(
            vec![
                row(&id(1).to_string(), "anthropic", "claude", 10),
                row(&id(2).to_string(), "cursor", "auto", 7),
            ],
            &set(&[id(1), id(2)]),
        );
        assert_eq!(folded.len(), 2);
        let keys: Vec<_> = folded
            .iter()
            .map(|r| (r.provider.clone(), r.model.clone()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("anthropic".to_string(), "claude".to_string()),
                ("cursor".to_string(), "auto".to_string())
            ]
        );
        assert!(folded
            .iter()
            .all(|r| r.agent_id == RESTRICTED_AGENTS_ROW_ID));
    }

    /// 可见的 agent **原样透传**，且行序不变（桶坐在它第一个成员原来的位置上）。
    #[test]
    fn visible_rows_pass_through_in_order() {
        let visible = id(9);
        let folded = fold_restricted_usage_by_agent(
            vec![
                row(&visible.to_string(), "anthropic", "claude", 1),
                row(&id(1).to_string(), "anthropic", "claude", 10),
                row(&visible.to_string(), "cursor", "auto", 2),
            ],
            &set(&[id(1)]),
        );
        assert_eq!(folded.len(), 3);
        assert_eq!(folded[0].agent_id, visible.to_string());
        assert_eq!(folded[1].agent_id, RESTRICTED_AGENTS_ROW_ID);
        assert_eq!(folded[2].agent_id, visible.to_string());
    }

    /// 空 restricted 集合 ⇒ **原样返回**（上游 `foldRestrictedAgents` 的早退）。
    #[test]
    fn an_empty_restricted_set_short_circuits() {
        let rows = vec![row(&id(1).to_string(), "anthropic", "claude", 1)];
        assert_eq!(
            fold_restricted_usage_by_agent(rows.clone(), &set(&[])),
            rows
        );
    }

    /// `?days=` 三条口径：默认 30 / 上界 365 / **非法值静默回落**（上游没有 400 分支）。
    #[test]
    fn days_follows_the_upstream_silent_fallback() {
        let q = |raw: &str| -> HashMap<String, String> {
            HashMap::from([("days".to_string(), raw.to_string())])
        };
        assert_eq!(days_of(&HashMap::new()), 30);
        assert_eq!(days_of(&q("1")), 1);
        assert_eq!(days_of(&q("365")), 365);
        for bad in ["0", "-1", "366", "abc", "1.5"] {
            assert_eq!(days_of(&q(bad)), 30, "{bad}");
        }
    }

    /// 两半 cutoff **不混用**：三条日期序列 N+1 桶、三条 per-agent 恰好 N 桶。
    ///
    /// 用例走 `since_from_days(now, …)`（而不是走 `Utc::now()` 的 `since_cutoff`）
    /// 以便把「今天」钉成一个具体的日子 —— 否则断言会在跨日那一刻变红，
    /// 而那与 cutoff 口径无关。
    #[test]
    fn the_two_cutoff_halves_are_not_mixed_up() {
        let shanghai: Tz = "Asia/Shanghai".parse().unwrap();
        // 上海本地 2026-09-27 12:00 == 2026-09-27T04:00Z。
        let now = Utc.with_ymd_and_hms(2026, 9, 27, 4, 0, 0).unwrap();
        let days = 30u32;

        let headroom_days = i64::from(cutoff_days_with_convention(
            days,
            cutoff_for(DashboardRoute::UsageDaily),
        ));
        let exact_days = i64::from(cutoff_days_with_convention(
            days,
            cutoff_for(DashboardRoute::AgentRunTime),
        ));
        assert_eq!(headroom_days, 30, "日期序列 = N+1 个桶");
        assert_eq!(exact_days, 29, "per-agent = 恰好 N 个桶");

        let headroom = since_from_days(now, headroom_days, shanghai);
        let exact = since_from_days(now, exact_days, shanghai);
        // 上海本地 2026-08-28 00:00 == 2026-08-27T16:00Z（今天 09-27 往前 30 天）。
        assert_eq!(
            headroom,
            Utc.with_ymd_and_hms(2026, 8, 27, 16, 0, 0).unwrap()
        );
        assert_eq!(exact, Utc.with_ymd_and_hms(2026, 8, 28, 16, 0, 0).unwrap());
        assert_eq!(exact - headroom, Duration::days(1), "两半正好差一天");

        // **恰好 3 条**走 ExactDays —— 与上游 `dashboard.go` 的六个调用点一一对应。
        let exact_routes: Vec<DashboardRoute> = DashboardRoute::ALL
            .into_iter()
            .filter(|r| cutoff_for(*r) == CutoffConvention::ExactDays)
            .collect();
        assert_eq!(
            exact_routes,
            vec![
                DashboardRoute::AgentRunTime,
                DashboardRoute::FailuresByAgent,
                DashboardRoute::UsageByAgent,
            ]
        );
        // 🔴 这条断言记录**已知分歧**：anchor 冻结的 `CutoffConvention::for_route`
        // 把 `usage/by-agent` / `failures/by-agent` 也算成 N+1，与上游逐字相反。
        // owner 裁决前，本片按**上游**实现（见 `cutoff_for` 的注释）。
        assert_ne!(
            CutoffConvention::for_route(DashboardRoute::UsageByAgent),
            CutoffConvention::ExactDays,
            "anchor 的 for_route 与上游不一致；本片以 cutoff_for 为准（docs/32 §9.13 已登记）"
        );
    }

    /// tz 只影响 cutoff 的边界与日期分桶的轴；**非法 tz 永不报错**。
    #[test]
    fn an_invalid_tz_falls_through_instead_of_erroring() {
        assert!(!valid_tz("Mars/Olympus"));
        assert!(valid_tz("UTC"));
        assert!(valid_tz("Asia/Shanghai"));
    }
}

/// 真库用例（门 ⑥）：usage 两条的**逐字段**证据（tz 日界 / 服务端可见性折叠）。
///
/// 造行只碰 `task_usage_hourly` / `agent` / `agent_invocation_target` —— 与两条聚合
/// **真实读取**的表一一对应；仓库里那张 `task_usage_dashboard_daily` 在迁移 103 之后
/// 已不存在，「本片不读它」这条纪律由 `mc-repos` 那侧的
/// `no_legacy_rollup_tables_are_referenced` 钉住（这里连表都造不出来）。
#[cfg(test)]
mod db_tests {
    use super::super::failures::test_support::{app, fixture, get_json, seed, seed_hourly, Seed};
    use super::*;
    use chrono::{Duration, Utc};

    /// 一个落在窗口内、且能区分 UTC 与 Asia/Shanghai 日界的桶（23:00Z）。
    fn bucket() -> DateTime<Utc> {
        (Utc::now() - Duration::days(2))
            .date_naive()
            .and_hms_opt(23, 0, 0)
            .expect("valid naive time")
            .and_utc()
    }

    fn bucket_date_utc() -> String {
        bucket().format("%Y-%m-%d").to_string()
    }

    fn bucket_date_shanghai() -> String {
        let b: Tz = "Asia/Shanghai".parse().unwrap();
        bucket().with_timezone(&b).format("%Y-%m-%d").to_string()
    }

    // -- GET /api/dashboard/usage/daily ---------------------------------

    /// 真库造行 ⇒ `usage/daily` 的**逐字段**证据：时区日界 + `LOWER(provider)` 归一 +
    /// 四类 token / cost / 四类 uncosted / `task_count` 的求和。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn usage_daily_slices_calendar_days_under_the_viewers_tz() {
        let db = fixture!();
        let seed: Seed = seed(&db).await;
        // 两个**相邻的小时桶**（23:00Z / 22:00Z）落在同一个日历日里 ——
        // 23:00Z 对 UTC 还在当天，对 Asia/Shanghai 已经进了第二天。
        seed_hourly(
            &db,
            &seed,
            seed.public_agent,
            Some(seed.project_a),
            "Anthropic",
            "claude",
            bucket(),
            100,
        )
        .await;
        seed_hourly(
            &db,
            &seed,
            seed.public_agent,
            Some(seed.project_a),
            "Anthropic",
            "claude",
            bucket() - Duration::hours(1),
            50,
        )
        .await;
        let app = app(db);

        let (status, body) = get_json(
            &app,
            "/api/dashboard/usage/daily?tz=UTC",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        assert_eq!(rows.len(), 1, "两行同 provider/model 必须并成一个桶");
        let row = &rows[0];
        assert_eq!(row["date"], bucket_date_utc());
        assert_eq!(row["provider"], "anthropic", "provider 必须 LOWER 归一");
        assert_eq!(row["model"], "claude");
        // `seed_hourly(base)` 写的是 base / base+1 / base+2 / base+3 四个 token、
        // base+10 的 cost、base+11..=base+13 的 uncosted、task_count = 1。
        assert_eq!(row["input_tokens"], 150, "100 + 50");
        assert_eq!(row["output_tokens"], 152, "101 + 51");
        assert_eq!(row["cache_read_tokens"], 154, "102 + 52");
        assert_eq!(row["cache_write_tokens"], 156, "103 + 53");
        assert_eq!(row["cost_usd_ticks"], 170, "110 + 60");
        assert_eq!(row["uncosted_input_tokens"], 172, "111 + 61");
        assert_eq!(row["uncosted_output_tokens"], 174, "112 + 62");
        assert_eq!(row["uncosted_cache_read_tokens"], 176, "113 + 63");
        assert_eq!(row["uncosted_cache_write_tokens"], 178, "114 + 64");
        assert_eq!(row["task_count"], 2);

        // 同一批行、同一时刻，看的人在 Asia/Shanghai ⇒ **落在另一个日历日**。
        let (status, body) = get_json(
            &app,
            "/api/dashboard/usage/daily?tz=Asia/Shanghai",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        assert_eq!(rows[0]["date"], bucket_date_shanghai());
        assert_ne!(
            rows[0]["date"],
            bucket_date_utc(),
            "两条 tz 必须给出不同的日界"
        );
    }

    // -- GET /api/dashboard/usage/by-agent ------------------------------

    /// 真库造行 ⇒ `usage/by-agent` 的**服务端可见性折叠**：
    /// private（owner 自己的）+ `kind='system'` 承运 agent 折进哨兵桶，
    /// `public_to` 的 agent 原样透传；且**合并后总额不丢**。
    #[tokio::test]
    #[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
    async fn usage_by_agent_folds_private_and_system_agents_server_side() {
        let db = fixture!();
        let seed = seed(&db).await;
        for (offset, base) in [(0i64, 10i64), (1, 20), (2, 30)] {
            seed_hourly(
                &db,
                &seed,
                seed.public_agent,
                None,
                "anthropic",
                "claude",
                bucket() - Duration::hours(offset),
                base,
            )
            .await;
        }
        seed_hourly(
            &db,
            &seed,
            seed.private_agent,
            None,
            "anthropic",
            "claude",
            bucket(),
            40,
        )
        .await;
        seed_hourly(
            &db,
            &seed,
            seed.system_agent,
            None,
            "anthropic",
            "claude",
            bucket(),
            50,
        )
        .await;
        let app = app(db);

        // 以**普通 member** 身份看：public_to + workspace 目标可见；private 与 system 折叠。
        let (status, body) = get_json(
            &app,
            "/api/dashboard/usage/by-agent",
            seed.member,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        assert_eq!(rows.len(), 2, "可见 agent 一行 + 哨兵桶一行");

        let visible = rows
            .iter()
            .find(|r| r["agent_id"] == serde_json::json!(seed.public_agent.to_string()))
            .expect("public_to agent 必须原样透传");
        assert_eq!(visible["input_tokens"], 60, "10 + 20 + 30");
        assert_eq!(visible["provider"], "anthropic");

        let bucket_row = rows
            .iter()
            .find(|r| r["agent_id"] == serde_json::json!(RESTRICTED_AGENTS_ROW_ID))
            .expect("受限 agent 必须折进哨兵桶");
        assert_eq!(bucket_row["input_tokens"], 90, "40 + 50，一个都不能少");
        assert_eq!(bucket_row["task_count"], 2);
        assert_eq!(
            bucket_row["cost_usd_ticks"],
            50 + 60,
            "cost_usd_ticks = base+10"
        );
        assert_eq!(
            bucket_row["provider"], "anthropic",
            "桶保留 provider/model 维度"
        );
        assert_eq!(bucket_row["model"], "claude");

        // 响应里**不得**出现那两个裸 UUID。
        let raw = body.to_string();
        assert!(!raw.contains(&seed.private_agent.to_string()));
        assert!(!raw.contains(&seed.system_agent.to_string()));

        // workspace **owner** 看同一条：private agent 是他自己的 ⇒ 不折叠。
        let (status, body) = get_json(
            &app,
            "/api/dashboard/usage/by-agent",
            seed.owner,
            seed.workspace,
        )
        .await;
        assert_eq!(status, 200);
        let rows = body.as_array().expect("bare JSON array");
        assert!(rows
            .iter()
            .any(|r| r["agent_id"] == serde_json::json!(seed.private_agent.to_string())));
        // system 承运 agent 对**任何人**都不点名 ⇒ 哨兵桶仍在。
        assert!(rows
            .iter()
            .any(|r| r["agent_id"] == serde_json::json!(RESTRICTED_AGENTS_ROW_ID)));
    }
}
