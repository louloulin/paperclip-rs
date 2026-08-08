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
}
