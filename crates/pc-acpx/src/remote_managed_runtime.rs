//! `pc-acpx::remote_managed_runtime` - port of `remote-managed-runtime.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! Pure helpers for SSH-backed managed runtimes. The async staging
//! logic is deferred to a follow-up round (depends on `ssh.ts`); this
//! module ports the session-identity type, the equality check, and the
//! exclude-pattern generator.

use serde::{Deserialize, Serialize};
use crate::exclude_patterns::exclude_pattern_matches;
use crate::git_workspace_sync::GIT_ARCHIVE_EXCLUDES;

/// Local mirror of `SshRemoteExecutionSpec` from `ssh.ts`. The full ssh
/// module is deferred; for now we only need the fields that participate
/// in session-identity comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SshRemoteExecutionSpec {
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub remote_cwd: String,
}

/// Heavy-directory exclude patterns used when staging additional
/// sources for a remote runtime. Mirrors Node
/// `REMOTE_ADDITIONAL_SOURCE_HEAVY_DIR_EXCLUDES`.
pub const REMOTE_ADDITIONAL_SOURCE_HEAVY_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "dist",
    "build",
    "out",
    "coverage",
    ".next",
    ".turbo",
    ".cache",
    ".git",
];

/// Expand a list of base names into the four pattern shapes Node uses
/// for "exclude base name anywhere in the tree":
/// `name`, `name/*`, `*/name`, `*/name/*`.
/// Mirrors `entry => [entry, `${entry}/*`, `*/${entry}`, `*/${entry}/*`]`.
#[must_use]
pub fn expand_heavy_dir_excludes() -> Vec<String> {
    let mut patterns = Vec::new();
    for entry in REMOTE_ADDITIONAL_SOURCE_HEAVY_DIRS {
        patterns.push(entry.to_string());
        patterns.push(format!("{entry}/*"));
        patterns.push(format!("*/{entry}"));
        patterns.push(format!("*/{entry}/*"));
    }
    patterns
}

/// Build the runtime-root directory for a remote managed runtime.
/// Mirrors the inline calculation in Node `prepareRemoteManagedRuntime`:
/// `posix.join(workspaceRemoteDir, ".paperclip-runtime", adapterKey)`.
#[must_use]
pub fn resolve_runtime_root_dir(workspace_remote_dir: &str, adapter_key: &str) -> String {
    format!("{workspace_remote_dir}/.paperclip-runtime/{adapter_key}")
}

/// Build the per-run workspace remote directory. Mirrors Node:
/// `posix.join(baseWorkspaceRemoteDir, ".paperclip-runtime", "runs", runId, "workspace")`.
#[must_use]
pub fn resolve_run_workspace_remote_dir(base_workspace_remote_dir: &str, run_id: &str) -> String {
    format!("{base_workspace_remote_dir}/.paperclip-runtime/runs/{run_id}/workspace")
}

/// Build the per-asset remote directory under the runtime root. Mirrors
/// `posix.join(runtimeRootDir, asset.key)`.
#[must_use]
pub fn resolve_asset_remote_dir(runtime_root_dir: &str, asset_key: &str) -> String {
    format!("{runtime_root_dir}/{asset_key}")
}

/// The session identity used to compare a saved session against the
/// current runtime spec. Mirrors Node `buildRemoteExecutionSessionIdentity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteExecutionSessionIdentity {
    pub transport: &'static str,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub remote_cwd: String,
}

/// Build the session identity for an SSH spec. Mirrors Node
/// `buildRemoteExecutionSessionIdentity`.
#[must_use]
pub fn build_remote_execution_session_identity(
    spec: Option<&SshRemoteExecutionSpec>,
) -> Option<RemoteExecutionSessionIdentity> {
    spec.map(|s| RemoteExecutionSessionIdentity {
        transport: "ssh",
        host: s.host.clone(),
        port: s.port,
        username: s.username.clone(),
        remote_cwd: s.remote_cwd.clone(),
    })
}

