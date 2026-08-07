//! `pc-acpx::local_process_sandbox` - port of `local-process-sandbox.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! Pure parsing helpers for local-process sandbox configuration. The
//! full `buildLocalProcessSandboxSpawnTarget` function requires
//! `bubblewrap` (`bwrap`) which is platform-specific; this module
//! ports the pure data-construction helpers that adapter callers use to
//! build sandbox options from a runtime config.

/// Filesystem access mode for a sandbox path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalProcessSandboxAccess {
    Ro,
    Rw,
}

impl LocalProcessSandboxAccess {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ro => "ro",
            Self::Rw => "rw",
        }
    }
}

impl std::fmt::Display for LocalProcessSandboxAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Network scope for a sandboxed process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalProcessNetworkScope {
    Deny,
    Allowlist,
}

/// A path exposed to the sandbox with a given access mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalProcessSandboxPath {
    pub path: String,
    pub access: LocalProcessSandboxAccess,
}

/// A path alias mapping a sandbox-visible path to a workspace-relative target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalProcessSandboxPathAlias {
    pub path: String,
    pub target: String,
}

/// Full sandbox options for a local process run.
#[derive(Debug, Clone, Default)]
pub struct LocalProcessSandboxOptions {
    pub workspace_dir: String,
    pub filesystem_scope: Option<String>,
    pub managed_paths: Vec<LocalProcessSandboxPath>,
    pub extra_paths: Vec<LocalProcessSandboxPath>,
    pub path_aliases: Vec<LocalProcessSandboxPathAlias>,
    pub outbound_restore_paths: Vec<String>,
    pub home_dir: Option<String>,
    pub network_scope: Option<LocalProcessNetworkScope>,
    pub network_allowlist: Vec<String>,
    pub network_trusted_urls: Vec<String>,
    pub command: Option<String>,
}

/// Result of building a local-process sandbox spawn target. Mirrors Node
/// `LocalProcessSandboxSpawnTarget`.
#[derive(Debug, Clone)]
pub struct LocalProcessSandboxSpawnTarget {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Option<std::collections::BTreeMap<String, Option<String>>>,
}

fn normalize_absolute_path(candidate: &str, label: &str) -> Result<String, String> {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} must be an absolute path."));
    }
    if !trimmed.starts_with('/') {
        return Err(format!("{label} must be an absolute path."));
    }
    Ok(trimmed.to_string())
}

fn parse_network_allowlist_entry(entry: &str, index: usize) -> Result<String, String> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return Err(format!("networkAllowlist[{index}] must not be empty."));
    }
    // Parse as URL: must be hostname[:port] or origin URL
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    // Find scheme:// separator
    let after_scheme = with_scheme
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| {
            format!("networkAllowlist[{index}] must be a hostname, hostname:port, or origin URL.")
        })?;
    // Find the authority (up to first /, ?, #)
    let authority_end = after_scheme
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() {
        return Err(format!(
            "networkAllowlist[{index}] must be a hostname, hostname:port, or origin URL."
        ));
    }
    // Reject userinfo
    if authority.contains('@') {
        return Err(format!(
            "networkAllowlist[{index}] must be a hostname, hostname:port, or origin URL."
        ));
    }
    // Parse host[:port]
    let (hostname, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (h, Some(p)),
        _ => (authority, None),
    };
    let hostname = hostname.to_lowercase();
    if hostname.is_empty() || hostname == "*" || hostname.starts_with("*.") {
        return Err(format!(
            "networkAllowlist[{index}] must use an exact hostname; wildcards are not supported."
        ));
    }
    Ok(match port {
        Some(p) => format!("{hostname}:{p}"),
        None => hostname,
    })
}

/// Parse a network allowlist from an unknown config value.
/// Mirrors Node `parseLocalProcessNetworkAllowlist`.
pub fn parse_local_process_network_allowlist(value: &serde_json::Value) -> Vec<String> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .enumerate()
        .map(|(index, entry)| {
            let s = entry
                .as_str()
                .ok_or_else(|| format!("networkAllowlist[{index}] must be a string."))?;
            parse_network_allowlist_entry(s, index)
        })
    .collect::<Result<Vec<_>, _>>()
    .unwrap_or_default()
}

/// Parse a network scope from an unknown config value.
/// Mirrors Node `parseLocalProcessNetworkScope`.
pub fn parse_local_process_network_scope(value: &serde_json::Value) -> Option<LocalProcessNetworkScope> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) if s.is_empty() => None,
        serde_json::Value::String(s) if s == "deny" => Some(LocalProcessNetworkScope::Deny),
        serde_json::Value::String(s) if s == "allowlist" => Some(LocalProcessNetworkScope::Allowlist),
        _ => None,
    }
}

/// Parse a filesystem scope from an unknown config value.
/// Mirrors Node `parseLocalProcessFilesystemScope`.
pub fn parse_local_process_filesystem_scope(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) if s.is_empty() => None,
        serde_json::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Parse extra paths from an unknown config value.
