//! `pull_request` 事件：扇出 → 投递级关闭裁决 → 逐 workspace 镜像 → 自动推进 → 快照入队。
//!
//! 上游 `handlePullRequestEvent`（`github.go:1258-1324`）+ `resolveCloseIntentPolicy`
//! （`:1352-1466`）+ `mirrorPullRequestForWorkspace`（`:1535-1739`）+ `advanceIssueToDone`
//! （`:1922-1975`）。
//!
//! # 顺序是契约（四条，逐字对齐上游）
//!
//! 1. `installation.id == 0` ⇒ 直接返回（载荷不足，没有可归属的 workspace）；
//! 2. 没有任何绑定 ⇒ **静默丢弃**（来自一个我们从没接过的 installation，那是一次扫描；
//!    上游**不**把它记成错误，否则一次全仓扫描会刷爆日志）；
//! 3. **先**裁定投递级关闭裁决，**再**逐 workspace 镜像 —— 镜像那一趟只能**收紧**裁决，
//!    绝不能重新推导出另一个答案（否则同一个「Closes X」在两个 workspace 里会各自算出
//!    「我说了算」）；
//! 4. 四个效果按序落地：PR 行 upsert → 关联账增删 → 持久化态上的推进闸门 → 广播；
//!    PR 行的 upsert **无条件**跑（重开 GitHub 功能能恢复历史，不需要回填），关联账是
//!    「新副作用」，由 workspace 的自动关联开关把门。
//!
//! # 两个「容易被顺手简化掉」的语义
//!
//! - **裸提及不建链**：body 里的 `Related MUL-1` 不是主张；PR 仍可编辑时它还会**删掉**早先
//!   claim 建过的行（MUL-3739 / MUL-7072）；
//! - **推进闸门读的是持久化后的聚合**，不是「本次载荷有没有关闭词」：一个 `Closes MUL-1` 的
//!   PR 先合并、只带链接的兄弟 PR 后关闭时，仍然是 MUL-1 前进。
//!
//! 查询落点的理由（为什么不放 `mc-repos/src/github/*.rs`）见 `docs/32` §18.2 的 D3。

use std::collections::{BTreeMap, HashMap};

use mc_core::Id;
use mc_repos::github::pull_request::{GithubPullRequestRepo, NewGithubPullRequest};
use mc_repos::issue::{IssueRepo, IssueRow, IssueUpdate};
use mc_vcs_github::closepolicy::CloseIntentPolicy;
use mc_vcs_github::links::{extract_closing_identifiers, issue_number_for_prefix};
use mc_vcs_github::mirror::{MirrorPlan, MirrorRequest};
use mc_vcs_github::payload::PullRequestEventPayload;
use mc_vcs_github::payload::{parse_gh_time, parse_gh_time_required, str_ptr_or_nil};
use mc_vcs_github::port::{PrRefreshRequest, RefreshReason};
use serde_json::Value;
use uuid::Uuid;

use super::installations::list_installation_bindings;
use super::{pr_refresh_port, AppState};
use crate::routes::github::issue_pr::PullRequestCard;

