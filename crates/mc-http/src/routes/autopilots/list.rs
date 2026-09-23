//! autopilot **读面**（列表 / 单体 / cron-preview / usage）—— M5-1 实现。
//!
//! - **写者**：M5-1（`docs/44` §3.2）。`mount.rs::mount_slice_autopilot()` →
//!   `autopilots::router()` 已接好，本文件只实现自己的 `router()`，**不改** `mount.rs` /
//!   `routes/mod.rs` / `autopilots/mod.rs`。
//! - **路由**（`router.go` L2101–L2104 与 L2123）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 1 | GET | `/api/autopilots/`（+ 无斜杠别名） | `ListAutopilots` | 84 |
//! | 2 | GET | `/api/autopilots/cron-preview` | `CronPreview` | 31 (+8) |
//! | 3 | GET | `/api/autopilots/usage` | `GetAutopilotQuotaUsage` | 33 |
//! | 4 | GET | `/api/autopilots/:id/`（+ 无斜杠别名） | `GetAutopilot` | 73 |
//!
//! - **#1/#4 是双形态**（chi `Mount`，见 `autopilots/mod.rs` 的形态纪律）；#2/#3 单形态
//!   —— 给 #2/#3 加尾斜杠别名会被门 ⑦ 的 `slash_alias_audit.py` 判 `EXTRA_ALIAS`。
//! - **列表的三个派生列**（`docs/44` §4.2 M5-1）：`trigger_kinds` / `next_run_at` /
//!   `last_run_status` 由 `mc-repos::autopilot::AutopilotRepo::list` 的三条子查询一次带出
//!   （**不是** N+1，也不在 http 层拼 SQL）。契约以上游 JSON 为准，见 `../dto.rs`。
//! - **`can_write` 是 `Option<bool>`**（不带 caller 时省略），见 `../dto.rs`。
//! - **权限**：读面用 `../access.rs` 的可见性判定；`usage` 走 `mc_autopilot::quota`
//!   （限额来自 entitlement 平面，没有商业默认值可抄）。
//!
//! # 四个 handler 的**共享骨架**（与上游的 middleware 位置差异）
//!
//! 上游这四条路由都在 `RequireWorkspaceMember` 组里，所以成员门槛是**中间件**；本仓没有
//! 那层中间件，四条 handler 各自显式走 `resolve_workspace_id` → [`require_member`]，
//! 顺序与上游等价（**先** workspace → **先** 成员，**再**碰业务）：
//!
//! ```text
//! X-Workspace-ID / ?workspace_id=  →  400 invalid workspace id
//! 非成员                            →  404 workspace（不是 403）
//! ```
//!
//! ## 两个「失败要往哪边倒」的对照（上游刻意做成不对称，别顺手统一）
//!
//! | 数据 | 出错时 | 为什么 |
//! | --- | --- | --- |
//! | `subscribers`（列表 + 详情） | **500 fail closed** | 空数组是**断言**（「没有订阅者」）而不是缺省；写面是整表替换，静默的空值会让一次 PATCH 抹掉真订阅者（MUL-6680） |
//! | `triggers` / `collaborators`（详情） | **fail open → `[]`** | 两者是附加信息；触发器读失败时主对象仍然可用，`[]` 不会被回写 |
//!
//! ## 与上游的偏离（完整表见 `docs/46-M5-1-READ-FACE.md` §5）
//!
//! - **workspace 解析**只有 `X-Workspace-ID` header 与 `?workspace_id=` 两个来源（上游
//!   middleware 的 5/6 优先级），不支持 slug —— 与本仓 `inbox` 面同一口径；
//! - **`total` 是本次返回条数**（与上游一致），不是分页总数：这个端点不分页；
//! - `cron-preview` 的 `next_runs` 用 `Z` 结尾的**秒精度** RFC3339，逐字对齐 Go 的
//!   `time.RFC3339`（本仓 `Timestamp::as_iso()` 是 `+00:00` 形式，见 handler 内注释）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{SecondsFormat, Utc};
use serde_json::json;
use uuid::Uuid;

use mc_autopilot::cron::{next_occurrences_after_utc, resolve_timezone};
use mc_errors::Error;
use mc_repos::autopilot::AutopilotRepo;

use crate::error::ApiResult;
use crate::routes::agents::repo_err;
use crate::routes::auth_user::AuthUser;
use crate::routes::autopilots::access::{
    acting_user_id, can_manage_access, load_in_workspace, member_can_write, owns_for_write,
    require_member,
};
use crate::routes::autopilots::dto::{
    autopilot_list_to_response, autopilot_to_response, collaborator_entry, redact_webhook_secrets,
    subscriber_entry, trigger_to_response, AutopilotDetailEnvelope, AutopilotListEnvelope,
    AutopilotQuotaUsageResponse, AutopilotSubscriberEntry, CronPreviewErrorBody,
};
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

/// 上游 `CronPreview` 的 `previewCount`：排程编辑器一次预览三个触发时刻。
pub const CRON_PREVIEW_COUNT: usize = 3;

