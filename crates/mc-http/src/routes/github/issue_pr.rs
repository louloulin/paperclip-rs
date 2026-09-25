//! `GET /api/issues/:id/pull-requests`（`router.go:2011`）—— 写者 **M8-4**。
//!
//! # 这条路由的历史（anchor 的「原地搬运」，`docs/61` §3.1 / §9.7）
//!
//! 本路由原先注册在 `crate::routes::issues::router()`（`issues/mod.rs:193`，handler
//! `not_implemented`）。M8-0 把它**搬到这里**，因为它的真实现归 M8-4（GitHub PR 读面），
//! 而 M8-4 不该回来改 `issues/mod.rs`（冻结的 anchor 文件）。搬运的三条不变量：
//!
//! 1. **注册键逐字不变**（`GET /api/issues/:id/pull-requests`）⇒ ⑦ 的 `local` 不变；
//! 2. **handler 名曾是 `not_implemented`**（门 ⑦ 的占位正则认它）⇒ 本片把它换成真实现，
//!    `implemented_placeholder` 减一、`implemented_real` 加一（`docs/61` §6.1 的 M8-4 行：
//!    `local +1`（webhook 新增）而 `implemented` 只 `+1`，正是这条的算术）；
//! 3. 路由**必须存在**（`docs/61` §2.7 第 7 条）。
//!
//! # 真实现（上游 `ListPullRequestsForIssue`，`github.go:964-1009`）
//!
//! 逐字照抄上游的四步：
//!
//! 1. `loadIssueForUser` ⇒ 本仓的 `resolve_workspace` + `require_workspace_member` + `load_issue`
//!    （`:id` 既接受 UUID 也接受 identifier）；
//! 2. 列 `github_pull_request`（按 `issue_pull_request` 收窄）× `snapshot_head_sha` 聚合的 check 计数；
//!    **每一行**顺带做一次**页面访问触发的刷新入队**（`maybe_enqueue_on_view`，非阻塞：
//!    这次请求照旧返回可能陈旧的快照，新快照经 `pull_request:updated` 事件到达）；
//! 3. 把**自建 VCS provider**（Forgejo / Gitea / GitLab）的 PR 并进同一张列表 —— 它们在
//!    `vcs_pull_request` 里，映射成同一形状（`provider` 字段区分）；
//! 4. 合并后按 `pr_created_at` **新到旧**稳定排序，包 `{"pull_requests": […]}`。
//!
//! # 「列表顺序」的一处登记（`docs/32` §9.12）
//!
//! 上游用 `sort.SliceStable` 比较 **RFC3339 字符串**（`out[i].PRCreatedAt > out[j].PRCreatedAt`）；
//! 本仓比较 `DateTime` 值本身。两者在「全部时间戳都是同一 UTC 形态」时结果相同，而本仓的
//! 格式化永远产出 `…Z` ⇒ 本仓的比较不受格式化细节影响（严格更稳）。
//!
//! # 卡片形状：为什么在这一个文件里（登记 `docs/32` §9.12）
//!
//! 上游 `GitHubPullRequestResponse`（`github.go:65-141`，**31 个字段**）是 GitHub 与 VCS
//! 两条路径**共用**的响应形状。本仓 `mc_vcs_github::dto::GithubPullRequestResponse` 是 M8-1
//! 的**冻结**文件，只覆盖基础列子集，且其模块头逐字把其余字段（snapshot / checks / additions）
//! 判给「**M8-4 的填充面**」。因此本片在**自己的**读面文件里落完整的 [`PullRequestCard`]，
//! 并让 `webhook.rs` 的广播复用同一份映射（一个形状、一个写者、零重复）。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::github::pull_request::{GithubPullRequestRepo, GithubPullRequestRow};
use mc_repos::vcs::pull_request::{IssueVcsPullRequestRow, VcsPullRequestRepo};
use serde::{Deserialize, Serialize};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::github::webhook::pr_refresh_port;
// `issues` 的子模块声明都是 `mod`（私有）⇒ 只能走它的 `pub(crate) use` 再导出面。
use crate::routes::issues::{issue_repo, load_issue, repo_err, resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

/// 快照陈旧阈值（上游 `prSnapshotStaleThreshold = 30 * time.Minute`）。
///
/// 健康的管道至少每个 sweep 间隔（~10m）刷一次开着的 PR；越过 30m 说明刷新没落地
/// （GitHub 故障 / 密钥被吊销）⇒ 卡片把展示的数据标成「上一次已知」而不是空白。
pub const PR_SNAPSHOT_STALE_SECONDS: i64 = 30 * 60;

/// 本文件的路由切片（1 个注册键，单形态：上游 `r.Get("/pull-requests")` 是 plain 子路由）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/issues/:id/pull-requests", get(list_pull_requests))
}

