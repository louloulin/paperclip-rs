//! M5-3：autopilot **trigger** 写面（create / update / delete）。
//!
//! - **写者**：M5-3（`docs/44` §3.2）。切片只实现本文件的 `router()`；凭据两条在
//!   `../credentials.rs`（同片，已按 §6.3 拆好防门 ⑩）。
//! - **路由**（`router.go` L2110–L2112）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 10 | POST | `/api/autopilots/:id/triggers` | `CreateAutopilotTrigger` | 202 (+56) |
//! | 11 | PATCH | `/api/autopilots/:id/triggers/:triggerId/`（+ 无斜杠别名） | `UpdateAutopilotTrigger` | 172 |
//! | 12 | DELETE | `/api/autopilots/:id/triggers/:triggerId/`（+ 无斜杠别名） | `DeleteAutopilotTrigger` | 74 |
//!
//! - **#11/#12 是双形态**（`Route(":triggerId") + Patch("/") / Delete("/")`）；#10 单形态。
//! - **cron 校验落在 `mc_autopilot::trigger`**（5 字段手写解析器，选型记录见该 crate 的 `lib.rs`）；
//!   本文件只做参数落库 + 调 `computeNextRun`75 算 `next_run_at`。**不要**在路由层写第二份
//!   cron 解析（`cron-preview`(#2) 与 M5-8 的 job 共用同一个解析器）。
//! - **时区**：`autopilot_trigger.timezone` 是 `TEXT NULL DEFAULT 'UTC'`（**可空** ⇒ 空值时按
//!   `UTC` 解包，语义与 `issue_wakeup.timezone` 的 NOT NULL 不同）；校验用
//!   `mc_autopilot::trigger` 的 `Timezone`（IANA）。
//! - **token 铸造**：`createWebhookTriggerWithMintedToken`56 生成 + 唯一冲突重试；
//!   路径形态由 `webhookPathForToken`4 定 ⇒ 与 M5-5 的 ingress 入口**逐字一致**
//!   （`POST /api/webhooks/autopilots/{token}`）。
//! - **provider 闭集** `{generic, github}`（`isAllowedWebhookProvider`9 归本片）。
//!
//! # M5-3 落地（`docs/51-M5-3-TRIGGER-WRITE.md`）
//!
//! ## 三条 handler 的**校验顺序就是契约**
//!
//! 上游把「哪个字段先被拒」写成了顺序（`kind` → schedule 的 `cron_expression` → webhook 的
//! `timezone` → `event_filters` → `provider` → 时区合法性 → kind 分支）。同一份非法输入在
//! 不同顺序下得到**不同的 400 文案**，所以本文件逐字照抄那个 if 序列，不做「顺手合并校验」。
//!
//! ## 三条 share 的解析链（`resolve_write_scope` / `load_bound_trigger_row`）
//!
//! 上游在**路由组中间件**里做成员门槛，本仓没有那层 ⇒ 每个 handler 显式解析：
//! `resolve_workspace_id`（无/非 UUID → 400）→ `acting_user_id` → [`require_member`]
//! （非成员 → **404** `workspace`，不是 403）→ [`load_in_workspace`]（跨工作区/不存在 →
//! **404** `autopilot`）→ [`require_write`]（成员但无写权 → **403**）。trigger 的存在性一律走
//! [`load_bound_trigger_row`]：`get_by_id` 不绑 autopilot，**必须**比对 `row.autopilot_id`，
//! 否则 `/api/autopilots/A/triggers/{B 的 trigger}` 会被当成 A 的触发器操作。
//!
//! ## 与上游的四处有意偏离（逐条理由见 `docs/51`）
//!
//! 1. **不写 `autopilot_rule_version`、不重盖 `published_by_*`**：`docs/44` §4.2 把版本写入判给
//!    M5-2（`autopilot/write.rs`），而 C 波 `M5-2 ∥ M5-3` 并行 ⇒ 本片调用不到未合入的兄弟。
//!    影响面：M5-4 派发时按 `source=trigger_owner` 归属的 `published_by_id` 在**编辑**后不更新
//!    （创建时已按上游播种）。
//! 2. **不广播 WS**（上游三条路径都 `h.publish(...)`）：与 `squads.rs` 的既有处置一致
//!    （`docs/40` §5 同款偏离），等 M3-7 的广播面。
//! 3. **唯一冲突耗尽后的 500 文案**：上游 `could not mint unique webhook token` 只在内部
//!    error 里，对外恒 `failed to create trigger`；本仓 `repo_err` 把它折成
//!    `{"code":"database_error"}`。仓储层错误一律走 [`repo_err`]（全仓写面同口径）。
//! 4. **`next_run_at` = NULL 当「永不触发」**：上游 robfig `sched.Next` 对「5 年窗口内不再触发」
//!    返回**零值时间 + nil error** ⇒ 库里落 `0001-01-01T00:00:00Z`；本片落 `NULL`（该列是纯展示，
//!    调度判定走 `cron` 模块 + DB 时间，见 `mc_autopilot::trigger::next_run_at_for` 的注释）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{patch, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use mc_autopilot::dto::{AutopilotTriggerResponse, WebhookEventFilter};
use mc_autopilot::trigger::{
    encode_webhook_event_filters, encode_webhook_event_filters_always, is_allowed_webhook_provider,
    next_run_at_for, validate_webhook_event_filters, Timezone, DEFAULT_TIMEZONE,
    TRIGGER_KIND_SCHEDULE, TRIGGER_KIND_WEBHOOK, WEBHOOK_PROVIDER_GENERIC,
};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::autopilot::trigger::{
    create_trigger, delete_trigger, generate_webhook_token, update_trigger, AutopilotTriggerRepo,
    NewTrigger, TriggerPatch, WEBHOOK_TOKEN_ATTEMPTS,
};
use mc_repos::autopilot::{AutopilotRepo, AutopilotRow, AutopilotTriggerRow};
use mc_repos::RepoError;

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, not_found, parse_uuid, repo_err};
use crate::routes::auth_user::AuthUser;
use crate::routes::autopilots::access::{
    acting_user_id, load_in_workspace, require_member, require_write,
};
use crate::routes::autopilots::dto::trigger_to_response;
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