/// 上游 `handlePullRequestEvent`（`github.go:1258`）。
pub(super) async fn handle_pull_request_event(state: &AppState, payload: &PullRequestEventPayload) {
    let installation_id = payload.installation.id;
    if installation_id == 0 {
        return;
    }
    let bindings = match list_installation_bindings(state, installation_id).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, installation_id, "github: lookup installation failed");
            return;
        }
    };
    if bindings.is_empty() {
        return;
    }
    // #4855：一个 installation 可以绑多个 workspace，仓库的事件属于**每一个**绑定 ⇒ 扇出。
    // 先在所有 workspace 关联之前把「谁才允许对关闭关键词动手」裁定下来，并让整次投递共用
    // 同一个裁决，避免镜像那一趟重新推导出别的答案。
    let policy = resolve_close_intent_policy(state, &bindings, payload).await;
    for binding in &bindings {
        mirror_pull_request_for_workspace(
            state,
            binding.workspace_id(),
            installation_id,
            payload,
            &policy,
        )
        .await;
    }
    // PR 行现在带着新的 head ⇒ 请 API 管道去取权威的 CI + 可合并性快照。webhook 只是门铃，
    // 它自己的 mergeable/checks 载荷不再用于展示（MUL-5265）。
    //
    // ⚠️ 与上游的一处差异（登记 D4）：上游一次 `Enqueue(installationID, owner, repo, number)`；
    // 本仓端口（anchor 冻结形状）的定位键含 `workspace_id` ⇒ 每个绑定各入队一次。
    for binding in &bindings {
        pr_refresh_port().enqueue(PrRefreshRequest {
            workspace_id: binding.workspace_id(),
            repo_owner: payload.repository.owner.login.clone(),
            repo_name: payload.repository.name.clone(),
            pr_number: payload.pull_request.number,
            head_sha: str_ptr_or_nil(&payload.pull_request.head.sha),
            reason: RefreshReason::Webhook,
        });
    }
}

/// 上游 `resolveCloseIntentPolicy`（`github.go:1352`）。
///
/// 只在**多绑定**且载荷里有关闭关键词时才做扫描（单绑定情形歧义不可能存在，一次读都不做）。
/// 任何一次「读不完整」都整体放弃本次投递的关闭意图 —— 没被查过的 workspace 可能是第二个
/// 解析者，不排除它就称不上「唯一」。
async fn resolve_close_intent_policy(
    state: &AppState,
    bindings: &[mc_repos::github::installation::GithubInstallationRow],
    payload: &PullRequestEventPayload,
) -> CloseIntentPolicy {
    if bindings.len() < 2 {
        return CloseIntentPolicy::unrestricted();
    }
    let closing = extract_closing_identifiers(&[
        payload.pull_request.title.as_str(),
        payload.pull_request.body.as_str(),
    ]);
    if closing.is_empty() {
        return CloseIntentPolicy::withheld();
    }
    let mut resolvers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for binding in bindings {
        let workspace_id = binding.workspace_id();
        let Ok(settings) = workspace_settings(state, workspace_id).await else {
            tracing::warn!(
                %workspace_id,
                "github: cannot load bound workspace, withholding close intent"
            );
            return CloseIntentPolicy::withheld();
        };
        let Ok(auto_link) = auto_link_prs_enabled(&settings) else {
            tracing::warn!(
                %workspace_id,
                "github: cannot read workspace auto-link setting, withholding close intent"
            );
            return CloseIntentPolicy::withheld();
        };
        // 自动关联关掉的 workspace 从不写关联账 ⇒ 它不是**竞争**的主张者，不得压掉一个本该
        // 动手的 workspace。
        if !auto_link {
            continue;
        }
        let prefix = issue_prefix(state, workspace_id).await;
        for identifier in &closing {
            let Some(number) = issue_number_for_prefix(identifier, &prefix) else {
                continue;
            };
            match issue_exists_by_number(state, workspace_id, number).await {
                // 被证明的缺失：本 workspace 没有这个 issue。
                Ok(false) => {}
                Ok(true) => resolvers
                    .entry(identifier.clone())
                    .or_default()
                    .push(workspace_id.to_string()),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        identifier,
                        "github: cannot resolve identifier in bound workspace, withholding close intent"
                    );
                    return CloseIntentPolicy::withheld();
                }
            }
        }
    }
    let (policy, ambiguous) = CloseIntentPolicy::from_resolvers(resolvers);
    for identifier in ambiguous {
        // 它防住的症状（issue 无来由地变 done）本来无从追回 GitHub ⇒ 留一条指名道姓的
        // breadcrumb，让运维去改前缀。
        tracing::warn!(
            identifier,
            "github: ambiguous closing identifier across bound workspaces, withholding close intent"
        );
    }
    policy
}

