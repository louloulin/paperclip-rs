//! `POST /api/webhooks/vcs/:connectionId`（`router.go:1500`，**公开块**）—— 写者 **M8-2**（`LUM-1799`）。
//!
//! 上游对应物是 `internal/handler/vcs_webhook.go`（335 行）。
//!
//! # 凭据与选路（`docs/61` §1.6）
//!
//! 路径里的 `connectionId` 同时决定 **workspace** / **provider** / **解密密钥**：
//! 取行 → 从 registry 取 provider → 用 `secretbox` 解出该连接的 webhook secret →
//! 按该 provider 的签名方案验签。三条方案：
//! Forgejo/Gitea = `X-Gitea-Signature` HMAC-SHA256、GitLab = `X-Gitlab-Token` **明文常量时间比较**
//! （`docs/61` §2.7 第 3 条）。
//!
//! # 失败语义（逐条对齐上游，**不统一**）
//!
//! | 情形 | 状态码 | 文案 |
//! | --- | :-: | --- |
//! | 产品边界关 / 缺 `MULTICA_VCS_SECRET_KEY` | **404** | `unknown connection` |
//! | `connectionId` 不是 UUID | 400 | `connection id must be a valid uuid` |
//! | body 读失败 / 超限 | 400 / 413 | `read body failed` |
//! | 连接不存在（或库错） | **404** | `unknown connection` |
//! | 连接上的 provider 不认识 | **500** | `unknown provider` |
//! | 解封 webhook secret 失败 | 500 | `secret error` |
//! | 验签失败 | **401** | `invalid signature` |
//! | 成功（含**未建模**事件） | 202 | —— |
//!
//! ⚠️ 产品边界关闭时上游刻意回**裸 404**（与"连接不存在"同一个响应），因为那条路径不该
//! 泄漏任何配置信息 —— 这也是它**不**回 503 的原因。
//!
//! # 响应形状：**扁平** `{"error":"…"}` + 尾随换行
//!
//! 上游这一族用 `writeError` ⇒ `{"error": msg}`（`writeJSON` 还补一个尾随 `\n`，注释逐字
//! "Match the trailing newline that json.Encoder.Encode historically appended"）。
//! provider 的投递 UI 按扁平体解析 ⇒ 本文件手写形状，不复用本仓标准的嵌套错误体
//! （与 `routes/webhooks/autopilots.rs` 同一判断）。
//!
//! ⚠️ 公开路由**不得**挂会话 middleware（`docs/61` §2.7 第 4 条）：本文件不取 [`AuthUser`]，
//! 本子 router 也不套任何鉴权层。
//!
//! # 本波的范围：**只做镜像**
//!
//! 上游的 `mirrorVCSPullRequest` 还做「按 identifier 自动关联 issue」与「合并后自动关单」。
//! 那套机制的**写者**是 M8-4（`mc-vcs-github/src/{links,closepolicy}.rs`，共享给 GitHub 面），
//! `docs/61` §1.6 的两列对照把 VCS 侧明确定为「PR 镜像 + CI 状态镜像，**无**自动关联/关闭」
//! ⇒ 本文件只 upsert + 广播。`issue_vcs_pull_request` 的写入原语在
//! `mc_repos::vcs::pull_request`（由 M8-4 调用），本波不调用。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, FailedToBufferBody};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_repos::vcs::commit_status::{NewVcsCommitStatus, VcsCommitStatusRepo};
use mc_repos::vcs::connection::{VcsConnectionRepo, VcsConnectionRow};
use mc_repos::vcs::pull_request::{NewVcsPullRequest, VcsPullRequestRepo};
use mc_vcs::events::{CIStatusEvent, EventKind, PullRequestEvent};
use serde_json::{json, Value};

use crate::routes::vcs::connections::{open_secret, provider_registry};
use crate::state::AppState;

