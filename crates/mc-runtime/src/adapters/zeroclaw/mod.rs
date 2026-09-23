//! `zeroclaw`（`ZeroClaw` CLI）adapter —— 上游 `server/pkg/agent/zeroclaw.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `zeroclaw.go` L250（`["acp"] ++ filtered custom_args`） |
//! | `BLOCKED` | `zeroclawBlockedArgs`（L29） |
//! | [`FLAVOR`] | `zeroclawBackend.Execute` 的会话建/恢复两段（L403 起） |
//! | [`super::acp_core::AcpDecoder`] | 复用的 `hermesClient` |
//!
//! # 命令面
//!
//! `zeroclaw acp`。两处与别家不同：
//!
//! 1. **`session/resume` 只发 `{sessionId}`**（[`AcpResumeParams::SessionOnly`]）：
//!    `ZeroClaw` 的三个会话处理器都不读 `params.mcpServers`，`session/new` 里它只是
//!    被显式传成空数组；resume 索性一个字都不多带。
//! 2. **没有 `session/set_model`**：`ZeroClaw` 的模型属于 agent profile
//!    （`agents.<alias>.model_provider`），不能按会话选 ⇒
//!    [`AcpModelSelection::Unsupported`]。`initialize._meta.zeroclaw.defaultModel`
//!    是**进程级**的（第一个配好 provider 的模型），与 `session/new` 之后真正生效的
//!    那个 alias 无关，所以也不拿它做用量归属。
//!
//! `--agent` / `--agent-alias` 在封锁表里（`zeroclaw acp` 根本没有这个 flag，clap
//! 会直接 `error: unexpected argument`）；上游把它们从 `custom_args` 里**取出来**
//! 转成 `session/new` 的 `agentAlias` 参数。本 crate 的 [`AcpFlavor`] 里没有
//! "会话参数"这一档（只有 `session_meta_key` 一个固定键），所以本片只挡住不让它
//! 进 argv，**不搬运** alias —— 依赖 `[acp].default_agent` 或"恰好一个 agent"的
//! 自动选择；否则对端会以 `-32602`（缺 `agentAlias`）失败。缺口记在 `docs/33` §11。
//!
//! # 有意不搬的部分
//!
//! 上游还要求 `ZeroClaw` ≥ 0.8.0，并用 `initialize.sessionCapabilities.resume` 做闸门
//! （没宣告就先失败、把会话标成不可恢复）；本 crate 不做能力闸门，直接发
//! `session/resume`（同 mcode 的口径）。`zeroclawReaderDrainGrace` /
//! `zeroclawNotificationQuietTime` 属于 M3-3 看门狗。
//!
//! `choice-N` 选项的旧版问答桥（上游 L101 起：只认 `choice-` 前缀，其它选项一律
//! 不作为答案）同样没搬：本 crate 的权限选择走 [`super::acp_core`] 的
//! session-scoped 规则，`choice-N` 那种"用户题"形态不在本片范围内。

use std::path::Path;

use super::acp_core::{
    AcpAuth, AcpFlavor, AcpModelSelection, AcpPromptFields, AcpProvider, AcpResume,
    AcpResumeParams, AcpToolAliases,
};
use super::cli_core::args::{filter_extra_args, ArgPolicy, ArgValueMode};
use super::cli_core::CliCoreConfig;
use crate::adapter::LaunchRequest;
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字（也是默认可执行文件名）。
pub(crate) const LABEL: &str = "zeroclaw";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`zeroclawBlockedArgs`）。
///
/// `login` / `auth` / `--login` / `--auth` / `--help` / `-h` 都是"不会启动 ACP
/// server"的模式；`--agent` / `--agent-alias` 是 clap 不认的 flag（见模块头，
/// 上游把它们搬到 `session/new` 参数里，本片只挡）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("acp", ArgValueMode::Standalone),
    ("--help", ArgValueMode::Standalone),
    ("-h", ArgValueMode::Standalone),
    ("login", ArgValueMode::Standalone),
    ("auth", ArgValueMode::Standalone),
    ("--login", ArgValueMode::Standalone),
    ("--auth", ArgValueMode::Standalone),
    ("--agent", ArgValueMode::WithValue),
    ("--agent-alias", ArgValueMode::WithValue),
];

/// 已知取值参数（`acp` 子命令没有别的 daemon 认识的取值 flag）。
const MODES: &[(&str, ArgValueMode)] = &[];

/// `extra_args` 过滤策略（prompt 走 ACP 的 `session/prompt`，不进 argv）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 本 provider 的 ACP 差异表。
pub static FLAVOR: AcpFlavor = AcpFlavor {
    kind: AgentType::Zeroclaw,
    label: LABEL,
    // `session/resume`（**不是** `session/load`：load 会把旧 transcript 重播成
    // 本轮的输出）。
    resume: AcpResume::Resume,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    thinking_config: None,
    tool_aliases: AcpToolAliases::Kimi,
    model_selection: AcpModelSelection::Unsupported,
    session_meta_key: None,
    session_configs: &[],
    // 只发 `{sessionId}`（上游字面如此）。
    resume_params: AcpResumeParams::SessionOnly,
};

/// 组装 argv（`["acp"] ++ filtered`）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["acp".to_owned()];
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// zeroclaw adapter。
#[derive(Debug)]
pub struct Zeroclaw {
    config: CliCoreConfig,
}

impl Zeroclaw {
    /// 默认配置：可执行文件 `zeroclaw`（走 `PATH`）。
    pub fn new() -> Self {
        Self {
            config: CliCoreConfig::new(LABEL),
        }
    }

    /// 指定可执行文件。
    pub fn with_executable(executable: impl Into<std::path::PathBuf>) -> Self {
        Self {
            config: CliCoreConfig::new(executable),
        }
    }
}

impl Default for Zeroclaw {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for Zeroclaw {
    fn flavor() -> &'static AcpFlavor {
        &FLAVOR
    }

    fn build_args(request: &LaunchRequest) -> Vec<String> {
        build_args(request)
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }
}

impl TestableAdapter for Zeroclaw {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "zeroclaw 0.8.0\n".to_owned(),
            expected_version: Some("0.8.0".to_owned()),
            success_stdout: super::acp_core::conformance_success_stdout(
                "zeroclaw-ses-1",
                "ok",
                false,
            ),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "zeroclaw exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_the_acp_subcommand_plus_custom_args() {
        let request = LaunchRequest::new("干点活")
            .with_model("claude-sonnet-4")
            .with_extra_args(["--verbose"]);
        assert_eq!(
            build_args(&request),
            vec!["acp", "--verbose"],
            "模型 / cwd 都不进 argv"
        );
    }

    #[test]
    fn agent_alias_and_login_modes_never_reach_argv() {
        let request = LaunchRequest::new("p").with_extra_args([
            "--agent",
            "coder",
            "--agent-alias=qa",
            "auth",
            "--login",
        ]);
        assert_eq!(build_args(&request), vec!["acp"]);
    }

    #[test]
    fn the_alias_flag_is_blocked_because_clap_rejects_it() {
        // 封锁不是风格问题：`zeroclaw acp --agent x` 会被 clap 直接拒绝。
        assert!(BLOCKED.contains(&("--agent", ArgValueMode::WithValue)));
        assert!(BLOCKED.contains(&("--agent-alias", ArgValueMode::WithValue)));
    }

    crate::adapter_conformance!(Zeroclaw);
}
