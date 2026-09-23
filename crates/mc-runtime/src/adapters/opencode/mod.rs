//! `opencode` adapter —— 上游 `server/pkg/agent/opencode.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `opencodeBackend.Execute`（`opencode.go` L95 起） |
//! | `BLOCKED` | `opencodeBlockedArgs`（L36） |
//! | [`OpenCodeFamilyDecoder`] | `opencode.go` 的 NDJSON 事件处理 |
//!
//! # 传输
//!
//! `opencode run` **没有** `--prompt`：prompt 只能走 stdin（`run` 的变参位置参数
//! 会与管道输入合并，所以不传位置参数时管道内容就是整轮消息）。上游因此明确把
//! prompt 放在 stdin —— 一是 Windows `CreateProcess` 的 32,767 字符上限
//! （经 `.cmd` shim 时只有 8,191，见上游 #6538），二是进程列表里不该出现 prompt。
//!
//! # 与上游的差异
//!
//! 1. **按 1.x 契约生成 argv**：上游会用探测到的 CLI 版本区分 1.x / 2.x
//!    （`opencodeUsesV2Contract`），2.x 去掉了 `--dir` 与 `--variant`
//!    （传 `--dir` 会让 2.x 直接退出，上游 #8586）。本 crate 的
//!    `build_args` 是 [`LaunchRequest`] 的纯函数，拿不到探测版本，因此只实现
//!    1.x 契约；2.x 的版本感知 argv 记在 M4 欠账里（见 `docs/33`）。
//! 2. **不转发 `--max-turns`**：上游也只是记一条 warning —— `opencode run` 不支持。
//! 3. **不注入系统提示词**：runtime brief 走 workdir 里的 AGENTS.md，由 CLI 自己加载。

use std::path::Path;

use super::cli_core::args::{filter_extra_args, ArgPolicy, ArgValueMode};
use super::cli_core::{
    CliCapabilities, CliCoreConfig, CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use super::opencode_family::OpenCodeFamilyDecoder;
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字。
pub(crate) const LABEL: &str = "opencode";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`opencodeBlockedArgs`）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    // daemon↔CLI 的 JSON 输出格式。
    ("--format", ArgValueMode::WithValue),
    // 任务 workdir 锚点（skills / AGENTS.md 发现）。
    ("--dir", ArgValueMode::WithValue),
    // 由 LaunchRequest::thinking_level 拥有。
    ("--variant", ArgValueMode::WithValue),
    // 非交互权限由 daemon 管。
    ("--dangerously-skip-permissions", ArgValueMode::Standalone),
];

/// 已知取值参数。
const MODES: &[(&str, ArgValueMode)] = &[
    ("--model", ArgValueMode::WithValue),
    ("--session", ArgValueMode::WithValue),
    ("--agent", ArgValueMode::WithValue),
    ("--log-level", ArgValueMode::WithValue),
];

/// `extra_args` 过滤策略（prompt 走 stdin ⇒ 位置参数必须剔除）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 组装 argv（上游 opencode 1.x 契约）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = ["run", "--format", "json", "--dangerously-skip-permissions"]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
    // `--dir` 把项目发现锚在任务 workdir 上，否则会退回 daemon 的 PWD。
    if let Some(cwd) = request.cwd.as_deref() {
        args.push("--dir".to_owned());
        args.push(cwd.display().to_string());
    }
    if let Some(model) = request.model.as_deref().filter(|model| !model.is_empty()) {
        args.push("--model".to_owned());
        args.push(model.to_owned());
    }
    if let Some(level) = request
        .thinking_level
        .as_deref()
        .filter(|level| !level.is_empty())
    {
        args.push("--variant".to_owned());
        args.push(level.to_owned());
    }
    if let Some(session) = request
        .resume_session
        .as_deref()
        .filter(|session| !session.is_empty())
    {
        args.push("--session".to_owned());
        args.push(session.to_owned());
    }
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// opencode adapter。
#[derive(Debug)]
pub struct Opencode {
    config: CliCoreConfig,
}

impl Opencode {
    /// 默认配置：可执行文件 `opencode`（走 `PATH`）。
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

    /// 当前配置。
    pub fn config(&self) -> &CliCoreConfig {
        &self.config
    }
}

impl Default for Opencode {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Opencode {
    fn kind(&self) -> AgentType {
        AgentType::Opencode
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::StdinText,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::JsonLine,
                streaming: true,
                // NDJSON 里没有推理事件（上游 opencode.go 里没有 MessageThinking）：
                // `--variant` 只是"推理等级"旋钮，不等于流里有推理内容。
                thinking: false,
                tool_events: true,
                usage_reporting: true,
                resume: true,
            },
            prompt_write_is_fatal: true,
            build_args,
        }
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }

    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder> {
        Box::new(OpenCodeFamilyDecoder::new(
            LABEL,
            request.model.clone().unwrap_or_else(|| LABEL.to_owned()),
            // opencode 有 fail-closed 判据：step 没闭合 / 一行都没解析出来 ⇒ 失败。
            true,
        ))
    }
}

impl TestableAdapter for Opencode {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "opencode 1.17.7\n".to_owned(),
            expected_version: Some("1.17.7".to_owned()),
            success_stdout: concat!(
                r#"{"type":"step_start","sessionID":"ses_conf_1","part":{}}"#,
                "\n",
                r#"{"type":"text","sessionID":"ses_conf_1","part":{"text":"ok"}}"#,
                "\n",
                r#"{"type":"step_finish","sessionID":"ses_conf_1","part":{"reason":"stop","tokens":{"input":10,"output":5,"cache":{"read":1,"write":0}}}}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(16),
            junk_stdout: concat!(
                "opencode: 无法解析的横幅\n",
                r#"{"type":"future.event","sessionID":"ses_conf_1"}"#,
                "\n",
                r#"{"type":"text","sessionID":"ses_conf_1","part":{"text":"ok"}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "opencode exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_anchors_the_workdir_and_never_carries_the_prompt() {
        let request = LaunchRequest::new("干点活")
            .with_cwd("/work/task")
            .with_model("claude-sonnet-4")
            .with_thinking_level("high")
            .with_resume_session("ses-old");
        assert_eq!(
            build_args(&request),
            vec![
                "run",
                "--format",
                "json",
                "--dangerously-skip-permissions",
                "--dir",
                "/work/task",
                "--model",
                "claude-sonnet-4",
                "--variant",
                "high",
                "--session",
                "ses-old",
            ]
        );
    }

    #[test]
    fn managed_flags_cannot_be_overridden() {
        let request = LaunchRequest::new("p").with_extra_args([
            "--format",
            "text",
            "--dir",
            "/elsewhere",
            "--variant",
            "low",
            "注入的位置参数",
            "--dangerously-skip-permissions",
        ]);
        let args = build_args(&request);
        assert_eq!(args.iter().filter(|arg| *arg == "--format").count(), 1);
        assert_eq!(args.iter().filter(|arg| *arg == "--variant").count(), 0);
        assert_eq!(args.iter().filter(|arg| *arg == "--dir").count(), 0);
        for banned in ["text", "/elsewhere", "low", "注入的位置参数"] {
            assert!(!args.iter().any(|arg| arg == banned), "{banned} 不该出现");
        }
    }

    crate::adapter_conformance!(Opencode);
}