/// `GET /api/issues/:id/pull-requests` 的响应信封（上游 `map[string]any{"pull_requests": out}`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuePullRequestsResponse {
    pub pull_requests: Vec<PullRequestCard>,
}

/// 上游 `GitHubPullRequestResponse`（31 个字段，逐字对齐字段名与可空性）。
///
/// ⚠️ write-only / 脱敏：本形状里**不得**出现 installation id、App 私钥、installation token
/// 或 webhook secret（`docs/61` §2.4 的四条判据）。它只承载 GitHub 的公开数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestCard {
    pub id: String,
    /// `github` / `forgejo` / `gitea` / `gitlab` —— 前端据此选主机图标与措辞。
    pub provider: String,
    pub workspace_id: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub number: i32,
    pub title: String,
    pub state: String,
    pub html_url: String,
    pub branch: Option<String>,
    pub author_login: Option<String>,
    pub author_avatar_url: Option<String>,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub pr_created_at: String,
    pub pr_updated_at: String,
    /// GitHub REST 的 `mergeable_state`（为兼容保留；卡片现在读下面那组 GraphQL 字段）。
    pub mergeable_state: Option<String>,
    /// 只有 GitHub 行有（上游 `json:"snapshot_available,omitempty"` ⇒ 非 GitHub **缺席**）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_available: Option<bool>,
    /// 只回答「有没有冲突」：`mergeable` / `conflicting` / `unknown` / null。
    pub mergeable: Option<String>,
    /// GitHub 的合并裁决（小写）：`clean` / `dirty` / `blocked` / … / null。
    pub merge_state_status: Option<String>,
    /// `statusCheckRollup.state` 的小写形态；**null 表示 rollup 为 null（还没 check）**，
    /// 绝不能被渲染成「通过」。
    pub checks_rollup: Option<String>,
    /// 由快照派生的粗粒度兼容别名：`passed` / `failed` / `pending` / null。
    pub checks_conclusion: Option<String>,
    pub checks_total: i64,
    pub checks_passed: i64,
    pub checks_failed: i64,
    pub checks_running: i64,
    /// 老客户端读的旧键（与 `checks_running` 同值）。
    pub checks_pending: i64,
    pub failed_check_names: Vec<String>,
    pub snapshot_stale: bool,
    pub snapshot_fetched_at: Option<String>,
    pub additions: i32,
    pub deletions: i32,
    pub changed_files: i32,
}

impl PullRequestCard {
    /// 上游 `githubPullRequestToResponse`：**单行**（无 check 聚合），广播用。
    ///
    /// 注释逐字：「A bare PR row has no aggregated check counts — webhook broadcasts of a
    /// single PR fall through here and the frontend re-queries the list for the full snapshot」
    /// ⇒ `checks_*` 全 0、`failed_check_names` 空数组、`snapshot_stale` 恒 `false`。
    pub fn from_github_row(row: &GithubPullRequestRow, snapshot_enabled: bool) -> Self {
        let available = snapshot_available(row, snapshot_enabled);
        let mut card = Self::blank_base(
            row.id(),
            "github",
            row.workspace_id(),
            &row.repo_owner,
            &row.repo_name,
            row.pr_number,
            &row.title,
            &row.state,
            &row.html_url,
        );
        card.branch.clone_from(&row.branch);
        card.author_login.clone_from(&row.author_login);
        card.author_avatar_url.clone_from(&row.author_avatar_url);
        card.merged_at = row.merged_at.map(|at| at.to_rfc3339());
        card.closed_at = row.closed_at.map(|at| at.to_rfc3339());
        card.pr_created_at = row.pr_created_at.to_rfc3339();
        card.pr_updated_at = row.pr_updated_at.to_rfc3339();
        card.mergeable_state.clone_from(&row.mergeable_state);
        card.snapshot_available = Some(available);
        if available {
            card.mergeable = lowercase(row.api_mergeable.as_deref());
            card.merge_state_status = lowercase(row.api_merge_state_status.as_deref());
            card.checks_rollup = lowercase(row.checks_rollup_state.as_deref());
            card.checks_conclusion =
                rollup_to_conclusion(row.checks_rollup_state.as_deref(), 0, 0, 0);
            card.snapshot_fetched_at = row.snapshot_fetched_at.map(|at| at.to_rfc3339());
        }
        card.additions = row.additions;
        card.deletions = row.deletions;
        card.changed_files = row.changed_files;
        card
    }

