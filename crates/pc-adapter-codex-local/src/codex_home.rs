//! Codex home 解析与基础管理工具（对齐 Node codex-home.ts）。
//!
//! 提供：
//! - `resolve_shared_codex_home_dir` — 解析非托管 `CODEX_HOME`
//! - `resolve_managed_codex_home_dir` — 解析 Paperclip 托管 home
//! - `is_managed_codex_home_path` — 判断路径是否位于公司托管目录
//! - `codex_home_has_usable_auth` — 异步检查 auth.json 是否含可用凭据
//! - `merge_managed_codex_mcp_gateways` — 合并 MCP gateway 列表
//! - `path_exists` — 异步路径存在性检查
//! - `CODEX_SYNC_ALLOWLIST` — 沙箱同步时允许的文件白名单

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 沙箱同步到 codex 时允许的文件白名单（与 Node `CODEX_SYNC_ALLOWLIST` 一致）。
pub const CODEX_SYNC_ALLOWLIST: &[&str] = &[
    "config.json",
    "config.toml",
    "instructions.md",
    "auth.json",
    "skills",
];

/// MCP gateway 描述符，与 Node `ManagedCodexMcpGateway` 对齐。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedCodexMcpGateway {
    pub name: String,
    pub endpoint_path: String,
    pub bearer_token: String,
}

