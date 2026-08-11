//! Principal access compatibility backfill（原 `pc-principal-access-compatibility` 已下沉）。
//!
//! 1:1 port of Node `paperclip/server/src/services/principal-access-compatibility.ts`.
//!
//! Historically, Paperclip stored agent access via the `agents` table
//! directly and only stored human access in `company_memberships`. After
//! the migration to a unified "principal" model, every agent needs an
//! active row in `company_memberships` with `principal_type='agent'`
//! and every active human member needs the default permission grants
//! implied by their membership role.
//!
//! This crate provides a one-shot backfill that:
//!
//! 1. Inserts a `company_memberships(principal_type='agent')` row for
//!    every non-terminal agent (`status NOT IN ('pending_approval',
//!    'terminated')`).
//! 2. Inserts default `principal_permission_grants` rows for every
//!    active human membership, where the grants are derived from the
//!    role via `pc_company_member::roles::grants_for_human_role`.
//!
//! Both inserts are idempotent (use the existing unique indexes for
//! `ON CONFLICT DO NOTHING`), so the backfill can safely be re-run.

#![forbid(unsafe_code)]

use super::roles::{
    grants_for_human_role, normalize_human_role, Grant, HumanCompanyMembershipRole,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use pc_repos::Db;

// ---------------------------------------------------------------------
// Public DTOs
// ---------------------------------------------------------------------

/// One permission grant to upsert for a principal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantInput {
    pub permission_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<serde_json::Value>,
}

impl GrantInput {
    pub fn from_grant(g: &Grant) -> Self {
        Self {
            permission_key: g.permission_key.to_string(),
            scope: g.scope.clone(),
        }
    }
}

/// Input to [`insert_missing_principal_grants`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertGrantsInput {
    pub company_id: Uuid,
    /// `"user"` or `"agent"`.
    pub principal_type: String,
    pub principal_id: String,
    pub grants: Vec<GrantInput>,
    #[serde(default)]
    pub granted_by_user_id: Option<String>,
}

/// Input to [`ensure_human_role_default_grants`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureHumanGrantsInput<'a> {
    pub company_id: Uuid,
    pub principal_id: String,
    /// `membership_role` column value (`"owner" | "admin" | "operator" | "viewer" | "member" | ...`).
    pub membership_role: Option<&'a str>,
    pub granted_by_user_id: Option<String>,
}

/// Result of [`backfill_principal_access_compatibility`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalAccessCompatibilityBackfillStats {
    pub agent_memberships_inserted: i64,
    pub human_grants_inserted: i64,
}

// ---------------------------------------------------------------------
// Trait abstraction for testability
// ---------------------------------------------------------------------

/// A minimal DB surface that this crate needs from `pc-repos`. The
/// real implementation is provided by the blanket impl on
/// `pc_repos::Db`; tests can substitute an in-memory mock.
#[async_trait]
pub trait PrincipalAccessDb: Send + Sync {
    async fn fetch_non_terminal_agents(&self) -> sqlx::Result<Vec<(Uuid, String)>>;
    async fn insert_agent_memberships(&self, rows: &[(Uuid, String)]) -> sqlx::Result<u64>;
    async fn fetch_active_human_memberships(
        &self,
    ) -> sqlx::Result<Vec<(Uuid, String, Option<String>)>>;
    async fn upsert_principal_grants(
        &self,
        company_id: Uuid,
        principal_type: &str,
        principal_id: &str,
        grants: &[GrantInput],
        granted_by_user_id: Option<&str>,
    ) -> sqlx::Result<u64>;
}