/// 上游 `CronPreview` 的 `tz` 缺省。
pub const CRON_PREVIEW_DEFAULT_TIMEZONE: &str = "UTC";

/// autopilot 读面 router（4 条路由 / 6 个注册键）。
///
/// matchit 里**字面量段优先于参数段** ⇒ `cron-preview` / `usage` 不会被 `:id` 吃掉；
/// 但注册顺序仍按上游 #1→#4 的语义顺序写，避免日后有人「优化」成通配。
pub fn router() -> Router<Arc<AppState>> {
    // 注意：axum 0.7（matchit 0.7）路径参数语法是 `:id`，不是 `{id}`（那是 axum 0.8）；
    // 写成 `{id}` 会把整段当字面量，路由恒 404。
    //
    // #1/#4 的无斜杠别名必须**同方法集合**一起注册：上游 chi 的 `Route(...) + Get("/")` 两种
    // 形态都服务，而 axum 不做归一化（少一个是 404，不是 307）。
    Router::new()
        .route("/api/autopilots", get(list_autopilots))
        .route("/api/autopilots/", get(list_autopilots))
        .route("/api/autopilots/cron-preview", get(cron_preview))
        .route("/api/autopilots/usage", get(get_quota_usage))
        .route("/api/autopilots/:id", get(get_autopilot))
        .route("/api/autopilots/:id/", get(get_autopilot))
}

