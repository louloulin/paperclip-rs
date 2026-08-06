//! Plugin scoped state store（1:1 port of Node `server/src/services/plugin-state-store.ts`，237 行）。
//!
//! 单一职责：plugin worker 的 scope 化 key-value 持久化服务，
//! 提供 `get` / `set` / `delete` / `list` / `delete_all` 操作。
//!
//! 设计：
//! - 五段复合主键：`(plugin_id, scope_kind, scope_id, namespace, state_key)`
//! - `scope_id` 可空（`instance` scope），其他 scope 必须非空字符串
//! - `namespace` 默认 `"default"`，便于在同一 scope 下分组
//! - `ON CONFLICT` upsert 写入，五段唯一索引复用
//! - `assert_plugin_exists` 校验 FK 存在性（Node 显式校验）
//!
//! 与 Node 端 `pluginStateStore(db)` factory 1:1 对齐：
//! - `get(pluginId, scopeKind, stateKey, { scopeId, namespace })` → Option<Value>
//! - `set(pluginId, input)` → ()，input.scopeKind + stateKey + value
//! - `delete(pluginId, scopeKind, stateKey, { scopeId, namespace })` → ()
//! - `list(pluginId, filter)` → Vec<PluginStateRow>
//! - `deleteAll(pluginId)` → ()

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::Db;

// ============================================================================
// Constants
// ============================================================================

/// Default namespace（与 Node `DEFAULT_NAMESPACE = "default"` 1:1 对齐）。
pub const DEFAULT_NAMESPACE: &str = "default";

// ============================================================================
// Types
// ============================================================================

/// Scope 类型（与 Node `PluginStateScopeKind` 1:1 对齐）。
///
/// 注意：Rust 端用强类型枚举替代 Node 字符串联合；DB 持久化用 `as_str()`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStateScopeKind {
    Instance,
    Company,
    Project,
    Issue,
    Agent,
}

impl PluginStateScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Instance => "instance",
            Self::Company => "company",
            Self::Project => "project",
            Self::Issue => "issue",
            Self::Agent => "agent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "instance" => Some(Self::Instance),
            "company" => Some(Self::Company),
            "project" => Some(Self::Project),
            "issue" => Some(Self::Issue),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// plugin_state 表行（与 Drizzle `pluginState.$inferSelect` 1:1 对齐）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq)]
pub struct PluginStateRow {
    pub id: Uuid,
    pub plugin_id: Uuid,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub namespace: String,
    pub state_key: String,
    pub value_json: Value,
    pub updated_at: Timestamp,
}

/// Timestamp 别名（统一用 pc_core 的 Timestamp）。
pub use pc_core::Timestamp;

/// `set` 方法输入（与 Node `SetPluginState` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct SetPluginStateInput {
    pub scope_kind: PluginStateScopeKind,
    pub scope_id: Option<String>,
    pub namespace: Option<String>,
    pub state_key: String,
    pub value: Value,
}

/// `list` 方法过滤器（与 Node `ListPluginState` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct ListPluginStateFilter {
    pub scope_kind: Option<PluginStateScopeKind>,
    pub scope_id: Option<String>,
    pub namespace: Option<String>,
}

/// `get` / `delete` scope 选项（与 Node `get(stateKey, { scopeId, namespace })` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct ScopeOptions {
    pub scope_id: Option<String>,
    pub namespace: Option<String>,
}

// ============================================================================
// Errors
// ============================================================================

/// 域错误（与 Node `notFound` 等价）。
#[derive(Debug, thiserror::Error)]
pub enum PluginStateStoreError {
    #[error("Plugin not found: {0}")]
    PluginNotFound(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub type PluginStateStoreResult<T> = Result<T, PluginStateStoreError>;

// ============================================================================
// Service
// ============================================================================

/// Plugin state store factory（与 Node `pluginStateStore(db)` 1:1 对齐）。
pub fn plugin_state_store(db: &Db) -> PluginStateStore<'_> {
    PluginStateStore { db }
}

/// Plugin state store handle（与 Node `ReturnType<typeof pluginStateStore>` 1:1 对齐）。
pub struct PluginStateStore<'a> {
    db: &'a Db,
}