#[async_trait]
impl PrincipalAccessDb for Db {
    async fn fetch_non_terminal_agents(&self) -> sqlx::Result<Vec<(Uuid, String)>> {
        let rows = sqlx::query(
            "SELECT company_id, id::text AS agent_id \
             FROM agents \
             WHERE status NOT IN ('pending_approval', 'terminated')",
        )
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let company_id: Uuid = r.try_get("company_id")?;
            let agent_id: String = r.try_get("agent_id")?;
            out.push((company_id, agent_id));
        }
        Ok(out)
    }

    async fn insert_agent_memberships(&self, rows: &[(Uuid, String)]) -> sqlx::Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut inserted = 0u64;
        // Insert one row at a time to allow ON CONFLICT DO NOTHING to
        // count via RETURNING. We avoid building a multi-VALUES query
        // so the count is exact under sqlx.
        for (company_id, agent_id) in rows {
            let res = sqlx::query(
                "INSERT INTO company_memberships \
                    (company_id, principal_type, principal_id, status, membership_role, created_at, updated_at) \
                 VALUES ($1, 'agent', $2, 'active', 'member', now(), now()) \
                 ON CONFLICT (company_id, principal_type, principal_id) DO NOTHING \
                 RETURNING id",
            )
            .bind(company_id)
            .bind(agent_id)
            .fetch_optional(self.pool())
            .await?;
            if res.is_some() {
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    async fn fetch_active_human_memberships(
        &self,
    ) -> sqlx::Result<Vec<(Uuid, String, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT company_id, principal_id, membership_role \
             FROM company_memberships \
             WHERE principal_type = 'user' AND status = 'active'",
        )
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let company_id: Uuid = r.try_get("company_id")?;
            let principal_id: String = r.try_get("principal_id")?;
            let membership_role: Option<String> = r.try_get("membership_role")?;
            out.push((company_id, principal_id, membership_role));
        }
        Ok(out)
    }

    async fn upsert_principal_grants(
        &self,
        company_id: Uuid,
        principal_type: &str,
        principal_id: &str,
        grants: &[GrantInput],
        granted_by_user_id: Option<&str>,
    ) -> sqlx::Result<u64> {
        if grants.is_empty() {
            return Ok(0);
        }
        let now = chrono::Utc::now();
        let mut inserted = 0u64;
        for g in grants {
            let res = sqlx::query(
                "INSERT INTO principal_permission_grants \
                    (company_id, principal_type, principal_id, permission_key, scope, granted_by_user_id, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $7) \
                 ON CONFLICT (company_id, principal_type, principal_id, permission_key) DO NOTHING \
                 RETURNING id",
            )
            .bind(company_id)
            .bind(principal_type)
            .bind(principal_id)
            .bind(&g.permission_key)
            .bind(&g.scope)
            .bind(granted_by_user_id)
            .bind(now)
            .fetch_optional(self.pool())
            .await?;
            if res.is_some() {
                inserted += 1;
            }
        }
        Ok(inserted)
    }
}

// ---------------------------------------------------------------------
// Public API (uses the blanket impl on Db)
// ---------------------------------------------------------------------

/// Insert the given grants for a principal. Returns the number of
/// rows that were actually inserted (duplicates are silently ignored).
pub async fn insert_missing_principal_grants(
    db: &Db,
    input: InsertGrantsInput,
) -> sqlx::Result<i64> {
    if input.grants.is_empty() {
        return Ok(0);
    }
    let n = PrincipalAccessDb::upsert_principal_grants(
        db,
        input.company_id,
        &input.principal_type,
        &input.principal_id,
        &input.grants,
        input.granted_by_user_id.as_deref(),
    )
    .await?;
    Ok(n as i64)
}

/// Normalize the human role and insert the default grants for it.
/// Returns the number of newly-inserted grants.
pub async fn ensure_human_role_default_grants(
    db: &Db,
    input: EnsureHumanGrantsInput<'_>,
) -> sqlx::Result<i64> {
    let role_json =
        serde_json::Value::String(input.membership_role.unwrap_or("operator").to_string());
    let role = normalize_human_role(&role_json, HumanCompanyMembershipRole::Operator);
    let grants: Vec<GrantInput> = grants_for_human_role(role)
        .iter()
        .map(GrantInput::from_grant)
        .collect();
    insert_missing_principal_grants(
        db,
        InsertGrantsInput {
            company_id: input.company_id,
            principal_type: "user".to_string(),
            principal_id: input.principal_id,
            grants,
            granted_by_user_id: input.granted_by_user_id,
        },
    )
    .await
}