/// body 上限（上游 `io.ReadAll(io.LimitReader(r.Body, 10<<20))` 的 `10 MiB`）。
///
/// ⚠️ 与上游的一处差异（登记 `docs/32` §9.12）：上游用 `LimitReader` **静默截断**，截断后的
/// body 验签必然失败 ⇒ 超大投递得到的是 **401**；本仓用 `DefaultBodyLimit` 显式回 **413**。
/// 选择它的理由是 `Bytes` 抽取器给不出"截断后的前缀"，而 413 比"看起来像签名不匹配"更容易
/// 被 provider 的投递 UI 解释清楚。
pub const VCS_WEBHOOK_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// 本文件的路由切片（1 个注册键，单形态）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/webhooks/vcs/:connectionId", post(handle_vcs_webhook))
        .layer(DefaultBodyLimit::max(VCS_WEBHOOK_MAX_BODY_BYTES))
}

/// 上游 `HandleVCSWebhook`（`vcs_webhook.go:91`）。
async fn handle_vcs_webhook(
    State(state): State<Arc<AppState>>,
    Path(raw_connection_id): Path<String>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    // ① 产品边界 / 密钥：两条都回**裸 404**（上游刻意不让这条路径泄漏配置信息）。
    if !state.vcs_keys.is_enabled() || !state.vcs_keys.is_configured() {
        return flat_error(StatusCode::NOT_FOUND, "unknown connection");
    }
    // ② 路径参数（上游 `parseUUIDOrBadRequest` ⇒ 这一族也是**扁平**错误体）。
    let Ok(connection_id) = uuid::Uuid::parse_str(raw_connection_id.trim()) else {
        return flat_error(
            StatusCode::BAD_REQUEST,
            "connection id must be a valid uuid",
        );
    };
    // ③ body（413 / 400）。
    let body = match body {
        Ok(body) => body,
        Err(BytesRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError(_))) => {
            return flat_error(StatusCode::PAYLOAD_TOO_LARGE, "payload too large");
        }
        Err(rejection) => {
            tracing::debug!(error = %rejection, "vcs: failed to read request body");
            return flat_error(StatusCode::BAD_REQUEST, "read body failed");
        }
    };

    // ④ 取连接（**不**收窄 workspace：这条路径正是用来发现 workspace 的）。
    let repo = VcsConnectionRepo::new(state.db.clone());
    let connection = match repo.find_by_id(Id(connection_id)).await {
        Ok(Some(row)) => row,
        Ok(None) => return flat_error(StatusCode::NOT_FOUND, "unknown connection"),
        Err(err) => {
            // 库错与"不存在"同判（上游逐字：`!errors.Is(err, pgx.ErrNoRows)` 之外只 warn）。
            tracing::warn!(error = %err, "vcs: lookup connection failed");
            return flat_error(StatusCode::NOT_FOUND, "unknown connection");
        }
    };

    // ⑤ provider：连接上的 kind 必须在 registry 里（未注册 ⇒ **可区分的 500**，不是 panic、
    //    也不是静默按某个 provider 继续跑）。
    let Some(kind) = connection.provider_kind() else {
        tracing::error!(provider = %connection.provider, "vcs: connection has unknown provider");
        return flat_error(StatusCode::INTERNAL_SERVER_ERROR, "unknown provider");
    };
    let Ok(provider) = provider_registry().get(kind) else {
        tracing::error!(provider = %connection.provider, "vcs: provider not registered");
        return flat_error(StatusCode::INTERNAL_SERVER_ERROR, "unknown provider");
    };

    // ⑥ 解封该连接的 webhook secret。错误值**只带原因**（`SecretBoxError` 不含载荷）。
    let Some(secret_box) = state.vcs_keys.secret_box() else {
        // ① 已经判过 is_configured() ⇒ 理论不可达；保守回 500 而不是继续用空密钥验签。
        tracing::error!("vcs: secret box missing after configured check");
        return flat_error(StatusCode::INTERNAL_SERVER_ERROR, "secret error");
    };
    let secret = if let Ok(secret) = open_secret(secret_box, connection.encrypted_webhook_secret())
    {
        secret.unwrap_or_default()
    } else {
        tracing::error!("vcs: decrypt webhook secret failed");
        return flat_error(StatusCode::INTERNAL_SERVER_ERROR, "secret error");
    };

    // ⑦ 验签（常量时间；provider 各自实现）。
    if !provider.verify_signature(&secret, &headers, &body) {
        return flat_error(StatusCode::UNAUTHORIZED, "invalid signature");
    }

    // ⑧ 事件分类 → 各自的镜像路径。未建模的事件**确认但忽略**（不是错误）。
    match provider.event_kind(&headers) {
        EventKind::PullRequest => match provider.parse_pull_request(&body) {
            Ok(event) => mirror_pull_request(&state, &connection, &event).await,
            Err(err) => {
                tracing::warn!(provider = %connection.provider, error = %err, "vcs: bad pull_request payload");
            }
        },
        EventKind::CIStatus => match provider.parse_ci_status(&body) {
            Ok(event) => mirror_ci_status(&state, &connection, &event).await,
            Err(err) => {
                tracing::warn!(provider = %connection.provider, error = %err, "vcs: bad status payload");
            }
        },
        EventKind::Other => {}
    }

    StatusCode::ACCEPTED.into_response()
}

