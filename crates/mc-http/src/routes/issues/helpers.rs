//! `/api/issues*` 的错误 / 参数小工具（从 `issues.rs` 拆出，R7 单文件 800 行上限；
//! `scripts/file_size_check.py` + 门 ⑩ 执行）。
#![allow(clippy::option_option)]

use crate::state::AppState;
use axum::http::HeaderMap;
use chrono::NaiveDate;
use mc_core::issue::AssigneeType;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::RepoError;
use serde::{Deserialize, Deserializer};
use uuid::Uuid;

use super::context::parse_target_id;
use super::DATE_FORMAT;

// ---------------------------------------------------------------------------
// 错误 / 参数小工具
// ---------------------------------------------------------------------------

pub(crate) fn validation(message: impl Into<String>) -> Error {
    Error::Validation {
        message: message.into(),
        details: Vec::new(),
    }
}

pub(crate) fn repo_err(e: RepoError) -> Error {
    match e {
        RepoError::NotFound => Error::NotFound {
            resource: "issue".into(),
        },
        RepoError::Conflict => Error::Conflict {
            message: "issue was modified concurrently; refetch and retry".into(),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

pub(crate) fn status_repo_err(e: RepoError) -> Error {
    match e {
        RepoError::NotFound => Error::NotFound {
            resource: "issue_status".into(),
        },
        RepoError::Conflict => Error::Conflict {
            message: "issue status key is reserved or still in use".into(),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

pub(crate) fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 日期补丁的三态转换：`None` 不动、`null`/`""` 清空、`YYYY-MM-DD` 写入。
pub(crate) fn parse_date_patch(
    field: &str,
    value: Option<Option<String>>,
) -> Result<Option<Option<NaiveDate>>, Error> {
    match value {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(raw)) if raw.trim().is_empty() => Ok(Some(None)),
        Some(Some(raw)) => NaiveDate::parse_from_str(raw.trim(), DATE_FORMAT)
            .map(|d| Some(Some(d)))
            .map_err(|_| validation(format!("{field} must be formatted as YYYY-MM-DD"))),
    }
}

/// `Option<Option<T>>` 的 serde helper：字段缺失 → `None`，`null` → `Some(None)`，
/// 有值 → `Some(Some(v))`。
pub(crate) fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// 上游把 assignee 类型写作 `member`（历史命名），本仓统一用 `user`。
pub(crate) fn normalize_assignee_type(raw: &str) -> &str {
    match raw.trim() {
        "member" => "user",
        other => other,
    }
}

/// `(assignee_type, assignee_id)` 的**存在性**校验（上游 `validateAssigneePair` 的移植，
/// `server/internal/handler/issue.go:3971`）。
///
/// 上游在三个入口调用它：`CreateIssue`（`L3138`）、`UpdateIssue`（`L3814`，
/// 「只要补丁碰到 assignee 任一半段」就校验补丁后的最终状态）、`BatchUpdateIssues`（`L4562`）。
/// 语义：两者同时缺失 = 未指派（合法）；只给一个 → 400；两个都给 → 目标必须在本
/// workspace 内真实存在。
///
/// | 上游 `assignee_type` | 校验 | 失败 |
/// | --- | --- | --- |
/// | `member` | `member(workspace_id, user_id)` 命中 | 400 |
/// | `agent` | `agent(id, workspace_id)` 命中且未归档 | 400 |
/// | `squad` | `squad` 命中、未归档、leader agent 存在且未归档 | 400 |
/// | 其它 | — | 400 `assignee_type must be 'member', 'agent', or 'squad'` |
///
/// 本仓偏离（`docs/11-M2-ISSUE.md` §5）：
///
/// 1. 入参 `member` 被 `normalize_assignee_type` 归一成 `user`，故这里 `User` 分支查的是
///    `member` 表（本仓 `issue.assignee_type` 无 `member` 取值）；
/// 2. `autopilot` 是 `mc-core` / `0001` CHECK 允许的第四种类型，这里同样只做存在性 ——
///    上游 handler 直接拒绝 `autopilot`，本仓保留该能力 ⇒ 已登记为已知偏离；
/// 3. 上游 agent/squad 分支还有一道 `canInvokeAgent` 权限门（403，"you do not have
///    permission to assign work to this agent"）；本仓 M2 没有 agent 可见性/私有点判定面，
///    故只做存在性 + 归档位（`archived_at`）。
///
/// 目标 id 形态非法 → 400（`assignee_id must be a uuid`，上游 `parseUUIDOrBadRequest`）。
pub(crate) async fn validate_assignee_target(
    state: &AppState,
    workspace_id: Id,
    kind: AssigneeType,
    raw_id: &str,
) -> Result<(), Error> {
    let target = parse_target_id("assignee_id", raw_id)?;
    let pool = state.db.pool();
    let db = |e: sqlx::Error| Error::Database(e.to_string());

    match kind {
        AssigneeType::User => {
            let found: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2)",
            )
            .bind(workspace_id.0)
            .bind(target.0)
            .fetch_one(pool)
            .await
            .map_err(db)?;
            if !found {
                return Err(validation(
                    "assignee_id does not refer to a member of this workspace",
                ));
            }
        }
        AssigneeType::Agent => {
            let archived: Option<bool> = sqlx::query_scalar(
                "SELECT archived_at IS NOT NULL FROM agent WHERE workspace_id = $1 AND id = $2",
            )
            .bind(workspace_id.0)
            .bind(target.0)
            .fetch_optional(pool)
            .await
            .map_err(db)?;
            match archived {
                Some(false) => {}
                Some(true) => return Err(validation("cannot assign to an archived agent")),
                None => {
                    return Err(validation(
                        "assignee_id does not refer to an agent of this workspace",
                    ))
                }
            }
        }
        AssigneeType::Squad => {
            // 上游 `squad.leader_id`（`NOT NULL`）；本仓退役的 `0001` 里叫 `leader_agent_id`。
            let row: Option<(bool, Option<Uuid>)> = sqlx::query_as(
                "SELECT archived_at IS NOT NULL, leader_id FROM squad \
                 WHERE workspace_id = $1 AND id = $2",
            )
            .bind(workspace_id.0)
            .bind(target.0)
            .fetch_optional(pool)
            .await
            .map_err(db)?;
            let Some((archived, leader)) = row else {
                return Err(validation(
                    "assignee_id does not refer to a squad in this workspace",
                ));
            };
            if archived {
                return Err(validation("cannot assign to an archived squad"));
            }
            let leader_ok = match leader {
                Some(leader_id) => sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS (SELECT 1 FROM agent \
                     WHERE id = $1 AND workspace_id = $2 AND archived_at IS NULL)",
                )
                .bind(leader_id)
                .bind(workspace_id.0)
                .fetch_one(pool)
                .await
                .map_err(db)?,
                // 无 leader 的 squad：上游 `GetAgent(NULL)` 同样报错（400）。
                None => false,
            };
            if !leader_ok {
                return Err(validation(
                    "squad leader is archived; cannot assign to this squad",
                ));
            }
        }
        AssigneeType::Autopilot => {
            let found: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM autopilot WHERE workspace_id = $1 AND id = $2)",
            )
            .bind(workspace_id.0)
            .bind(target.0)
            .fetch_one(pool)
            .await
            .map_err(db)?;
            if !found {
                return Err(validation(
                    "assignee_id does not refer to an autopilot of this workspace",
                ));
            }
        }
    }
    Ok(())
}

/// 解析 `attachment_ids`（上游 `parseUUIDSliceOrBadRequest`，`handler.go:678`）。
///
/// 上游在 `CreateIssue` 里逐元素 `util.ParseUUID`，任一无效应即 400 `invalid attachment_ids`，
/// **且发生在任何写库之前**（`TestCreateIssueRejectsMalformedAttachmentIDBeforeWrite` 断言
/// 创建前后 issue 计数不变）。本仓 M2 没有 `attachment` 表（storage 面归 M5，见 `docs/11` §6），
/// 因此这里只校验形态、不绑定 —— 返回值有意忽略，调用点写作 `_attachment_ids`。
pub(crate) fn parse_attachment_ids(raw: &[String]) -> Result<Vec<Uuid>, Error> {
    let mut parsed = Vec::with_capacity(raw.len());
    for id in raw {
        parsed.push(Uuid::parse_str(id.trim()).map_err(|_| validation("invalid attachment_ids"))?);
    }
    Ok(parsed)
}
