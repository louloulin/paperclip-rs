//! 4 条 runtime 用量/活动读端点（上游 `handler/runtime.go` L85-352）。
//!
//! | method | path | 上游 | 默认窗口 |
//! |---|---|---|---|
//! | GET | `/api/runtimes/:runtimeId/usage` | `GetRuntimeUsage` | 90 天（N+1 桶） |
//! | GET | `/api/runtimes/:runtimeId/usage/by-agent` | `GetRuntimeUsageByAgent` | 30 天 |
//! | GET | `/api/runtimes/:runtimeId/usage/by-hour` | `GetRuntimeUsageByHour` | 30 天 |
//! | GET | `/api/runtimes/:runtimeId/activity` | `GetRuntimeTaskActivity` | **无窗口**（全量） |
//!
//! 四个都回**裸 JSON 数组**（上游 `writeJSON(w, 200, resp)`）。
//! 门是 `requireRuntimeReadAccess`：成员 **且** `usable_by` —— `private` 机器的用量
//! 连 owner/admin 之外的成员都读不到，失败一律 404（不做存在性预言机）。
//!
//! ## `days=N` 为什么是 N+1 个日历桶
//!
//! `sinceFromDays` 取「今天本地零点往前推 N 天」，于是 `days=N` 覆盖
//! today-N … today 共 **N+1** 个桶。这是**刻意的余量**，不是 off-by-one：
//! runtime 详情页的 prior-window 差值要往回够到 today-2N，缺了最老那桶它会静默丢数据。
//! 别「收紧」成 -(N-1)。
//!
//! `activity` 的行**不带日期维度**，客户端没法像 dashboard 那样自己裁掉多余的一天，
//! 所以它既不吃 `days` 也不做裁剪（上游同样不传 `Since`）。
//!
//! ## 时区
//!
//! `?tz=` 合法 IANA 名 → 用它；否则读 `user.timezone`（冷路径，浏览器端总会带 `?tz=`）；
//! 仍不可用 → `UTC`。**永不报错**：tz 只是渲染关注点。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Duration, LocalResult, TimeZone, Utc};
use chrono_tz::Tz;
use mc_repos::runtime::AgentRuntimeRepo;
use mc_repos::Repository;
use serde::Deserialize;

use super::access::{load_readable_runtime, repo_err};
use super::dto::{ActivityDto, RuntimeUsageByAgentDto, RuntimeUsageByHourDto, RuntimeUsageDto};
use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// `usage` 的默认窗口（上游 `parseSinceParamInTZ(r, 90, viewTZ)`）。
const USAGE_DEFAULT_DAYS: i64 = 90;
/// `by-agent` / `by-hour` 的默认窗口。
const ROLLUP_DEFAULT_DAYS: i64 = 30;
/// `?days=` 的合法上界（上游 `parsed > 0 && parsed <= 365`）。
const MAX_DAYS: i64 = 365;

/// usage 四个端点的查询串。多出来的部分（`workspace_id` 等）被 serde 忽略。
#[derive(Debug, Default, Deserialize)]
pub(super) struct UsageQuery {
    #[serde(default)]
    days: Option<String>,
    #[serde(default)]
    tz: Option<String>,
}

fn repo(state: &AppState) -> AgentRuntimeRepo {
    AgentRuntimeRepo::new(state.db.clone())
}

// ---------------------------------------------------------------------------
// GET /api/runtimes/:runtimeId/usage
// ---------------------------------------------------------------------------

