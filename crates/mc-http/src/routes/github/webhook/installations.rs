//! `installation` 事件：卸载时的绑定清理、账号元数据刷新、以及「还没有绑定时的 pending 暂存」。
//!
//! 上游 `handleInstallationEvent`（`github.go:1132-1256`）。三个分组逐字对齐：
//!
//! | action | 动作 |
//! | --- | --- |
//! | `deleted` / `suspend` | 信任**整体失效** ⇒ `DELETE … RETURNING` 掉**每一条** workspace 绑定 + 清 pending + 每个 workspace 一条 `github_installation:deleted` 广播 |
//! | `created` / `new_permissions_accepted` / `unsuspend` | 无绑定 ⇒ 暂存 pending（等 setup 回调消费）；有绑定 ⇒ 刷新展示元数据 + 清 pending + 每条绑定一条 `github_installation:created` 广播 |
//! | 其它 | 确认（202）后什么都不做 |
//!
//! # 三条逐字对齐上游的细节
//!
//! 1. **广播里不出现数字 `installation_id`**：它是 Connect/Disconnect 的管理手柄，非 admin
//!    成员不该看到；前端收到事件只做 installations 查询失效，不读载荷（上游注释逐字）。
//! 2. **`DELETE … RETURNING` 而不是 `DELETE`**：每条广播要能定位到**自己的** workspace ——
//!    没有 `WorkspaceID` 的事件会被实时监听丢掉，已经打开的 Settings 页签就会一直陈旧。
//! 3. **`account_type` 走 `coalesce(…, "User")`，`avatar` 走 `strPtrOrNil`**（空串 ⇒ NULL）；
//!    `account_login` 先 `TrimSpace` 再判空，空 ⇒ 只打一条 warn 就返回（不写半条记录）。
//!
//! 查询落点的理由（为什么不放 `mc-repos/src/github/installation.rs`）见 `docs/32` §18.2 的 D3。

use super::AppState;
use mc_repos::github::installation::{GithubInstallationRepo, GithubInstallationRow};
use mc_vcs_github::payload::{
    installation_account_from_payload, InstallationEventPayload, INSTALLATION_ACTIONS_DELETING,
    INSTALLATION_ACTIONS_UPSERTING,
};

use crate::routes::github::dto::GithubInstallationResponse;
use crate::routes::github::install::installation_created_envelope;

/// `github_installation` 的全列（顺序与 `GithubInstallationRow` 一一对应）。
const INSTALLATION_COLUMNS: &str =
    "id, workspace_id, installation_id, account_login, account_type, \
                                    account_avatar_url, connected_by_id, created_at, updated_at";

