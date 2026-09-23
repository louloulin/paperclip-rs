//! `codearts`（华为云 `CodeArts` CLI）adapter —— 上游 `server/pkg/agent/codearts.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `codeartsBackend.Execute`（`codearts.go` L80 起） |
//! | `BLOCKED` | `codeartsBlockedArgs`（L38） |
//! | [`OpenCodeFamilyDecoder`] | codearts 的事件处理（与 `OpenCode` 同 schema） |
//!
//! # 与 opencode 的关系
//!
//! codearts 是 `OpenCode` 的**派生** CLI：事件 schema 逐字段相同
//! （`type` / `sessionID` / `part` / `error`），所以复用
//! [`super::opencode_family::OpenCodeFamilyDecoder`]。但命令面是**独立拥有**的
//! （上游专门为此把它从 opencode 后端里拆出来），因此 argv 与屏蔽表在本模块另写：
//!
//! * 没有 `--dir`（上游注释：CodeArts 不暴露该 flag，靠 `cmd.Dir` + `PWD` 锚定）
//! * 非交互开关是 `--auto`（不是 `--dangerously-skip-permissions`，后者是 `OpenCode` 专有）
//! * 不支持 `--variant` / `--max-turns`（上游只记 warning 并忽略 `thinking_level`）
//!
//! # 传输
//!
//! 与 opencode 同款：prompt 走 stdin，绝不进 argv（Windows 32,767 字符上限，
//! `run` 也没有 `--prompt`）。上游为此还专门屏蔽了 `--sandbox` 等 daemon 自有参数。

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
pub(crate) const LABEL: &str = "codearts";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`codeartsBlockedArgs`）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("--format", ArgValueMode::WithValue),
    // daemon 自有的非交互权限模式。
    ("--auto", ArgValueMode::Standalone),
    // daemon 自有的沙箱策略。
    ("--sandbox", ArgValueMode::Standalone),
    // CodeArts 不支持 `--dir`：workdir 由 current_dir + PWD 锚定。
    ("--dir", ArgValueMode::WithValue),
    // CodeArts 不支持推理等级。
    ("--variant", ArgValueMode::WithValue),
    // OpenCode 专有的权限开关。
    ("--dangerously-skip-permissions", ArgValueMode::Standalone),
];

/// 已知取值参数。
const MODES: &[(&str, ArgValueMode)] = &[
    ("--model", ArgValueMode::WithValue),
    ("--session", ArgValueMode::WithValue),
    ("--log-level", ArgValueMode::WithValue),
];

/// `extra_args` 过滤策略（prompt 走 stdin ⇒ 位置参数必须剔除）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 组装 argv（上游 codearts 契约）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = ["run", "--format", "json", "--auto"]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
    if let Some(model) = request.model.as_deref().filter(|model| !model.is_empty()) {
        args.push("--model".to_owned());
        args.push(model.to_owned());
    }
    // thinking_level 被有意忽略（上游同款：CodeArts 没有这个旋钮）。
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

/// codearts adapter。
#[derive(Debug)]
pub struct Codearts {
    config: CliCoreConfig,
}

impl Codearts {
    /// 默认配置：可执行文件 `codearts`（走 `PATH`）。
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

impl Default for Codearts {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Codearts {
    fn kind(&self) -> AgentType {
        AgentType::Codearts
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::StdinText,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::JsonLine,
                streaming: true,
                // 与 opencode 同 schema：流里没有推理事件。
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
            // 派生自 opencode ⇒ 同样 fail-closed。
            true,
        ))
    }
}

impl TestableAdapter for Codearts {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "codearts 1.4.0\n".to_owned(),
            expected_version: Some("1.4.0".to_owned()),
            success_stdout: concat!(
                r#"{"type":"step_start","sessionID":"codearts-ses-1","part":{}}"#,
                "\n",
                r#"{"type":"text","sessionID":"codearts-ses-1","part":{"text":"ok"}}"#,
                "\n",
                r#"{"type":"step_finish","sessionID":"codearts-ses-1","part":{"reason":"stop","tokens":{"input":10,"output":5,"cache":{"read":1,"write":0}}}}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(16),
            junk_stdout: concat!(
                "codearts: 无法解析的横幅\n",
                r#"{"type":"future.event","sessionID":"codearts-ses-1"}"#,
                "\n",
                r#"{"type":"text","sessionID":"codearts-ses-1","part":{"text":"ok"}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "codearts exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_matches_upstream_codearts_surface() {
        let request = LaunchRequest::new("干点活")
            .with_cwd("/work/task")
            .with_model("codearts-model")
            .with_thinking_level("high")
            .with_resume_session("ses-old");
        assert_eq!(
            build_args(&request),
            vec![
                "run",
                "--format",
                "json",
                "--auto",
                "--model",
                "codearts-model",
                "--session",
                "ses-old",
            ],
            "codearts 没有 --dir / --variant，thinking_level 被忽略"
        );
    }

    #[test]
    fn managed_flags_cannot_be_overridden() {
        let request = LaunchRequest::new("p").with_extra_args([
            "--auto",
            "--sandbox",
            "--dir",
            "/elsewhere",
            "--variant",
            "low",
            "--dangerously-skip-permissions",
            "--format",
            "text",
        ]);
        let args = build_args(&request);
        assert_eq!(args.iter().filter(|arg| *arg == "--auto").count(), 1);
        for banned in [
            "--sandbox",
            "--dir",
            "--variant",
            "--dangerously-skip-permissions",
            "/elsewhere",
            "low",
            "text",
        ] {
            assert!(!args.iter().any(|arg| arg == banned), "{banned} 不该出现");
        }
    }

    crate::adapter_conformance!(Codearts);
}
