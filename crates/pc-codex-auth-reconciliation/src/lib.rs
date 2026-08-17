//! Startup backfill: seed `auth.json` into already-isolated `codex_local`
//! managed homes that were created (by the isolation guard) before the
//! execute-time seeding fix landed.
//!
//! 对齐 Node `services/codex-auth-reconciliation.ts`：
//! - 从 `agents` 表筛选 `adapter_type = 'codex_local'` 的行
//! - 解析 `adapter_config.env.{CODEX_HOME, OPENAI_API_KEY}` 的三种 binding：
//!   `plain`(可解析的字面量)、`secret`(`secret_ref` 不能解析的 binding)、
//!   `none`(未配置)
//! - 对每个 agent 调用 `pc-adapter-codex-local` 的 `reconcile_managed_codex_home`
//!   并聚合统计 seeded / already_seeded / external_override / no_managed_home /
//!   source_auth_missing / failed

use pc_adapter_codex_local::codex_home::{
    reconcile_managed_codex_home, ReconcileManagedCodexHomeInput, ReconcileManagedCodexHomeStatus,
};
use pc_repos::Db;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

/// 启动时 `codex_local` auth 调和结果。
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodexAuthReconciliationSummary {
    pub scanned: usize,
    pub seeded: usize,
    pub already_seeded: usize,
    pub external_override: usize,
    pub no_managed_home: usize,
    pub source_auth_missing: usize,
    pub failed: usize,
    pub seeded_agent_ids: Vec<String>,
}

/// 调用方传入的副作用 hook（用于测试 / 注入不同的 reconcile 实现）。
#[async_trait::async_trait]
pub trait CodexAuthReconciler: Send + Sync {
    async fn reconcile(
        &self,
        input: ReconcileManagedCodexHomeInput,
    ) -> Result<ReconcileManagedCodexHomeResult, CodexAuthReconciliationError>;
}

#[derive(Debug, Clone)]
pub struct ReconcileManagedCodexHomeResult {
    pub status: ReconcileManagedCodexHomeStatus,
    pub home: Option<String>,
}

/// 调 `pc-adapter-codex-local::reconcile_managed_codex_home` 的默认实现。
#[derive(Debug, Clone, Default)]
pub struct AdapterCodexLocalReconciler;

#[async_trait::async_trait]
impl CodexAuthReconciler for AdapterCodexLocalReconciler {
    async fn reconcile(
        &self,
        input: ReconcileManagedCodexHomeInput,
    ) -> Result<ReconcileManagedCodexHomeResult, CodexAuthReconciliationError> {
        reconcile_managed_codex_home(input)
            .await
            .map(|r| ReconcileManagedCodexHomeResult {
                status: r.status,
                home: r.home,
            })
            .map_err(CodexAuthReconciliationError::AdapterIo)
    }
}

/// Service 状态：拥有 `Db` 句柄 + 可注入 reconciler。
#[derive(Clone)]
pub struct CodexAuthReconciliationService {
    db: Db,
    reconciler: Arc<dyn CodexAuthReconciler>,
}