/// 上游 `mirrorPullRequestForWorkspace`（`github.go:1535`）—— 对**一个** workspace 的镜像。
async fn mirror_pull_request_for_workspace(
    state: &AppState,
    workspace_id: Id,
    installation_id: i64,
    payload: &PullRequestEventPayload,
    close_policy: &CloseIntentPolicy,
) {
    let request = MirrorRequest {
        workspace_id,
        installation_id,
        event: payload,
        close_policy,
    };
    let (mergeable_state, clear_mergeable_state) = request.mergeable_write();
    let pr_repo = GithubPullRequestRepo::new(state.db.clone());
    let row = match pr_repo
        .upsert(NewGithubPullRequest {
            workspace_id,
            installation_id,
            repo_owner: payload.repository.owner.login.clone(),
            repo_name: payload.repository.name.clone(),
            pr_number: payload.pull_request.number,
            title: payload.pull_request.title.clone(),
            state: request.derived_state().to_string(),
            html_url: payload.pull_request.html_url.clone(),
            branch: str_ptr_or_nil(&payload.pull_request.head.ref_name),
            author_login: str_ptr_or_nil(&payload.pull_request.user.login),
            author_avatar_url: str_ptr_or_nil(&payload.pull_request.user.avatar_url),
            merged_at: parse_gh_time(&payload.pull_request.merged_at),
            closed_at: parse_gh_time(&payload.pull_request.closed_at),
            pr_created_at: parse_gh_time_required(&payload.pull_request.created_at),
            pr_updated_at: parse_gh_time_required(&payload.pull_request.updated_at),
            head_sha: payload.pull_request.head.sha.clone(),
            mergeable_state,
            additions: payload.pull_request.additions,
            deletions: payload.pull_request.deletions,
            changed_files: payload.pull_request.changed_files,
            clear_mergeable_state,
        })
        .await
    {
        Ok(row) => row,
        Err(error) => {
            tracing::warn!(%error, "github: upsert pr failed");
            return;
        }
    };

    let mut plan = MirrorPlan::default();
    if workspace_auto_link_prs_enabled(state, workspace_id).await {
        plan = apply_auto_link(state, workspace_id, &request, row.id(), &pr_repo).await;
    }

    // 广播给 workspace：任何打开着的 issue 详情页都会重查它的 PR 列表。
    let card = PullRequestCard::from_github_row(&row, pr_refresh_port().enabled());
    let envelope = mc_realtime::EventEnvelope::new(
        "pull_request",
        workspace_id.to_string(),
        None,
        serde_json::json!({
            "pull_request": card,
            "linked_issue_ids": plan
                .linked_issue_ids()
                .iter()
                .map(Id::to_string)
                .collect::<Vec<_>>(),
        }),
    )
    .with_type("pull_request:updated");
    state.realtime.publish(envelope);
}

/// 「自动关联」那一段（上游 `if h.workspaceAutoLinkPRsEnabled(ctx, wsID) { … }`）：解析标识符 →
/// 算计划（纯函数）→ 写关联账增删 → 终态时跑推进闸门。
///
/// 抽成一个函数是因为 `mirror_pull_request_for_workspace` 已经到 clippy 的 100 行线上
///（`too_many_lines`）；拆点选在「该不该动手」这个语义边界上。
async fn apply_auto_link(
    state: &AppState,
    workspace_id: Id,
    request: &MirrorRequest<'_>,
    pull_request_id: Id,
    pr_repo: &GithubPullRequestRepo,
) -> MirrorPlan {
    let prefix = issue_prefix(state, workspace_id).await;
    let mut resolved: HashMap<String, Id> = HashMap::new();
    let mut rows: HashMap<Id, IssueRow> = HashMap::new();
    for identifier in request.identifiers() {
        let Some(number) = issue_number_for_prefix(&identifier, &prefix) else {
            continue;
        };
        match issue_by_number(state, workspace_id, number).await {
            Ok(Some(issue)) => {
                resolved.insert(identifier, issue.id());
                rows.insert(issue.id(), issue);
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, identifier, "github: issue lookup failed"),
        }
    }
    // 决策是纯函数（`mc-vcs-github/src/mirror.rs`），I/O 在本层 —— 边界理由见模块头。
    let plan = request.plan(&resolved, true, &prefix);
    for link in &plan.links {
        if let Err(error) = pr_repo
            .link_issue(
                link.issue_id,
                pull_request_id,
                Some("system"),
                None,
                link.close_intent,
                plan.preserve_close_intent,
            )
            .await
        {
            tracing::warn!(%error, "github: link failed");
        }
    }
    for issue_id in &plan.unlinks {
        if let Err(error) = pr_repo.unlink_issue(*issue_id, pull_request_id).await {
            tracing::warn!(%error, "github: unlink failed");
        }
    }
    if plan.should_advance {
        advance_reevaluated_issues(state, workspace_id, &plan, &rows).await;
    }
    plan
}

