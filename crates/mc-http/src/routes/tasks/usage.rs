//! `client-usage` / `issue-usage` / task messages（上游 `client_usage.go:49`、
//! `daemon.go:5792`、`daemon.go:5724`）。
//!
//! `POST /api/client-usage` 是**唯一**不要求 workspace 的路由：上游
//! `resolveWorkspaceID` 取不到值时记录 `workspace_id = NULL`，只有显式给了
//! workspace 才校验成员身份（`client_usage.go:98`）。因此本 handler 不走
//! [`super::TaskScope`]，而是自己按「有就给、没有就算」的口径解析。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::Utc;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::task::{ClientUsageUpsert, IssueUsageSummaryRow, TaskMessageRow};
use serde_json::{Map, Value};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;

use super::dto::{ClientUsageRequest, ClientUsageRuntimeProbe, IssueUsageDto, TaskMessageDto};
use super::{bad_request, non_empty_query, parse_uuid, repo_err, TaskScope};

/// 请求体上限（上游 `clientUsageBodyLimit = 16 * 1024`，`client_usage.go:20`）。
const CLIENT_USAGE_BODY_LIMIT: usize = 16 * 1024;

/// 客户端平台 / 版本 / OS 的来源 header（上游 `middleware/client.go:30-34`）。
const CLIENT_PLATFORM_HEADER: &str = "x-client-platform";
const CLIENT_VERSION_HEADER: &str = "x-client-version";
const CLIENT_OS_HEADER: &str = "x-client-os";

// ---------------------------------------------------------------------------
// POST /api/client-usage
// ---------------------------------------------------------------------------