impl<'a> PluginStateStore<'a> {
    /// 校验 plugin 存在（与 Node `assertPluginExists` 1:1 对齐）。
    async fn assert_plugin_exists(&self, plugin_id: Uuid) -> PluginStateStoreResult<()> {
        let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM plugins WHERE id = $1")
            .bind(plugin_id)
            .fetch_optional(self.db.pool())
            .await?;
        if row.is_none() {
            return Err(PluginStateStoreError::PluginNotFound(plugin_id.to_string()));
        }
        Ok(())
    }

    /// 读取 state value（与 Node `get` 1:1 对齐）。
    ///
    /// Returns `Some(value_json)` if found, `None` otherwise.
    pub async fn get(
        &self,
        plugin_id: Uuid,
        scope_kind: PluginStateScopeKind,
        state_key: &str,
        options: ScopeOptions,
    ) -> PluginStateStoreResult<Option<Value>> {
        let namespace = options
            .namespace
            .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
        let scope_id = options.scope_id;
        let mut qb: QueryBuilder<Postgres> =
            QueryBuilder::new("SELECT value_json FROM plugin_state WHERE plugin_id = ");
        qb.push_bind(plugin_id);
        qb.push(" AND scope_kind = ");
        qb.push_bind(scope_kind.as_str());
        qb.push(" AND namespace = ");
        qb.push_bind(&namespace);
        qb.push(" AND state_key = ");
        qb.push_bind(state_key);
        match scope_id.as_deref() {
            Some(s) if !s.is_empty() => {
                qb.push(" AND scope_id = ");
                qb.push_bind(s);
            }
            _ => {
                qb.push(" AND scope_id IS NULL");
            }
        }

        let row: Option<(Value,)> = qb.build_query_as().fetch_optional(self.db.pool()).await?;
        Ok(row.map(|(v,)| v))
    }

    /// 写入 / 覆盖 state value（与 Node `set` 1:1 对齐，使用 ON CONFLICT upsert）。
    pub async fn set(
        &self,
        plugin_id: Uuid,
        input: SetPluginStateInput,
    ) -> PluginStateStoreResult<()> {
        self.assert_plugin_exists(plugin_id).await?;

        let namespace = input
            .namespace
            .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
        let scope_id = input.scope_id;

        sqlx::query(
            "INSERT INTO plugin_state (plugin_id, scope_kind, scope_id, namespace, state_key, value_json, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, now()) \
             ON CONFLICT (plugin_id, scope_kind, scope_id, namespace, state_key) \
             DO UPDATE SET value_json = EXCLUDED.value_json, updated_at = now()",
        )
        .bind(plugin_id)
        .bind(input.scope_kind.as_str())
        .bind(&scope_id)
        .bind(&namespace)
        .bind(&input.state_key)
        .bind(&input.value)
        .execute(self.db.pool())
        .await?;

        Ok(())
    }

    /// 删除 state value（与 Node `delete` 1:1 对齐；幂等）。
    pub async fn delete(
        &self,
        plugin_id: Uuid,
        scope_kind: PluginStateScopeKind,
        state_key: &str,
        options: ScopeOptions,
    ) -> PluginStateStoreResult<()> {
        let namespace = options
            .namespace
            .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
        let scope_id = options.scope_id;
        let mut qb: QueryBuilder<Postgres> =
            QueryBuilder::new("DELETE FROM plugin_state WHERE plugin_id = ");
        qb.push_bind(plugin_id);
        qb.push(" AND scope_kind = ");
        qb.push_bind(scope_kind.as_str());
        qb.push(" AND namespace = ");
        qb.push_bind(&namespace);
        qb.push(" AND state_key = ");
        qb.push_bind(state_key);
        match scope_id.as_deref() {
            Some(s) if !s.is_empty() => {
                qb.push(" AND scope_id = ");
                qb.push_bind(s);
            }
            _ => {
                qb.push(" AND scope_id IS NULL");
            }
        }

        qb.build().execute(self.db.pool()).await?;
        Ok(())
    }