// ---------------------------------------------------------------------------
// 镜像
// ---------------------------------------------------------------------------

/// 上游 `mirrorVCSPullRequest` 的**镜像**部分（自动关联 / 自动关闭不在本波，见模块文档）。
async fn mirror_pull_request(
    state: &AppState,
    connection: &VcsConnectionRow,
    event: &PullRequestEvent,
) {
    if event.repo_owner.is_empty() || event.repo_name.is_empty() || event.number == 0 {
        tracing::warn!(
            provider = %connection.provider,
            "vcs: pull_request missing repo identity"
        );
        return;
    }

    let now = Utc::now();
    let upserted = VcsPullRequestRepo::new(state.db.clone())
        .upsert(NewVcsPullRequest {
            workspace_id: connection.workspace_id(),
            connection_id: connection.id(),
            provider: connection.provider.clone(),
            repo_owner: event.repo_owner.clone(),
            repo_name: event.repo_name.clone(),
            pr_number: event.number,
            title: event.title.clone(),
            state: event.state.clone(),
            html_url: event.html_url.clone(),
            branch: event.branch.clone(),
            head_sha: event.head_sha.clone(),
            author_login: event.author_login.clone(),
            author_avatar_url: event.author_avatar_url.clone(),
            merged_at: parse_time(event.merged_at.as_deref()),
            closed_at: parse_time(event.closed_at.as_deref()),
            // 上游 `parseGHTimeRequired`：解析不出就用**摄入时间**，绝不写 NULL
            // （两列都是 NOT NULL，而且 `pr_updated_at` 是单调守卫的输入）。
            pr_created_at: parse_time(event.created_at.as_deref()).unwrap_or(now),
            pr_updated_at: parse_time(event.updated_at.as_deref()).unwrap_or(now),
            additions: event.additions,
            deletions: event.deletions,
            changed_files: event.changed_files,
        })
        .await;

    match upserted {
        Ok(row) => {
            let workspace_id = connection.workspace_id.to_string();
            let envelope = mc_realtime::EventEnvelope::new(
                "pull_request",
                workspace_id,
                None,
                json!({
                    "pull_request": pull_request_payload(&row),
                    // 本波没有自动关联 ⇒ 列表恒空（形状与上游一致：上游把它当数组发）。
                    "linked_issue_ids": Vec::<String>::new(),
                }),
            )
            .with_type("pull_request:updated");
            state.realtime.publish(envelope);
        }
        Err(err) => {
            tracing::warn!(error = %err, "vcs: upsert pr failed");
        }
    }
}

/// 上游 `mirrorVCSCIStatus`（镜像 + 扇出刷新）。
async fn mirror_ci_status(state: &AppState, connection: &VcsConnectionRow, event: &CIStatusEvent) {
    if event.sha.is_empty() || event.state.is_empty() {
        return;
    }
    let repo = VcsCommitStatusRepo::new(state.db.clone());
    let upserted = repo
        .upsert(NewVcsCommitStatus {
            connection_id: connection.id(),
            sha: event.sha.clone(),
            context: event.context.clone(),
            state: event.state.clone(),
            target_url: event.target_url.clone(),
            description: event.description.clone(),
            // 用 provider 自己的事件时间戳，让单调守卫是真的；payload 没带才回落摄入时间。
            updated_at: parse_time(event.updated_at.as_deref()).unwrap_or_else(Utc::now),
        })
        .await;
    if let Err(err) = upserted {
        tracing::warn!(error = %err, "vcs: upsert commit status failed");
        return;
    }

    let pr_repo = VcsPullRequestRepo::new(state.db.clone());
    let issue_ids = match pr_repo
        .list_issue_ids_for_head(connection.id(), &event.sha)
        .await
    {
        Ok(ids) => ids,
        Err(err) => {
            tracing::warn!(error = %err, "vcs: lookup issues for status failed");
            return;
        }
    };
    let workspace_id = connection.workspace_id.to_string();
    for issue_id in issue_ids {
        let envelope = mc_realtime::EventEnvelope::new(
            "pull_request",
            workspace_id.clone(),
            None,
            json!({ "issue_id": issue_id.0.to_string() }),
        )
        .with_type("pull_request:updated");
        state.realtime.publish(envelope);
    }
}

