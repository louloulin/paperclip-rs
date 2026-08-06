//! 默认 agent instructions bundle。
//!
//! 对齐 `paperclip/server/src/services/default-agent-instructions.ts`：
//! - 按 role 解析 onboarding 资源包：default / ceo
//! - `default` 只含 `AGENTS.md`；`ceo` 含 `AGENTS.md` / `HEARTBEAT.md` /
//!   `SOUL.md` / `TOOLS.md`
//! - 文件用 `include_str!` 嵌入二进制，运行时无文件 I/O（与 Node 端
//!   `fs.readFile` 语义一致，但避免部署期路径配置）
//! - role 解析对未知 role 默认走 `default`

use std::collections::BTreeMap;

const DEFAULT_AGENTS_MD: &str = include_str!("../assets/onboarding-assets/default/AGENTS.md");
const CEO_AGENTS_MD: &str = include_str!("../assets/onboarding-assets/ceo/AGENTS.md");
const CEO_HEARTBEAT_MD: &str = include_str!("../assets/onboarding-assets/ceo/HEARTBEAT.md");
const CEO_SOUL_MD: &str = include_str!("../assets/onboarding-assets/ceo/SOUL.md");
const CEO_TOOLS_MD: &str = include_str!("../assets/onboarding-assets/ceo/TOOLS.md");

/// Onboarding bundle role。
///
/// 对齐 Node `DefaultAgentBundleRole`：
/// - `default` — 默认新 agent（仅 AGENTS.md）
/// - `ceo` — CEO agent（4 个文件）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentInstructionsRole {
    Default,
    Ceo,
}

impl AgentInstructionsRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Ceo => "ceo",
        }
    }
}

/// 把任意 role 字符串归一化到已知 bundle role。
///
/// 未知 role 一律回落 `default`，对齐 Node 端 `role === "ceo" ? "ceo" : "default"`。
pub fn resolve_default_agent_instructions_bundle_role(role: &str) -> AgentInstructionsRole {
    if role == "ceo" {
        AgentInstructionsRole::Ceo
    } else {
        AgentInstructionsRole::Default
    }
}

/// 返回该 role 对应的所有文件内容，键为文件名（如 `"AGENTS.md"`）。
///
/// 顺序：按文件名 ASCII 升序（`BTreeMap` 默认行为），保证调用方拿到的
/// 顺序稳定。对齐 Node `Object.fromEntries(entries)` 在 V8 现代实现下
/// 仍保持插入顺序，但用 BTreeMap 显式收敛更稳。
pub fn load_default_agent_instructions_bundle(
    role: AgentInstructionsRole,
) -> BTreeMap<&'static str, &'static str> {
    let mut bundle: BTreeMap<&'static str, &'static str> = BTreeMap::new();
    match role {
        AgentInstructionsRole::Default => {
            bundle.insert("AGENTS.md", DEFAULT_AGENTS_MD);
        }
        AgentInstructionsRole::Ceo => {
            bundle.insert("AGENTS.md", CEO_AGENTS_MD);
            bundle.insert("HEARTBEAT.md", CEO_HEARTBEAT_MD);
            bundle.insert("SOUL.md", CEO_SOUL_MD);
            bundle.insert("TOOLS.md", CEO_TOOLS_MD);
        }
    }
    bundle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_string_round_trip() {
        assert_eq!(
            resolve_default_agent_instructions_bundle_role("ceo"),
            AgentInstructionsRole::Ceo
        );
        assert_eq!(
            resolve_default_agent_instructions_bundle_role("default"),
            AgentInstructionsRole::Default
        );
    }

    #[test]
    fn unknown_role_falls_back_to_default() {
        for role in &["", "agent", "manager", "CFO", "Ceo", "CEO "] {
            assert_eq!(
                resolve_default_agent_instructions_bundle_role(role),
                AgentInstructionsRole::Default,
                "role {role:?} should fall back to default"
            );
        }
    }

    #[test]
    fn only_ceo_matches_ceo() {
        // 反向验证：必须严格 `==` 才算 ceo
        assert_eq!(
            resolve_default_agent_instructions_bundle_role("ceo"),
            AgentInstructionsRole::Ceo
        );
        assert_eq!(
            resolve_default_agent_instructions_bundle_role("default"),
            AgentInstructionsRole::Default
        );
    }

    #[test]
    fn default_bundle_contains_only_agents_md() {
        let bundle = load_default_agent_instructions_bundle(AgentInstructionsRole::Default);
        assert_eq!(bundle.len(), 1);
        assert!(bundle.contains_key("AGENTS.md"));
        // 文件确实被嵌入，内容非空
        let body = bundle["AGENTS.md"];
        assert!(!body.trim().is_empty());
    }

    #[test]
    fn ceo_bundle_has_four_files() {
        let bundle = load_default_agent_instructions_bundle(AgentInstructionsRole::Ceo);
        let keys: Vec<&str> = bundle.keys().copied().collect();
        assert_eq!(
            keys,
            vec!["AGENTS.md", "HEARTBEAT.md", "SOUL.md", "TOOLS.md"]
        );
        for (file, body) in &bundle {
            assert!(!body.trim().is_empty(), "{file} should have content");
        }
    }

    #[test]
    fn ceo_agents_md_mentions_role_keyword() {
        // sanity：CEO bundle 的 AGENTS.md 应当确实讲 CEO 角色
        let body = load_default_agent_instructions_bundle(AgentInstructionsRole::Ceo)["AGENTS.md"];
        assert!(
            body.to_lowercase().contains("ceo"),
            "expected CEO bundle AGENTS.md to mention 'ceo', got first 200 chars: {}",
            &body.chars().take(200).collect::<String>()
        );
    }

    #[test]
    fn as_str_matches_node_constants() {
        assert_eq!(AgentInstructionsRole::Default.as_str(), "default");
        assert_eq!(AgentInstructionsRole::Ceo.as_str(), "ceo");
    }
}
