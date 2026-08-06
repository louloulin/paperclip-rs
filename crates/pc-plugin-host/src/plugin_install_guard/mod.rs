//! Plugin install guard（与 Node `server/src/services/plugin-install-guard.ts` 1:1 对齐）。
//!
//! ## 职责
//! 1. **Cloud install floor**：`is_cloud_managed_instance(env)` → 若 true，则禁止
//!    npm/registry 安装和任意 `localPath`，**仅允许**位于 bundled plugin catalog
//!    root 内部的 `localPath`。
//! 2. **localPath canonicalization**：把原始 `localPath` 字符串解析成绝对路径，
//!    resolve 所有 symlink，验证是已存在的可读目录。
//! 3. **Catalog containment**：canonicalized 路径必须**严格位于** bundled
//!    catalog root 内部（segment-based 比较，不是字符串前缀）。
//!
//! ## 设计原则
//! - 全部基于 tokio async IO（`tokio::fs::canonicalize` / `tokio::fs::metadata`）
//! - 不持任何状态；纯函数 + async IO
//! - 决策基于 env presence（不读 managed-config 文档内容），防止 corrupted
//!   document 静默 widen install surface
//! - 与 bundled_plugins 模块（Round 114）共享 `MANAGED_CONFIG_ENV_KEY` 常量
//!
//! ## 失败语义
//! - Cloud instance + 非 catalog 路径 → `false`（fail closed）
//! - canonicalization 失败 → `false`（fail closed）
//! - 不存在 / 不可读 → `false`（fail closed）

use std::path::{Component, Path, PathBuf};

// ============================================================================
// Constants
// ============================================================================

/// Cloud managed-config env key（与 Node `MANAGED_CONFIG_ENV_KEY` 1:1 对齐）。
///
/// 与 bundled_plugins 模块保持同名常量以确保一致。
pub const MANAGED_CONFIG_ENV_KEY: &str = "PAPERCLIP_MANAGED_CONFIG";

/// Bundled plugin catalog root（与 Node `BUNDLED_LOCAL_PLUGIN_ROOT` 1:1 对齐）。
///
/// 与 bundled_plugins::DEFAULT_BUNDLED_CATALOG_ROOT 不同：
/// - `BUNDLED_LOCAL_PLUGIN_ROOT` = repo 内的 `packages/plugins`（开发态）
/// - `DEFAULT_BUNDLED_CATALOG_ROOT` = release image 的 `/app/packages/plugins`（生产态）
///
/// Node 端这两个常量共存：dev 测试用 `BUNDLED_LOCAL_PLUGIN_ROOT`，
/// 部署态 release 用 `STANDALONE_BUNDLED_PLUGIN_ROOT`（与之同值）。
/// Rust 端保留 `DEFAULT_BUNDLED_CATALOG_ROOT`（Round 114 已 port）+ 当前 `BUNDLED_LOCAL_PLUGIN_ROOT`，
/// 两个常量职责清晰分离。
pub const BUNDLED_LOCAL_PLUGIN_ROOT: &str = "/app/packages/plugins";

// ============================================================================
// EnvMap
// ============================================================================

/// Env-like map（与 bundled_plugins::EnvMap 同义）。
pub type EnvMap = std::collections::HashMap<String, String>;

// ============================================================================
// is_cloud_managed_instance
// ============================================================================

/// 是否为 cloud managed instance（与 Node `isCloudManagedInstance(env)` 1:1 对齐）。
///
/// **Presence-based**：只看 env 是否有 `MANAGED_CONFIG_ENV_KEY`，不读文档内容。
/// 决策基于 env presence 意味着 corrupted/truncated/attacker-influenced document
/// **不能**禁用 install floor（fail closed）。
pub fn is_cloud_managed_instance(env: &EnvMap) -> bool {
    env.contains_key(MANAGED_CONFIG_ENV_KEY)
}

// ============================================================================
// LocalPluginPathValidation
// ============================================================================