impl std::fmt::Debug for CodexAuthReconciliationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexAuthReconciliationService")
            .field("db", &"<Db>")
            .field("reconciler", &"<dyn CodexAuthReconciler>")
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum CodexAuthReconciliationError {
    #[error("postgres error: {0}")]
    Postgres(#[from] sqlx::Error),
    #[error("adapter io error: {0}")]
    AdapterIo(#[from] std::io::Error),
}

impl CodexAuthReconciliationService {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            reconciler: Arc::new(AdapterCodexLocalReconciler),
        }
    }

    pub fn with_reconciler(db: Db, reconciler: Arc<dyn CodexAuthReconciler>) -> Self {
        Self { db, reconciler }
    }

    /// 列出所有 `adapter_type = 'codex_local'` 的 agent。
    pub async fn list_codex_local_agents(
        &self,
    ) -> Result<Vec<CodexLocalAgentRow>, CodexAuthReconciliationError> {
        let rows = sqlx::query_as::<_, CodexLocalAgentRow>(
            "SELECT id, company_id, adapter_config::text AS adapter_config_text \
             FROM agents WHERE adapter_type = 'codex_local'",
        )
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// 启动调和：扫描所有 `codex_local` agent 并 seed auth.json。
    pub async fn reconcile_on_startup(
        &self,
    ) -> Result<CodexAuthReconciliationSummary, CodexAuthReconciliationError> {
        self.reconcile_on_startup_with(|_| true).await
    }

    /// 启动调和（带前置过滤谓词，便于测试）。
    pub async fn reconcile_on_startup_with<F>(
        &self,
        mut predicate: F,
    ) -> Result<CodexAuthReconciliationSummary, CodexAuthReconciliationError>
    where
        F: FnMut(&CodexLocalAgentRow) -> bool,
    {
        let mut summary = CodexAuthReconciliationSummary::default();
        let rows = self.list_codex_local_agents().await?;
        for row in rows {
            if !predicate(&row) {
                continue;
            }
            summary.scanned += 1;

            let env = parse_adapter_env(&row.adapter_config_text);
            let configured_codex_home = env
                .as_ref()
                .and_then(|e| read_plain_env_value(e.get("CODEX_HOME")));
            let api_key_binding =
                classify_api_key_binding(env.as_ref().and_then(|e| e.get("OPENAI_API_KEY")));

            let home_for_warn = configured_codex_home.clone();
            let input = ReconcileManagedCodexHomeInput {
                company_id: Some(row.company_id.to_string()),
                configured_codex_home,
                api_key: match &api_key_binding {
                    ApiKeyBinding::Plain { value } => Some(value.clone()),
                    _ => None,
                },
                api_key_secret_bound: matches!(api_key_binding, ApiKeyBinding::Secret),
                env: None,
            };

            match self.reconciler.reconcile(input).await {
                Ok(result) => match result.status {
                    ReconcileManagedCodexHomeStatus::Seeded => {
                        summary.seeded += 1;
                        summary.seeded_agent_ids.push(row.id.to_string());
                        info!(
                            agent_id = %row.id,
                            company_id = %row.company_id,
                            home = ?result.home,
                            "seeded auth into already-isolated codex_local managed home"
                        );
                    }
                    ReconcileManagedCodexHomeStatus::AlreadySeeded => {
                        summary.already_seeded += 1;
                    }
                    ReconcileManagedCodexHomeStatus::ExternalOverride => {
                        summary.external_override += 1;
                    }
                    ReconcileManagedCodexHomeStatus::NoManagedHome => {
                        summary.no_managed_home += 1;
                    }
                    ReconcileManagedCodexHomeStatus::SourceAuthMissing => {
                        summary.source_auth_missing += 1;
                    }
                },
                Err(err) => {
                    summary.failed += 1;
                    warn!(
                        agent_id = %row.id,
                        company_id = %row.company_id,
                        home = ?home_for_warn,
                        error = %err,
                        "failed to reconcile codex_local managed home on startup"
                    );
                }
            }
        }

        Ok(summary)
    }
}

/// 从 DB SELECT 的 agent 行投影。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CodexLocalAgentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub adapter_config_text: String,
}

/// 解析 `adapter_config.env` 子对象；如果 `adapter_config` 不存在或不是对象则返回 None。
pub fn parse_adapter_env(adapter_config_text: &str) -> Option<serde_json::Map<String, Value>> {
    let value: Value = serde_json::from_str(adapter_config_text).ok()?;
    let object = value.as_object()?;
    let env = object.get("env")?;
    env.as_object().cloned()
}

/// `OPENAI_API_KEY` binding 的三种分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeyBinding {
    Plain { value: String },
    Secret,
    None,
}