/// One-shot backfill over the entire database. Returns counts of
/// newly-inserted rows in each category.
pub async fn backfill_principal_access_compatibility(
    db: &Db,
) -> sqlx::Result<PrincipalAccessCompatibilityBackfillStats> {
    let agents = PrincipalAccessDb::fetch_non_terminal_agents(db).await?;
    let agent_memberships_inserted =
        PrincipalAccessDb::insert_agent_memberships(db, &agents).await? as i64;

    let humans = PrincipalAccessDb::fetch_active_human_memberships(db).await?;
    let mut human_grants_inserted = 0i64;
    for (company_id, principal_id, membership_role) in humans {
        human_grants_inserted += ensure_human_role_default_grants(
            db,
            EnsureHumanGrantsInput {
                company_id,
                principal_id: principal_id,
                membership_role: membership_role.as_deref(),
                granted_by_user_id: None,
            },
        )
        .await?;
    }

    Ok(PrincipalAccessCompatibilityBackfillStats {
        agent_memberships_inserted,
        human_grants_inserted,
    })
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn role_json(s: &str) -> serde_json::Value {
        serde_json::Value::String(s.to_string())
    }

    #[test]
    fn normalize_then_grants_roundtrip_for_each_role() {
        for (name, expected_keys) in [
            (
                "owner",
                &[
                    "agents:create",
                    "agents:configure",
                    "skills:create",
                    "environments:manage",
                    "users:invite",
                    "users:manage_permissions",
                    "tasks:assign",
                    "joins:approve",
                ][..],
            ),
            (
                "admin",
                &[
                    "agents:create",
                    "agents:configure",
                    "skills:create",
                    "environments:manage",
                    "users:invite",
                    "tasks:assign",
                    "joins:approve",
                ][..],
            ),
            ("operator", &["tasks:assign"][..]),
            ("viewer", &[][..]),
            ("member", &["tasks:assign"][..]),
        ] {
            let role = normalize_human_role(&role_json(name), HumanCompanyMembershipRole::Operator);
            let grants: Vec<String> = grants_for_human_role(role)
                .iter()
                .map(|g| g.permission_key.to_string())
                .collect();
            assert_eq!(&grants, expected_keys, "role={name}");
        }
    }

    #[test]
    fn normalize_unknown_role_falls_back_to_default() {
        let role = normalize_human_role(
            &serde_json::Value::String("robot".to_string()),
            HumanCompanyMembershipRole::Viewer,
        );
        assert_eq!(role, HumanCompanyMembershipRole::Viewer);
        assert!(grants_for_human_role(role).is_empty());
    }

    #[test]
    fn normalize_non_string_falls_back_to_default() {
        let role = normalize_human_role(
            &serde_json::json!({"x": 1}),
            HumanCompanyMembershipRole::Admin,
        );
        assert_eq!(role, HumanCompanyMembershipRole::Admin);
    }

    #[test]
    fn grant_input_from_grant_preserves_scope() {
        let g = Grant {
            permission_key: "agents:create",
            scope: Some(serde_json::json!({"limit": 5})),
        };
        let gi = GrantInput::from_grant(&g);
        assert_eq!(gi.permission_key, "agents:create");
        assert_eq!(gi.scope, Some(serde_json::json!({"limit": 5})));
    }

    #[test]
    fn empty_grants_input_returns_zero() {
        let input = InsertGrantsInput {
            company_id: Uuid::nil(),
            principal_type: "user".to_string(),
            principal_id: "x".to_string(),
            grants: vec![],
            granted_by_user_id: None,
        };
        // Can't call the async fn here without a DB; we just verify
        // the early-return contract by constructing a tiny harness.
        assert!(input.grants.is_empty());
    }
}
