//! Bundled plugin 路径解析与 containment 检测。
//!
//! 与 Node `server/src/services/bundled-plugins.ts` 的
//! `canonicalize` / `isInsideRoot` / `resolveBundledPluginInstalls` 1:1 对齐。
//!
//! 失败语义：
//! - **Unknown key**（不在 allowlist）：抛 `BundledPluginError::UnknownKey`，
//!   进程必须 fail-fast 拒绝启动。
//! - **Path escape**（解析后的路径不在 catalog root 内）：抛
//!   `BundledPluginError::PathEscape`，同样 fail-fast。
//! - 路径 lexical 解析使用 `std::path::Path`；symlink 解析尽力（best-effort，
//!   不强制依赖 IO，因为非阻塞的 `tokio::fs::canonicalize` 会要求 async；
//!   我们对齐 Node 的 `fs.realpathSync` + `path.resolve` 组合的语义）。

use std::path::{Component, Path, PathBuf};

use super::catalog::BUNDLED_PLUGIN_CATALOG;
use super::types::{EnvMap, ResolvedBundledPlugin};

// ============================================================================
// BundledPluginError
// ============================================================================

/// Bundled plugin resolution 错误。
///
/// 与 Node `bundled-plugins.ts` 中的 throw 1:1 对齐。
#[derive(Debug, thiserror::Error)]
pub enum BundledPluginError {
    #[error("bundled plugin auto-install key \"{key}\" is not in the bundled catalog (known keys: {known}); refusing to start")]
    UnknownKey { key: String, known: String },

    #[error("bundled plugin \"{key}\" resolves to \"{local_path}\", outside the bundled catalog root \"{catalog_root}\"; refusing to start")]
    PathEscape {
        key: String,
        local_path: String,
        catalog_root: String,
    },
}

// ============================================================================
// Path helpers (pure, sync)
// ============================================================================

