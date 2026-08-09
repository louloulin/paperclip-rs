//! pc-authz：从 DB 行构建 [`Context`]。
//!
//! 调用方负责：
//! - 提供 `pc_db::Db` 连接
//! - 提供 [`pc_auth::Actor`]
//!
//! 返回 [`Context`]，可以直接传入 [`crate::evaluate`]。

use pc_auth::{Actor, CompanyMembership};
use pc_db::Db;
use pc_repos::company_member::CompanyMemberRepo;
use pc_repos::principal_permission_grant::PrincipalPermissionGrantRepo;
use uuid::Uuid;

use crate::policy::Context;
use crate::types::{CompanyRole, PermissionKey};

/// 从 DB 行构造 [`Context`]。
///
/// 行为：
/// - **System**：返回空 Context（system 全局 allow）。
/// - **Anonymous**：返回空 Context。
/// - **User**：
///   - 加载所有 active membership（`principal_type='user'`）
///   - 在每个公司加载 grants
///   - 推断 role（最高 role）
/// - **Agent**：
///   - 仅把 `agent_id`/`company_id` 作为单条 membership 注入
///   - 加载 grants
///
/// 该函数不做 cache。调用方按需缓存结果。
pub async fn build_context(db: &Db, actor: &Actor) -> Context {
    match actor {
        Actor::System | Actor::Anonymous => Context::anonymous(),
        Actor::User {
            id,
            is_instance_admin,
            ..
        } => build_user_context(db, id, *is_instance_admin).await,
        Actor::Agent { id, .. } => build_agent_context(db, *id).await,
    }
}

async fn build_user_context(db: &Db, user_id: &str, is_instance_admin: bool) -> Context {
    if is_instance_admin {
        // 管理员短路：不需要加载 membership / grants
        return Context {
            is_instance_admin: true,
            ..Context::default()
        };
    }

    let repo = CompanyMemberRepo::new(db);
    let rows = repo
        .list_active_for_principal_user(user_id)
        .await
        .unwrap_or_default();

    let mut memberships: Vec<CompanyMembership> = Vec::with_capacity(rows.len());
    let mut top_role: Option<CompanyRole> = None;
    for (cid, role) in rows {
        let cid_parsed = match Uuid::parse_str(&cid) {
            Ok(c) => c,
            Err(_) => continue,
        };
        memberships.push(CompanyMembership {
            company_id: cid_parsed,
            role: Some(role.clone()),
            status: Some("active".into()),
        });
        let parsed = CompanyRole::from_str_opt(&role);
        top_role = match (top_role, parsed) {
            (None, Some(r)) => Some(r),
            (Some(a), Some(b)) if b.is_admin_or_above() && !a.is_admin_or_above() => Some(b),
            (Some(a), Some(b)) if a.is_admin_or_above() && b.is_admin_or_above() => {
                Some(if rank(b) > rank(a) { b } else { a })
            }
            (Some(a), _) => Some(a),
            (None, None) => None,
        };
    }

    // 收集 grants：跨该公司集合去重
    let grant_repo = PrincipalPermissionGrantRepo::new(db);
    let mut grants_set: std::collections::BTreeSet<PermissionKey> = Default::default();
    for m in &memberships {
        if let Ok(rows) = grant_repo
            .list_for_principal(m.company_id, "user", user_id)
            .await
        {
            for row in rows {
                if let Some(p) = parse_permission_key(&row.permission_key) {
                    grants_set.insert(p);
                }
            }
        }
    }

    Context {
        memberships,
        grants: grants_set.into_iter().collect(),
        role: top_role,
        is_instance_admin: false,
        ..Context::default()
    }
}

async fn build_agent_context(db: &Db, agent_id: Uuid) -> Context {
    // agent 在 company_memberships 表中以 principal_type='agent' 列出
    let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT company_id::text, membership_role, status FROM company_memberships \
         WHERE principal_id = $1 AND principal_type = 'agent' AND status = 'active'",
    )
    .bind(agent_id.to_string())
    .fetch_all(db.pool())
    .await
    .unwrap_or_default();

    let memberships: Vec<CompanyMembership> = rows
        .into_iter()
        .filter_map(|(cid, role, status)| {
            Uuid::parse_str(&cid).ok().map(|c| CompanyMembership {
                company_id: c,
                role,
                status,
            })
        })
        .collect();

    let grant_repo = PrincipalPermissionGrantRepo::new(db);
    let mut grants_set: std::collections::BTreeSet<PermissionKey> = Default::default();
    for m in &memberships {
        if let Ok(rows) = grant_repo
            .list_for_principal(m.company_id, "agent", &agent_id.to_string())
            .await
        {
            for row in rows {
                if let Some(p) = parse_permission_key(&row.permission_key) {
                    grants_set.insert(p);
                }
            }
        }
    }

    Context {
        memberships,
        grants: grants_set.into_iter().collect(),
        role: None,
        is_instance_admin: false,
        ..Context::default()
    }
}