/// Whether a previously-saved session identity matches the current SSH
/// spec. Mirrors Node `remoteExecutionSessionMatches`.
#[must_use]
pub fn remote_execution_session_matches(
    saved: &serde_json::Value,
    current: Option<&SshRemoteExecutionSpec>,
) -> bool {
    let Some(current_identity) = build_remote_execution_session_identity(current) else {
        return false;
    };
    let saved_obj = match saved.as_object() {
        Some(o) => o,
        None => return false,
    };
    let transport = saved_obj.get("transport").and_then(|v| v.as_str()).unwrap_or("");
    let host = saved_obj.get("host").and_then(|v| v.as_str()).unwrap_or("");
    let username = saved_obj.get("username").and_then(|v| v.as_str()).unwrap_or("");
    let remote_cwd = saved_obj.get("remoteCwd").and_then(|v| v.as_str()).unwrap_or("");
    let port = saved_obj.get("port").and_then(|v| v.as_u64()).map(|p| p as u16);

    transport == current_identity.transport
        && host == current_identity.host
        && username == current_identity.username
        && remote_cwd == current_identity.remote_cwd
        && port == current_identity.port
}

/// Check whether a relative path should be excluded from the heavy-dir
/// exclude set. Mirrors Node `shouldExcludePath(relative, heavyExcludes)`.
#[must_use]
pub fn should_exclude_heavy_dir(relative: &str) -> bool {
    let excludes = expand_heavy_dir_excludes();
    excludes
        .iter()
        .any(|p| exclude_pattern_matches(relative, p))
}

/// The exclude list for git-backed workspaces. Mirrors Node's
/// `[...GIT_ARCHIVE_EXCLUDES, ".paperclip-runtime"]`.
#[must_use]
pub fn git_backed_workspace_excludes() -> Vec<String> {
    let mut out: Vec<String> = GIT_ARCHIVE_EXCLUDES.iter().map(|s| s.to_string()).collect();
    out.push(".paperclip-runtime".to_string());
    out
}

/// The exclude list for non-git-backed workspaces (only
/// `.paperclip-runtime`).
#[must_use]
pub fn non_git_backed_workspace_excludes() -> Vec<String> {
    vec![".paperclip-runtime".to_string()]
}

// =============================================================================
// R474 — prepareRemoteManagedRuntime 决策纯函数
// =============================================================================

/// 单个远程运行时资产（对齐 Node `RemoteManagedRuntimeAsset`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteManagedRuntimeAsset {
    pub key: String,
    pub local_dir: String,
    pub follow_symlinks: bool,
    pub exclude: Option<Vec<String>>,
    /// 是否需要在运行结束后 restore（对齐 Node `asset.restore` 存在性）。
    pub restore: bool,
}

/// 额外引用项目（对齐 Node `SandboxAdditionalSource`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdditionalSource {
    pub local_path: String,
    pub project_id: String,
}

/// 远程受管运行时布局决策（对齐 Node `PreparedRemoteManagedRuntime` 的
/// 纯数据部分；不含闭包 restoreWorkspace / restore asset 回调）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRemoteManagedRuntimeLayout {
    pub workspace_remote_dir: String,
    pub runtime_root_dir: String,
    pub asset_dirs: std::collections::BTreeMap<String, String>,
    pub additional_source_dirs: std::collections::BTreeMap<String, String>,
}

/// 判定额外引用项目的 localPath 是否为 POSIX 绝对路径。
/// 对齐 Node `path.posix.isAbsolute(localPath)`。
#[must_use]
pub fn additional_source_local_path_is_absolute(local_path: &str) -> bool {
    local_path.starts_with('/')
}

/// 判定 projectId 是否为简单路径段（非空、不含 `/` `\` `..`）。
/// 对齐 Node `projectId.length === 0 || includes("/") || includes("\\") || includes("..")` 的取反。
#[must_use]
pub fn additional_source_project_id_is_valid(project_id: &str) -> bool {
    !project_id.is_empty()
        && !project_id.contains('/')
        && !project_id.contains('\\')
        && !project_id.contains("..")
}

