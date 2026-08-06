//! Bundled plugin catalog 常量与解析。
//!
//! 与 Node `server/src/services/bundled-plugins.ts` 的
//! `DEFAULT_BUNDLED_CATALOG_ROOT` / `BUNDLED_CATALOG_ROOT_ENV_VAR` /
//! `BUNDLED_PLUGIN_CATALOG` / `SELF_HOSTED_AUTO_INSTALL_KEYS` /
//! `resolveBundledCatalogRoot` 1:1 对齐。
//!
//! 单一职责：提供 bundled plugin allowlist 与环境变量解析，不含任何
//! 安装 / 路径 escape 检测 / 异步逻辑。

use std::sync::LazyLock;

use super::types::{BundledPluginCatalogEntry, EnvMap};

// ============================================================================
// Constants
// ============================================================================

/// 默认 catalog 根目录（与 Node `DEFAULT_BUNDLED_CATALOG_ROOT` 1:1 对齐）。
pub const DEFAULT_BUNDLED_CATALOG_ROOT: &str = "/app/packages/plugins";

/// 可覆盖 catalog 根目录的环境变量（与 Node `BUNDLED_CATALOG_ROOT_ENV_VAR` 1:1 对齐）。
pub const BUNDLED_CATALOG_ROOT_ENV_VAR: &str = "PAPERCLIP_BUNDLED_PLUGIN_ROOT";

/// Kubernetes 路径覆盖环境变量（保留 legacy 兼容）。
pub const KUBERNETES_PLUGIN_PATH_ENV_VAR: &str = "PAPERCLIP_KUBERNETES_PLUGIN_PATH";

// ============================================================================
// BUNDLED_PLUGIN_CATALOG (positive allowlist)
// ============================================================================

/// Bundled plugin positive allowlist（与 Node `BUNDLED_PLUGIN_CATALOG` 1:1 对齐）。
///
/// 修改此表 = 修改允许 auto-install 的全部 sandbox provider。
/// 不要把未审计的 key 加入此表。
///
/// 使用 `LazyLock<Vec<_>>` 而不是 `const` 是因为 Rust stable 的 const 上下文
/// 不支持 `String::to_string()` / `String::from()`。
pub static BUNDLED_PLUGIN_CATALOG: LazyLock<Vec<BundledPluginCatalogEntry>> = LazyLock::new(|| {
    vec![
        BundledPluginCatalogEntry {
            key: "cloudflare".to_string(),
            plugin_key: "paperclip.cloudflare-sandbox-provider".to_string(),
            relative_path: "sandbox-providers/cloudflare".to_string(),
            path_override_env_var: None,
        },
        BundledPluginCatalogEntry {
            key: "daytona".to_string(),
            plugin_key: "paperclip.daytona-sandbox-provider".to_string(),
            relative_path: "sandbox-providers/daytona".to_string(),
            path_override_env_var: None,
        },
        BundledPluginCatalogEntry {
            key: "e2b".to_string(),
            plugin_key: "paperclip.e2b-sandbox-provider".to_string(),
            relative_path: "sandbox-providers/e2b".to_string(),
            path_override_env_var: None,
        },
        BundledPluginCatalogEntry {
            key: "exe-dev".to_string(),
            plugin_key: "paperclip.exe-dev-sandbox-provider".to_string(),
            relative_path: "sandbox-providers/exe-dev".to_string(),
            path_override_env_var: None,
        },
        BundledPluginCatalogEntry {
            key: "kubernetes".to_string(),
            plugin_key: "paperclip.kubernetes-sandbox-provider".to_string(),
            relative_path: "sandbox-providers/kubernetes".to_string(),
            path_override_env_var: Some(KUBERNETES_PLUGIN_PATH_ENV_VAR.to_string()),
        },
        BundledPluginCatalogEntry {
            key: "modal".to_string(),
            plugin_key: "paperclip.modal-sandbox-provider".to_string(),
            relative_path: "sandbox-providers/modal".to_string(),
            path_override_env_var: None,
        },
        BundledPluginCatalogEntry {
            key: "novita".to_string(),
            plugin_key: "paperclip.novita-sandbox-provider".to_string(),
            relative_path: "sandbox-providers/novita".to_string(),
            path_override_env_var: None,
        },
    ]
});

// ============================================================================
// Self-hosted default keys
// ============================================================================

/// Self-hosted 实例无 managed config 时的默认 auto-install key 列表
/// （与 Node `SELF_HOSTED_AUTO_INSTALL_KEYS` 1:1 对齐）。
///
/// 仅 `kubernetes`：保留 pre-refactor 行为。
pub const SELF_HOSTED_AUTO_INSTALL_KEYS: &[&str] = &["kubernetes"];

