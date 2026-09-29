//! inbox 请求上下文：workspace/user 解析 + 仓储装配。
//!
//! 从 `routes/inbox.rs` 拆出（门 ⑩ 第 8 批）。**0 路由、0 行为变更**：
//! 父模块用 `pub(crate) use` / `pub use` 把符号原样重导出，外部路径逐字不变。

use std::collections::HashMap;
use axum::http::HeaderMap;

use mc_core::Id;
use mc_errors::Error;
use mc_repos::inbox::{InboxItemRow, InboxRepo};

use crate::routes::auth_user::AuthUser;
use crate::routes::inbox::query::{bad_request, query_value, repo_err};
use crate::routes::inbox::WORKSPACE_ID_HEADER;
use crate::routes::invitations::require_workspace_member;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 请求上下文
// ---------------------------------------------------------------------------

/// 一次 inbox 请求的 workspace/user 上下文 + 仓储。
pub(super) struct InboxScope {
    pub(super) workspace_id: Id,
    pub(super) user_id: Id,
    pub(super) repo: InboxRepo,
}

impl InboxScope {
    /// 解析 workspace（400）→ 校验成员身份（404）→ 装配仓储。
    pub(super) async fn resolve(
        state: &AppState,
        user: AuthUser,
        headers: &HeaderMap,
        query: &HashMap<String, String>,
    ) -> Result<Self, Error> {
        let workspace_id = resolve_workspace_id(headers, query)?;
        require_workspace_member(state, workspace_id, user.id()).await?;
        Ok(Self {
            workspace_id,
            user_id: user.id(),
            repo: InboxRepo::new(state.db.clone()),
        })
    }

    /// `loadInboxItemForUser` 的等价物：归属校验（别人的通知与不存在的通知都 404）。
    pub(super) async fn load_item(&self, raw_id: &str) -> Result<InboxItemRow, Error> {
        let id = Id::parse(raw_id).map_err(|_| bad_request("invalid inbox item id"))?;
        self.repo
            .get_for_user(id, self.workspace_id, self.user_id)
            .await
            .map_err(|e| repo_err(e, "inbox item"))
    }
}

pub(crate) fn resolve_workspace_id(
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Result<Id, Error> {
    let raw = headers
        .get(WORKSPACE_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .or_else(|| query_value(query, "workspace_id"));
    match raw {
        Some(raw) => Id::parse(raw).map_err(|_| bad_request("invalid workspace id")),
        None => Err(bad_request("invalid workspace id")),
    }
}