/// trigger 写面 router（3 条路由 / 5 个注册键）。
///
/// #11/#12 的无斜杠别名**必须同方法集合**一起注册：上游 chi 的
/// `Route(":triggerId") + Patch("/") / Delete("/")` 两种形态都服务，而 axum 0.7
/// （matchit 0.7）不做归一化 —— 少一个是 404 而不是 307（门 ⑦ 的
/// `slash_alias_audit.py` 直接判 `MISSING_ALIAS`）。
///
/// 反之 `#10` 是 plain 子路由（上游只有 `POST /triggers` 一个形态）⇒ **不要**加尾斜杠别名
/// （会判 `EXTRA_ALIAS`）。
pub fn router() -> Router<Arc<AppState>> {
    // 路径参数语法是 `:id` / `:triggerId`（matchit 0.7），写成 `{id}` 会把整段当字面量、恒 404。
    Router::new()
        .route(
            "/api/autopilots/:id/triggers",
            post(create_autopilot_trigger),
        )
        .route(
            "/api/autopilots/:id/triggers/:triggerId",
            patch(update_autopilot_trigger).delete(delete_autopilot_trigger),
        )
        .route(
            "/api/autopilots/:id/triggers/:triggerId/",
            patch(update_autopilot_trigger).delete(delete_autopilot_trigger),
        )
}

// ---------------------------------------------------------------------------
// 请求体（私有形状：写集内文件自己持有，不进 `dto.rs`）
// ---------------------------------------------------------------------------