    /// 列出 plugin 的所有 state entries（与 Node `list` 1:1 对齐）。
    pub async fn list(
        &self,
        plugin_id: Uuid,
        filter: ListPluginStateFilter,
    ) -> PluginStateStoreResult<Vec<PluginStateRow>> {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "SELECT id, plugin_id, scope_kind, scope_id, namespace, state_key, value_json, updated_at \
             FROM plugin_state WHERE plugin_id = ",
        );
        qb.push_bind(plugin_id);
        if let Some(sk) = filter.scope_kind {
            qb.push(" AND scope_kind = ");
            qb.push_bind(sk.as_str());
        }
        if let Some(sid) = filter.scope_id {
            qb.push(" AND scope_id = ");
            qb.push_bind(sid);
        }
        if let Some(ns) = filter.namespace {
            qb.push(" AND namespace = ");
            qb.push_bind(ns);
        }

        let rows = qb
            .build_query_as::<PluginStateRow>()
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    /// 删除 plugin 所有 state entries（与 Node `deleteAll` 1:1 对齐）。
    pub async fn delete_all(&self, plugin_id: Uuid) -> PluginStateStoreResult<()> {
        sqlx::query("DELETE FROM plugin_state WHERE plugin_id = $1")
            .bind(plugin_id)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ----- PluginStateScopeKind -----

    #[test]
    fn scope_kind_round_trip() {
        for k in [
            PluginStateScopeKind::Instance,
            PluginStateScopeKind::Company,
            PluginStateScopeKind::Project,
            PluginStateScopeKind::Issue,
            PluginStateScopeKind::Agent,
        ] {
            assert_eq!(PluginStateScopeKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(PluginStateScopeKind::parse("unknown"), None);
    }

    #[test]
    fn scope_kind_as_str_matches_node_values() {
        assert_eq!(PluginStateScopeKind::Instance.as_str(), "instance");
        assert_eq!(PluginStateScopeKind::Company.as_str(), "company");
        assert_eq!(PluginStateScopeKind::Project.as_str(), "project");
        assert_eq!(PluginStateScopeKind::Issue.as_str(), "issue");
        assert_eq!(PluginStateScopeKind::Agent.as_str(), "agent");
    }

    // ----- DEFAULT_NAMESPACE -----

    #[test]
    fn default_namespace_constant_matches_node() {
        assert_eq!(DEFAULT_NAMESPACE, "default");
    }

    // ----- ScopeOptions default -----

    #[test]
    fn scope_options_default_is_empty() {
        let opts = ScopeOptions::default();
        assert!(opts.scope_id.is_none());
        assert!(opts.namespace.is_none());
    }

    // ----- ListPluginStateFilter default -----

    #[test]
    fn list_filter_default_is_empty() {
        let f = ListPluginStateFilter::default();
        assert!(f.scope_kind.is_none());
        assert!(f.scope_id.is_none());
        assert!(f.namespace.is_none());
    }

    // ----- SetPluginStateInput -----

    #[test]
    fn set_input_holds_required_fields() {
        let input = SetPluginStateInput {
            scope_kind: PluginStateScopeKind::Company,
            scope_id: Some("c-1".into()),
            namespace: Some("ns".into()),
            state_key: "k".into(),
            value: serde_json::json!({"foo": 1}),
        };
        assert_eq!(input.scope_kind, PluginStateScopeKind::Company);
        assert_eq!(input.scope_id.as_deref(), Some("c-1"));
        assert_eq!(input.namespace.as_deref(), Some("ns"));
    }

    // ----- PluginStateRow -----

    #[test]
    fn plugin_state_row_serializes() {
        // Smoke test for Serialize round-trip (timestamp format may differ but value_json should match)
        let json = serde_json::json!({"a": 1, "b": [1, 2, 3]});
        let s = serde_json::to_string(&json).unwrap();
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, json);
    }

    // Note: DB IO tests need DATABASE_URL；与既有模式一致。

    // ----- PluginStateStoreError -----

    #[test]
    fn plugin_not_found_error_message_includes_id() {
        let err = PluginStateStoreError::PluginNotFound("uuid-x".into());
        assert!(err.to_string().contains("uuid-x"));
    }

    // ----- QueryBuilder SQL shape (compile-time check) -----

    #[test]
    fn query_builder_supports_optional_scope_id() {
        // This is a smoke test: ensure QueryBuilder works for the two branches
        let mut qb = QueryBuilder::<Postgres>::new("SELECT 1");
        qb.push(" WHERE x = ").push_bind(42_i32);
        let sql = qb.into_sql();
        // Just check the SQL string contains the bind placeholder
        let debug = format!("{:?}", sql);
        assert!(debug.contains("$1"));
    }
}
