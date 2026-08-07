//! `pc-acpx` agent command resolver — pure async wrapper around
//! `find_ancestor_bin` for the built-in agent commands.
//!
//! Mirrors Node `resolveBuiltInAgentCommand` from `acpx-engine/execute.ts`.
//! The resolver walks a package's `node_modules/.bin` for the local lane and
//! returns the bare binary name on the remote lane (where the host's local
//! bin layout is irrelevant).

use std::path::Path;

use crate::bin::{find_ancestor_bin, Platform};

// ============================================================================
// Public types
// ============================================================================

/// Built-in agent command resolved by `resolve_built_in_agent_command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltInAgentCommand {
    /// Absolute path to the resolved binary (or the bare name on the remote
    /// lane where there is no host-relative resolution).
    pub command: String,
    /// Shell-safe form of `command` (quoted for shells). On the local lane
    /// this is `shellQuote(command)`; on the remote lane it is identical to
    /// `command`.
    pub shell_command: String,
}

/// Input for `resolve_built_in_agent_command`.
#[derive(Debug, Clone)]
pub struct ResolveBuiltInAgentCommandInput {
    pub agent: String,
    pub package_root_dir: String,
    pub execution_target_is_remote: bool,
    pub platform: Platform,
}

// ============================================================================
// Main entry
// ============================================================================

/// Resolve the built-in agent command for the given agent. The function
/// returns `None` for agents that have no built-in command (e.g. custom
/// agents). The Gemini agent is always `"gemini --acp"` regardless of lane;
/// for the Claude and Codex agents the lane determines the resolution.
pub async fn resolve_built_in_agent_command(
    input: &ResolveBuiltInAgentCommandInput,
) -> Option<BuiltInAgentCommand> {
    let agent = input.agent.trim();
    if agent == "gemini" {
        return Some(BuiltInAgentCommand {
            command: "gemini --acp".to_string(),
            shell_command: "gemini --acp".to_string(),
        });
    }
    let bin_name = match agent {
        "claude" => "claude-agent-acp",
        "codex" => "codex-acp",
        _ => return None,
    };
    if input.execution_target_is_remote {
        return Some(BuiltInAgentCommand {
            command: bin_name.to_string(),
            shell_command: bin_name.to_string(),
        });
    }
    let package_root = Path::new(&input.package_root_dir);
    let resolved = find_ancestor_bin(package_root, bin_name, input.platform).await;
    let resolved = resolved
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| bin_name.to_string());
    let shell_resolved = shell_quote(&resolved);
    Some(BuiltInAgentCommand {
        command: resolved,
        shell_command: shell_resolved,
    })
}

// ============================================================================
// Shell quoting
// ============================================================================

/// Minimal POSIX shell quoting. Wraps the value in single quotes when it
/// contains whitespace or shell metacharacters; escapes embedded single
/// quotes via `'\''`. Returns the input verbatim when no quoting is needed.
///
/// Mirrors the Node `shellQuote` behavior used by `acpx-engine/execute.ts`.
pub fn shell_quote(value: &str) -> String {
    if !needs_quoting(value) {
        return value.to_string();
    }
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

fn needs_quoting(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    value.chars().any(|c| {
        matches!(
            c,
            ' ' | '\t'
                | '\n'
                | '\r'
                | '"'
                | '\''
                | '\\'
                | '$'
                | '`'
                | '!'
                | '&'
                | '*'
                | '?'
                | '|'
                | '<'
                | '>'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | ';'
                | '#'
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "pc-acpx-agent-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[tokio::test]
    async fn gemini_always_returns_fixed_command() {
        let input = ResolveBuiltInAgentCommandInput {
            agent: "gemini".into(),
            package_root_dir: "/nonexistent".into(),
            execution_target_is_remote: false,
            platform: Platform::Posix,
        };
        let result = resolve_built_in_agent_command(&input).await.unwrap();
        assert_eq!(result.command, "gemini --acp");
        assert_eq!(result.shell_command, "gemini --acp");
    }

    #[tokio::test]
    async fn claude_resolves_an_ancestor_bin_when_local() {
        let root = unique_root("claude_local");
        let bin_dir = root.join("node_modules/.bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let bin_path = bin_dir.join("claude-agent-acp");
        tokio::fs::write(&bin_path, "#!/bin/sh\n").await.unwrap();
        let input = ResolveBuiltInAgentCommandInput {
            agent: "claude".into(),
            package_root_dir: root.to_string_lossy().into_owned(),
            execution_target_is_remote: false,
            platform: Platform::Posix,
        };
        let result = resolve_built_in_agent_command(&input).await.unwrap();
        assert_eq!(result.command, bin_path.to_string_lossy());
        // The tempdir path does not contain shell metacharacters, so the
        // shell_command is the verbatim path (no quoting needed). The test in
        // `shell_quote` covers the quoting branches.
        assert_eq!(result.shell_command, bin_path.to_string_lossy());
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn claude_falls_back_to_bare_name_when_missing() {
        let input = ResolveBuiltInAgentCommandInput {
            agent: "claude".into(),
            package_root_dir: "/nonexistent".into(),
            execution_target_is_remote: false,
            platform: Platform::Posix,
        };
        let result = resolve_built_in_agent_command(&input).await.unwrap();
        assert_eq!(result.command, "claude-agent-acp");
        assert_eq!(result.shell_command, "claude-agent-acp");
    }

    #[tokio::test]
    async fn claude_remote_returns_bare_bin_name() {
        let input = ResolveBuiltInAgentCommandInput {
            agent: "claude".into(),
            package_root_dir: "/nonexistent".into(),
            execution_target_is_remote: true,
            platform: Platform::Posix,
        };
        let result = resolve_built_in_agent_command(&input).await.unwrap();
        assert_eq!(result.command, "claude-agent-acp");
        assert_eq!(result.shell_command, "claude-agent-acp");
    }

    #[tokio::test]
    async fn codex_agent_uses_correct_bin_name() {
        let input = ResolveBuiltInAgentCommandInput {
            agent: "codex".into(),
            package_root_dir: "/nonexistent".into(),
            execution_target_is_remote: false,
            platform: Platform::Posix,
        };
        let result = resolve_built_in_agent_command(&input).await.unwrap();
        assert_eq!(result.command, "codex-acp");
    }

    #[tokio::test]
    async fn unknown_agent_returns_none() {
        let input = ResolveBuiltInAgentCommandInput {
            agent: "custom".into(),
            package_root_dir: "/nonexistent".into(),
            execution_target_is_remote: false,
            platform: Platform::Posix,
        };
        assert!(resolve_built_in_agent_command(&input).await.is_none());
    }

    #[test]
    fn shell_quote_passes_through_simple_paths() {
        assert_eq!(shell_quote("/opt/bin/claude"), "/opt/bin/claude");
        assert_eq!(shell_quote("claude-agent-acp"), "claude-agent-acp");
    }

    #[test]
    fn shell_quote_wraps_paths_with_whitespace() {
        assert_eq!(shell_quote("/opt/some dir/cli"), "'/opt/some dir/cli'");
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn shell_quote_quotes_empty_string() {
        assert_eq!(shell_quote(""), "''");
    }
}