fn parse_permission_key(s: &str) -> Option<PermissionKey> {
    match s {
        "agents:create" => Some(PermissionKey::AgentsCreate),
        "agents:configure" => Some(PermissionKey::AgentsConfigure),
        "agents:suggest-changes" => Some(PermissionKey::AgentsSuggestChanges),
        "skills:create" => Some(PermissionKey::SkillsCreate),
        "skills:suggest-changes" => Some(PermissionKey::SkillsSuggestChanges),
        "environments:manage" => Some(PermissionKey::EnvironmentsManage),
        "tools:admin" => Some(PermissionKey::ToolsAdmin),
        "tools:manage_connections" => Some(PermissionKey::ToolsManageConnections),
        "tools:manage_profiles" => Some(PermissionKey::ToolsManageProfiles),
        "tools:view_audit" => Some(PermissionKey::ToolsViewAudit),
        "audit:view_agent_actions" => Some(PermissionKey::AuditViewAgentActions),
        "tools:use" => Some(PermissionKey::ToolsUse),
        "tools:manage_runtime" => Some(PermissionKey::ToolsManageRuntime),
        "inbox:manage" => Some(PermissionKey::InboxManage),
        "users:invite" => Some(PermissionKey::UsersInvite),
        "users:manage_permissions" => Some(PermissionKey::UsersManagePermissions),
        "tasks:assign" => Some(PermissionKey::TasksAssign),
        "tasks:assign_scope" => Some(PermissionKey::TasksAssignScope),
        "tasks:manage_active_checkouts" => Some(PermissionKey::TasksManageActiveCheckouts),
        "pipelines:write" => Some(PermissionKey::PipelinesWrite),
        "joins:approve" => Some(PermissionKey::JoinsApprove),
        _ => None,
    }
}

fn rank(r: CompanyRole) -> u8 {
    match r {
        CompanyRole::Owner => 4,
        CompanyRole::Admin => 3,
        CompanyRole::Operator => 2,
        CompanyRole::Member => 1,
        CompanyRole::Viewer => 0,
    }
}

/// R555：从 DB 构造 Context，并从 issue body 提取 mention IDs。
///
/// 这是一个高阶 helper,把 [`build_context`] 与 [`Context::with_mentions_from_body`]
/// 合并为一个调用。典型用法是 issue 路由(`POST /api/issues`、`POST /api/issues/:id/comments`),
/// 在 evaluate 之前注入 issue body / comment body 中的 mention。
///
/// 行为：
/// 1. 调用 `build_context` 加载 memberships + grants + role
/// 2. 用 `with_mentions_from_body` 解析 body 中的 `agent://` / `user://` 提及
/// 3. 返回带 mention 信息的 Context
///
/// **单次 DB 查询**:复用 `build_context` 的查询,无额外 round-trip。
pub async fn build_context_with_issue_body(
    db: &Db,
    actor: &Actor,
    issue_body: &str,
) -> Context {
    let ctx = build_context(db, actor).await;
    ctx.with_mentions_from_body(issue_body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_permission_key_round_trip() {
        for key in [
            PermissionKey::AgentsCreate,
            PermissionKey::AgentsConfigure,
            PermissionKey::SkillsCreate,
            PermissionKey::ToolsAdmin,
            PermissionKey::ToolsUse,
            PermissionKey::InboxManage,
            PermissionKey::UsersInvite,
            PermissionKey::UsersManagePermissions,
            PermissionKey::TasksAssign,
            PermissionKey::PipelinesWrite,
            PermissionKey::JoinsApprove,
        ] {
            assert_eq!(parse_permission_key(key.as_str()), Some(key));
        }
        assert_eq!(parse_permission_key("unknown:foo"), None);
    }

    #[test]
    fn parse_permission_key_handles_all_21() {
        let all_strs = [
            "agents:create",
            "agents:configure",
            "agents:suggest-changes",
            "skills:create",
            "skills:suggest-changes",
            "environments:manage",
            "tools:admin",
            "tools:manage_connections",
            "tools:manage_profiles",
            "tools:view_audit",
            "audit:view_agent_actions",
            "tools:use",
            "tools:manage_runtime",
            "inbox:manage",
            "users:invite",
            "users:manage_permissions",
            "tasks:assign",
            "tasks:assign_scope",
            "tasks:manage_active_checkouts",
            "pipelines:write",
            "joins:approve",
        ];
        for s in all_strs {
            assert!(parse_permission_key(s).is_some(), "missing parser for {s}");
        }
    }
}
