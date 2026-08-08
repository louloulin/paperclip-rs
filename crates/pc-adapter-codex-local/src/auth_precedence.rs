//! Codex sandbox auth precedence 解析（对齐 Node auth-precedence.ts）。
//!
//! 在 sandbox 环境下当 configured API key 或 host `auth.json` 同时存在时，
//! sandbox 自身的 `auth.json` 会被"shadowed"。该模块负责推导胜出方
//! 与是否发出警告。

use serde::{Deserialize, Serialize};

/// 当 sandbox 中存在 auth.json 但 host/configured 凭据更优先时打印的警告。
pub const CODEX_SANDBOX_AUTH_PRECEDENCE_WARNING: &str =
    "snapshot login present but configured or host credentials take precedence";

/// 警告日志行（带 `[paperclip]` 前缀与换行）。
pub const CODEX_SANDBOX_AUTH_PRECEDENCE_WARNING_LOG_LINE: &str =
    "[paperclip] Warning: snapshot login present but configured or host credentials take precedence.\n";

/// 检测 host 上是否存在 auth.json 的 shell 命令片段。
pub const CODEX_SANDBOX_AUTH_EXISTS_COMMAND: &str = "test -f \"$HOME/.codex/auth.json\"";

/// Auth precedence 胜出方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexAuthPrecedenceWinner {
    ConfiguredApiKey,
    HostAuthJson,
    SandboxAuthJson,
    None,
}

/// 输入：三种凭据来源是否各自存在。
#[derive(Debug, Clone, Copy, Default)]
pub struct CodexAuthPrecedenceInput {
    pub configured_api_key: bool,
    pub host_auth_json: bool,
    pub sandbox_auth_json: bool,
}

/// 解析结果：胜出方 + 是否被 shadow + 是否警告。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexAuthPrecedenceResolution {
    pub winner: CodexAuthPrecedenceWinner,
    pub sandbox_login_shadowed: bool,
    pub should_warn: bool,
}

/// 推导 auth precedence 顺序：
/// 1. `configured_api_key`（最高）
/// 2. `host_auth_json`
/// 3. `sandbox_auth_json`
/// 4. `none`（皆无）
///
/// 当 sandbox 中存在 auth.json 但被更高优先级覆盖时，`should_warn=true`。
#[must_use]
pub fn resolve_codex_auth_precedence(
    input: CodexAuthPrecedenceInput,
) -> CodexAuthPrecedenceResolution {
    let winner = if input.configured_api_key {
        CodexAuthPrecedenceWinner::ConfiguredApiKey
    } else if input.host_auth_json {
        CodexAuthPrecedenceWinner::HostAuthJson
    } else if input.sandbox_auth_json {
        CodexAuthPrecedenceWinner::SandboxAuthJson
    } else {
        CodexAuthPrecedenceWinner::None
    };
    let sandbox_login_shadowed = input.sandbox_auth_json
        && matches!(
            winner,
            CodexAuthPrecedenceWinner::ConfiguredApiKey | CodexAuthPrecedenceWinner::HostAuthJson
        );
    CodexAuthPrecedenceResolution {
        winner,
        sandbox_login_shadowed,
        should_warn: sandbox_login_shadowed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_api_key_wins_over_host_and_sandbox() {
        let r = resolve_codex_auth_precedence(CodexAuthPrecedenceInput {
            configured_api_key: true,
            host_auth_json: true,
            sandbox_auth_json: true,
        });
        assert_eq!(r.winner, CodexAuthPrecedenceWinner::ConfiguredApiKey);
        assert!(r.sandbox_login_shadowed);
        assert!(r.should_warn);
    }

    #[test]
    fn host_auth_json_wins_when_no_configured_key() {
        let r = resolve_codex_auth_precedence(CodexAuthPrecedenceInput {
            configured_api_key: false,
            host_auth_json: true,
            sandbox_auth_json: true,
        });
        assert_eq!(r.winner, CodexAuthPrecedenceWinner::HostAuthJson);
        assert!(r.sandbox_login_shadowed);
        assert!(r.should_warn);
    }

    #[test]
    fn sandbox_auth_json_wins_when_only_it_present() {
        let r = resolve_codex_auth_precedence(CodexAuthPrecedenceInput {
            configured_api_key: false,
            host_auth_json: false,
            sandbox_auth_json: true,
        });
        assert_eq!(r.winner, CodexAuthPrecedenceWinner::SandboxAuthJson);
        assert!(!r.sandbox_login_shadowed);
        assert!(!r.should_warn);
    }

    #[test]
    fn none_when_no_auth_anywhere() {
        let r = resolve_codex_auth_precedence(CodexAuthPrecedenceInput::default());
        assert_eq!(r.winner, CodexAuthPrecedenceWinner::None);
        assert!(!r.sandbox_login_shadowed);
        assert!(!r.should_warn);
    }

    #[test]
    fn configured_api_key_alone_no_warning() {
        let r = resolve_codex_auth_precedence(CodexAuthPrecedenceInput {
            configured_api_key: true,
            host_auth_json: false,
            sandbox_auth_json: false,
        });
        assert_eq!(r.winner, CodexAuthPrecedenceWinner::ConfiguredApiKey);
        assert!(!r.sandbox_login_shadowed);
    }

    #[test]
    fn host_auth_json_alone_no_warning() {
        let r = resolve_codex_auth_precedence(CodexAuthPrecedenceInput {
            configured_api_key: false,
            host_auth_json: true,
            sandbox_auth_json: false,
        });
        assert_eq!(r.winner, CodexAuthPrecedenceWinner::HostAuthJson);
        assert!(!r.sandbox_login_shadowed);
    }

    #[test]
    fn warning_constants_have_expected_text() {
        assert!(CODEX_SANDBOX_AUTH_PRECEDENCE_WARNING.contains("snapshot login"));
        assert!(CODEX_SANDBOX_AUTH_PRECEDENCE_WARNING_LOG_LINE.contains("[paperclip]"));
        assert!(CODEX_SANDBOX_AUTH_PRECEDENCE_WARNING_LOG_LINE.ends_with('\n'));
        assert!(CODEX_SANDBOX_AUTH_EXISTS_COMMAND.contains("auth.json"));
    }
}