/// 上游 `mirrorPullRequestForWorkspace` 尾部的「推进到 done」那一段。
///
/// 终态的 PR 事件（`merged` / `closed`）可能是最后一个在飞的兄弟 PR 落地的时刻 ⇒ 每次关联
/// **写完之后**再跑一次闸门（读持久化后的聚合），三条同时成立才推进：
/// ① issue 还不是终态；② 没有任何关联 PR 还在 `open` / `draft`（**跨 provider**：GitHub +
/// 自建 VCS 一起看）；③ 至少有一个合并且带 `close_intent` 的 PR。
///
/// 第 ③ 条是「`Follow up in MUL-2` / `Unblocks MUL-3` 与 `Closes MUL-1` 不同判」的原因，
/// 也是「全关闭但都没合并」时**不**自动关 issue 的原因（那种情形该由人决定）。
async fn advance_reevaluated_issues(
    state: &AppState,
    workspace_id: Id,
    plan: &MirrorPlan,
    rows: &HashMap<Id, IssueRow>,
) {
    let terminal = IssueRepo::new(state.db.clone())
        .terminal_status_keys(workspace_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "github: cannot read terminal status catalog");
            vec!["done".to_string(), "cancelled".to_string()]
        });
    let mut advanced: Vec<Id> = Vec::new();
    for issue_id in &plan.reeval {
        // `MUL-007` 与 `MUL-7` 解析到同一个 issue 时 `reeval` 会有重复项：上游会推进两次
        // （第二次是同一个值的 UPDATE），本仓去重以免重复广播（登记 D5）。
        if advanced.contains(issue_id) {
            continue;
        }
        advanced.push(*issue_id);
        let Some(issue) = rows.get(issue_id) else {
            continue;
        };
        if terminal.contains(&issue.status) {
            continue;
        }
        let aggregate = match close_aggregate(state, *issue_id).await {
            Ok(aggregate) => aggregate,
            Err(error) => {
                tracing::warn!(%error, "github: count linked pr states failed");
                continue;
            }
        };
        if aggregate.open_count == 0 && aggregate.merged_with_close_intent_count > 0 {
            advance_issue_to_done(state, workspace_id, issue).await;
        }
    }
}

/// 上游 `advanceIssueToDone`（`github.go:1922`）。
async fn advance_issue_to_done(state: &AppState, workspace_id: Id, issue: &IssueRow) {
    // 还在 Triage 的 issue 只能靠被接受离开：一个合并的 「Closes」 PR 可以链上它，
    // 但**不得**把它推出去（MUL-7189 §2.2）。
    if issue.triage_state.is_some() {
        return;
    }
    let patch = IssueUpdate {
        status: Some("done".to_string()),
        ..IssueUpdate::default()
    };
    match IssueRepo::new(state.db.clone())
        .update(workspace_id, issue.id(), &patch)
        .await
    {
        Ok(updated) => {
            let envelope = mc_realtime::EventEnvelope::new(
                "issue",
                workspace_id.to_string(),
                None,
                serde_json::json!({
                    "issue_id": updated.id().to_string(),
                    "status": updated.status,
                    "status_changed": true,
                    "prev_status": issue.status,
                    "source": "github_pr_merged",
                }),
            )
            .with_type("issue:updated");
            state.realtime.publish(envelope);
        }
        Err(error) => tracing::warn!(%error, "github: advance issue to done failed"),
    }
}

