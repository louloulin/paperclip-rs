//! Runtime 领域类型：26 种 CLI + daemon pair 模型。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Local,
    Cloud,
}

impl Default for RuntimeMode {
    fn default() -> Self {
        Self::Local
    }
}

/// Runtime 状态（与 multica `runtime.Status` 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeStatus {
    Online,
    Offline,
    OnlineLastSeen,
}

impl Default for RuntimeStatus {
    fn default() -> Self {
        Self::Offline
    }
}

/// 26 种 runtime profile。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeProfile {
    ClaudeCode,
    Codex,
    Cursor,
    Copilot,
    Kimi,
    OpenCode,
    OpenClaw,
    Hermes,
    Pi,
    Antigravity,
    CodeBuddy,
    DevEco,
    Grok,
    KiroCli,
    QoderCli,
    QoderCliCn,
    Qwen,
    QwenPaw,
    Reasonix,
    TraeCli,
    Dsh,
    Omp,
    Mcode,
    Dim,
    CodeArts,
    ZeroClaw,
}

impl RuntimeProfile {
    pub fn all() -> [RuntimeProfile; 26] {
        [
            Self::ClaudeCode,
            Self::Codex,
            Self::Cursor,
            Self::Copilot,
            Self::Kimi,
            Self::OpenCode,
            Self::OpenClaw,
            Self::Hermes,
            Self::Pi,
            Self::Antigravity,
            Self::CodeBuddy,
            Self::DevEco,
            Self::Grok,
            Self::KiroCli,
            Self::QoderCli,
            Self::QoderCliCn,
            Self::Qwen,
            Self::QwenPaw,
            Self::Reasonix,
            Self::TraeCli,
            Self::Dsh,
            Self::Omp,
            Self::Mcode,
            Self::Dim,
            Self::CodeArts,
            Self::ZeroClaw,
        ]
    }

    /// 对应 CLI 命令名（multica `scripts/agent-cli-command-names.txt`）。
    pub fn cli_command(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor-agent",
            Self::Copilot => "copilot",
            Self::Kimi => "kimi",
            Self::OpenCode => "opencode",
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
            Self::Pi => "pi",
            Self::Antigravity => "agy",
            Self::CodeBuddy => "codebuddy",
            Self::DevEco => "deveco",
            Self::Grok => "grok",
            Self::KiroCli => "kiro-cli",
            Self::QoderCli => "qodercli",
            Self::QoderCliCn => "qoderclicn",
            Self::Qwen => "qwen",
            Self::QwenPaw => "qwenpaw",
            Self::Reasonix => "reasonix",
            Self::TraeCli => "traecli",
            Self::Dsh => "dsh",
            Self::Omp => "omp",
            Self::Mcode => "mcode",
            Self::Dim => "dim",
            Self::CodeArts => "codearts",
            Self::ZeroClaw => "zeroclaw",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Copilot => "copilot",
            Self::Kimi => "kimi",
            Self::OpenCode => "opencode",
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
            Self::Pi => "pi",
            Self::Antigravity => "antigravity",
            Self::CodeBuddy => "codebuddy",
            Self::DevEco => "deveco",
            Self::Grok => "grok",
            Self::KiroCli => "kiro-cli",
            Self::QoderCli => "qodercli",
            Self::QoderCliCn => "qoderclicn",
            Self::Qwen => "qwen",
            Self::QwenPaw => "qwenpaw",
            Self::Reasonix => "reasonix",
            Self::TraeCli => "traecli",
            Self::Dsh => "dsh",
            Self::Omp => "omp",
            Self::Mcode => "mcode",
            Self::Dim => "dim",
            Self::CodeArts => "codearts",
            Self::ZeroClaw => "zeroclaw",
        }
    }
}

/// Runtime 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRuntime {
    pub id: Id,
    pub workspace_id: Id,
    pub name: String,
    pub mode: RuntimeMode,
    pub profile: RuntimeProfile,
    pub status: RuntimeStatus,
    pub timezone: Option<String>,
    pub last_seen_at: Option<Timestamp>,
    pub online_since: Option<Timestamp>,
    pub daemon_uuid: Option<String>,
    pub owner_user_id: Option<Id>,
    pub visibility: RuntimeMode, // reuse for: 公共 / 私有
    pub custom_name: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_26_profiles_listed() {
        assert_eq!(RuntimeProfile::all().len(), 26);
    }

    #[test]
    fn cli_command_is_unique() {
        let cmds: Vec<&str> = RuntimeProfile::all().iter().map(|p| p.cli_command()).collect();
        let mut sorted = cmds.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), cmds.len(), "CLI commands must be unique");
    }
}