/// 广播载荷里的 PR 形状（上游 `vcsPullRequestToResponse` 的字段子集；无聚合 check 计数 ——
/// 前端会重新查询 issue 的 PR 列表拿最新计数，上游注释逐字）。
fn pull_request_payload(row: &mc_repos::vcs::pull_request::VcsPullRequestRow) -> Value {
    json!({
        "id": row.id.to_string(),
        "provider": row.provider,
        "workspace_id": row.workspace_id.to_string(),
        "repo_owner": row.repo_owner,
        "repo_name": row.repo_name,
        "number": row.pr_number,
        "title": row.title,
        "state": row.state,
        "html_url": row.html_url,
        "branch": row.branch,
        "author_login": row.author_login,
        "author_avatar_url": row.author_avatar_url,
        "merged_at": row.merged_at.map(|at| at.to_rfc3339()),
        "closed_at": row.closed_at.map(|at| at.to_rfc3339()),
        "pr_created_at": row.pr_created_at.to_rfc3339(),
        "pr_updated_at": row.pr_updated_at.to_rfc3339(),
        "mergeable_state": Value::Null,
        "checks_conclusion": Value::Null,
        "additions": row.additions,
        "deletions": row.deletions,
        "changed_files": row.changed_files,
    })
}

/// 上游 `parseGHTime`：RFC3339（允许小数秒）→ `DateTime<Utc>`；认不出 ⇒ `None`。
///
/// ⚠️ **只认 RFC3339** 是刻意的：provider 层（`mc-vcs` 的 GitLab adapter）已经把
/// `"2017-09-20 08:31:45 UTC"` 这种方言归一化成 RFC3339 了（`events.rs` 的字段文档就是
/// 「RFC3339 或空串」）。在这里再认一遍方言等于给共享层开后门，也会掩盖 provider 的 bug。
fn parse_time(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// 上游 `writeError`：扁平 `{"error": msg}` + 尾随换行（与
/// `routes/webhooks/autopilots.rs` 的 `write_json` 同形）。
fn flat_error(status: StatusCode, message: &str) -> Response {
    let mut payload = json!({ "error": message }).to_string();
    payload.push('\n');
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    (status, headers, payload).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse_time`：RFC3339 两侧通过、方言与垃圾值 ⇒ `None`（回落摄入时间）。
    #[test]
    fn parse_time_accepts_rfc3339_only() {
        assert_eq!(
            parse_time(Some("2026-09-01T00:00:00Z")).map(|at| at.timestamp()),
            Some(1_788_220_800)
        );
        assert_eq!(
            parse_time(Some("2026-09-01T05:30:00+05:30")).map(|at| at.timestamp()),
            Some(1_788_220_800)
        );
        assert!(parse_time(Some("  2026-09-01T00:00:00Z  ")).is_some());
        for bad in [
            None,
            Some(""),
            Some("   "),
            // GitLab 的方言**不**该走到这里（provider 已归一化）—— 认它会掩盖 provider 的 bug。
            Some("2017-09-20 08:31:45 UTC"),
            Some("not a time"),
        ] {
            assert_eq!(parse_time(bad), None, "raw = {bad:?}");
        }
    }

    /// 扁平错误体：`application/json` + 尾随换行 + 上游文案。
    #[test]
    fn flat_error_shape_matches_write_error() {
        let response = flat_error(StatusCode::NOT_FOUND, "unknown connection");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    /// body 上限常量与上游 `10<<20` 逐字相等。
    #[test]
    fn body_limit_is_ten_mebibytes() {
        assert_eq!(VCS_WEBHOOK_MAX_BODY_BYTES, 10 * 1024 * 1024);
    }
}