/// 上游 `GetIssueCombinedPullRequestCloseAggregate`：**跨 provider** 的关闭聚合（SQL 逐字）。
///
/// 一个 issue 可以同时挂 GitHub 与自建 VCS 的 PR ⇒ 只看一边会让「另一边还有在飞的 PR」的
/// issue 被推进（任何一侧的 webhook 都对另一侧在飞的工作视而不见）。裸提及在两边都不建关联，
/// 所以路过式引用永远不会算作在飞。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CloseAggregate {
    open_count: i64,
    merged_with_close_intent_count: i64,
}

async fn close_aggregate(state: &AppState, issue_id: Id) -> Result<CloseAggregate, sqlx::Error> {
    let row: (i64, i64) = sqlx::query_as(
        "WITH combined AS ( \
             SELECT pr.state AS state, ipr.close_intent AS close_intent \
             FROM github_pull_request pr \
             JOIN issue_pull_request ipr ON ipr.pull_request_id = pr.id \
             WHERE ipr.issue_id = $1 \
             UNION ALL \
             SELECT pr.state AS state, ipr.close_intent AS close_intent \
             FROM vcs_pull_request pr \
             JOIN issue_vcs_pull_request ipr ON ipr.pull_request_id = pr.id \
             WHERE ipr.issue_id = $1 \
         ) \
         SELECT \
             COALESCE(SUM(CASE WHEN state IN ('open', 'draft') THEN 1 ELSE 0 END), 0)::bigint, \
             COALESCE(SUM(CASE WHEN state = 'merged' AND close_intent THEN 1 ELSE 0 END), 0)::bigint \
         FROM combined",
    )
    .bind(issue_id.0)
    .fetch_one(state.db.pool())
    .await?;
    Ok(CloseAggregate {
        open_count: row.0,
        merged_with_close_intent_count: row.1,
    })
}

// ---------------------------------------------------------------------------
// 三条共享读（关闭裁决扫描 + 镜像那一趟都用）
// ---------------------------------------------------------------------------

/// 上游 `GetWorkspace` 的 `settings` 一列（自动关联开关的来源）。
pub(super) async fn workspace_settings(
    state: &AppState,
    workspace_id: Id,
) -> Result<Value, sqlx::Error> {
    let settings: Option<Value> =
        sqlx::query_scalar("SELECT settings FROM workspace WHERE id = $1")
            .bind(workspace_id.0)
            .fetch_optional(state.db.pool())
            .await?;
    Ok(settings.unwrap_or(Value::Null))
}

/// 上游 `autoLinkPRsEnabledForWorkspace`：默认 `true`，`github_enabled` 显式为假则短路为假。
///
/// **不可解析的 settings blob 报错而不折叠进宽松默认**：写关联账的调用方继续取默认值，但
/// 关闭意图扫描必须能区分「自动关联开着」与「没查出来」（见 `closeIntentPolicy`）。
pub(super) fn auto_link_prs_enabled(settings: &Value) -> Result<bool, serde_json::Error> {
    let Value::Object(map) = settings else {
        // Go：`len(ws.Settings) == 0 ⇒ true`，其余形状由 `json.Unmarshal` 判错。本仓的
        // `NULL` 解成 `Value::Null`（对应上游的空字节），非对象 ⇒ 结构不符 ⇒ 报错。
        if settings.is_null() {
            return Ok(true);
        }
        return Err(invalid_settings("workspace settings is not an object"));
    };
    let read_flag = |name: &str| -> Result<Option<bool>, serde_json::Error> {
        match map.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(invalid_settings(format!(
                "workspace settings field {name} is not a boolean"
            ))),
        }
    };
    if read_flag("github_enabled")? == Some(false) {
        return Ok(false);
    }
    Ok(read_flag("github_auto_link_prs_enabled")?.unwrap_or(true))
}