/// 上游 `CreateAutopilotTriggerRequest`（`handler/autopilot.go:390`）。
///
/// 每个指针字段在 Go 里都是「缺省 / 显式 null」不可分的三态之一，本地一律用 `Option`：
/// serde 对 `Option<T>` 的缺省与 `null` 都给 `None`，与 Go 的 `*T` 同语义。
#[derive(Debug, Default, Deserialize)]
struct CreateAutopilotTriggerRequest {
    /// `schedule` / `webhook`（`api` 已废弃 ⇒ 400）；缺省 `""` ⇒ 400 `kind is required`。
    #[serde(default)]
    kind: String,
    /// schedule 的 cron 表达式。
    #[serde(default)]
    cron_expression: Option<String>,
    /// IANA 时区名；webhook 上非空即 400。
    #[serde(default)]
    timezone: Option<String>,
    /// 展示名（**不折叠空串**：`""` 落 `''`、`null`/缺省落 NULL）。
    #[serde(default)]
    label: Option<String>,
    /// `generic` / `github`；仅 webhook 合法。
    #[serde(default)]
    provider: Option<String>,
    /// 事件过滤；仅 webhook 合法。`null` 在 Go 里等价于缺省（nil 切片）。
    #[serde(default, deserialize_with = "null_as_default")]
    event_filters: Vec<WebhookEventFilter>,
    /// **上游 create 忽略此字段**（新触发器恒 `true`）—— 保留声明只为让 `enabled` 的**类型错误**
    /// 与 Go 一样折成 400：`json.Decode` 会校验它，哪怕随后的路径没人读。
    #[allow(dead_code)]
    #[serde(default)]
    enabled: Option<bool>,
}

/// 上游 `UpdateAutopilotTriggerRequest`（`handler/autopilot.go:416`）。
///
/// `event_filters` 的三态在这里天然成立：缺省/`null` → `None`（**保留原值**）、
/// `[]` → `Some(vec![])`（**清成 `[]`**，回到「接受全部事件」）、`[...]` → 替换。
#[derive(Debug, Default, Deserialize)]
struct UpdateAutopilotTriggerRequest {
    /// 启用开关。
    #[serde(default)]
    enabled: Option<bool>,
    /// 仅 schedule 可改（非 schedule 传了就 400）。
    #[serde(default)]
    cron_expression: Option<String>,
    /// 仅 schedule 可改（非 schedule 传了就 400）；`""` 是合法值（落 `''` ⇒ 按 UTC 解包）。
    #[serde(default)]
    timezone: Option<String>,
    /// 展示名。
    #[serde(default)]
    label: Option<String>,
    /// 事件过滤三态，见类型注释。
    #[serde(default)]
    event_filters: Option<Vec<WebhookEventFilter>>,
}

/// 把 JSON `null` 当「字段缺省」（Go 的 `[]T` 收到 `null` 就是 nil 切片）。
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// 上游 `json.NewDecoder(r.Body).Decode(&req)`：空 body / 形状不符 → 400 `invalid request body`。
///
/// 两处**不能简化**的语义：
///
/// 1. 空 body（长度 0）与纯空白 → `serde_json` 解析失败 → 400（Go 的 `json.Decode` 报 EOF）；
/// 2. 字面量 `null` **不是错误** —— Go 把它解码成零值结构体 ⇒ 这里当「字段全缺」处理，
///    于是 `POST …/triggers` 的 `null` 体会走到 `kind is required`（而不是 `invalid request body`）。
pub(super) fn decode_body<T: DeserializeOwned + Default>(body: &Bytes) -> Result<T, Error> {
    let value: JsonValue =
        serde_json::from_slice(body).map_err(|_| bad_request("invalid request body"))?;
    if value.is_null() {
        return Ok(T::default());
    }
    if !value.is_object() {
        return Err(bad_request("invalid request body"));
    }
    serde_json::from_value(value).map_err(|_| bad_request("invalid request body"))
}

// ---------------------------------------------------------------------------
// 三条路由共用的解析链（凭据两条在 `../credentials.rs` 里复用同一份）
// ---------------------------------------------------------------------------

