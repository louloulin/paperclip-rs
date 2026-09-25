//! 三族 CI 事件（`check_suite` / `check_run` / `status`）—— 上游 `triggerPRRefreshFromCIEvent`
//! （`github.go:1467-1534`）。
//!
//! **Plan C 的判据（上游注释逐字）**：这些事件是**纯触发器** —— 「their payload is never read
//! for display. Each just asks the API pipeline to re-fetch the authoritative snapshot for the
//! PR(s) it concerns.」⇒ 载荷里的 suite / conclusion 什么都不写，只用来定位要刷新的 PR：
//!
//! 1. `check_suite` / `check_run` 直接带 `pull_requests[].number`（去重、丢掉 0）；
//! 2. 没有 PR 号时（`status` 事件，或 `pull_requests` 为空的 check 事件）用 commit SHA 回查
//!    `github_pull_request.head_sha`；
//! 3. `enabled() == false`（缺 App 私钥）⇒ 一次都不入队（上游第一条短路）。
//!
//! # 与上游的两处差异（登记 `docs/32` §19.2 的 D4）
//!
//! - 上游**直接**按载荷里的号入队，不查库（未知 PR 也入队，由 Manager 稍后自己解析）；
//!   本仓端口的定位键含 `workspace_id`（`mc-vcs-github/src/port.rs`）⇒ 必须先把号映射回
//!   (workspace, 号) 才能入队，于是**只有已镜像的 PR** 能入队。未镜像的 PR 本来也没有可写
//!   快照的行，所以这条收窄在结果上等价。
//! - 上游用 `seen map[int32]struct{}` 按**号**去重；本仓按 `(workspace, 号)` 去重
//!   （同一号可以合法地属于多个 workspace）。

use mc_repos::github::check_suite::{GithubCheckSuiteRepo, PrNumbersByHeadSha, PrNumbersByRepo};
use mc_vcs_github::payload::CiEventPayload;
use mc_vcs_github::port::{PrRefreshRequest, RefreshReason, SharedPrRefresh};
use uuid::Uuid;

use super::{pr_refresh_port, AppState};

/// 上游 `triggerPRRefreshFromCIEvent`（`github.go:1467`）。
pub(super) async fn trigger_pr_refresh_from_ci_event(state: &AppState, body: &[u8]) {
    let port = pr_refresh_port();
    if !port.enabled() {
        return;
    }
    let Ok(payload) = serde_json::from_slice::<CiEventPayload>(body) else {
        return;
    };
    let installation_id = payload.installation.id;
    if installation_id == 0 || payload.repository.name.is_empty() {
        return;
    }
    let repo = GithubCheckSuiteRepo::new(state.db.clone());

    let direct = payload.direct_pull_request_numbers();
    if !direct.is_empty() {
        let params = PrNumbersByRepo {
            installation_id,
            repo_owner: payload.repository.owner.login.clone(),
            repo_name: payload.repository.name.clone(),
        };
        match repo.list_workspace_pr_numbers(&params, &direct).await {
            Ok(rows) => {
                let head_sha = payload.head_sha_for_lookup();
                for (workspace_id, number) in rows {
                    enqueue_webhook_refresh(
                        &port,
                        workspace_id,
                        &params.repo_owner,
                        &params.repo_name,
                        number,
                        head_sha.clone(),
                    );
                }
            }
            Err(error) => tracing::warn!(%error, "github: resolve PR numbers failed"),
        }
        return;
    }
    let Some(head_sha) = payload.head_sha_for_lookup() else {
        return;
    };
    let params = PrNumbersByHeadSha {
        installation_id,
        repo_owner: payload.repository.owner.login.clone(),
        repo_name: payload.repository.name.clone(),
        head_sha: head_sha.clone(),
    };
    match repo.list_workspace_pr_numbers_by_head_sha(&params).await {
        Ok(rows) => {
            for (workspace_id, number) in rows {
                enqueue_webhook_refresh(
                    &port,
                    workspace_id,
                    &params.repo_owner,
                    &params.repo_name,
                    number,
                    Some(head_sha.clone()),
                );
            }
        }
        Err(error) => tracing::warn!(%error, "github: resolve PR numbers by head sha failed"),
    }
}

fn enqueue_webhook_refresh(
    port: &SharedPrRefresh,
    workspace_id: Uuid,
    repo_owner: &str,
    repo_name: &str,
    pr_number: i32,
    head_sha: Option<String>,
) {
    port.enqueue(PrRefreshRequest {
        workspace_id: mc_core::Id(workspace_id),
        repo_owner: repo_owner.to_string(),
        repo_name: repo_name.to_string(),
        pr_number,
        head_sha,
        reason: RefreshReason::Webhook,
    });
}