    /// 上游 `issuePullRequestRowToResponse`：PR 行 + 按 `snapshot_head_sha` 聚合的 check 计数。
    pub fn from_github_detail(
        detail: &mc_repos::github::pull_request::IssuePullRequestDetailRow,
        snapshot_enabled: bool,
        now: DateTime<Utc>,
    ) -> Self {
        let row = &detail.pull_request;
        let mut card = Self::from_github_row(row, snapshot_enabled);
        let available = card.snapshot_available.unwrap_or(false);
        if available {
            card.checks_total = detail.checks_total;
            card.checks_passed = detail.checks_passed;
            card.checks_failed = detail.checks_failed;
            card.checks_running = detail.checks_running;
            card.checks_pending = detail.checks_running;
            card.failed_check_names
                .clone_from(&detail.failed_check_names);
            card.checks_conclusion = rollup_to_conclusion(
                row.checks_rollup_state.as_deref(),
                detail.checks_failed,
                detail.checks_running,
                detail.checks_passed,
            );
            // 只有**开着的** PR 会被标陈旧（终态 PR 的快照本来就该是最后一次）。
            if matches!(row.state.as_str(), "open" | "draft") {
                card.snapshot_stale = row.snapshot_fetched_at.is_some_and(|fetched| {
                    now.signed_duration_since(fetched).num_seconds() > PR_SNAPSHOT_STALE_SECONDS
                });
            }
        }
        card
    }

    /// 上游 `vcsPullRequestRowToResponse`：自建 VCS provider 的行。
    ///
    /// 三处与 GitHub 行的差别逐字：`provider` 来自**存储列**、`mergeable_state` 恒 `nil`、
    /// `snapshot_available` **缺席**（非 GitHub provider 走 `checks_conclusion` 这条兼容路）。
    pub fn from_vcs_detail(detail: &IssueVcsPullRequestRow) -> Self {
        let row = &detail.pull_request;
        let mut card = Self::blank_base(
            row.id(),
            &row.provider,
            row.workspace_id(),
            &row.repo_owner,
            &row.repo_name,
            row.pr_number,
            &row.title,
            &row.state,
            &row.html_url,
        );
        card.branch.clone_from(&row.branch);
        card.author_login.clone_from(&row.author_login);
        card.author_avatar_url.clone_from(&row.author_avatar_url);
        card.merged_at = row.merged_at.map(|at| at.to_rfc3339());
        card.closed_at = row.closed_at.map(|at| at.to_rfc3339());
        card.pr_created_at = row.pr_created_at.to_rfc3339();
        card.pr_updated_at = row.pr_updated_at.to_rfc3339();
        card.checks_conclusion = aggregate_checks_conclusion(
            detail.checks_failed,
            detail.checks_passed,
            detail.checks_pending,
            detail.checks_total,
        );
        card.checks_total = detail.checks_total;
        card.checks_passed = detail.checks_passed;
        card.checks_failed = detail.checks_failed;
        card.checks_pending = detail.checks_pending;
        card.checks_running = detail.checks_pending;
        card.additions = row.additions;
        card.deletions = row.deletions;
        card.changed_files = row.changed_files;
        card
    }

    #[allow(clippy::too_many_arguments)]
    fn blank_base(
        id: Id,
        provider: &str,
        workspace_id: Id,
        repo_owner: &str,
        repo_name: &str,
        number: i32,
        title: &str,
        state: &str,
        html_url: &str,
    ) -> Self {
        Self {
            id: id.to_string(),
            provider: provider.to_string(),
            workspace_id: workspace_id.to_string(),
            repo_owner: repo_owner.to_string(),
            repo_name: repo_name.to_string(),
            number,
            title: title.to_string(),
            state: state.to_string(),
            html_url: html_url.to_string(),
            branch: None,
            author_login: None,
            author_avatar_url: None,
            merged_at: None,
            closed_at: None,
            pr_created_at: String::new(),
            pr_updated_at: String::new(),
            mergeable_state: None,
            snapshot_available: None,
            mergeable: None,
            merge_state_status: None,
            checks_rollup: None,
            checks_conclusion: None,
            checks_total: 0,
            checks_passed: 0,
            checks_failed: 0,
            checks_running: 0,
            checks_pending: 0,
            failed_check_names: Vec::new(),
            snapshot_stale: false,
            snapshot_fetched_at: None,
            additions: 0,
            deletions: 0,
            changed_files: 0,
        }
    }
}