/// 计算额外引用项目的远程目录：`<runtimeRootDir>/project-<projectId>`。
/// 对齐 Node `path.posix.join(runtimeRootDir, \`project-${projectId}\`)`。
#[must_use]
pub fn resolve_additional_source_remote_dir(runtime_root_dir: &str, project_id: &str) -> String {
    let base = runtime_root_dir.trim_end_matches('/');
    format!("{base}/project-{project_id}")
}

/// 计算 workspace 远程目录：
/// `syncWorkspace` 时 `<base>/.paperclip-runtime/runs/<runId>/workspace`，
/// 否则原样使用 base。对齐 Node `prepareRemoteManagedRuntime` 内联逻辑。
#[must_use]
pub fn resolve_prepared_workspace_remote_dir(
    base_workspace_remote_dir: &str,
    run_id: &str,
    sync_workspace: bool,
) -> String {
    if sync_workspace {
        resolve_run_workspace_remote_dir(base_workspace_remote_dir, run_id)
    } else {
        base_workspace_remote_dir.to_string()
    }
}

/// 计算单个资产的远程目录并登记到 assetDirs。
/// 对齐 Node `assetDirs[asset.key] = path.posix.join(runtimeRootDir, asset.key)`。
#[must_use]
pub fn resolve_asset_dirs(
    runtime_root_dir: &str,
    assets: &[RemoteManagedRuntimeAsset],
) -> std::collections::BTreeMap<String, String> {
    let mut dirs = std::collections::BTreeMap::new();
    for asset in assets {
        dirs.insert(asset.key.clone(), resolve_asset_remote_dir(runtime_root_dir, &asset.key));
    }
    dirs
}

/// 计算额外引用项目的远程目录映射（仅校验通过的项目）。
/// 对齐 Node `additionalSourceDirs[projectId] = remoteDir`（失败项目被跳过）。
#[must_use]
pub fn resolve_additional_source_dirs(
    runtime_root_dir: &str,
    additional_sources: &[AdditionalSource],
) -> std::collections::BTreeMap<String, String> {
    let mut dirs = std::collections::BTreeMap::new();
    for source in additional_sources {
        if !additional_source_local_path_is_absolute(&source.local_path) {
            continue;
        }
        if !additional_source_project_id_is_valid(&source.project_id) {
            continue;
        }
        dirs.insert(
            source.project_id.clone(),
            resolve_additional_source_remote_dir(runtime_root_dir, &source.project_id),
        );
    }
    dirs
}