/// 合并两份 MCP gateway 列表，主列表优先（同名不覆盖）。对齐 Node
/// `mergeManagedCodexMcpGateways`。
#[must_use]
pub fn merge_managed_codex_mcp_gateways(
    primary: &[ManagedCodexMcpGateway],
    secondary: &[ManagedCodexMcpGateway],
) -> Vec<ManagedCodexMcpGateway> {
    let mut merged: Vec<ManagedCodexMcpGateway> = primary.to_vec();
    let mut names: std::collections::HashSet<String> =
        primary.iter().map(|g| g.name.clone()).collect();
    for gateway in secondary {
        if names.contains(&gateway.name) {
            continue;
        }
        merged.push(gateway.clone());
        names.insert(gateway.name.clone());
    }
    merged
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// 解析非托管的 `CODEX_HOME`：优先 `env.CODEX_HOME`，否则 `<home>/.codex`。
/// 对齐 Node `resolveSharedCodexHomeDir`。
#[must_use]
pub fn resolve_shared_codex_home_dir(env: &BTreeMap<String, String>, home_dir: &str) -> String {
    if let Some(from_env) = env.get("CODEX_HOME").and_then(|v| non_empty(Some(v))) {
        let pb = PathBuf::from(&from_env);
        let resolved = pb.canonicalize().unwrap_or(pb);
        return resolved.to_string_lossy().to_string();
    }
    PathBuf::from(home_dir).join(".codex").to_string_lossy().to_string()
}

/// 解析 Paperclip 托管 codex home 目录：`<instanceRoot>/companies/<companyId>/codex-home`。
/// 对齐 Node `resolveManagedCodexHomeDir`。
pub fn resolve_managed_codex_home_dir(
    env: &BTreeMap<String, String>,
    company_id: Option<&str>,
) -> Result<String, pc_acpx::instance_root::ResolvePaperclipInstanceRootError> {
    let instance_root = resolve_instance_root(env)?;
    Ok(if let Some(cid) = company_id.filter(|c| !c.is_empty()) {
        PathBuf::from(instance_root)
            .join("companies")
            .join(cid)
            .join("codex-home")
            .to_string_lossy()
            .to_string()
    } else {
        PathBuf::from(instance_root)
            .join("codex-home")
            .to_string_lossy()
            .to_string()
    })
}

/// 判断 homePath 是否位于托管公司目录下。对齐 Node `isManagedCodexHomePath`。
pub fn is_managed_codex_home_path(
    env: &BTreeMap<String, String>,
    company_id: Option<&str>,
    home_path: &str,
) -> Result<bool, pc_acpx::instance_root::ResolvePaperclipInstanceRootError> {
    let Some(cid) = company_id.filter(|c| !c.is_empty()) else {
        return Ok(false);
    };
    let instance_root = resolve_instance_root(env)?;
    let company_root = PathBuf::from(instance_root)
        .join("companies")
        .join(cid);
    let resolved_home = normalize_lexically(home_path);
    let resolved_root = normalize_lexically(&company_root.to_string_lossy());
    Ok(resolved_home == resolved_root
        || resolved_home.starts_with(&format!("{resolved_root}/")))
}

/// 异步检查路径是否存在（follows symlinks，行为同 Node `fs.access`）。
pub async fn path_exists(candidate: impl AsRef<Path>) -> bool {
    tokio::fs::metadata(candidate.as_ref()).await.is_ok()
}

/// 异步读取 `auth.json` 并验证是否含可用凭据（API key 或 tokens）。
/// 对齐 Node `codexHomeHasUsableAuth`。
pub async fn codex_home_has_usable_auth(home: impl AsRef<Path>) -> bool {
    let auth_path = home.as_ref().join("auth.json");
    if !path_exists(&auth_path).await {
        return false;
    }
    let raw = match tokio::fs::read_to_string(&auth_path).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    has_usable_auth_payload(&parsed)
}

fn has_usable_auth_payload(payload: &serde_json::Value) -> bool {
    let Some(obj) = payload.as_object() else {
        return false;
    };
    // 形式 1：直接的 OPENAI_API_KEY 字符串
    if let Some(key) = obj.get("OPENAI_API_KEY").and_then(|v| v.as_str()) {
        if !key.trim().is_empty() {
            return true;
        }
    }
    // 形式 2：tokens.account_id + (id_token|access_token|refresh_token)
    if let Some(tokens) = obj.get("tokens").and_then(|v| v.as_object()) {
        let account_id_ok = tokens
            .get("account_id")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let token_material_ok = ["id_token", "access_token", "refresh_token"]
            .iter()
            .any(|key| {
                tokens
                    .get(*key)
                    .and_then(|v| v.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            });
        if account_id_ok && token_material_ok {
            return true;
        }
    }
    false
}

// ---------- 内部辅助 ----------

fn resolve_instance_root(
    env: &BTreeMap<String, String>,
) -> Result<String, pc_acpx::instance_root::ResolvePaperclipInstanceRootError> {
    let home_dir = non_empty(env.get("PAPERCLIP_HOME").map(String::as_str));
    let instance_id = non_empty(env.get("PAPERCLIP_INSTANCE_ID").map(String::as_str));
    pc_acpx::instance_root::resolve_paperclip_instance_root_for_adapter(
        &pc_acpx::instance_root::ResolvePaperclipInstanceRootInput {
            home_dir: home_dir,
            instance_id: instance_id,
            env: Some(env.clone()),
        },
    )
}

fn normalize_lexically(path: &str) -> String {
    // 不依赖文件系统访问，使用 PathBuf 词法规范化。
    let pb = PathBuf::from(path);
    let mut out = PathBuf::new();
    for component in pb.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env_with(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn merge_gateways_prefers_primary_names() {
        let a = ManagedCodexMcpGateway {
            name: "primary".into(),
            endpoint_path: "/a".into(),
            bearer_token: "tok-a".into(),
        };
        let b = ManagedCodexMcpGateway {
            name: "secondary".into(),
            endpoint_path: "/b".into(),
            bearer_token: "tok-b".into(),
        };
        let c = ManagedCodexMcpGateway {
            name: "primary".into(),
            endpoint_path: "/a2".into(),
            bearer_token: "tok-a2".into(),
        };
        let merged = merge_managed_codex_mcp_gateways(&[a.clone()], &[b.clone(), c]);
        assert_eq!(merged, vec![a, b]);
    }

    #[test]
    fn merge_gateways_empty_inputs() {
        let merged = merge_managed_codex_mcp_gateways(&[], &[]);
        assert!(merged.is_empty());
    }

    #[test]
    fn resolve_shared_codex_home_prefers_env() {
        let env = env_with(&[("CODEX_HOME", "/tmp/custom-codex")]);
        let resolved = resolve_shared_codex_home_dir(&env, "/Users/me");
        // 不要求完全匹配（取决于环境是否真存在该路径），但应包含 CODEX_HOME 段。
        assert!(
            resolved.contains("custom-codex") || resolved == "/tmp/custom-codex",
            "got {resolved}"
        );
    }

    #[test]
    fn resolve_shared_codex_home_falls_back_to_home() {
        let env = env_with(&[]);
        let resolved = resolve_shared_codex_home_dir(&env, "/Users/me");
        assert!(resolved.ends_with("/.codex"));
    }

    #[test]
    fn is_managed_codex_home_path_requires_company_id() {
        let env = env_with(&[("PAPERCLIP_HOME", "/tmp"), ("PAPERCLIP_INSTANCE_ID", "default")]);
        let result = is_managed_codex_home_path(&env, None, "/anything");
        assert_eq!(result.unwrap(), false);
    }

    #[test]
    fn resolve_managed_codex_home_dir_without_company_id() {
        let env = env_with(&[("PAPERCLIP_HOME", "/tmp/pc-home")]);
        let dir = resolve_managed_codex_home_dir(&env, None).expect("resolve");
        assert!(dir.contains("codex-home"));
        assert!(!dir.contains("companies"));
    }

    #[test]
    fn resolve_managed_codex_home_dir_with_company_id() {
        let env = env_with(&[("PAPERCLIP_HOME", "/tmp/pc-home")]);
        let dir = resolve_managed_codex_home_dir(&env, Some("co_1")).expect("resolve");
        assert!(dir.contains("companies"));
        assert!(dir.contains("co_1"));
        assert!(dir.contains("codex-home"));
    }

    #[test]
    fn has_usable_auth_payload_accepts_openai_api_key() {
        let payload = json!({ "OPENAI_API_KEY": "sk-test" });
        assert!(has_usable_auth_payload(&payload));
    }

    #[test]
    fn has_usable_auth_payload_accepts_tokens_account() {
        let payload = json!({
            "tokens": {
                "account_id": "acc-1",
                "id_token": "id-xyz",
            }
        });
        assert!(has_usable_auth_payload(&payload));
    }

    #[test]
    fn has_usable_auth_payload_rejects_empty() {
        assert!(!has_usable_auth_payload(&json!({})));
        assert!(!has_usable_auth_payload(&json!({ "OPENAI_API_KEY": "" })));
        assert!(!has_usable_auth_payload(
            &json!({ "tokens": { "account_id": "a", "id_token": "" } })
        ));
    }

    #[test]
    fn has_usable_auth_payload_rejects_non_object() {
        assert!(!has_usable_auth_payload(&json!(null)));
        assert!(!has_usable_auth_payload(&json!("string")));
        assert!(!has_usable_auth_payload(&json!([])));
    }

    #[tokio::test]
    async fn path_exists_returns_true_for_existing() {
        let dir = std::env::temp_dir();
        assert!(path_exists(&dir).await);
    }

    #[tokio::test]
    async fn path_exists_returns_false_for_missing() {
        assert!(!path_exists("/this/path/definitely/does/not/exist/anywhere").await);
    }

    #[tokio::test]
    async fn codex_home_has_usable_auth_returns_false_when_no_auth_file() {
        let dir = std::env::temp_dir().join(format!("paperclip-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!codex_home_has_usable_auth(&dir).await);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn codex_home_has_usable_auth_returns_true_for_valid_auth() {
        let dir = std::env::temp_dir().join(format!("paperclip-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            r#"{"OPENAI_API_KEY": "sk-test"}"#,
        )
        .unwrap();
        assert!(codex_home_has_usable_auth(&dir).await);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