/// 上游 `currentGitHubSnapshotAvailable(enabled, headSHA, snapshotHeadSHA, fetchedAt)`。
///
/// 四个条件缺一即 `false` ⇒ **0 表示「未取到快照」**；只有 `statusCheckRollup` 本身为 null
/// 才是「没有 check」（卡片据此决定是隐藏还是显示「无 check」）。
pub fn snapshot_available(row: &GithubPullRequestRow, snapshot_enabled: bool) -> bool {
    snapshot_enabled && row.snapshot_fetched_at.is_some() && row.snapshot_matches_head()
}

/// 上游 `lowerTextPtr`：非空（不 trim）⇒ 小写 `Some`，否则 `None`。
fn lowercase(value: Option<&str>) -> Option<String> {
    value.filter(|text| !text.is_empty()).map(str::to_lowercase)
}

/// 上游 `aggregateChecksConclusion`：粗粒度 CI 结论（VCS 兼容路）。
///
/// 优先级：任一条 failed ⇒ `failed`；任一未完成 ⇒ `pending`；全完成且通过 ⇒ `passed`；
/// **一条都没有 ⇒ `None`**（渲染成「无 check」/隐藏）。
fn aggregate_checks_conclusion(
    failed: i64,
    passed: i64,
    pending: i64,
    total: i64,
) -> Option<String> {
    if total == 0 {
        return None;
    }
    if failed > 0 {
        Some("failed".to_string())
    } else if pending > 0 {
        Some("pending".to_string())
    } else if passed > 0 {
        Some("passed".to_string())
    } else {
        None
    }
}

/// 上游 `rollupToConclusion`：由 GraphQL rollup 派生兼容别名，认不得的枚举则退回落计数。
///
/// ⚠️ rollup 为 null / 空 ⇒ `None`（「还没 check」），**绝不**是 `passed`。
fn rollup_to_conclusion(
    rollup: Option<&str>,
    failed: i64,
    running: i64,
    passed: i64,
) -> Option<String> {
    let rollup = rollup.filter(|value| !value.is_empty())?;
    let conclusion = match rollup.to_uppercase().as_str() {
        "FAILURE" | "ERROR" => Some("failed"),
        "PENDING" | "EXPECTED" => Some("pending"),
        "SUCCESS" => Some("passed"),
        _ => {
            if failed > 0 {
                Some("failed")
            } else if running > 0 {
                Some("pending")
            } else if passed > 0 {
                Some("passed")
            } else {
                None
            }
        }
    };
    conclusion.map(str::to_string)
}

/// 上游 `ListPullRequestsForIssue`（`github.go:964`）。
async fn list_pull_requests(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssuePullRequestsResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    crate::routes::invitations::require_workspace_member(&state, workspace_id, user.id()).await?;
    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;

    let snapshot_enabled = pr_refresh_port().enabled();
    let now = Utc::now();
    let mut cards = Vec::new();

    let github = GithubPullRequestRepo::new(state.db.clone())
        .list_by_issue(issue.id())
        .await
        .map_err(repo_err)?;
    for detail in &github {
        // 页面访问触发（MUL-5265）：快照缺失或比 view TTL 旧 ⇒ 非阻塞地入队一次刷新。
        // 本次响应照旧返回（可能陈旧的）快照，新的经 `pull_request:updated` 事件到达。
        pr_refresh_port().maybe_enqueue_on_view(mc_vcs_github::port::PrRefreshRequest {
            workspace_id,
            repo_owner: detail.pull_request.repo_owner.clone(),
            repo_name: detail.pull_request.repo_name.clone(),
            pr_number: detail.pull_request.pr_number,
            head_sha: mc_vcs_github::payload::str_ptr_or_nil(&detail.pull_request.head_sha),
            reason: mc_vcs_github::port::RefreshReason::PageView,
        });
        cards.push(PullRequestCard::from_github_detail(
            detail,
            snapshot_enabled,
            now,
        ));
    }

    let vcs = VcsPullRequestRepo::new(state.db.clone())
        .list_by_issue(issue.id())
        .await
        .map_err(repo_err)?;
    for detail in &vcs {
        cards.push(PullRequestCard::from_vcs_detail(detail));
    }

    // 合并后按 `pr_created_at` 新到旧（上游 `sort.SliceStable`；比较对象见模块头的登记）。
    cards.sort_by(|left, right| right.pr_created_at.cmp(&left.pr_created_at));

    Ok(Json(IssuePullRequestsResponse {
        pull_requests: cards,
    }))
}