/// `canonicalize_local_plugin_path` 的返回结果（与 Node `LocalPluginPathValidation` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalPluginPathValidation {
    /// 成功：`canonical_path` 是绝对路径 + symlink resolved + 存在 + 是目录
    Ok { canonical_path: String },
    /// 失败：`reason` 是人类可读错误描述
    Failed { reason: String },
}

// ============================================================================
// canonicalize_local_plugin_path
// ============================================================================

/// Canonicalize + 校验 local plugin install path（与 Node
/// `canonicalizeLocalPluginPath(rawPath)` 1:1 对齐）。
///
/// - 空字节 (`\0`) → fail（防止 null byte injection）
/// - `path.resolve` 等价的 lexical resolve → 绝对路径
/// - `realpath` resolve 所有 symlink + `..` 段
/// - 必须存在且是目录
pub async fn canonicalize_local_plugin_path(raw_path: &str) -> LocalPluginPathValidation {
    if raw_path.contains('\0') {
        return LocalPluginPathValidation::Failed {
            reason: "path contains a null byte".to_string(),
        };
    }

    let absolute_path = lexical_resolve(raw_path);

    // tokio::fs::canonicalize 是异步版 realpath
    let canonical_path_buf = match tokio::fs::canonicalize(&absolute_path).await {
        Ok(p) => p,
        Err(_) => {
            return LocalPluginPathValidation::Failed {
                reason: format!("path does not exist: {}", absolute_path),
            };
        }
    };

    let canonical_path = canonical_path_buf.to_string_lossy().into_owned();

    // 验证是目录且可读
    match tokio::fs::metadata(&canonical_path).await {
        Ok(meta) => {
            if !meta.is_dir() {
                return LocalPluginPathValidation::Failed {
                    reason: format!("path is not a directory: {}", canonical_path),
                };
            }
        }
        Err(_) => {
            return LocalPluginPathValidation::Failed {
                reason: format!("path is not readable: {}", canonical_path),
            };
        }
    }

    LocalPluginPathValidation::Ok { canonical_path }
}

// ============================================================================
// is_within_bundled_plugin_root
// ============================================================================