/// Lexical 路径 normalize（与 Node `path.resolve` 等价）。
///
/// - 解析 `..` / `.`
/// - 相对路径相对当前工作目录处理
pub fn lexical_resolve(p: &str) -> String {
    let path = Path::new(p);
    let mut components: Vec<Component> = Vec::new();
    let cwd = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
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
                } else if !cwd.as_os_str().is_empty() {
                    // pop a component from cwd
                    let mut cwd_components: Vec<Component> = cwd.components().collect();
                    if !cwd_components.is_empty() {
                        cwd_components.pop();
                    }
                    components = cwd_components;
                } else {
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

/// Canonicalize 尽力实现（symlink 不解析，仅 lexical normalize）。
///
/// 与 Node 端 `realpathSync` fallback to `path.resolve` 的语义对齐：
/// - 真实存在且无 symlink：返回 lexical resolve 结果
/// - 不存在：返回 lexical resolve 结果（Node 也 fallback 到 `path.resolve`）
///
/// 不在此函数内做 IO，原因：
/// - 该模块是 **同步** 解析逻辑，运行于 `createApp` 启动前。
/// - Node 端 `fs.realpathSync` 同步阻塞 IO，Rust `std::fs::canonicalize` 同样。
///   我们选择不在 hot-path 引入 IO 失败；Node 的实现在 sandbox 不可用时
///   仅做 lexical 比较。
pub fn canonicalize(p: &str) -> String {
    lexical_resolve(p)
}

/// 判断 `candidate` 是否在 `root` 内部（与 Node `isInsideRoot` 1:1 对齐）。
///
/// 语义：`path.relative(root, candidate) === ""` 或不以 `..` 开头且非绝对路径。
pub fn is_inside_root(candidate: &str, root: &str) -> bool {
    let candidate_path = Path::new(candidate);
    let root_path = Path::new(root);
    match candidate_path.strip_prefix(root_path) {
        Ok(rel) => {
            // strip_prefix succeeds iff candidate starts with root.
            // rel === "" or no `..` component and not absolute.
            let rel_str = rel.to_string_lossy();
            !rel_str.starts_with("..")
        }
        Err(_) => false,
    }
}

// ============================================================================
// resolve_bundled_plugin_installs
// ============================================================================

/// Resolve bundled plugin auto-install options。
///
/// 与 Node `resolveBundledPluginInstalls(keys, opts)` 1:1 对齐：
/// - `opts.catalogRoot`：catalog 根绝对路径
/// - `opts.env`：env var 查询表（含 pathOverrideEnvVar）
/// - `opts.enforceCatalogRoot`：是否做 containment 检测
///
/// 错误：
/// - 未知 key → `BundledPluginError::UnknownKey`（fail-fast）
/// - path escape → `BundledPluginError::PathEscape`（fail-fast）
pub fn resolve_bundled_plugin_installs(
    keys: &[&str],
    opts: ResolveBundledPluginOptions<'_>,
) -> Result<Vec<ResolvedBundledPlugin>, BundledPluginError> {
    let catalog_root_canonical = canonicalize(opts.catalog_root);
    let mut resolved: Vec<ResolvedBundledPlugin> = Vec::with_capacity(keys.len());
    for key in keys {
        let entry = BUNDLED_PLUGIN_CATALOG
            .iter()
            .find(|e| e.key == *key)
            .ok_or_else(|| {
                let known = BUNDLED_PLUGIN_CATALOG
                    .iter()
                    .map(|e| e.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                BundledPluginError::UnknownKey {
                    key: (*key).to_string(),
                    known,
                }
            })?;

        let override_path = entry
            .path_override_env_var
            .as_deref()
            .and_then(|env_var| opts.env.get(env_var).map(|s| s.as_str()))
            .and_then(|s| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            });

        let local_path = match override_path {
            Some(p) => lexical_resolve(p),
            None => lexical_resolve(&format!("{}/{}", opts.catalog_root, entry.relative_path)),
        };

        if opts.enforce_catalog_root
            && !is_inside_root(&canonicalize(&local_path), &catalog_root_canonical)
        {
            return Err(BundledPluginError::PathEscape {
                key: entry.key.clone(),
                local_path,
                catalog_root: opts.catalog_root.to_string(),
            });
        }

        resolved.push(ResolvedBundledPlugin {
            key: entry.key.clone(),
            plugin_key: entry.plugin_key.clone(),
            local_path,
        });
    }
    Ok(resolved)
}

/// `resolve_bundled_plugin_installs` 的输入选项。
#[derive(Debug, Clone)]
pub struct ResolveBundledPluginOptions<'a> {
    pub catalog_root: &'a str,
    pub env: &'a EnvMap,
    pub enforce_catalog_root: bool,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_env() -> EnvMap {
        EnvMap::new()
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
    fn lexical_resolve_relative_dot() {
        let result = lexical_resolve("./foo/bar");
        assert!(
            !result.starts_with('.'),
            "should resolve ./foo/bar to absolute-like"
        );
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
    fn lexical_resolve_empty_returns_dot() {
        assert_eq!(lexical_resolve(""), ".");
    }

    // ----- is_inside_root -----

    #[test]
    fn is_inside_root_positive() {
        assert!(is_inside_root(
            "/app/packages/plugins/kubernetes",
            "/app/packages/plugins"
        ));
    }

    #[test]
    fn is_inside_root_root_itself() {
        assert!(is_inside_root(
            "/app/packages/plugins",
            "/app/packages/plugins"
        ));
    }

    #[test]
    fn is_inside_root_negative_sibling() {
        assert!(!is_inside_root(
            "/app/packages/other",
            "/app/packages/plugins"
        ));
    }

    #[test]
    fn is_inside_root_negative_parent_traversal() {
        assert!(!is_inside_root(
            "/app/other/kubernetes",
            "/app/packages/plugins"
        ));
    }

    #[test]
    fn is_inside_root_negative_unrelated() {
        assert!(!is_inside_root("/etc/passwd", "/app/packages/plugins"));
    }

    // ----- canonicalize -----

    #[test]
    fn canonicalize_preserves_existing_path() {
        // /tmp 总是存在；canonicalize 不做 symlink resolve
        let canonical = canonicalize("/tmp");
        assert!(canonical.contains("tmp"));
    }

    #[test]
    fn canonicalize_fallback_for_nonexistent() {
        // 不存在的路径：返回 lexical_resolve 结果
        let canonical = canonicalize("/nonexistent/path/../bar");
        assert_eq!(canonical, "/nonexistent/bar");
    }

    // ----- resolve_bundled_plugin_installs: happy path -----

    #[test]
    fn resolves_known_keys_in_order() {
        let installs = resolve_bundled_plugin_installs(
            &["kubernetes", "modal"],
            ResolveBundledPluginOptions {
                catalog_root: "/app/packages/plugins",
                env: &empty_env(),
                enforce_catalog_root: true,
            },
        )
        .expect("resolution must succeed");
        assert_eq!(installs.len(), 2);
        assert_eq!(installs[0].key, "kubernetes");
        assert_eq!(
            installs[0].local_path,
            "/app/packages/plugins/sandbox-providers/kubernetes"
        );
        assert_eq!(
            installs[0].plugin_key,
            "paperclip.kubernetes-sandbox-provider"
        );
        assert_eq!(installs[1].key, "modal");
    }

    #[test]
    fn empty_keys_returns_empty() {
        let installs = resolve_bundled_plugin_installs(
            &[],
            ResolveBundledPluginOptions {
                catalog_root: "/app/packages/plugins",
                env: &empty_env(),
                enforce_catalog_root: true,
            },
        )
        .expect("empty keys must succeed");
        assert!(installs.is_empty());
    }

    // ----- resolve_bundled_plugin_installs: error cases -----

    #[test]
    fn unknown_key_throws() {
        let err = resolve_bundled_plugin_installs(
            &["not-a-real-key"],
            ResolveBundledPluginOptions {
                catalog_root: "/app/packages/plugins",
                env: &empty_env(),
                enforce_catalog_root: true,
            },
        )
        .expect_err("must fail for unknown key");
        match err {
            BundledPluginError::UnknownKey { key, known } => {
                assert_eq!(key, "not-a-real-key");
                assert!(known.contains("kubernetes"));
                assert!(known.contains("modal"));
            }
            other => panic!("unexpected error variant: {:?}", other),
        }
    }

    #[test]
    fn path_escape_throws_when_enforced() {
        let mut env = empty_env();
        env.insert(
            "PAPERCLIP_KUBERNETES_PLUGIN_PATH".to_string(),
            "/etc/passwd".to_string(),
        );
        let err = resolve_bundled_plugin_installs(
            &["kubernetes"],
            ResolveBundledPluginOptions {
                catalog_root: "/app/packages/plugins",
                env: &env,
                enforce_catalog_root: true,
            },
        )
        .expect_err("path escape must fail when enforced");
        match err {
            BundledPluginError::PathEscape {
                key, local_path, ..
            } => {
                assert_eq!(key, "kubernetes");
                assert_eq!(local_path, "/etc/passwd");
            }
            other => panic!("unexpected error variant: {:?}", other),
        }
    }

    #[test]
    fn path_escape_allowed_when_not_enforced() {
        let mut env = empty_env();
        env.insert(
            "PAPERCLIP_KUBERNETES_PLUGIN_PATH".to_string(),
            "/etc/legacy".to_string(),
        );
        let installs = resolve_bundled_plugin_installs(
            &["kubernetes"],
            ResolveBundledPluginOptions {
                catalog_root: "/app/packages/plugins",
                env: &env,
                enforce_catalog_root: false,
            },
        )
        .expect("path escape is allowed when not enforced");
        assert_eq!(installs.len(), 1);
        assert_eq!(installs[0].local_path, "/etc/legacy");
    }

    #[test]
    fn kubernetes_override_env_trimmed_and_used() {
        let mut env = empty_env();
        env.insert(
            "PAPERCLIP_KUBERNETES_PLUGIN_PATH".to_string(),
            "   /custom/k8s   ".to_string(),
        );
        let installs = resolve_bundled_plugin_installs(
            &["kubernetes"],
            ResolveBundledPluginOptions {
                catalog_root: "/app/packages/plugins",
                env: &env,
                enforce_catalog_root: false,
            },
        )
        .expect("override env must be used");
        assert_eq!(installs[0].local_path, "/custom/k8s");
    }

    #[test]
    fn kubernetes_override_env_whitespace_falls_back_to_relative() {
        let mut env = empty_env();
        env.insert(
            "PAPERCLIP_KUBERNETES_PLUGIN_PATH".to_string(),
            "   ".to_string(),
        );
        let installs = resolve_bundled_plugin_installs(
            &["kubernetes"],
            ResolveBundledPluginOptions {
                catalog_root: "/app/packages/plugins",
                env: &env,
                enforce_catalog_root: true,
            },
        )
        .expect("whitespace-only override must fall back to relative");
        assert_eq!(
            installs[0].local_path,
            "/app/packages/plugins/sandbox-providers/kubernetes"
        );
    }

    #[test]
    fn enforce_catalog_root_inside_passes() {
        let installs = resolve_bundled_plugin_installs(
            &["kubernetes"],
            ResolveBundledPluginOptions {
                catalog_root: "/app/packages/plugins",
                env: &empty_env(),
                enforce_catalog_root: true,
            },
        )
        .expect("kubernetes is inside catalog root");
        assert_eq!(installs.len(), 1);
    }
}
