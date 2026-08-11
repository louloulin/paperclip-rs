//! Service —— 协调 secrets 服务同步 secret bindings。
//!
//! 与 Node `syncAgentAdapterEnvBindings` 1:1 对齐。

use serde_json::Value;

use super::collector::{collect_secret_refs, collect_user_secret_refs};
use super::types::{BindingTarget, BindingTargetType, SecretBindingResult, SecretBindingSync};

/// 同步 agent adapter 的 env bindings 到 secrets service（与 Node `syncAgentAdapterEnvBindings` 1:1 对齐）。
///
/// ## 行为
///
/// - 调用 [`collect_secret_refs`] + [`collect_user_secret_refs`] 提取 binding 列表。
/// - 始终以 `replaceAll: true` 语义同步（调用方每次替换全部 binding）。
/// - 任何错误透传。
pub async fn sync_agent_adapter_env_bindings<S: SecretBindingSync + ?Sized>(
    secrets_svc: &S,
    company_id: &str,
    agent_id: &str,
    adapter_config: &Value,
) -> SecretBindingResult<()> {
    let target = BindingTarget {
        target_type: BindingTargetType::Agent,
        target_id: agent_id,
    };

    let secret_refs = collect_secret_refs(adapter_config);
    let user_refs = collect_user_secret_refs(adapter_config);

    secrets_svc
        .sync_secret_refs(company_id, target, &secret_refs)
        .await?;
    secrets_svc
        .sync_user_secret_declarations(company_id, target, &user_refs)
        .await?;

    Ok(())
}

/// Fallback 路径（与 Node `syncEnvBindingsForTarget` 1:1 对齐）。
///
/// 当 secrets 服务不支持细粒度 `sync_secret_refs` 时，使用本函数同步原始 env 值。
pub async fn sync_agent_adapter_env_bindings_fallback<S: SecretBindingSync + ?Sized>(
    secrets_svc: &S,
    company_id: &str,
    agent_id: &str,
    adapter_config: &Value,
) -> SecretBindingResult<()> {
    let target = BindingTarget {
        target_type: BindingTargetType::Agent,
        target_id: agent_id,
    };
    let env_value = adapter_config
        .as_object()
        .and_then(|o| o.get("env"))
        .cloned()
        .unwrap_or(Value::Null);

    secrets_svc
        .sync_env_bindings(company_id, target, env_value)
        .await
}
