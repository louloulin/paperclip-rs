//! `dsh`（Multica DSH bundle）adapter —— 上游 `server/pkg/agent/dsh.go`。
//!
//! # 与上游的对应关系
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`LAUNCH_ARGS`] | `dshLaunchArgs`（`dsh.go` L119） |
//! | [`DshDecoder`]（[`super::dsh::decode`]） | `dshBackend.Execute` 的 stdout 循环 + `handleDshFrame`（L376） |
//! | 运行骨架 | [`super::cli_core`]（spawn / stdout 循环 / 终态归因） |
//!
//! # 为什么是 `JsonLine` + `JsonRpc` 传输
//!
//! DSH 自己就是 agent 运行时（自有 agent 循环 / 会话存储 / 模型目录 / 工具 / MCP），
//! 我们只跟它说一套**版本化的逐行 JSON** 协议：启动 argv 固定，`execute` 帧（带 prompt）
//! 在 spawn 之后立刻写进 stdin，`cancel` 帧在取消时补发。因此 stdin 全程常开
//! （[`PromptTransport::JsonRpc`]），而不是"写完 prompt 就关"的 `StdinText`。
//!
//! # 与上游的差异
//!
//! 1. **`extra_args` 不下发**：上游 `dshLaunchArgs()` 是写死的 `--profile multica --stdio`，
//!    `opts.CustomArgs` 根本不进 argv。本 crate 忠实照做 —— 传进来的 `extra_args`
//!    会被整段丢掉（见 `cannot_inject_anything_into_argv` 用例），这是**有意**的：
//!    dsh 的启动参数改动会直接改掉协议形态（例如换 profile）。
//! 2. **不做模型目录发现**：上游 `discoverDshModels` 走 `--list-models` 把 DSH 的模型
//!    目录拉进来；本 crate 的模型目录是 [`crate::catalog`] 的静态表，属 M3-2 范畴。
//! 3. **`cmd.WaitDelay` 不做等价实现**：上游给 dsh 10s 的 WaitDelay（等管道排空再
//!    回收），本 crate 的 `cli_core::run` 用固定宽限收尾（见 `docs/33` §5）。
//! 4. 协议面的其余差异（终态正文口径、非 `completed` 归因、`mcp_servers` 恒空等）
//!    逐条写在 [`super::dsh::decode`] 的模块文档里。

mod decode;

pub(crate) use decode::DshDecoder;

use std::path::Path;

use super::cli_core::{
    CliCapabilities, CliCoreConfig, CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, LivePlan, TestableAdapter};

/// 日志 / 错误串里的名字。
pub(crate) const LABEL: &str = "dsh";

/// 启动参数（`dshLaunchArgs`：profile 与 stdio 传输都是守护进程定的，不允许被改写）。
pub(crate) const LAUNCH_ARGS: &[&str] = &["--profile", "multica", "--stdio"];

/// 组装 argv。
///
/// **刻意忽略 `extra_args`**：上游不把 `opts.CustomArgs` 交给 dsh（差异 1），
/// 而"过滤掉危险参数"这种半吊子做法仍会放过 profile 之外的形态改动，
/// 不如整段不下发、由协议帧承载全部意图。
pub fn build_args(_request: &LaunchRequest) -> Vec<String> {
    LAUNCH_ARGS.iter().map(|arg| (*arg).to_owned()).collect()
}

/// dsh adapter。
#[derive(Debug)]
pub struct Dsh {
    config: CliCoreConfig,
}

impl Dsh {
    /// 默认配置：可执行文件 `dsh`（走 `PATH`）。
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

impl Default for Dsh {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Dsh {
    fn kind(&self) -> AgentType {
        AgentType::Dsh
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::JsonRpc,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::JsonLine,
                streaming: true,
                thinking: true,
                tool_events: true,
                usage_reporting: true,
                resume: true,
            },
            // `execute` 帧写不下去 = dsh 收不到任何指令，run 没有任何意义 ⇒ 致命。
            prompt_write_is_fatal: true,
            build_args,
        }
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }

    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder> {
        Box::new(DshDecoder::new(request))
    }
}

impl TestableAdapter for Dsh {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    /// `JsonLine` 族默认是 `Eof`（写完即关 stdin），但 dsh 的 stdin 常开、
    /// 客户端只发 `execute` 一帧 ⇒ 声明成 `GateOnce`。
    fn conformance_live_plan() -> Option<LivePlan> {
        Some(LivePlan::GateOnce("\"type\":\"execute\""))
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "dsh 1.2.0\n".to_owned(),
            expected_version: Some("1.2.0".to_owned()),
            success_stdout: concat!(
                r#"{"v":1,"type":"ready","runtime":"dsh"}"#,
                "\n",
                r#"{"v":1,"type":"session","session_id":"dsh-ses-1"}"#,
                "\n",
                r#"{"v":1,"type":"thinking","content":"先看一眼"}"#,
                "\n",
                r#"{"v":1,"type":"tool_call","call_id":"call-1","name":"read","arguments":"{\"path\":\"README.md\"}"}"#,
                "\n",
                r##"{"v":1,"type":"tool_result","call_id":"call-1","name":"read","output":"# ok"}"##,
                "\n",
                r#"{"v":1,"type":"usage","provider":"anthropic","model":"claude","input_tokens":10,"output_tokens":5}"#,
                "\n",
                r#"{"v":1,"type":"text","content":"ok"}"#,
                "\n",
                r#"{"v":1,"type":"result","status":"completed","output":"ok","session_id":"dsh-ses-1"}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: concat!(
                "dsh: 无法解析的横幅\n",
                r#"{"v":1,"type":"future.chunk"}"#,
                "\n",
                r#"{"v":1,"type":"text","content":"ok"}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "dsh exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_the_fixed_launch_prefix() {
        assert_eq!(
            build_args(&LaunchRequest::new("干点活")),
            vec!["--profile", "multica", "--stdio"]
        );
    }

    #[test]
    fn cannot_inject_anything_into_argv() {
        // 差异 1：dsh 的 argv 完全由我们写死，用户参数（连位置参数一起）全部丢掉。
        let request = LaunchRequest::new("干点活").with_extra_args([
            "--profile",
            "other",
            "--stdio=0",
            "注入的位置参数",
        ]);
        assert_eq!(
            build_args(&request),
            vec!["--profile", "multica", "--stdio"]
        );
    }

    #[test]
    fn the_model_and_thinking_level_are_protocol_fields_not_argv_flags() {
        let request = LaunchRequest::new("p")
            .with_model("anthropic/claude")
            .with_thinking_level("high")
            .with_resume_session("ses-1");
        let args = build_args(&request);
        assert!(!args.iter().any(|arg| arg.starts_with("--model")));
        assert!(!args.iter().any(|arg| arg.starts_with("--resume")));

        // 但它们确实进了 `execute` 帧。
        let frame = decode::execute_frame(&request);
        assert!(frame.contains(r#""resume_session_id":"ses-1""#));
        assert!(frame.contains(r#""reasoning_effort":"high""#));
        assert!(frame.contains(r#""provider":"anthropic""#));
    }

    crate::adapter_conformance!(Dsh);
}