/// `GET /api/autopilots[/]`（上游 `ListAutopilots` 84 行）。
///
/// `status` 查询参数的三态（上游 `sqlc.narg` 语义）：**缺省/空串** ⇒ 列出除 `archived`
/// 之外的全部；**给了值** ⇒ 精确过滤该状态，且**没有白名单校验** —— 传
/// `?status=不存在的状态` 得到空列表（不是 400）。上游如此，别「顺手加校验」。
async fn list_autopilots(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<AutopilotListEnvelope>> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    // 成员门槛（上游是路由组中间件）：非成员 → 404 workspace。
    let role = require_member(&state, workspace_id, user_id).await?;

    let repo = AutopilotRepo::new(state.db.clone());
    let status = query
        .get("status")
        .map(String::as_str)
        .filter(|value| !value.is_empty());
    let rows = repo
        .list(workspace_id, status)
        .await
        .map_err(|err| repo_err(err, "autopilot"))?;

    // 订阅者：整页一次批量查询（上游 MUL-6680 的修复点），**失败即 500**（见模块文档）。
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let subscribers = repo
        .list_subscribers_for_autopilots(&ids)
        .await
        .map_err(|err| repo_err(err, "autopilot subscriber"))?;
    let mut by_autopilot: HashMap<Uuid, Vec<AutopilotSubscriberEntry>> = HashMap::new();
    for row in &subscribers {
        by_autopilot
            .entry(row.autopilot_id)
            .or_default()
            .push(subscriber_entry(row));
    }

    // 协作者授权一次取成集合（上游 `ListAutopilotIDsForCollaborator`，**不带** workspace 过滤），
    // 这样逐行 `can_write` 不产生 N+1。读失败降级为「无授权」：`can_write=false` 是安全的那一侧。
    let collaborator_ids: HashSet<Uuid> = repo
        .list_autopilot_ids_for_collaborator(user_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();

    let autopilots = rows
        .iter()
        .map(|row| {
            let subscribers = by_autopilot.remove(&row.id).unwrap_or_default();
            let mut resp = autopilot_list_to_response(row, subscribers);
            let can_write = owns_for_write(&row.created_by_type, row.created_by_id, &role, user_id)
                || collaborator_ids.contains(&row.id);
            resp.can_write = Some(can_write);
            // `can_manage_access` **不在列表响应里**（上游只在详情上盖这个章）。
            resp
        })
        .collect::<Vec<_>>();

    Ok(Json(AutopilotListEnvelope {
        total: autopilots.len(),
        autopilots,
    }))
}

/// `GET /api/autopilots/:id[/]`（上游 `GetAutopilot` 73 行）。
///
/// 顺序逐字对照上游：加载主对象 → 订阅者（fail closed）→ 算写权 → 触发器（fail open + 抹凭据）
/// → 协作者（fail open）。**写权必须在触发器之前算出来**：它是凭据抹除的唯一开关。
async fn get_autopilot(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> ApiResult<Json<AutopilotDetailEnvelope>> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    let role = require_member(&state, workspace_id, user_id).await?;
    let repo = AutopilotRepo::new(state.db.clone());

    // 非 UUID → 400；不存在 / 跨工作区 → 404（`../access.rs`）。
    let row = load_in_workspace(&repo, &id, workspace_id).await?;

    let subscribers = repo
        .list_subscribers(row.id)
        .await
        .map_err(|err| repo_err(err, "autopilot subscriber"))?
        .iter()
        .map(subscriber_entry)
        .collect::<Vec<_>>();
    let mut autopilot = autopilot_to_response(&row, subscribers);

    // 写权两连：能写（含协作者）与**能改授权**（不含协作者 —— 协作者能写但不能转授权）。
    let can_write = member_can_write(&repo, &row, &role, user_id).await;
    let can_manage = can_manage_access(&row, &role, user_id);
    autopilot.can_write = Some(can_write);
    autopilot.can_manage_access = Some(can_manage);

    // 触发器：读失败 → `[]`（fail open，见模块文档）。非写者拿不到 webhook token/URL。
    let triggers = repo
        .list_triggers(row.id)
        .await
        .unwrap_or_default()
        .iter()
        .map(|trigger| {
            let mut resp = trigger_to_response(trigger);
            if !can_write {
                redact_webhook_secrets(&mut resp);
            }
            resp
        })
        .collect::<Vec<_>>();

    // 协作者：同上 fail open（管理访问列表的 UI 会自己重试）。
    let collaborators = repo
        .list_collaborators(row.id)
        .await
        .unwrap_or_default()
        .iter()
        .map(collaborator_entry)
        .collect::<Vec<_>>();

    Ok(Json(AutopilotDetailEnvelope {
        autopilot,
        triggers,
        collaborators,
    }))
}

/// `GET /api/autopilots/cron-preview`（上游 `CronPreview` 31 + `writeCronPreviewError` 8 行）。
///
/// 纯计算：`expr` 解析失败与 `tz` 不认识**都**是 400，但 `code` 不同
/// （`invalid_cron` / `invalid_timezone`），因为排程编辑器靠它决定高亮哪个输入框。
/// 语法合法但永不触发的表达式（`0 0 30 2 *`）返回**短数组**而不是错误 —— 「永不跑」与
/// 「你的 cron 写错了」靠状态码区分。
///
/// 唯一的**扁平**错误体（`{"error": "msg", "code": …}`），见 `../dto.rs`。
async fn cron_preview(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    // 上游把成员校验放在路由组里；这条 handler 自己不碰任何资源，但仍必须在组内。
    require_member(&state, workspace_id, acting_user_id(user)).await?;

    let expr = query
        .get("expr")
        .map(String::as_str)
        .filter(|value| !value.is_empty());
    let Some(expr) = expr else {
        return Ok(CronPreviewErrorBody::bad_request(
            mc_autopilot::cron::CODE_INVALID_CRON,
            "expr is required",
        ));
    };
    let timezone = query
        .get("tz")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(CRON_PREVIEW_DEFAULT_TIMEZONE);
    // 时区先校验（上游 `ValidateTimezone`）：`tz` 参数不认识 ⇒ `invalid_timezone`；
    // 而表达式自带的 `TZ=` 前缀不认识由解析器报错 ⇒ `invalid_cron`（`mc-autopilot::cron`
    // 的 `CronError::code()` 已经把这个分类固化了，别在这里重新分）。
    if let Err(err) = resolve_timezone(timezone) {
        return Ok(CronPreviewErrorBody::bad_request(
            err.code(),
            err.to_string(),
        ));
    }

    let occurrences =
        match next_occurrences_after_utc(expr, timezone, Utc::now(), CRON_PREVIEW_COUNT) {
            Ok(occurrences) => occurrences,
            Err(err) => {
                return Ok(CronPreviewErrorBody::bad_request(
                    err.code(),
                    err.to_string(),
                ))
            }
        };
    // 上游 `at.Format(time.RFC3339)`：**秒精度 + `Z`**。本仓 `Timestamp::as_iso()` 是
    // `+00:00` 形式（两者指同一时刻），但这里是 `[]string` 而不是时间戳字段，逐字对齐 Go 的
    // 输出更省事（也让「整串相等」的契约测试能直接比）。
    let next_runs = occurrences
        .iter()
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "next_runs": next_runs })).into_response())
}

/// `GET /api/autopilots/usage`（上游 `GetAutopilotQuotaUsage` 33 行）。
///
/// 本仓**没有** entitlement 平面（商业默认值不可抄）⇒ 缺省形态是 `{"action":"off", …全 null}`；
/// 装了 [`mc_autopilot::quota::install_policy_provider`] 之后才有 `observe` / `enforce`。
/// gate 关掉时 `mc_autopilot::quota::quota_usage` 在**读配额表之前**返回，所以没有策略的
/// 工作区不会因为 `autopilot_quota_period` 缺行而报错。
async fn get_quota_usage(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<AutopilotQuotaUsageResponse>> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    require_member(&state, workspace_id, acting_user_id(user)).await?;
    let usage = mc_autopilot::quota::quota_usage(state.db.pool(), workspace_id)
        .await
        .map_err(|err| {
            // 上游是 500 + 固定文案；这里保留固定文案并把根因接在后面（本仓错误体带 message）。
            tracing::warn!(error = %err, "failed to load autopilot quota usage");
            Error::Internal(format!(
                "failed to load autopilot quota usage: {}",
                err.message()
            ))
        })?;
    Ok(Json(usage))
}