/// 上游 `handleInstallationEvent`（`github.go:1132`）。
pub(super) async fn handle_installation_event(
    state: &AppState,
    payload: &InstallationEventPayload,
) {
    let installation_id = payload.installation.id;
    if INSTALLATION_ACTIONS_DELETING.contains(&payload.action.as_str()) {
        let deleted = match delete_installation_bindings(state, installation_id).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, installation_id, "github: delete installation failed");
                return;
            }
        };
        if let Err(error) = GithubInstallationRepo::new(state.db.clone())
            .delete_pending(installation_id)
            .await
        {
            tracing::warn!(%error, installation_id, "github: delete pending installation failed");
        }
        for row in &deleted {
            let envelope = mc_realtime::EventEnvelope::new(
                "github_installation",
                row.workspace_id().to_string(),
                None,
                serde_json::json!({ "id": row.id().to_string() }),
            )
            .with_type("github_installation:deleted");
            state.realtime.publish(envelope);
        }
        return;
    }
    if !INSTALLATION_ACTIONS_UPSERTING.contains(&payload.action.as_str()) {
        return;
    }

    let Some((login, account_type, avatar)) = installation_account_from_payload(payload) else {
        tracing::warn!(
            installation_id,
            "github: installation payload missing account login"
        );
        return;
    };
    let existing = match list_installation_bindings(state, installation_id).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, installation_id, "github: lookup installation failed");
            return;
        }
    };
    if existing.is_empty() {
        // 光凭 webhook 不知道它对应哪个 workspace。若 setup 回调还没建出绑定，就先留一份
        // 账号元数据，等回调建完 `github_installation` 后再消费。
        if let Err(error) = GithubInstallationRepo::new(state.db.clone())
            .upsert_pending(
                installation_id,
                &login,
                Some(&account_type),
                avatar.as_deref(),
            )
            .await
        {
            tracing::warn!(%error, installation_id, "github: store pending installation failed");
        }
        return;
    }
    // 刷新每一条绑定上的展示元数据（`workspace_id` / `connected_by_id` 不动）。
    let refreshed = match refresh_installation_account(
        state,
        installation_id,
        &login,
        &account_type,
        avatar.as_deref(),
    )
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, installation_id, "github: refresh installation failed");
            return;
        }
    };
    if let Err(error) = GithubInstallationRepo::new(state.db.clone())
        .delete_pending(installation_id)
        .await
    {
        tracing::warn!(%error, installation_id, "github: delete pending installation failed");
    }
    for row in &refreshed {
        // 形状 = list 端点的**最弱角色视图**（`installation_id` 不在里面）：WS 扇出没有
        // per-recipient 视角，admin 客户端靠重查列表拿回管理手柄（上游注释逐字）。
        let card = GithubInstallationResponse::from_row(row).without_installation_id();
        state.realtime.publish(installation_created_envelope(
            &row.workspace_id().to_string(),
            &card,
        ));
    }
}

/// 上游 `ListGitHubInstallationsByInstallationID`：一个 installation 绑定的全部 workspace 行。
///
/// 这是扇出（#4855）与「刷新账号元数据」的共同输入 —— `pull_request` 那一族也用它。
pub(super) async fn list_installation_bindings(
    state: &AppState,
    installation_id: i64,
) -> Result<Vec<GithubInstallationRow>, sqlx::Error> {
    let sql = format!(
        "SELECT {INSTALLATION_COLUMNS} FROM github_installation WHERE installation_id = $1"
    );
    sqlx::query_as::<_, GithubInstallationRow>(&sql)
        .bind(installation_id)
        .fetch_all(state.db.pool())
        .await
}

/// 上游 `DeleteGitHubInstallationByInstallationID`：删掉一个 installation 的**全部**绑定，
/// 并把删掉的行返回（每条广播要定位自己的 workspace）。
async fn delete_installation_bindings(
    state: &AppState,
    installation_id: i64,
) -> Result<Vec<GithubInstallationRow>, sqlx::Error> {
    let sql = format!(
        "DELETE FROM github_installation WHERE installation_id = $1 RETURNING {INSTALLATION_COLUMNS}"
    );
    sqlx::query_as::<_, GithubInstallationRow>(&sql)
        .bind(installation_id)
        .fetch_all(state.db.pool())
        .await
}

/// 上游 `UpdateGitHubInstallationAccountByInstallationID`：刷新每条绑定的展示元数据，
/// **不动** `workspace_id` / `connected_by_id`。
async fn refresh_installation_account(
    state: &AppState,
    installation_id: i64,
    account_login: &str,
    account_type: &str,
    account_avatar_url: Option<&str>,
) -> Result<Vec<GithubInstallationRow>, sqlx::Error> {
    let sql = format!(
        "UPDATE github_installation \
         SET account_login = $2, account_type = $3, account_avatar_url = $4, updated_at = now() \
         WHERE installation_id = $1 RETURNING {INSTALLATION_COLUMNS}"
    );
    sqlx::query_as::<_, GithubInstallationRow>(&sql)
        .bind(installation_id)
        .bind(account_login)
        .bind(account_type)
        .bind(account_avatar_url)
        .fetch_all(state.db.pool())
        .await
}