fn invalid_settings(message: impl Into<String>) -> serde_json::Error {
    serde_json::Error::io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

/// 上游 `workspaceAutoLinkPRsEnabled`（读库失败 ⇒ `true`，保持历史行为）。
pub(super) async fn workspace_auto_link_prs_enabled(state: &AppState, workspace_id: Id) -> bool {
    match workspace_settings(state, workspace_id).await {
        Ok(settings) => auto_link_prs_enabled(&settings).unwrap_or(true),
        Err(_) => true,
    }
}

/// 上游 `h.getIssuePrefix(ctx, workspaceID)`：读库失败 ⇒ 空串（于是任何 identifier 都解析不到）。
pub(super) async fn issue_prefix(state: &AppState, workspace_id: Id) -> String {
    IssueRepo::new(state.db.clone())
        .workspace_prefix(workspace_id)
        .await
        .unwrap_or_default()
}

/// 上游 `GetIssueByNumber`（只判存在）：关闭意图扫描只需要「有没有」。
async fn issue_exists_by_number(
    state: &AppState,
    workspace_id: Id,
    number: i32,
) -> Result<bool, sqlx::Error> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM issue WHERE workspace_id = $1 AND number = $2")
            .bind(workspace_id.0)
            .bind(number)
            .fetch_optional(state.db.pool())
            .await?;
    Ok(row.is_some())
}

/// 上游 `GetIssueByNumber`（取整行）：镜像那一趟需要 id / status / `triage_state`。
///
/// 两步（先取 id 再走 `IssueRepo::get`）而不是 `get_by_identifier`：上游按**号**查，而号与
/// `issue.identifier` 的字符串形态是两件事（`MUL-007` 与 `MUL-7` 同号不同串）⇒ 按号查才是
/// 逐字对齐的那条路。
async fn issue_by_number(
    state: &AppState,
    workspace_id: Id,
    number: i32,
) -> Result<Option<IssueRow>, mc_repos::RepoError> {
    let id: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM issue WHERE workspace_id = $1 AND number = $2")
            .bind(workspace_id.0)
            .bind(number)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|error| mc_repos::RepoError::Db(error.to_string()))?;
    match id {
        Some((id,)) => Ok(Some(
            IssueRepo::new(state.db.clone())
                .get(workspace_id, Id(id))
                .await?,
        )),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn auto_link_defaults_to_true_and_short_circuits_on_github_enabled() {
        // 空 / NULL ⇒ 默认 true（RFC MUL-2414 之前建的 workspace 保持历史行为）。
        assert!(auto_link_prs_enabled(&Value::Null).unwrap());
        assert!(auto_link_prs_enabled(&json!({})).unwrap());
        assert!(auto_link_prs_enabled(&json!({ "github_auto_link_prs_enabled": true })).unwrap());
        // 显式关掉主开关 ⇒ 短路为 false（即便自动关联那一项是 true）。
        assert!(!auto_link_prs_enabled(
            &json!({ "github_enabled": false, "github_auto_link_prs_enabled": true })
        )
        .unwrap());
        // 主开关开着但自动关联关掉 ⇒ false。
        assert!(!auto_link_prs_enabled(
            &json!({ "github_enabled": true, "github_auto_link_prs_enabled": false })
        )
        .unwrap());
        // 显式 null 等同缺席。
        assert!(auto_link_prs_enabled(
            &json!({ "github_enabled": null, "github_auto_link_prs_enabled": null })
        )
        .unwrap());
    }

    #[test]
    fn unparseable_settings_are_an_error_not_a_permissive_default() {
        // 非对象 / 字段类型不符 ⇒ Err（关闭意图扫描必须能区分「开着」与「没查出来」）。
        assert!(auto_link_prs_enabled(&json!("nope")).is_err());
        assert!(auto_link_prs_enabled(&json!([])).is_err());
        assert!(auto_link_prs_enabled(&json!({ "github_enabled": "yes" })).is_err());
        assert!(auto_link_prs_enabled(&json!({ "github_auto_link_prs_enabled": 1 })).is_err());
    }
}