/// 计算完整远程受管运行时布局。
///
/// 对齐 Node `prepareRemoteManagedRuntime` 的纯决策部分：
/// - workspaceRemoteDir（syncWorkspace 分支）
/// - runtimeRootDir（`<workspaceRemoteDir>/.paperclip-runtime/<adapterKey>`）
/// - assetDirs（每个 asset 一个远程目录）
/// - additionalSourceDirs（每个校验通过的 project 一个远程目录）
///
/// 真实 SSH 同步 / snapshot / restore 由调用方组合 `ssh.rs` 执行器完成。
#[must_use]
pub fn prepare_remote_managed_runtime_layout(
    base_workspace_remote_dir: &str,
    run_id: &str,
    adapter_key: &str,
    sync_workspace: bool,
    assets: &[RemoteManagedRuntimeAsset],
    additional_sources: &[AdditionalSource],
) -> PreparedRemoteManagedRuntimeLayout {
    let workspace_remote_dir =
        resolve_prepared_workspace_remote_dir(base_workspace_remote_dir, run_id, sync_workspace);
    let runtime_root_dir = resolve_runtime_root_dir(&workspace_remote_dir, adapter_key);
    let asset_dirs = resolve_asset_dirs(&runtime_root_dir, assets);
    let additional_source_dirs =
        resolve_additional_source_dirs(&runtime_root_dir, additional_sources);
    PreparedRemoteManagedRuntimeLayout {
        workspace_remote_dir,
        runtime_root_dir,
        asset_dirs,
        additional_source_dirs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_spec() -> SshRemoteExecutionSpec {
        SshRemoteExecutionSpec {
            host: "host.example.com".to_string(),
            port: Some(22),
            username: "paperclip".to_string(),
            remote_cwd: "/home/paperclip/work".to_string(),
        }
    }

    #[test]
    fn ssh_spec_construction() {
        let spec = sample_spec();
        assert_eq!(spec.host, "host.example.com");
        assert_eq!(spec.port, Some(22));
    }

    #[test]
    fn expand_heavy_dir_excludes_generates_four_shapes_per_base() {
        let excludes = expand_heavy_dir_excludes();
        // 10 base dirs × 4 shapes = 40 patterns
        assert_eq!(excludes.len(), 40);
        assert!(excludes.contains(&"node_modules".to_string()));
        assert!(excludes.contains(&"node_modules/*".to_string()));
        assert!(excludes.contains(&"*/node_modules".to_string()));
        assert!(excludes.contains(&"*/node_modules/*".to_string()));
    }

    #[test]
    fn resolve_runtime_root_dir_appends_paperclip_runtime_and_adapter() {
        assert_eq!(
            resolve_runtime_root_dir("/home/user/work", "claude_local"),
            "/home/user/work/.paperclip-runtime/claude_local"
        );
    }

    #[test]
    fn resolve_run_workspace_remote_dir_builds_per_run_path() {
        assert_eq!(
            resolve_run_workspace_remote_dir("/home/user/work", "run_abc"),
            "/home/user/work/.paperclip-runtime/runs/run_abc/workspace"
        );
    }

    #[test]
    fn resolve_asset_remote_dir_nests_under_runtime_root() {
        assert_eq!(
            resolve_asset_remote_dir("/ws/.paperclip-runtime/claude", "auth.json"),
            "/ws/.paperclip-runtime/claude/auth.json"
        );
    }

    #[test]
    fn session_identity_is_built_from_spec() {
        let identity = build_remote_execution_session_identity(Some(&sample_spec()));
        let identity = identity.unwrap();
        assert_eq!(identity.transport, "ssh");
        assert_eq!(identity.host, "host.example.com");
        assert_eq!(identity.username, "paperclip");
        assert_eq!(identity.port, Some(22));
        assert_eq!(identity.remote_cwd, "/home/paperclip/work");
    }

    #[test]
    fn session_identity_is_none_for_none_spec() {
        assert!(build_remote_execution_session_identity(None).is_none());
    }

    #[test]
    fn session_matches_with_equal_spec() {
        let current = sample_spec();
        let saved = json!({
            "transport": "ssh",
            "host": "host.example.com",
            "port": 22,
            "username": "paperclip",
            "remoteCwd": "/home/paperclip/work"
        });
        assert!(remote_execution_session_matches(&saved, Some(&current)));
    }

    #[test]
    fn session_mismatch_on_different_host() {
        let current = sample_spec();
        let saved = json!({
            "transport": "ssh",
            "host": "other.example.com",
            "port": 22,
            "username": "paperclip",
            "remoteCwd": "/home/paperclip/work"
        });
        assert!(!remote_execution_session_matches(&saved, Some(&current)));
    }

    #[test]
    fn session_mismatch_on_different_port() {
        let current = sample_spec();
        let saved = json!({
            "transport": "ssh",
            "host": "host.example.com",
            "port": 2222,
            "username": "paperclip",
            "remoteCwd": "/home/paperclip/work"
        });
        assert!(!remote_execution_session_matches(&saved, Some(&current)));
    }

    #[test]
    fn session_mismatch_on_different_cwd() {
        let current = sample_spec();
        let saved = json!({
            "transport": "ssh",
            "host": "host.example.com",
            "port": 22,
            "username": "paperclip",
            "remoteCwd": "/different/path"
        });
        assert!(!remote_execution_session_matches(&saved, Some(&current)));
    }

    #[test]
    fn session_match_returns_false_when_current_is_none() {
        let saved = json!({
            "transport": "ssh",
            "host": "host.example.com"
        });
        assert!(!remote_execution_session_matches(&saved, None));
    }

    #[test]
    fn session_match_returns_false_for_non_object_saved() {
        let current = sample_spec();
        assert!(!remote_execution_session_matches(&json!("not an object"), Some(&current)));
        assert!(!remote_execution_session_matches(&json!(null), Some(&current)));
    }

    #[test]
    fn session_match_handles_missing_port() {
        let mut current = sample_spec();
        current.port = None;
        let saved = json!({
            "transport": "ssh",
            "host": "host.example.com",
            "port": null,
            "username": "paperclip",
            "remoteCwd": "/home/paperclip/work"
        });
        assert!(remote_execution_session_matches(&saved, Some(&current)));
    }

    #[test]
    fn should_exclude_heavy_dir_matches_node_modules() {
        assert!(should_exclude_heavy_dir("node_modules/foo.js"));
        assert!(should_exclude_heavy_dir("a/node_modules/foo.js"));
        assert!(should_exclude_heavy_dir("a/node_modules"));
        assert!(should_exclude_heavy_dir("node_modules"));
        assert!(!should_exclude_heavy_dir("src/index.ts"));
    }

    #[test]
    fn git_backed_workspace_excludes_includes_git_and_paperclip_runtime() {
        let excludes = git_backed_workspace_excludes();
        assert!(excludes.contains(&".git".to_string()));
        assert!(excludes.contains(&".git/*".to_string()));
        assert!(excludes.contains(&".paperclip-runtime".to_string()));
    }

    #[test]
    fn non_git_backed_workspace_excludes_only_paperclip_runtime() {
        let excludes = non_git_backed_workspace_excludes();
        assert_eq!(excludes, vec![".paperclip-runtime".to_string()]);
    }

    // ------------------------------------------------------------------
    // R474 — prepareRemoteManagedRuntime 决策
    // ------------------------------------------------------------------

    #[test]
    fn additional_source_local_path_is_absolute_detects_absolute() {
        assert!(additional_source_local_path_is_absolute("/workspace/project-a"));
        assert!(!additional_source_local_path_is_absolute("workspace/project-a"));
        assert!(!additional_source_local_path_is_absolute("relative"));
        assert!(!additional_source_local_path_is_absolute(""));
    }

    #[test]
    fn additional_source_project_id_is_valid_rejects_bad_segments() {
        assert!(additional_source_project_id_is_valid("project-a"));
        assert!(additional_source_project_id_is_valid("a1"));
        assert!(!additional_source_project_id_is_valid(""));
        assert!(!additional_source_project_id_is_valid("a/b"));
        assert!(!additional_source_project_id_is_valid("a\\b"));
        assert!(!additional_source_project_id_is_valid(".."));
        assert!(!additional_source_project_id_is_valid("a/.."));
    }

    #[test]
    fn resolve_additional_source_remote_dir_builds_project_path() {
        assert_eq!(
            resolve_additional_source_remote_dir("/runtime/root", "proj-1"),
            "/runtime/root/project-proj-1"
        );
        assert_eq!(
            resolve_additional_source_remote_dir("/runtime/root/", "proj-1"),
            "/runtime/root/project-proj-1"
        );
    }

    #[test]
    fn resolve_prepared_workspace_remote_dir_sync_branch() {
        assert_eq!(
            resolve_prepared_workspace_remote_dir("/remote/workspace", "run-1", true),
            "/remote/workspace/.paperclip-runtime/runs/run-1/workspace"
        );
    }

    #[test]
    fn resolve_prepared_workspace_remote_dir_no_sync_branch() {
        assert_eq!(
            resolve_prepared_workspace_remote_dir("/remote/workspace", "run-1", false),
            "/remote/workspace"
        );
    }

    #[test]
    fn resolve_asset_dirs_maps_each_key() {
        let assets = vec![
            RemoteManagedRuntimeAsset {
                key: "skills".to_string(),
                local_dir: "/home/user/skills".to_string(),
                follow_symlinks: true,
                exclude: None,
                restore: false,
            },
            RemoteManagedRuntimeAsset {
                key: "home".to_string(),
                local_dir: "/home/user/codex-home".to_string(),
                follow_symlinks: true,
                exclude: None,
                restore: true,
            },
        ];
        let dirs = resolve_asset_dirs("/remote/workspace/.paperclip-runtime/codex", &assets);
        assert_eq!(
            dirs.get("skills").unwrap(),
            "/remote/workspace/.paperclip-runtime/codex/skills"
        );
        assert_eq!(
            dirs.get("home").unwrap(),
            "/remote/workspace/.paperclip-runtime/codex/home"
        );
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn resolve_additional_source_dirs_skips_invalid_sources() {
        let sources = vec![
            AdditionalSource {
                local_path: "/workspace/a".to_string(),
                project_id: "proj-a".to_string(),
            },
            AdditionalSource {
                local_path: "relative/b".to_string(),
                project_id: "proj-b".to_string(),
            },
            AdditionalSource {
                local_path: "/workspace/c".to_string(),
                project_id: "bad/segment".to_string(),
            },
            AdditionalSource {
                local_path: "/workspace/d".to_string(),
                project_id: "proj-d".to_string(),
            },
        ];
        let dirs = resolve_additional_source_dirs("/runtime/root", &sources);
        assert_eq!(
            dirs.get("proj-a").unwrap(),
            "/runtime/root/project-proj-a"
        );
        assert_eq!(
            dirs.get("proj-d").unwrap(),
            "/runtime/root/project-proj-d"
        );
        assert!(!dirs.contains_key("proj-b"));
        assert!(!dirs.contains_key("bad/segment"));
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn prepare_remote_managed_runtime_layout_full_flow() {
        let assets = vec![RemoteManagedRuntimeAsset {
            key: "config-seed".to_string(),
            local_dir: "/home/user/seed".to_string(),
            follow_symlinks: false,
            exclude: Some(vec![".tmp".to_string()]),
            restore: false,
        }];
        let sources = vec![AdditionalSource {
            local_path: "/workspace/referenced".to_string(),
            project_id: "ref-1".to_string(),
        }];
        let layout = prepare_remote_managed_runtime_layout(
            "/remote/workspace",
            "run-42",
            "claude",
            true,
            &assets,
            &sources,
        );
        assert_eq!(
            layout.workspace_remote_dir,
            "/remote/workspace/.paperclip-runtime/runs/run-42/workspace"
        );
        assert_eq!(
            layout.runtime_root_dir,
            "/remote/workspace/.paperclip-runtime/runs/run-42/workspace/.paperclip-runtime/claude"
        );
        assert_eq!(
            layout.asset_dirs.get("config-seed").unwrap(),
            "/remote/workspace/.paperclip-runtime/runs/run-42/workspace/.paperclip-runtime/claude/config-seed"
        );
        assert_eq!(
            layout.additional_source_dirs.get("ref-1").unwrap(),
            "/remote/workspace/.paperclip-runtime/runs/run-42/workspace/.paperclip-runtime/claude/project-ref-1"
        );
    }

    #[test]
    fn prepare_remote_managed_runtime_layout_no_sync_uses_base_dir() {
        let layout = prepare_remote_managed_runtime_layout(
            "/remote/workspace",
            "run-42",
            "codex",
            false,
            &[],
            &[],
        );
        assert_eq!(layout.workspace_remote_dir, "/remote/workspace");
        assert_eq!(
            layout.runtime_root_dir,
            "/remote/workspace/.paperclip-runtime/codex"
        );
        assert!(layout.asset_dirs.is_empty());
        assert!(layout.additional_source_dirs.is_empty());
    }
}
