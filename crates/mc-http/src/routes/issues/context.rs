//! `/api/issues*` 的请求上下文：workspace 解析 + status key 目录（从 `issues.rs` 拆出，
//! R7 单文件 800 行上限）。

use crate::state::AppState;
use axum::http::HeaderMap;
use mc_core::status::{StatusCategory, CANONICAL_KEYS};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::issue::{IssueRepo, IssueRow};
use mc_repos::issue_status::{parse_category, IssueStatusRepo, DEFAULT_STATUSES};
use serde::Deserialize;
use std::collections::HashMap;
use uuid::Uuid;

use super::helpers::{header_str, repo_err, status_repo_err, validation};
use super::{WORKSPACE_ID_HEADER, WORKSPACE_SLUG_HEADER};

// ---------------------------------------------------------------------------
// workspace 解析
// ---------------------------------------------------------------------------

/// workspace 选择器：`?workspace_id=` / `?workspace_slug=`（header 优先）。
#[derive(Debug, Default, Deserialize)]
pub struct WorkspaceQuery {
    pub workspace_id: Option<String>,
    pub workspace_slug: Option<String>,
}

/// 解析目标 workspace：header `x-workspace-id` → header `x-workspace-slug` →
/// `?workspace_id` → `?workspace_slug`（slug 要求 `archived_at IS NULL`）。
///
/// 上游是从 session 的 "current workspace" / task token 里取；本仓 M1 的 auth 只有
/// `X-Multica-User-Id` dev-mode 提取器，没有 workspace 上下文，因此显式传参（详见
/// `docs/11-M2-ISSUE.md`）。四个来源都缺 → 400。
pub(crate) async fn resolve_workspace(
    state: &AppState,
    headers: &HeaderMap,
    query: &WorkspaceQuery,
) -> Result<Id, Error> {
    if let Some(raw) = header_str(headers, WORKSPACE_ID_HEADER).or(query.workspace_id.as_deref()) {
        return Id::parse(raw).map_err(|_| validation("workspace_id must be a uuid"));
    }
    if let Some(slug) =
        header_str(headers, WORKSPACE_SLUG_HEADER).or(query.workspace_slug.as_deref())
    {
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM workspace WHERE slug = $1 AND archived_at IS NULL")
                .bind(slug)
                .fetch_optional(state.db.pool())
                .await
                .map_err(|e| Error::Database(e.to_string()))?;
        return row.map(|(id,)| Id::from(id)).ok_or(Error::NotFound {
            resource: "workspace".into(),
        });
    }
    Err(validation(
        "workspace_id (or workspace_slug) is required: pass the x-workspace-id header or ?workspace_id=",
    ))
}

pub(crate) fn issue_repo(state: &AppState) -> IssueRepo {
    IssueRepo::new(state.db.clone())
}

pub(crate) fn status_repo(state: &AppState) -> IssueStatusRepo {
    IssueStatusRepo::new(state.db.clone())
}

/// `:id` 既接受 UUID 也接受 identifier（`LUM-1348`）。
pub(crate) async fn load_issue(
    repo: &IssueRepo,
    workspace_id: Id,
    raw: &str,
) -> Result<IssueRow, Error> {
    let needle = raw.trim();
    match Id::parse(needle) {
        Ok(id) => repo.get(workspace_id, id).await.map_err(repo_err),
        Err(_) => repo
            .get_by_identifier(workspace_id, &needle.to_uppercase())
            .await
            .map_err(repo_err),
    }
}

pub(crate) fn parse_target_id(field: &str, raw: &str) -> Result<Id, Error> {
    Id::parse(raw.trim()).map_err(|_| validation(format!("{field} must be a uuid")))
}

// ---------------------------------------------------------------------------
// status 目录（内置 7 个 + workspace 自定义）
// ---------------------------------------------------------------------------

/// status key → (展示名, 生命周期分类)。内置目录打底，DB 里的自定义 status 覆盖。
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusCatalog {
    pub(crate) names: HashMap<String, String>,
    pub(crate) categories: HashMap<String, StatusCategory>,
}

impl StatusCatalog {
    pub(crate) fn with_builtins() -> Self {
        let mut catalog = Self::default();
        for (key, name, category, _position) in DEFAULT_STATUSES {
            catalog.names.insert(key.to_string(), name.to_string());
            if let Some(parsed) = parse_category(category) {
                catalog.categories.insert(key.to_string(), parsed);
            }
        }
        catalog
    }

    pub(crate) fn insert(&mut self, key: &str, name: &str, category: Option<StatusCategory>) {
        self.names.insert(key.to_string(), name.to_string());
        if let Some(category) = category {
            self.categories.insert(key.to_string(), category);
        }
    }

    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.names.contains_key(key)
    }

    pub(crate) fn category_of(&self, key: &str) -> Option<StatusCategory> {
        self.categories.get(key).copied()
    }

    /// 某个分类下的全部 key（内置 + 自定义）——用于 `status_category` 过滤展开。
    pub(crate) fn keys_in_category(&self, category: StatusCategory) -> Vec<String> {
        self.categories
            .iter()
            .filter(|(_, c)| **c == category)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// 自定义 status 的展示名（内置 key 返回 `None`，上游用空 `status_name` 表示内置）。
    pub(crate) fn custom_name(&self, key: &str) -> Option<String> {
        if CANONICAL_KEYS.contains(&key) {
            return None;
        }
        self.names.get(key).cloned()
    }

    /// 某个 issue 的展示名（自定义优先，其次行上的 `status_name`，内置回退空串）。
    pub(crate) fn display_name(&self, row: &IssueRow) -> String {
        if let Some(name) = row.status_name.as_deref().filter(|n| !n.is_empty()) {
            return name.to_string();
        }
        if CANONICAL_KEYS.contains(&row.status.as_str()) {
            return String::new();
        }
        self.names.get(&row.status).cloned().unwrap_or_default()
    }
}

pub(crate) async fn load_catalog(
    state: &AppState,
    workspace_id: Id,
) -> Result<StatusCatalog, Error> {
    let mut catalog = StatusCatalog::with_builtins();
    let rows = status_repo(state)
        .list(workspace_id)
        .await
        .map_err(status_repo_err)?;
    for row in rows {
        catalog.insert(&row.key, &row.name, parse_category(&row.category));
    }
    Ok(catalog)
}