// ============================================================================
// resolveBundledCatalogRoot
// ============================================================================

/// 解析 catalog root（与 Node `resolveBundledCatalogRoot(env)` 1:1 对齐）。
///
/// 优先 env override（trim 后非空），否则回退到 `DEFAULT_BUNDLED_CATALOG_ROOT`。
pub fn resolve_bundled_catalog_root(env: &EnvMap) -> String {
    if let Some(override_value) = env.get(BUNDLED_CATALOG_ROOT_ENV_VAR) {
        let trimmed = override_value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    DEFAULT_BUNDLED_CATALOG_ROOT.to_string()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_catalog_root_constant_matches_node() {
        assert_eq!(DEFAULT_BUNDLED_CATALOG_ROOT, "/app/packages/plugins");
    }

    #[test]
    fn env_var_name_matches_node() {
        assert_eq!(
            BUNDLED_CATALOG_ROOT_ENV_VAR,
            "PAPERCLIP_BUNDLED_PLUGIN_ROOT"
        );
        assert_eq!(
            KUBERNETES_PLUGIN_PATH_ENV_VAR,
            "PAPERCLIP_KUBERNETES_PLUGIN_PATH"
        );
    }

    #[test]
    fn bundled_plugin_catalog_has_seven_entries() {
        assert_eq!(BUNDLED_PLUGIN_CATALOG.len(), 7);
        let keys: Vec<&str> = BUNDLED_PLUGIN_CATALOG
            .iter()
            .map(|e| e.key.as_str())
            .collect();
        assert!(keys.contains(&"cloudflare"));
        assert!(keys.contains(&"daytona"));
        assert!(keys.contains(&"e2b"));
        assert!(keys.contains(&"exe-dev"));
        assert!(keys.contains(&"kubernetes"));
        assert!(keys.contains(&"modal"));
        assert!(keys.contains(&"novita"));
    }

    #[test]
    fn kubernetes_entry_has_path_override_env_var() {
        let entry = BUNDLED_PLUGIN_CATALOG
            .iter()
            .find(|e| e.key == "kubernetes")
            .expect("kubernetes entry must exist");
        assert_eq!(
            entry.path_override_env_var.as_deref(),
            Some("PAPERCLIP_KUBERNETES_PLUGIN_PATH")
        );
        assert_eq!(entry.relative_path, "sandbox-providers/kubernetes");
        assert_eq!(entry.plugin_key, "paperclip.kubernetes-sandbox-provider");
    }

    #[test]
    fn other_entries_have_no_path_override() {
        for entry in BUNDLED_PLUGIN_CATALOG.iter() {
            if entry.key == "kubernetes" {
                continue;
            }
            assert!(
                entry.path_override_env_var.is_none(),
                "entry {} unexpectedly has pathOverrideEnvVar",
                entry.key
            );
        }
    }

    #[test]
    fn self_hosted_auto_install_keys_only_kubernetes() {
        assert_eq!(SELF_HOSTED_AUTO_INSTALL_KEYS, &["kubernetes"]);
    }

    #[test]
    fn resolve_bundled_catalog_root_default_when_env_empty() {
        let env: EnvMap = EnvMap::new();
        assert_eq!(resolve_bundled_catalog_root(&env), "/app/packages/plugins");
    }

    #[test]
    fn resolve_bundled_catalog_root_uses_env_override() {
        let mut env = EnvMap::new();
        env.insert(
            BUNDLED_CATALOG_ROOT_ENV_VAR.to_string(),
            "/tmp/test-catalog".to_string(),
        );
        assert_eq!(resolve_bundled_catalog_root(&env), "/tmp/test-catalog");
    }

    #[test]
    fn resolve_bundled_catalog_root_trims_whitespace() {
        let mut env = EnvMap::new();
        env.insert(
            BUNDLED_CATALOG_ROOT_ENV_VAR.to_string(),
            "   /tmp/trimmed   ".to_string(),
        );
        assert_eq!(resolve_bundled_catalog_root(&env), "/tmp/trimmed");
    }

    #[test]
    fn resolve_bundled_catalog_root_whitespace_only_falls_back() {
        let mut env = EnvMap::new();
        env.insert(BUNDLED_CATALOG_ROOT_ENV_VAR.to_string(), "   ".to_string());
        assert_eq!(resolve_bundled_catalog_root(&env), "/app/packages/plugins");
    }
}