/// `POST /api/client-usage`（上游 `UpsertClientUsage`）→ **204**。
///
/// 上游的 `RequireHumanActor` 中间件在本仓天然满足（`AuthUser` 只接受成员身份）。
pub(crate) async fn upsert_client_usage(
    State(state): State<Arc<crate::state::AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<StatusCode> {
    // `http.MaxBytesReader` 超限会让 `Decode` 失败 ⇒ 上游同样回 "invalid request body"。
    if body.len() > CLIENT_USAGE_BODY_LIMIT {
        return Err(bad_request("invalid request body"));
    }
    // `DisallowUnknownFields` + 双次 Decode 的 EOF 检查，等价于一次严格解析
    // （`serde_json` 对尾部多余内容直接报错）。
    let req: ClientUsageRequest =
        serde_json::from_slice(&body).map_err(|_| bad_request("invalid request body"))?;
    let install_id = parse_uuid(&req.install_id, "install_id")?;

    let client_type = header_str(&headers, CLIENT_PLATFORM_HEADER)
        .trim()
        .to_ascii_lowercase();
    if client_type != "web" && client_type != "desktop" {
        return Err(bad_request("client platform must be web or desktop"));
    }
    let mut client_version = header_str(&headers, CLIENT_VERSION_HEADER)
        .trim()
        .to_owned();
    if client_version.is_empty() {
        "unknown".clone_into(&mut client_version);
    }
    if !valid_client_version(&client_version) {
        return Err(bad_request("invalid client version"));
    }
    let client_os = normalize_client_usage_os(&header_str(&headers, CLIENT_OS_HEADER));

    let probe = match req.runtime {
        None => None,
        Some(probe) => {
            if client_type != "desktop" {
                return Err(bad_request("runtime data is only accepted from desktop"));
            }
            Some(validate_runtime_probe(&probe)?)
        }
    };

    // workspace 可选：上游 `resolveWorkspaceID` 返回 "" 时整段跳过校验。
    // 显式给了但非法 → 400 `invalid workspace id`；给了但不是成员 → **403**
    // `workspace not found`（上游 `client_usage.go:107/119` 用的就是这个 403）。
    let workspace_id = client_usage_workspace(&headers, &query)?;
    if let Some(workspace_id) = workspace_id {
        let member: Option<(String,)> =
            sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
                .bind(workspace_id.0)
                .bind(user.id().0)
                .fetch_optional(state.db.pool())
                .await
                .map_err(|e| Error::Database(e.to_string()))?;
        if member.is_none() {
            return Err(Error::Forbidden {
                message: "workspace not found".to_owned(),
            }
            .into());
        }
    }

    let payload = ClientUsageUpsert {
        user_id: user.id(),
        client_type,
        install_id,
        workspace_id,
        client_version,
        os: client_os,
        // `has_runtime_probe` 由 `runtime_probed_at.is_some()` 推导（见仓储层文档）：
        // 「本次上报带探针」与「本次探针结果」必须同一个布尔位，否则
        // `probe_result = 'error'` 会被 SQL 判成「无探针」而保留旧值。
        runtime_probed_at: probe.as_ref().map(|_| Utc::now()),
        probe_result: probe.as_ref().map(|p| p.result.clone()),
        runtime_count: probe.as_ref().and_then(|p| p.runtime_count),
        provider_summary: probe.as_ref().and_then(|p| p.provider_summary.clone()),
        online_count: probe.as_ref().and_then(|p| p.online_count),
        offline_count: probe.as_ref().and_then(|p| p.offline_count),
    };
    mc_repos::task::TaskRepo::new(&state.db)
        .upsert_client_usage(&payload)
        .await
        .map_err(|e| repo_err(e, "client usage"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// 通过校验的探针（上游 `validatedRuntimeProbe`，`client_usage.go:36`）。
///
/// 注意 `probe_result == "error"` 时**四个计数必须缺席**，而 SQL 只看
/// `has_runtime_probe`：因此 `runtime_probed_at` 一律由「有没有 `runtime` 字段」
/// 决定，不随探针成败变化。
struct ValidatedProbe {
    result: String,
    runtime_count: Option<i32>,
    provider_summary: Option<Value>,
    online_count: Option<i32>,
    offline_count: Option<i32>,
}

fn validate_runtime_probe(probe: &ClientUsageRuntimeProbe) -> ApiResult<ValidatedProbe> {
    let result = probe
        .probe_result
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if result != "success" && result != "error" {
        return Err(bad_request("runtime probe_result must be success or error"));
    }
    if result == "error" {
        if probe.runtime_count.is_some()
            || probe.provider_summary.is_some()
            || probe.online_count.is_some()
            || probe.offline_count.is_some()
        {
            return Err(bad_request("failed runtime probes must not include counts"));
        }
        return Ok(ValidatedProbe {
            result,
            runtime_count: None,
            provider_summary: None,
            online_count: None,
            offline_count: None,
        });
    }

    let (Some(runtime_count), Some(provider_summary), Some(online_count), Some(offline_count)) = (
        probe.runtime_count,
        probe.provider_summary.as_ref(),
        probe.online_count,
        probe.offline_count,
    ) else {
        return Err(bad_request("successful runtime probes require all counts"));
    };
    if !(0..=1000).contains(&runtime_count)
        || online_count < 0
        || offline_count < 0
        || online_count + offline_count != runtime_count
    {
        return Err(bad_request("invalid runtime counts"));
    }
    if provider_summary.len() > 32 {
        return Err(bad_request("too many runtime providers"));
    }
    let mut total: i64 = 0;
    let mut summary = Map::new();
    for (provider, count) in provider_summary {
        if !valid_provider_name(provider) || !(0..=1000).contains(count) {
            return Err(bad_request("invalid runtime provider summary"));
        }
        total += i64::from(*count);
        summary.insert(provider.clone(), Value::from(*count));
    }
    if total != i64::from(runtime_count) {
        return Err(bad_request(
            "runtime provider counts do not match runtime_count",
        ));
    }

    Ok(ValidatedProbe {
        result,
        runtime_count: Some(runtime_count),
        provider_summary: Some(Value::Object(summary)),
        online_count: Some(online_count),
        offline_count: Some(offline_count),
    })
}

/// 上游 `providerNamePattern = ^[a-z0-9][a-z0-9_-]{0,63}$`。
///
/// 手写而不引 `regex`：mc-http 没有该依赖，且这条规则用字节判定即可
/// （模式里没有多字节字符类）。
fn valid_provider_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    rest.len() <= 63
        && rest
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// 上游 `clientVersionPattern = ^[\x20-\x7e]{1,64}$`（可打印 ASCII，1..=64）。
fn valid_client_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    !bytes.is_empty() && bytes.len() <= 64 && bytes.iter().all(|b| (0x20..=0x7e).contains(b))
}

/// 上游 `normalizeClientUsageOS`（`client_usage.go:176`）：白名单外的值一律 `unknown`。
fn normalize_client_usage_os(value: &str) -> String {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "macos" | "windows" | "linux" | "ios" | "android" | "chromeos" => value,
        _ => "unknown".to_owned(),
    }
}

/// 从 `x-workspace-id` / `?workspace_id` 取 workspace；**都没有** ⇒ `None`。
///
/// 与 [`crate::routes::inbox::resolve_workspace_id`] 的差别只有一处：那条对缺失
/// 返回 400，本条按上游把缺失当作「不记录 workspace」。
fn client_usage_workspace(
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> ApiResult<Option<Id>> {
    let raw = header_str(headers, crate::routes::issues::WORKSPACE_ID_HEADER);
    let raw = raw.trim();
    let raw = if raw.is_empty() {
        non_empty_query(query, "workspace_id")
    } else {
        Some(raw.to_owned())
    };
    match raw {
        None => Ok(None),
        Some(raw) => Id::parse(&raw)
            .map(Some)
            .map_err(|_| bad_request("invalid workspace id")),
    }
}

fn header_str(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

// ---------------------------------------------------------------------------
// GET /api/issues/:id/usage
// ---------------------------------------------------------------------------

/// `GET /api/issues/:id/usage`（上游 `GetIssueUsage`）→ 恒一行聚合。
pub(crate) async fn issue_usage(
    State(state): State<Arc<crate::state::AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> ApiResult<Json<IssueUsageDto>> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let issue = scope.issue(&id).await?;
    let row = scope
        .repo
        .issue_usage_summary(Id::from(issue.id))
        .await
        .map_err(|e| repo_err(e, "issue usage"))?;
    Ok(Json(IssueUsageDto::from_row(&row)))
}

impl IssueUsageDto {
    fn from_row(row: &IssueUsageSummaryRow) -> Self {
        Self {
            total_input_tokens: row.total_input_tokens,
            total_output_tokens: row.total_output_tokens,
            total_cache_read_tokens: row.total_cache_read_tokens,
            total_cache_write_tokens: row.total_cache_write_tokens,
            cost_usd_ticks: row.total_cost_usd_ticks,
            uncosted_input_tokens: row.uncosted_input_tokens,
            uncosted_output_tokens: row.uncosted_output_tokens,
            uncosted_cache_read_tokens: row.uncosted_cache_read_tokens,
            uncosted_cache_write_tokens: row.uncosted_cache_write_tokens,
            task_count: row.task_count,
            terminal_task_count: row.terminal_task_count,
            metered_task_count: row.metered_task_count,
            unreported_task_count: row.unreported_task_count,
        }
    }
}

// ---------------------------------------------------------------------------
// GET /api/tasks/:task_id/messages
// ---------------------------------------------------------------------------

/// `GET /api/tasks/:task_id/messages`（上游 `ListTaskMessagesByUser`）。
///
/// `?since=` 只接受整数（`strconv.Atoi` 语义）——不是数字就 400，不做「忽略」。
pub(crate) async fn list_task_messages(
    State(state): State<Arc<crate::state::AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(task_id): Path<String>,
) -> ApiResult<Json<Vec<TaskMessageDto>>> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let task = scope.task_in_workspace(&task_id).await?;

    let since_seq = match non_empty_query(&query, "since") {
        None => None,
        Some(raw) => Some(
            raw.parse::<i32>()
                .map_err(|_| bad_request("invalid since parameter"))?,
        ),
    };

    let rows = scope
        .repo
        .list_task_messages(Id::from(task.id), since_seq)
        .await
        .map_err(|e| repo_err(e, "task messages"))?;
    let issue_id = task.issue_id.map(|v| v.to_string());
    Ok(Json(
        rows.iter()
            .map(|row| TaskMessageDto::from_row(row, issue_id.as_deref()))
            .collect(),
    ))
}

impl TaskMessageDto {
    fn from_row(row: &TaskMessageRow, issue_id: Option<&str>) -> Self {
        Self {
            task_id: row.task_id.to_string(),
            issue_id: issue_id.map(str::to_owned),
            seq: row.seq,
            r#type: row.r#type.clone(),
            tool: non_empty(row.tool.as_deref()),
            call_id: non_empty(row.call_id.as_deref()),
            content: non_empty(row.content.as_deref()),
            // 上游把 `input` 解成 `map[string]any`：非对象（数组 / 标量 / `null`）
            // 都让解组失败并留下 nil map ⇒ 省略。
            input: row.input.clone().filter(Value::is_object),
            // `output` 上游不做 JSON 解析，原样输出字符串。
            output: non_empty(row.output.as_deref()),
            output_truncated: row.output_truncated,
            created_at: super::dto::ts(row.created_at),
        }
    }
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.is_empty()).map(str::to_owned)
}

/// 纯函数单测：探针 / provider 名 / 版本 / OS 归一化的边界。
///
/// 被测函数回 `ApiResult`，而 [`ApiError`] 没有 `Debug`（`error.rs` 未 derive）⇒
/// 用下面两个 helper 把错误降级成 `String` 再断言，避免为一个测试去改跨切片的 `error.rs`。
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn ok<T>(result: ApiResult<T>) -> T {
        match result {
            Ok(value) => value,
            Err(e) => panic!("expected Ok, got {}", e.0),
        }
    }

    fn err<T>(result: ApiResult<T>) -> String {
        match result {
            Ok(_) => panic!("expected Err"),
            Err(e) => e.0.to_string(),
        }
    }

    #[test]
    fn provider_name_follows_upstream_pattern() {
        assert!(valid_provider_name("claude"));
        assert!(valid_provider_name("a"));
        assert!(valid_provider_name("codex-cli_2"));
        assert!(!valid_provider_name(""));
        assert!(!valid_provider_name("-lead"));
        assert!(!valid_provider_name("Upper"));
        assert!(!valid_provider_name(&"a".repeat(65)));
        assert!(valid_provider_name(&"a".repeat(64)));
    }

    #[test]
    fn client_version_is_printable_ascii_up_to_64_bytes() {
        assert!(valid_client_version("1.2.3"));
        assert!(!valid_client_version(""));
        assert!(!valid_client_version("bad\n"));
        assert!(!valid_client_version("版本"));
        assert!(!valid_client_version(&"a".repeat(65)));
    }

    #[test]
    fn os_normalization_whitelists_only_known_platforms() {
        assert_eq!(normalize_client_usage_os(" MacOS "), "macos");
        assert_eq!(normalize_client_usage_os("linux"), "linux");
        assert_eq!(normalize_client_usage_os("plan9"), "unknown");
        assert_eq!(normalize_client_usage_os(""), "unknown");
    }

    #[test]
    fn failed_probe_must_omit_counts() {
        let probe = ClientUsageRuntimeProbe {
            probe_result: Some("error".to_owned()),
            ..ClientUsageRuntimeProbe::default()
        };
        let validated = ok(validate_runtime_probe(&probe));
        assert_eq!(validated.result, "error");
        assert!(validated.runtime_count.is_none());
    }

    #[test]
    fn success_probe_requires_matching_provider_total() {
        let mut probe = ClientUsageRuntimeProbe {
            probe_result: Some("SUCCESS".to_owned()),
            runtime_count: Some(3),
            online_count: Some(3),
            offline_count: Some(0),
            ..ClientUsageRuntimeProbe::default()
        };
        assert!(
            !err(validate_runtime_probe(&probe)).is_empty(),
            "missing summary"
        );
        probe.provider_summary = Some(
            [("claude".to_owned(), 1)]
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>(),
        );
        assert!(
            !err(validate_runtime_probe(&probe)).is_empty(),
            "provider total 1 != runtime_count 3"
        );
        probe.provider_summary = Some(
            [("claude".to_owned(), 3)]
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>(),
        );
        let validated = ok(validate_runtime_probe(&probe));
        assert_eq!(validated.result, "success");
        assert_eq!(validated.online_count, Some(3));
    }

    #[test]
    fn client_usage_workspace_is_optional_but_validated_when_present() {
        let empty = HeaderMap::new();
        let query = HashMap::new();
        assert!(ok(client_usage_workspace(&empty, &query)).is_none());

        let mut query = HashMap::new();
        query.insert("workspace_id".to_owned(), "not-a-uuid".to_owned());
        assert!(!err(client_usage_workspace(&empty, &query)).is_empty());

        let mut headers = HeaderMap::new();
        let ws = Uuid::now_v7();
        headers.insert(
            crate::routes::issues::WORKSPACE_ID_HEADER,
            ws.to_string().parse().expect("header value"),
        );
        assert_eq!(
            ok(client_usage_workspace(&headers, &HashMap::new())).map(|id| id.0),
            Some(ws)
        );
    }
}