/// 400 helper 的本地副本（与 `routes/agents.rs` / `routes/github/install.rs` 同款；本仓惯例是
/// 各切片各持一份，避免改公共 helper）。
#[allow(dead_code)]
pub(crate) fn bad_request(message: impl Into<String>) -> Error {
    Error::Validation {
        message: message.into(),
        details: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollup_to_conclusion_maps_the_four_graphql_verdicts() {
        assert_eq!(
            rollup_to_conclusion(Some("SUCCESS"), 0, 0, 0).as_deref(),
            Some("passed")
        );
        assert_eq!(
            rollup_to_conclusion(Some("failure"), 0, 0, 0).as_deref(),
            Some("failed")
        );
        assert_eq!(
            rollup_to_conclusion(Some("ERROR"), 0, 0, 0).as_deref(),
            Some("failed")
        );
        assert_eq!(
            rollup_to_conclusion(Some("pending"), 0, 0, 0).as_deref(),
            Some("pending")
        );
        assert_eq!(
            rollup_to_conclusion(Some("EXPECTED"), 0, 0, 0).as_deref(),
            Some("pending")
        );
        // null / 空 ⇒ None（「还没 check」），绝不是 passed。
        assert_eq!(rollup_to_conclusion(None, 3, 0, 9), None);
        assert_eq!(rollup_to_conclusion(Some(""), 3, 0, 9), None);
        // 认不得的枚举 ⇒ 退回落计数。
        assert_eq!(
            rollup_to_conclusion(Some("weird"), 2, 0, 9).as_deref(),
            Some("failed")
        );
        assert_eq!(
            rollup_to_conclusion(Some("weird"), 0, 1, 9).as_deref(),
            Some("pending")
        );
        assert_eq!(
            rollup_to_conclusion(Some("weird"), 0, 0, 9).as_deref(),
            Some("passed")
        );
        assert_eq!(rollup_to_conclusion(Some("weird"), 0, 0, 0), None);
    }

    #[test]
    fn aggregate_checks_conclusion_priority_and_empty_case() {
        assert_eq!(aggregate_checks_conclusion(0, 0, 0, 0), None);
        // failed 压过 pending 与 passed。
        assert_eq!(
            aggregate_checks_conclusion(1, 2, 3, 6).as_deref(),
            Some("failed")
        );
        assert_eq!(
            aggregate_checks_conclusion(0, 2, 3, 5).as_deref(),
            Some("pending")
        );
        assert_eq!(
            aggregate_checks_conclusion(0, 2, 0, 2).as_deref(),
            Some("passed")
        );
    }

    #[test]
    fn card_serializes_snapshot_available_as_absent_for_vcs_providers() {
        // `IssueVcsPullRequestRow` 只有 `FromRow`（没有 `Deserialize`）⇒ 字面量构造。
        let now = DateTime::parse_from_rfc3339("2026-09-25T00:00:00Z")
            .expect("ts")
            .with_timezone(&Utc);
        let vcs = IssueVcsPullRequestRow {
            pull_request: mc_repos::vcs::pull_request::VcsPullRequestRow {
                id: uuid::Uuid::nil(),
                workspace_id: uuid::Uuid::nil(),
                connection_id: uuid::Uuid::nil(),
                provider: "gitlab".into(),
                repo_owner: "acme".into(),
                repo_name: "api".into(),
                pr_number: 3,
                title: "t".into(),
                state: "open".into(),
                html_url: "https://gitlab/x".into(),
                branch: None,
                head_sha: "sha".into(),
                author_login: None,
                author_avatar_url: None,
                merged_at: None,
                closed_at: None,
                pr_created_at: now,
                pr_updated_at: now,
                additions: 1,
                deletions: 2,
                changed_files: 3,
                created_at: now,
                updated_at: now,
            },
            checks_total: 4,
            checks_passed: 1,
            checks_failed: 2,
            checks_pending: 1,
        };
        let card = PullRequestCard::from_vcs_detail(&vcs);
        assert_eq!(card.provider, "gitlab");
        assert_eq!(card.checks_running, card.checks_pending);
        assert_eq!(card.checks_conclusion.as_deref(), Some("failed"));
        let json = serde_json::to_value(&card).expect("serialize");
        assert!(
            json.get("snapshot_available").is_none(),
            "非 GitHub provider ⇒ 该字段**缺席**（上游 omitempty）"
        );
        assert!(
            json["mergeable"].is_null(),
            "其余 Option 字段是 null 而不是缺席"
        );
        assert_eq!(json["failed_check_names"], serde_json::json!([]));
    }
}