/// 提取字面量（非 secret）的 env 值。
///
/// 支持嵌套 `{ "type": "plain", "value": "..." }` 与裸字符串。
pub fn read_plain_env_value(value: Option<&Value>) -> Option<String> {
    let Some(value) = value else { return None };
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Object(obj) => {
            if obj.get("type").and_then(Value::as_str) == Some("plain") {
                read_plain_env_value(obj.get("value"))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 把 env binding 归类为 plain / secret / none。
pub fn classify_api_key_binding(value: Option<&Value>) -> ApiKeyBinding {
    if let Some(plain) = read_plain_env_value(value) {
        return ApiKeyBinding::Plain { value: plain };
    }
    if let Some(obj) = value.and_then(Value::as_object) {
        if obj
            .get("type")
            .and_then(Value::as_str)
            .map(|t| t != "plain")
            .unwrap_or(false)
        {
            return ApiKeyBinding::Secret;
        }
    }
    ApiKeyBinding::None
}


#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r786_parse_adapter_env_valid_json() {
        let env = parse_adapter_env(r#"{"env": {"OPENAI_API_KEY": "abc", "CODEX_HOME": "/home/x"}}"#).unwrap();
        assert_eq!(env.get("OPENAI_API_KEY").and_then(|v| v.as_str()), Some("abc"));
        assert_eq!(env.get("CODEX_HOME").and_then(|v| v.as_str()), Some("/home/x"));
    }

    #[test]
    fn r786_parse_adapter_env_no_env_returns_none() {
        let env = parse_adapter_env(r#"{"name": "foo"}"#);
        assert!(env.is_none());
    }

    #[test]
    fn r786_parse_adapter_env_invalid_json_returns_none() {
        assert!(parse_adapter_env("not json").is_none());
        assert!(parse_adapter_env("").is_none());
    }

    #[test]
    fn r786_parse_adapter_env_env_not_object_returns_none() {
        let env = parse_adapter_env(r#"{"env": "not an object"}"#);
        assert!(env.is_none());
    }

    #[test]
    fn r786_parse_adapter_env_supports_plain_object() {
        let env = parse_adapter_env(r#"{"env": {"K": "v"}}"#).unwrap();
        assert_eq!(env.len(), 1);
    }

    #[test]
    fn r786_read_plain_env_value_string() {
        let v = json!("hello");
        assert_eq!(read_plain_env_value(Some(&v)), Some("hello".to_string()));
    }

    #[test]
    fn r786_read_plain_env_value_trims_whitespace() {
        let v = json!("  hello  ");
        assert_eq!(read_plain_env_value(Some(&v)), Some("hello".to_string()));
    }

    #[test]
    fn r786_read_plain_env_value_empty_string_returns_none() {
        let v = json!("");
        assert_eq!(read_plain_env_value(Some(&v)), None);
        let v = json!("   ");
        assert_eq!(read_plain_env_value(Some(&v)), None);
    }

    #[test]
    fn r786_read_plain_env_value_plain_type() {
        let v = json!({"type": "plain", "value": "plain-val"});
        assert_eq!(read_plain_env_value(Some(&v)), Some("plain-val".to_string()));
    }

    #[test]
    fn r786_read_plain_env_value_secret_type_returns_none() {
        let v = json!({"type": "secret", "secret_ref": "abc"});
        assert_eq!(read_plain_env_value(Some(&v)), None);
    }

    #[test]
    fn r786_read_plain_env_value_non_string_returns_none() {
        let v = json!(42);
        assert_eq!(read_plain_env_value(Some(&v)), None);
        let v = json!(true);
        assert_eq!(read_plain_env_value(Some(&v)), None);
    }

    #[test]
    fn r786_read_plain_env_value_none_input() {
        assert_eq!(read_plain_env_value(None), None);
    }

    #[test]
    fn r786_read_plain_env_value_nested_plain() {
        let v = json!({"type": "plain", "value": {"type": "plain", "value": "deep"}});
        assert_eq!(read_plain_env_value(Some(&v)), Some("deep".to_string()));
    }

    #[test]
    fn r786_classify_api_key_plain_string() {
        let v = json!("sk-abc");
        assert_eq!(
            classify_api_key_binding(Some(&v)),
            ApiKeyBinding::Plain { value: "sk-abc".to_string() }
        );
    }

    #[test]
    fn r786_classify_api_key_plain_object() {
        let v = json!({"type": "plain", "value": "sk-abc"});
        assert_eq!(
            classify_api_key_binding(Some(&v)),
            ApiKeyBinding::Plain { value: "sk-abc".to_string() }
        );
    }

    #[test]
    fn r786_classify_api_key_secret() {
        let v = json!({"type": "secret", "secret_ref": "key-1"});
        assert_eq!(classify_api_key_binding(Some(&v)), ApiKeyBinding::Secret);
    }

    #[test]
    fn r786_classify_api_key_none() {
        assert_eq!(classify_api_key_binding(None), ApiKeyBinding::None);
        let v = json!("");
        assert_eq!(classify_api_key_binding(Some(&v)), ApiKeyBinding::None);
    }

    #[test]
    fn r786_classify_api_key_unknown_type_returns_secret() {
        // type != "plain" -> classified as Secret (anything not plain is treated as secret)
        let v = json!({"type": "weird", "value": "x"});
        assert_eq!(classify_api_key_binding(Some(&v)), ApiKeyBinding::Secret);
    }

    #[test]
    fn r786_summary_default_is_zero() {
        let s = CodexAuthReconciliationSummary::default();
        assert_eq!(s.scanned, 0);
        assert_eq!(s.seeded, 0);
        assert_eq!(s.already_seeded, 0);
        assert_eq!(s.external_override, 0);
        assert_eq!(s.no_managed_home, 0);
        assert_eq!(s.source_auth_missing, 0);
        assert_eq!(s.failed, 0);
    }

    #[test]
    fn r786_summary_serialization_camel_case() {
        let mut s = CodexAuthReconciliationSummary::default();
        s.scanned = 5;
        s.seeded = 3;
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v.get("scanned").and_then(|x| x.as_u64()), Some(5));
        assert_eq!(v.get("seeded").and_then(|x| x.as_u64()), Some(3));
        assert!(v.get("alreadySeeded").is_some());
    }
}