/// upstream `GetRuntimeUsage`：按「调用方时区里的日历日 × provider × model」聚合。
pub(super) async fn get_runtime_usage(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(runtime): Path<String>,
    Query(query): Query<UsageQuery>,
) -> ApiResult<Json<Vec<RuntimeUsageDto>>> {
    let rt = load_readable_runtime(&state, &runtime, auth.id()).await?;
    let tz = resolve_viewing_tz(&state, auth.id(), query.tz.as_deref()).await;
    let since = since_cutoff(
        days_param(query.days.as_deref(), USAGE_DEFAULT_DAYS),
        0,
        &tz,
    );
    let rows = repo(&state)
        .list_runtime_usage(rt.id, since, &tz)
        .await
        .map_err(|e| repo_err(e, "runtime"))?;
    Ok(Json(
        rows.iter()
            .map(|row| RuntimeUsageDto::from_row(rt.id, row))
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// GET /api/runtimes/:runtimeId/usage/by-agent
// ---------------------------------------------------------------------------

/// upstream `GetRuntimeUsageByAgent`：没有日期维度，tz 只影响窗口边界。
pub(super) async fn get_runtime_usage_by_agent(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(runtime): Path<String>,
    Query(query): Query<UsageQuery>,
) -> ApiResult<Json<Vec<RuntimeUsageByAgentDto>>> {
    let rt = load_readable_runtime(&state, &runtime, auth.id()).await?;
    let tz = resolve_viewing_tz(&state, auth.id(), query.tz.as_deref()).await;
    let since = since_cutoff(
        days_param(query.days.as_deref(), ROLLUP_DEFAULT_DAYS),
        0,
        &tz,
    );
    let rows = repo(&state)
        .list_runtime_usage_by_agent(rt.id, since)
        .await
        .map_err(|e| repo_err(e, "runtime"))?;
    Ok(Json(
        rows.iter().map(RuntimeUsageByAgentDto::from).collect(),
    ))
}

// ---------------------------------------------------------------------------
// GET /api/runtimes/:runtimeId/usage/by-hour
// ---------------------------------------------------------------------------

/// upstream `GetRuntimeUsageByHour`：一天里的哪个小时（按调用方时区分桶）。
/// 零活动的桶不返回，客户端补 0..23。
pub(super) async fn get_runtime_usage_by_hour(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(runtime): Path<String>,
    Query(query): Query<UsageQuery>,
) -> ApiResult<Json<Vec<RuntimeUsageByHourDto>>> {
    let rt = load_readable_runtime(&state, &runtime, auth.id()).await?;
    let tz = resolve_viewing_tz(&state, auth.id(), query.tz.as_deref()).await;
    let since = since_cutoff(
        days_param(query.days.as_deref(), ROLLUP_DEFAULT_DAYS),
        0,
        &tz,
    );
    let rows = repo(&state)
        .get_runtime_usage_by_hour(rt.id, since, &tz)
        .await
        .map_err(|e| repo_err(e, "runtime"))?;
    Ok(Json(rows.iter().map(RuntimeUsageByHourDto::from).collect()))
}

// ---------------------------------------------------------------------------
// GET /api/runtimes/:runtimeId/activity
// ---------------------------------------------------------------------------

/// upstream `GetRuntimeTaskActivity`：按小时统计的启动任务数（全量，无 `days`）。
pub(super) async fn get_runtime_activity(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path(runtime): Path<String>,
    Query(query): Query<UsageQuery>,
) -> ApiResult<Json<Vec<ActivityDto>>> {
    let rt = load_readable_runtime(&state, &runtime, auth.id()).await?;
    let tz = resolve_viewing_tz(&state, auth.id(), query.tz.as_deref()).await;
    let rows = repo(&state)
        .get_runtime_task_activity(rt.id, &tz)
        .await
        .map_err(|e| repo_err(e, "runtime"))?;
    Ok(Json(rows.iter().map(ActivityDto::from).collect()))
}

// ---------------------------------------------------------------------------
// 窗口 / 时区
// ---------------------------------------------------------------------------

/// upstream `parseDaysCutoff` 的参数部分：`days` 缺省 / 非法 / 越界 → 默认值。
fn days_param(raw: Option<&str>, default_days: i64) -> i64 {
    raw.filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|days| (1..=MAX_DAYS).contains(days))
        .unwrap_or(default_days)
}

/// upstream `parseDaysCutoff` 的窗口部分。`trim_days` 把切点往前拉：`0` 保留 N+1 桶的
/// 余量，`1` 收紧到恰好 N 个日历桶。
fn since_cutoff(days: i64, trim_days: i64, tz: &str) -> DateTime<Utc> {
    let tz = tz.parse::<Tz>().unwrap_or(Tz::UTC);
    since_from_days(Utc::now(), days - trim_days, tz)
}

/// upstream `sinceFromDays`：`now` 本地日历日往前推 `days` 天的那个本地零点。
fn since_from_days(now: DateTime<Utc>, days: i64, tz: Tz) -> DateTime<Utc> {
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
/// - **歧义**（秋季重复的那个 00:00）：取较早的一支。Go 不保证选哪个，实际行为是第一个。
/// - **不存在**（春季被抹掉的 00:00，例如 `America/Santiago`）：往前推一小时 ——
///   Go 会把它规范到跳变之后的第一个瞬时，丢了这一步会让切点整整早一天。
fn resolve_local(tz: Tz, naive: chrono::NaiveDateTime) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        LocalResult::Ambiguous(earliest, _) => Some(earliest.with_timezone(&Utc)),
        LocalResult::None => tz
            .from_local_datetime(&(naive + Duration::hours(1)))
            .earliest()
            .map(|dt| dt.with_timezone(&Utc)),
    }
}

/// upstream `resolveViewingTZ`：`?tz=` → `user.timezone` → `"UTC"`。永不报错。
async fn resolve_viewing_tz(state: &AppState, user_id: mc_core::Id, raw: Option<&str>) -> String {
    if let Some(tz) = raw
        .map(str::trim)
        .filter(|tz| !tz.is_empty() && valid_tz(tz))
    {
        return tz.to_string();
    }
    // 冷路径：只有不带 `?tz=` 的 API 客户端会走到这里（上游同款注释）。
    let stored = mc_repos::user::UserRepo::new(state.db.clone())
        .get(&user_id)
        .await
        .ok()
        .and_then(|user| user.timezone);
    let Some(stored) = stored.map(|value| value.trim().to_string()) else {
        return "UTC".to_string();
    };
    if stored.is_empty() || !valid_tz(&stored) {
        return "UTC".to_string();
    }
    stored
}

/// 只接受 `chrono-tz` 认得的名字（等价于上游 `time.LoadLocation` 成功）。
fn valid_tz(name: &str) -> bool {
    name.parse::<Tz>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    /// `days=90` 必须给出 **91** 个日历桶的起点：today-90 的本地零点。
    #[test]
    fn cutoff_keeps_the_extra_day_of_headroom() {
        let shanghai: Tz = "Asia/Shanghai".parse().unwrap();
        // 2026-09-22T04:00Z = 上海 12:00 → 本地「今天」是 09-22。
        let cutoff = since_from_days(utc(2026, 9, 22, 4), 90, shanghai);
        // 上海 2026-06-24 00:00 == 2026-06-23T16:00Z。
        assert_eq!(cutoff, utc(2026, 6, 23, 16));
    }

    #[test]
    fn trimming_one_closes_the_window() {
        let shanghai: Tz = "Asia/Shanghai".parse().unwrap();
        assert_eq!(days_param(Some("1"), 30), 1);
        let cutoff = since_from_days(utc(2026, 9, 22, 4), 1 - 1, shanghai);
        assert_eq!(cutoff, utc(2026, 9, 21, 16));
    }

    #[test]
    fn days_param_falls_back_on_garbage_and_out_of_range() {
        assert_eq!(days_param(None, 90), 90);
        assert_eq!(days_param(Some(""), 90), 90);
        assert_eq!(days_param(Some("abc"), 90), 90);
        assert_eq!(days_param(Some("0"), 90), 90);
        assert_eq!(days_param(Some("-3"), 90), 90);
        assert_eq!(days_param(Some("366"), 90), 90);
        assert_eq!(days_param(Some("365"), 90), 365);
    }

    /// 春季跳变把本地午夜抹掉的时区：切点必须落在跳变之后的第一个瞬时，
    /// 而不是退回「按 UTC 解析」。
    #[test]
    fn dst_gap_midnight_normalises_forward() {
        let santiago: Tz = "America/Santiago".parse().unwrap();
        // 2026-09-06 是智利春季跳变日：本地 00:00 不存在（00:00 → 01:00）。
        let cutoff = since_from_days(utc(2026, 9, 10, 12), 4, santiago);
        // 期望 = 2026-09-06T01:00-03:00 == 2026-09-06T04:00Z（而不是 03:00Z）。
        assert_eq!(cutoff, utc(2026, 9, 6, 4));
    }

    #[test]
    fn unknown_zone_falls_back_to_utc() {
        assert!(!valid_tz("Mars/Olympus"));
        assert!(valid_tz("UTC"));
        assert!(valid_tz("Asia/Shanghai"));
    }
}