/// 写面的解析结果：**已验证**的工作区成员 + 有写权的 autopilot 行。
///
/// 四条判定（成员门槛 / 加载 / 写权）在上游是「中间件 + `requireAutopilotWrite`」两段，
/// 本地合成一条链，避免三个 handler 各写一遍权限顺序。
pub(super) struct WriteScope {
    /// acting user（本仓恒为已认证用户，见 `access.rs` 的偏离说明）。
    pub(super) user_id: Id,
    /// 目标 autopilot（`id` 用于 trigger 绑定比对，`workspace_id` 已在 `load_in_workspace` 里校验过）。
    pub(super) autopilot: AutopilotRow,
}

/// 上游 `loadAutopilotInWorkspace` + `requireAutopilotWrite` 的合并入口。
///
/// 判负顺序**逐字**对照上游，别调换（同一份坏输入下 400/403/404 的优先级是契约）：
/// 非 UUID workspace / 缺头 → 400；非成员 → 404 `workspace`；非 UUID autopilot → 400；
/// 跨工作区/不存在 → 404 `autopilot`；成员但无写权 → 403。
pub(super) async fn resolve_write_scope(
    state: &AppState,
    user: AuthUser,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    raw_autopilot_id: &str,
) -> Result<WriteScope, Error> {
    let workspace_id = resolve_workspace_id(headers, query)?;
    let user_id = acting_user_id(user);
    let role = require_member(state, workspace_id, user_id).await?;
    let repo = AutopilotRepo::new(state.db.clone());
    let autopilot = load_in_workspace(&repo, raw_autopilot_id, workspace_id).await?;
    require_write(&repo, &autopilot, &role, user_id).await?;
    Ok(WriteScope { user_id, autopilot })
}

/// 加载**绑定到本 autopilot** 的触发器（上游 `GetAutopilotTrigger` + `prev.AutopilotID != ap.ID`）。
///
/// `get_by_id` 不绑 autopilot（那是 M5-4 的 `GetAutopilotTriggerForAutopilot`），所以这里必须
/// 自己比对：跨自动机的 trigger id 与「不存在」**不可区分**（都 404 `trigger`），否则就是一个
/// 「这个 trigger id 属于谁」的探测器。
///
/// # Errors
///
/// 非 UUID → 400；不存在 / 不属于该 autopilot → 404 `trigger`；DB 故障 → 500。
pub(super) async fn load_bound_trigger_row(
    state: &AppState,
    autopilot_id: Uuid,
    trigger_id: Uuid,
) -> Result<AutopilotTriggerRow, Error> {
    let repo = AutopilotTriggerRepo::new(state.db.clone());
    match repo.get_by_id(trigger_id).await {
        Ok(row) if row.autopilot_id == autopilot_id => Ok(row),
        Ok(_) | Err(RepoError::NotFound) => Err(not_found("trigger")),
        Err(err) => Err(repo_err(err, "trigger")),
    }
}