/// canonical path 是否严格位于 bundled plugin catalog root 内部（与 Node
/// `isWithinBundledPluginRoot(canonicalPath, bundledRootOverride?)` 1:1 对齐）。
///
/// - bundled root 不存在 → fail closed（返回 false）
/// - root 本身不视为"内部"——install source 必须是 root 内的子目录
/// - segment-based 比较（`Path::strip_prefix`），不是字符串前缀
pub async fn is_within_bundled_plugin_root(
    canonical_path: &str,
    bundled_root_override: Option<&str>,
) -> bool {
    let bundled_root = bundled_root_override.unwrap_or(BUNDLED_LOCAL_PLUGIN_ROOT);

    let canonical_root_buf = match tokio::fs::canonicalize(bundled_root).await {
        Ok(p) => p,
        Err(_) => return false, // No catalog root on disk → fail closed
    };
    let canonical_root = canonical_root_buf.to_string_lossy().into_owned();

    let canonical_path_p = Path::new(canonical_path);
    let canonical_root_p = Path::new(&canonical_root);

    // segment-based 比较
    match canonical_path_p.strip_prefix(canonical_root_p) {
        Ok(rel) => {
            // rel 必须非空（root 本身不算内部） + 不以 `..` 开头
            let rel_str = rel.to_string_lossy();
            !rel_str.is_empty() && !rel_str.starts_with("..")
        }
        Err(_) => false,
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Lexical 路径 normalize（与 `std::path::Path::canonicalize` 的 lexical 部分等价）。
///
/// 与 Node `path.resolve` 1:1 对齐：解析 `..` / `.`，相对路径相对 cwd。
/// 不做 symlink resolve（那是 `realpath` 的工作）。
fn lexical_resolve(p: &str) -> String {
    let path = Path::new(p);
    let cwd = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };

    // Start with cwd components if path is relative
    let mut components: Vec<Component> = if path.is_absolute() {
        Vec::new()
    } else {
        cwd.components().collect()
    };

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(last) = components.last() {
                    if matches!(last, Component::ParentDir) || !is_normal(last) {
                        components.push(component);
                    } else {
                        components.pop();
                    }
                } else {
                    // Path goes above root - keep the .. to indicate this
                    components.push(component);
                }
            }
            other => components.push(other),
        }
    }

    let mut result = PathBuf::new();
    for c in components {
        result.push(c.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result.to_string_lossy().into_owned()
}

fn is_normal(c: &Component) -> bool {
    matches!(c, Component::Normal(_))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ----- is_cloud_managed_instance -----

    #[test]
    fn cloud_managed_when_env_key_present_with_empty_value() {
        let env: HashMap<String, String> =
            HashMap::from([(MANAGED_CONFIG_ENV_KEY.to_string(), String::new())]);
        // Presence-based: empty value also counts as cloud-managed
        assert!(is_cloud_managed_instance(&env));
    }

    #[test]
    fn cloud_managed_when_env_key_present_with_value() {
        let env: HashMap<String, String> =
            HashMap::from([(MANAGED_CONFIG_ENV_KEY.to_string(), "{\"v\":1}".to_string())]);
        assert!(is_cloud_managed_instance(&env));
    }

    #[test]
    fn not_cloud_managed_when_env_key_absent() {
        let env: HashMap<String, String> = HashMap::new();
        assert!(!is_cloud_managed_instance(&env));
    }

    // ----- lexical_resolve -----

    #[test]
    fn lexical_resolve_absolute_unchanged() {
        assert_eq!(
            lexical_resolve("/app/packages/plugins"),
            "/app/packages/plugins"
        );
    }

    #[test]
    fn lexical_resolve_relative_to_dot() {
        let result = lexical_resolve("./foo/bar");
        // relative paths are resolved against cwd, which always exists
        assert!(result.starts_with('/'));
        assert!(result.ends_with("/foo/bar"));
    }

    #[test]
    fn lexical_resolve_parent_dir_collapse() {
        let result = lexical_resolve("/app/foo/../bar");
        assert_eq!(result, "/app/bar");
    }

    #[test]
    fn lexical_resolve_multiple_parent() {
        let result = lexical_resolve("/app/foo/bar/../../baz");
        assert_eq!(result, "/app/baz");
    }

    #[test]
    fn lexical_resolve_empty_returns_cwd_or_dot() {
        // Node `path.resolve("")` returns cwd; 我们 lexical resolve 行为对齐
        // Empty input is treated as relative → resolves to cwd (which is "." if not absolute).
        // On macOS / Linux cwd is absolute like "/Users/..." → returns it.
        let result = lexical_resolve("");
        // Just check that it doesn't return empty string and contains at least one char.
        assert!(!result.is_empty());
        // Either "." or absolute cwd path; both are acceptable.
        assert!(result == "." || result.starts_with('/'));
    }

    // ----- canonicalize_local_plugin_path: null byte rejection -----

    #[tokio::test]
    async fn rejects_null_byte() {
        let result = canonicalize_local_plugin_path("/tmp/foo\0bar").await;
        match result {
            LocalPluginPathValidation::Failed { reason } => {
                assert!(reason.contains("null byte"));
            }
            _ => panic!("expected failure"),
        }
    }

    // ----- canonicalize_local_plugin_path: existing directory -----

    #[tokio::test]
    async fn canonicalizes_existing_directory() {
        // /tmp 总是存在
        let result = canonicalize_local_plugin_path("/tmp").await;
        match result {
            LocalPluginPathValidation::Ok { canonical_path } => {
                // canonical_path 可能是 /private/tmp（macOS）或 /tmp（Linux）
                assert!(canonical_path.ends_with("tmp"));
            }
            LocalPluginPathValidation::Failed { reason } => {
                panic!("expected ok, got: {}", reason);
            }
        }
    }

    #[tokio::test]
    async fn rejects_nonexistent_path() {
        let result =
            canonicalize_local_plugin_path("/nonexistent/path/that/should/not/exist/12345").await;
        match result {
            LocalPluginPathValidation::Failed { reason } => {
                assert!(reason.contains("does not exist"));
            }
            _ => panic!("expected failure"),
        }
    }

    #[tokio::test]
    async fn rejects_file_not_directory() {
        // Use a known file (this very test file's path → Cargo.toml via env var)
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let path_str = path.to_string_lossy().into_owned();
        let result = canonicalize_local_plugin_path(&path_str).await;
        match result {
            LocalPluginPathValidation::Failed { reason } => {
                assert!(
                    reason.contains("not a directory"),
                    "unexpected reason: {}",
                    reason
                );
            }
            LocalPluginPathValidation::Ok { canonical_path } => {
                panic!("expected failure for file, got ok: {}", canonical_path);
            }
        }
    }

    // ----- is_within_bundled_plugin_root -----

    #[tokio::test]
    async fn within_root_when_path_is_subdirectory() {
        // Create a temp dir + subdir
        let temp_root = std::env::temp_dir().join("paperclip_pig_test_root");
        let sub = temp_root.join("plugin_a");
        let _ = std::fs::create_dir_all(&sub);
        let canonical_path = tokio::fs::canonicalize(&sub).await.unwrap();
        let canonical_path_str = canonical_path.to_string_lossy().into_owned();

        let result =
            is_within_bundled_plugin_root(&canonical_path_str, Some(&temp_root.to_string_lossy()))
                .await;
        assert!(result);

        let _ = std::fs::remove_dir_all(&temp_root);
    }

    #[tokio::test]
    async fn not_within_root_when_path_is_root_itself() {
        let temp_root = std::env::temp_dir().join("paperclip_pig_test_root_itself");
        let _ = std::fs::create_dir_all(&temp_root);
        let canonical_path = tokio::fs::canonicalize(&temp_root).await.unwrap();
        let canonical_path_str = canonical_path.to_string_lossy().into_owned();

        let result =
            is_within_bundled_plugin_root(&canonical_path_str, Some(&temp_root.to_string_lossy()))
                .await;
        // Root itself 不算"内部"
        assert!(!result);

        let _ = std::fs::remove_dir_all(&temp_root);
    }

    #[tokio::test]
    async fn not_within_root_when_path_is_sibling() {
        let temp_a = std::env::temp_dir().join("paperclip_pig_test_a");
        let temp_b = std::env::temp_dir().join("paperclip_pig_test_b");
        let _ = std::fs::create_dir_all(&temp_a);
        let _ = std::fs::create_dir_all(&temp_b);

        let canonical_b = tokio::fs::canonicalize(&temp_b).await.unwrap();
        let canonical_b_str = canonical_b.to_string_lossy().into_owned();

        let result =
            is_within_bundled_plugin_root(&canonical_b_str, Some(&temp_a.to_string_lossy())).await;
        assert!(!result);

        let _ = std::fs::remove_dir_all(&temp_a);
        let _ = std::fs::remove_dir_all(&temp_b);
    }

    #[tokio::test]
    async fn not_within_root_when_bundled_root_missing() {
        // bundled_root 不存在 → fail closed
        let nonexistent_root = "/path/that/should/not/exist/bundled_root_99999";
        let result = is_within_bundled_plugin_root("/tmp", Some(nonexistent_root)).await;
        assert!(!result);
    }

    // ----- bundled_plugins_env integration -----

    #[test]
    fn bundled_plugins_env_uses_same_env_key() {
        // 与 bundled_plugins::MANAGED_CONFIG_ENV_KEY 保持一致
        assert_eq!(MANAGED_CONFIG_ENV_KEY, "PAPERCLIP_MANAGED_CONFIG");
    }
}
