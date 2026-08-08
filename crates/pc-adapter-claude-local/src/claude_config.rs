//! Claude config 路径解析（对齐 Node claude-config.ts 纯函数部分）。
//!
//! 提供：
//! - `resolve_shared_claude_config_dir` — 解析非托管 `CLAUDE_CONFIG_DIR`
//! - `resolve_managed_claude_config_seed_dir` — 解析托管 seed 目录
//! - `resolve_managed_claude_runtime_state_dir` — 解析运行时 state 目录

use std::collections::BTreeMap;
use std::path::PathBuf;

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// 解析非托管的 Claude config 目录：优先 `env.CLAUDE_CONFIG_DIR`，否则
/// `<home>/.claude`。对齐 Node `resolveSharedClaudeConfigDir`。
#[must_use]
pub fn resolve_shared_claude_config_dir(
    env: &BTreeMap<String, String>,
    home_dir: &str,
) -> String {
    if let Some(from_env) = env.get("CLAUDE_CONFIG_DIR").and_then(|v| non_empty(Some(v))) {
        let pb = PathBuf::from(&from_env);
        return pb
            .canonicalize()
            .unwrap_or(pb)
            .to_string_lossy()
            .to_string();
    }
    PathBuf::from(home_dir).join(".claude").to_string_lossy().to_string()
}

/// 解析 Paperclip 托管 Claude config seed 目录。
/// `<instanceRoot>/companies/<companyId>/claude-config-seed`
/// 对齐 Node `resolveManagedClaudeConfigSeedDir`。
pub fn resolve_managed_claude_config_seed_dir(
    env: &BTreeMap<String, String>,
    company_id: Option<&str>,
) -> Result<String, pc_acpx::instance_root::ResolvePaperclipInstanceRootError> {
    let root = resolve_instance_root(env)?;
    Ok(if let Some(cid) = company_id.filter(|c| !c.is_empty()) {
        PathBuf::from(root)
            .join("companies")
            .join(cid)
            .join("claude-config-seed")
            .to_string_lossy()
            .to_string()
    } else {
        PathBuf::from(root)
            .join("claude-config-seed")
            .to_string_lossy()
            .to_string()
    })
}

/// 解析 Paperclip 托管 Claude 运行时 state 目录：
/// `<instanceRoot>/companies/<companyId>/agents/<agentId>/claude-runtime`
/// 对齐 Node `resolveManagedClaudeRuntimeStateDir`。
pub fn resolve_managed_claude_runtime_state_dir(
    env: &BTreeMap<String, String>,
    company_id: &str,
    agent_id: &str,
) -> Result<String, pc_acpx::instance_root::ResolvePaperclipInstanceRootError> {
    let root = resolve_instance_root(env)?;
    Ok(PathBuf::from(root)
        .join("companies")
        .join(company_id)
        .join("agents")
        .join(agent_id)
        .join("claude-runtime")
        .to_string_lossy()
        .to_string())
}

// ---------- 内部辅助 ----------

fn resolve_instance_root(
    env: &BTreeMap<String, String>,
) -> Result<String, pc_acpx::instance_root::ResolvePaperclipInstanceRootError> {
    let home_dir = env
        .get("PAPERCLIP_HOME")
        .and_then(|v| non_empty(Some(v)));
    let instance_id = env
        .get("PAPERCLIP_INSTANCE_ID")
        .and_then(|v| non_empty(Some(v)));
    pc_acpx::instance_root::resolve_paperclip_instance_root_for_adapter(
        &pc_acpx::instance_root::ResolvePaperclipInstanceRootInput {
            home_dir,
            instance_id,
            env: Some(env.clone()),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn shared_config_dir_prefers_env() {
        let env = env_with(&[("CLAUDE_CONFIG_DIR", "/tmp/custom-claude")]);
        let resolved = resolve_shared_claude_config_dir(&env, "/Users/me");
        assert!(resolved.contains("custom-claude"));
    }

    #[test]
    fn shared_config_dir_falls_back_to_home() {
        let env = env_with(&[]);
        let resolved = resolve_shared_claude_config_dir(&env, "/Users/me");
        assert!(resolved.ends_with("/.claude"));
    }

    #[test]
    fn shared_config_dir_empty_env_falls_back_to_home() {
        let env = env_with(&[("CLAUDE_CONFIG_DIR", "   ")]);
        let resolved = resolve_shared_claude_config_dir(&env, "/Users/me");
        assert!(resolved.ends_with("/.claude"));
    }

    #[test]
    fn seed_dir_without_company() {
        let env = env_with(&[("PAPERCLIP_HOME", "/tmp/pc")]);
        let dir = resolve_managed_claude_config_seed_dir(&env, None).expect("resolve");
        assert!(dir.contains("claude-config-seed"));
        assert!(!dir.contains("companies"));
    }

    #[test]
    fn seed_dir_with_company() {
        let env = env_with(&[("PAPERCLIP_HOME", "/tmp/pc")]);
        let dir = resolve_managed_claude_config_seed_dir(&env, Some("co_42")).expect("resolve");
        assert!(dir.contains("companies"));
        assert!(dir.contains("co_42"));
        assert!(dir.contains("claude-config-seed"));
    }

    #[test]
    fn runtime_state_dir_contains_company_agent_and_segment() {
        let env = env_with(&[("PAPERCLIP_HOME", "/tmp/pc")]);
        let dir = resolve_managed_claude_runtime_state_dir(&env, "co_1", "agent_x").expect("resolve");
        assert!(dir.contains("companies"));
        assert!(dir.contains("co_1"));
        assert!(dir.contains("agents"));
        assert!(dir.contains("agent_x"));
        assert!(dir.contains("claude-runtime"));
    }

    #[test]
    fn resolve_instance_root_requires_home_or_default() {
        let env = env_with(&[]);
        let result = resolve_managed_claude_config_seed_dir(&env, Some("co_1"));
        assert!(result.is_ok());
    }
}