/// 事务提交失败的统一折法：`sqlx::Error` → 500 `database_error`（与 `repo_err` 同码）。
///
/// 独立函数而不是 `map_err(|e| Error::Database(e.to_string()))` 闭包：三个 handler 共用一份，
/// 且不必给 `ApiError` 加一个散落在本文件的 inherent impl。
// `needless_pass_by_value`：签名要当 `map_err` 的函数指针用（`FnOnce(sqlx::Error) -> Error`），
// 收引用就得在每个调用点多套一层闭包，反而更绕。
#[allow(clippy::needless_pass_by_value)]
fn commit_err(err: sqlx::Error) -> Error {
    Error::Database(err.to_string())
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `POST /api/autopilots/:id/triggers`（上游 `CreateAutopilotTrigger`，**201**）。
///
/// 上游 span 202 行，其中大半是「先后顺序即契约」的校验序列 ⇒ 本函数刻意不拆：
/// 拆成 `validate_*` 多段后，「哪个字段先被拒」会散落在调用顺序里，反而更容易抄错。
// 上游 202 行；校验顺序是契约（见模块文档），拆开会让顺序不可见。
#[allow(clippy::too_many_lines)]
async fn create_autopilot_trigger(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(autopilot_id): Path<String>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<AutopilotTriggerResponse>)> {
    let scope = resolve_write_scope(&state, user, &headers, &query, &autopilot_id).await?;
    let req = decode_body::<CreateAutopilotTriggerRequest>(&body)?;

    if req.kind.is_empty() {
        return Err(bad_request("kind is required").into());
    }
    if req.kind != TRIGGER_KIND_SCHEDULE && req.kind != TRIGGER_KIND_WEBHOOK {
        // "api" 是废弃 kind：既无调度器也无入口面，唯一的触发方式是手写 /trigger。
        // 上游对存量调用者给 400 而不是静默降级。
        return Err(bad_request("kind must be schedule or webhook").into());
    }
    let has_cron = matches!(req.cron_expression.as_deref(), Some(raw) if !raw.is_empty());
    if req.kind == TRIGGER_KIND_SCHEDULE && !has_cron {
        return Err(bad_request("cron_expression is required for schedule triggers").into());
    }
    let has_timezone = matches!(req.timezone.as_deref(), Some(raw) if !raw.is_empty());
    if req.kind == TRIGGER_KIND_WEBHOOK && has_timezone {
        // webhook 由外部 POST 按需触发，没有 next_run_at 要算 ⇒ 时区无意义。
        // 上游选择大声拒绝，而不是静默丢掉字段。
        return Err(bad_request("timezone is not valid for webhook triggers").into());
    }
    if req.kind != TRIGGER_KIND_WEBHOOK && !req.event_filters.is_empty() {
        return Err(bad_request("event_filters is only valid for webhook triggers").into());
    }
    if let Err(err) = validate_webhook_event_filters(&req.event_filters) {
        return Err(bad_request(err.to_string()).into());
    }
    // provider 只对 webhook 有意义，且取值是闭集：写入期拒绝未知值，免得拼错后悄悄退化成
    // generic，绕过 provider 专属的去重 / 签名行为。
    let provider = req.provider.as_deref().filter(|raw| !raw.is_empty());
    let provider_value = match provider {
        Some(raw) => {
            if req.kind != TRIGGER_KIND_WEBHOOK {
                return Err(bad_request("provider is only valid for webhook triggers").into());
            }
            if !is_allowed_webhook_provider(raw) {
                return Err(bad_request("provider must be generic or github").into());
            }
            raw
        }
        None => WEBHOOK_PROVIDER_GENERIC,
    };
    if has_timezone {
        // 时区合法性放在 provider 之后（上游顺序）：文案是 `invalid timezone "…"`。
        let raw = req.timezone.as_deref().unwrap_or_default();
        Timezone::parse(raw).map_err(|err| bad_request(err.to_string()))?;
    }

    let repo = AutopilotTriggerRepo::new(state.db.clone());
    // 先前的校验已经把 kind 收敛到 `schedule | webhook` 两个值 ⇒ 这里只需二分。
    let row = if req.kind == TRIGGER_KIND_SCHEDULE {
        let cron_expr = req.cron_expression.as_deref().unwrap_or_default();
        // 上游 `ptrToText(req.Timezone)`：**缺省是 `""` 而不是 NULL** ⇒ schedule 分支永远写一个
        // 非 NULL 的字符串。这个「`''` vs NULL」在 wire 上**可见**：schedule 创建后 `timezone`
        // 回 `""`，而 webhook 分支（不绑该列）回 `null`。读取侧两者都按 UTC 解包
        // （`Timezone::from_column`）⇒ 行为等价，只有响应形状不同。
        let tz_text = req.timezone.as_deref().unwrap_or_default();
        let timezone =
            Timezone::from_column(Some(tz_text)).map_err(|err| bad_request(err.to_string()))?;
        let next_run_at =
            next_run_at_for(cron_expr, &timezone).map_err(|err| bad_request(err.to_string()))?;
        let new = NewTrigger {
            autopilot_id: scope.autopilot.id,
            kind: TRIGGER_KIND_SCHEDULE,
            // 新触发器恒启用（上游 `Enabled: true`，请求体里的 enabled 被忽略）。
            enabled: true,
            cron_expression: Some(cron_expr),
            timezone: Some(tz_text),
            next_run_at,
            webhook_token: None,
            label: req.label.as_deref(),
            // schedule 不绑 provider（SQL 的 COALESCE 落 `generic`）—— 与上游一致。
            provider: None,
            event_filters: None,
            actor_id: scope.user_id.0,
        };
        let mut tx = repo.begin().await.map_err(|err| repo_err(err, "trigger"))?;
        match create_trigger(&mut tx, &new).await {
            Ok(row) => {
                tx.commit().await.map_err(commit_err)?;
                row
            }
            Err(err) => {
                let _ = tx.rollback().await;
                return Err(repo_err(err, "trigger").into());
            }
        }
    } else {
        // webhook：先铸 token 再 INSERT（不留 kind=webhook + token=NULL 的半行）；
        // 唯一索引冲突就换 token 重试 —— 每次尝试**自己一个事务**（上游逐字如此，
        // 因为冲突时那次尝试的 tx 已经回滚）。
        let filters = encode_webhook_event_filters(&req.event_filters);
        let mut created = None;
        for _ in 0..WEBHOOK_TOKEN_ATTEMPTS {
            let token = generate_webhook_token();
            let new = NewTrigger {
                autopilot_id: scope.autopilot.id,
                kind: TRIGGER_KIND_WEBHOOK,
                enabled: true,
                cron_expression: None,
                // webhook 不落时区（上游该分支不绑 `timezone` ⇒ NULL）。
                timezone: None,
                next_run_at: None,
                webhook_token: Some(&token),
                label: req.label.as_deref(),
                provider: Some(provider_value),
                event_filters: filters.clone(),
                actor_id: scope.user_id.0,
            };
            let mut tx = repo.begin().await.map_err(|err| repo_err(err, "trigger"))?;
            match create_trigger(&mut tx, &new).await {
                Ok(row) => {
                    tx.commit().await.map_err(commit_err)?;
                    created = Some(row);
                    break;
                }
                Err(RepoError::Conflict) => {
                    let _ = tx.rollback().await;
                }
                Err(err) => {
                    let _ = tx.rollback().await;
                    return Err(repo_err(err, "trigger").into());
                }
            }
        }
        created.ok_or_else(|| {
                Error::Internal(format!(
                    "failed to create trigger: could not mint a unique webhook token in {WEBHOOK_TOKEN_ATTEMPTS} attempts"
                ))
            })?
    };
    Ok((StatusCode::CREATED, Json(trigger_to_response(&row))))
}

/// `PATCH /api/autopilots/:id/triggers/:triggerId[/]`（上游 `UpdateAutopilotTrigger`，**200**）。
///
/// 三处**容易写歪**的地方：
///
/// 1. `next_run_at` 在 SQL 里是**直赋值**（没有 COALESCE）⇒ `TriggerPatch` 必须先播种**上一行的值**，
///    只有「schedule 且 cron 非空」时才重算；否则一次普通 PATCH 会把它抹成 NULL。
/// 2. `event_filters` 的清除路径要用 `encode_webhook_event_filters_always`（传 `[]` 而不是 NULL，
///    因为 SQL 那格是 `COALESCE(narg, event_filters)` —— NULL 是「保留原值」）。
/// 3. 非 schedule 传 `cron_expression` / `timezone` 要**先**拒（400），再谈其余字段。
// 上游 172 行；三态补丁的语义密度高，拆开看不出「哪格 COALESCE、哪格直赋值」。
#[allow(clippy::too_many_lines)]
async fn update_autopilot_trigger(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((autopilot_id, trigger_id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<AutopilotTriggerResponse>> {
    let scope = resolve_write_scope(&state, user, &headers, &query, &autopilot_id).await?;
    let trigger_uuid = parse_uuid(&trigger_id, "trigger id")?;
    let prev = load_bound_trigger_row(&state, scope.autopilot.id, trigger_uuid).await?;
    let req = decode_body::<UpdateAutopilotTriggerRequest>(&body)?;

    if prev.kind != TRIGGER_KIND_SCHEDULE {
        if req.cron_expression.is_some() {
            return Err(bad_request("cron_expression is only valid for schedule triggers").into());
        }
        if req.timezone.is_some() {
            return Err(bad_request("timezone is only valid for schedule triggers").into());
        }
    }

    let mut patch = TriggerPatch {
        id: prev.id,
        enabled: None,
        cron_expression: None,
        timezone: None,
        // 直赋值列：先播种上一行的值（上游 `params.NextRunAt = prev.NextRunAt`）。
        next_run_at: prev.next_run_at,
        label: None,
        event_filters: None,
    };
    if let Some(enabled) = req.enabled {
        patch.enabled = Some(enabled);
    }
    if let Some(cron_expr) = req.cron_expression.as_deref() {
        patch.cron_expression = Some(cron_expr);
    }
    if let Some(raw_tz) = req.timezone.as_deref() {
        if !raw_tz.is_empty() {
            Timezone::parse(raw_tz).map_err(|err| bad_request(err.to_string()))?;
        }
        // `""` 是合法值（落 `''` ⇒ 读面按 UTC 解包），别顺手折成 NULL。
        patch.timezone = Some(raw_tz);
    }
    if let Some(label) = req.label.as_deref() {
        patch.label = Some(label);
    }
    if let Some(filters) = req.event_filters.as_deref() {
        if prev.kind != TRIGGER_KIND_WEBHOOK {
            return Err(bad_request("event_filters is only valid for webhook triggers").into());
        }
        validate_webhook_event_filters(filters).map_err(|err| bad_request(err.to_string()))?;
        patch.event_filters = Some(encode_webhook_event_filters_always(filters));
    }

    // 重算 `next_run_at`：cron 取「请求里给的，否则上一行的」，时区同理（空 ⇒ UTC 兜底）。
    let mut cron_expr = prev.cron_expression.as_deref().unwrap_or_default();
    if let Some(given) = req.cron_expression.as_deref() {
        cron_expr = given;
    }
    let mut tz_name = prev.timezone.as_deref().unwrap_or(DEFAULT_TIMEZONE);
    if let Some(given) = req.timezone.as_deref() {
        tz_name = given;
    }
    if prev.kind == TRIGGER_KIND_SCHEDULE && !cron_expr.is_empty() {
        let timezone = Timezone::parse(tz_name).map_err(|err| bad_request(err.to_string()))?;
        patch.next_run_at =
            next_run_at_for(cron_expr, &timezone).map_err(|err| bad_request(err.to_string()))?;
    }

    let repo = AutopilotTriggerRepo::new(state.db.clone());
    let mut tx = repo.begin().await.map_err(|err| repo_err(err, "trigger"))?;
    let row = match update_trigger(&mut tx, &patch).await {
        Ok(row) => row,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(repo_err(err, "trigger").into());
        }
    };
    // 上游在这里把 `autopilot_rule_version` 与「实质变更时重盖 published_by_*」写进同一事务；
    // 本片不实现（`docs/44` §4.2 判给 M5-2，C 波并行 ⇒ 调用不到未合入的兄弟）。见模块文档第 1 条。
    tx.commit().await.map_err(commit_err)?;
    Ok(Json(trigger_to_response(&row)))
}

/// `DELETE /api/autopilots/:id/triggers/:triggerId[/]`（上游 `DeleteAutopilotTrigger`，**204**）。
///
/// 上游的解析顺序是：autopilot id / trigger id / workspace id 三个 400，然后
/// 404 `autopilot not found` → 403 → 404 `trigger not found`。本仓的 workspace 解析与成员门槛
/// 合并进了 [`resolve_write_scope`]（M5-1 的共享链），所以 trigger id 解析落在它之后 —— 与
/// 「成员门槛先行」的上游中间件语义一致，同时保证「autopilot id 的 400 早于 trigger id 的 400」。
async fn delete_autopilot_trigger(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((autopilot_id, trigger_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let scope = resolve_write_scope(&state, user, &headers, &query, &autopilot_id).await?;
    let trigger_uuid = parse_uuid(&trigger_id, "trigger id")?;
    let prev = load_bound_trigger_row(&state, scope.autopilot.id, trigger_uuid).await?;

    let repo = AutopilotTriggerRepo::new(state.db.clone());
    let mut tx = repo.begin().await.map_err(|err| repo_err(err, "trigger"))?;
    if let Err(err) = delete_trigger(&mut tx, prev.id).await {
        let _ = tx.rollback().await;
        return Err(repo_err(err, "trigger").into());
    }
    // 同 update：上游的 `recordAutopilotRuleVersion` 在本片留缺口（模块文档第 1 条）。
    tx.commit().await.map_err(commit_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_body_is_not_a_decode_error() {
        // Go 的 `json.Decode` 把字面量 `null` 解成零值结构体 ⇒ 走到「kind is required」，
        // 而不是 `invalid request body`。这条钉住 `decode_body` 的那个特判。
        let req = decode_body::<CreateAutopilotTriggerRequest>(&Bytes::from_static(b"null"))
            .expect("null body");
        assert_eq!(req.kind, "");
        assert!(req.event_filters.is_empty());
    }

    #[test]
    fn empty_and_blank_bodies_are_rejected() {
        for raw in [&b""[..], &b"   \n "[..]] {
            let err = decode_body::<CreateAutopilotTriggerRequest>(&Bytes::copy_from_slice(raw))
                .expect_err("empty body must fail");
            assert!(matches!(err, Error::Validation { .. }), "{err:?}");
        }
    }

    #[test]
    fn non_object_bodies_are_rejected() {
        for raw in [&b"[]"[..], &b"\"x\""[..], &b"3"[..]] {
            let err = decode_body::<CreateAutopilotTriggerRequest>(&Bytes::copy_from_slice(raw))
                .expect_err("non-object body must fail");
            assert!(matches!(err, Error::Validation { .. }), "{err:?}");
        }
    }

    #[test]
    fn create_event_filters_accepts_null_as_absent() {
        // Go 的 `[]T` 字段收到 `null` 就是 nil 切片（不是解码错误）。
        let req = decode_body::<CreateAutopilotTriggerRequest>(&Bytes::from_static(
            br#"{"kind":"webhook","event_filters":null}"#,
        ))
        .expect("null event_filters");
        assert!(req.event_filters.is_empty());
    }

    #[test]
    fn update_event_filters_keeps_the_three_states_apart() {
        let absent = decode_body::<UpdateAutopilotTriggerRequest>(&Bytes::from_static(b"{}"))
            .expect("absent");
        assert!(absent.event_filters.is_none(), "缺省 ⇒ 保留原值");
        let explicit_null = decode_body::<UpdateAutopilotTriggerRequest>(&Bytes::from_static(
            br#"{"event_filters":null}"#,
        ))
        .expect("explicit null");
        assert!(explicit_null.event_filters.is_none(), "null ⇒ 保留原值");
        let cleared = decode_body::<UpdateAutopilotTriggerRequest>(&Bytes::from_static(
            br#"{"event_filters":[]}"#,
        ))
        .expect("empty array");
        assert_eq!(cleared.event_filters.as_deref(), Some(&[][..]), "[] ⇒ 清空");
    }

    /// matchit 0.7 在**构建期**就 panic（路径冲突）⇒「能构建」是一条真断言：
    /// `:triggerId` 的双形态（#11/#12）必须与它下面的两个字面量子路由（#13/#14）共存，
    /// 且两个子 router 在聚合点（`super::super::router()`）合并不撞车。
    #[test]
    fn trigger_routes_coexist_with_the_sibling_subrouters() {
        let _ = super::super::router();
    }

    #[test]
    fn provider_whitelist_is_closed() {
        assert!(is_allowed_webhook_provider("generic"));
        assert!(is_allowed_webhook_provider("github"));
        assert!(!is_allowed_webhook_provider("gitlab"));
        assert!(!is_allowed_webhook_provider(""));
    }
}