/// Mirrors Node `parseLocalProcessSandboxExtraPaths`.
pub fn parse_local_process_sandbox_extra_paths(value: &serde_json::Value) -> Vec<LocalProcessSandboxPath> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if let Some(s) = entry.as_str() {
                match normalize_absolute_path(s, &format!("filesystemExtraPaths[{index}]")) {
                    Ok(p) => Some(LocalProcessSandboxPath {
                        path: p,
                        access: LocalProcessSandboxAccess::Ro,
                    }),
                    Err(_) => None,
                }
            } else if entry.is_object() && !entry.is_array() {
                let obj = entry.as_object().unwrap();
                let access = match obj.get("access").and_then(|v| v.as_str()) {
                    Some("rw") => LocalProcessSandboxAccess::Rw,
                    Some("ro") | None => LocalProcessSandboxAccess::Ro,
                    _ => return None,
                };
                let path = obj.get("path").and_then(|v| v.as_str())?;
                let p = normalize_absolute_path(path, &format!("filesystemExtraPaths[{index}].path")).ok()?;
                Some(LocalProcessSandboxPath { path: p, access })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn access_enum_round_trips() {
        assert_eq!(LocalProcessSandboxAccess::Ro.as_str(), "ro");
        assert_eq!(LocalProcessSandboxAccess::Rw.as_str(), "rw");
        assert_eq!(LocalProcessSandboxAccess::Ro.to_string(), "ro");
    }

    #[test]
    fn parse_network_allowlist_empty_array() {
        let v = json!([]);
        assert!(parse_local_process_network_allowlist(&v).is_empty());
    }

    #[test]
    fn parse_network_allowlist_non_array() {
        let v = json!({});
        assert!(parse_local_process_network_allowlist(&v).is_empty());
        let v = json!(null);
        assert!(parse_local_process_network_allowlist(&v).is_empty());
    }

    #[test]
    fn parse_network_allowlist_hostname_only() {
        let v = json!(["api.example.com"]);
        assert_eq!(
            parse_local_process_network_allowlist(&v),
            vec!["api.example.com"]
        );
    }

    #[test]
    fn parse_network_allowlist_hostname_with_port() {
        let v = json!(["api.example.com:8080"]);
        assert_eq!(
            parse_local_process_network_allowlist(&v),
            vec!["api.example.com:8080"]
        );
    }

    #[test]
    fn parse_network_allowlist_origin_url() {
        let v = json!(["https://api.example.com:443"]);
        assert_eq!(
            parse_local_process_network_allowlist(&v),
            vec!["api.example.com:443"]
        );
    }

    #[test]
    fn parse_network_allowlist_normalizes_hostname_case() {
        let v = json!(["API.Example.COM"]);
        assert_eq!(
            parse_local_process_network_allowlist(&v),
            vec!["api.example.com"]
        );
    }

    #[test]
    fn parse_network_allowlist_rejects_wildcard() {
        let v = json!(["*.example.com"]);
        assert!(parse_local_process_network_allowlist(&v).is_empty());
    }

    #[test]
    fn parse_network_allowlist_rejects_empty_entry() {
        let v = json!([""]);
        assert!(parse_local_process_network_allowlist(&v).is_empty());
    }

    #[test]
    fn parse_network_allowlist_rejects_non_string_entry() {
        let v = json!([123]);
        assert!(parse_local_process_network_allowlist(&v).is_empty());
    }

    #[test]
    fn parse_network_scope_deny() {
        assert_eq!(
            parse_local_process_network_scope(&json!("deny")),
            Some(LocalProcessNetworkScope::Deny)
        );
    }

    #[test]
    fn parse_network_scope_allowlist() {
        assert_eq!(
            parse_local_process_network_scope(&json!("allowlist")),
            Some(LocalProcessNetworkScope::Allowlist)
        );
    }

    #[test]
    fn parse_network_scope_null_returns_none() {
        assert_eq!(
            parse_local_process_network_scope(&json!(null)),
            None
        );
    }

    #[test]
    fn parse_network_scope_empty_string_returns_none() {
        assert_eq!(
            parse_local_process_network_scope(&json!("")),
            None
        );
    }

    #[test]
    fn parse_network_scope_invalid_returns_none() {
        assert_eq!(
            parse_local_process_network_scope(&json!("invalid")),
            None
        );
    }

    #[test]
    fn parse_filesystem_scope_workspace() {
        assert_eq!(
            parse_local_process_filesystem_scope(&json!("workspace")),
            Some("workspace".to_string())
        );
    }

    #[test]
    fn parse_filesystem_scope_null_returns_none() {
        assert_eq!(parse_local_process_filesystem_scope(&json!(null)), None);
    }

    #[test]
    fn parse_filesystem_scope_empty_returns_none() {
        assert_eq!(parse_local_process_filesystem_scope(&json!("")), None);
    }

    #[test]
    fn parse_extra_paths_string_array() {
        let v = json!(["/etc/ssl", "/usr/local/share"]);
        let result = parse_local_process_sandbox_extra_paths(&v);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, "/etc/ssl");
        assert_eq!(result[0].access, LocalProcessSandboxAccess::Ro);
    }

    #[test]
    fn parse_extra_paths_object_array_with_access() {
        let v = json!([
            { "path": "/data", "access": "rw" },
            { "path": "/cache" }
        ]);
        let result = parse_local_process_sandbox_extra_paths(&v);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, "/data");
        assert_eq!(result[0].access, LocalProcessSandboxAccess::Rw);
        assert_eq!(result[1].path, "/cache");
        assert_eq!(result[1].access, LocalProcessSandboxAccess::Ro);
    }

    #[test]
    fn parse_extra_paths_rejects_relative_paths() {
        let v = json!(["relative/path"]);
        let result = parse_local_process_sandbox_extra_paths(&v);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn parse_extra_paths_non_array_returns_empty() {
        let v = json!({});
        let result = parse_local_process_sandbox_extra_paths(&v);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn parse_extra_paths_skips_invalid_objects() {
        let v = json!([
            { "path": "/valid" },
            { "access": "rw" },
            { "path": 123 }
        ]);
        let result = parse_local_process_sandbox_extra_paths(&v);
        assert_eq!(result.len(), 1);
    }
}
