//! `openclaw` adapter —— 上游 `server/pkg/agent/openclaw.go`。
//!
//! # 与上游的对应关系
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `buildOpenclawArgs`（`openclaw.go` L214） |
//! | [`OpenclawDecoder`]（[`super::openclaw::decode`]） | `processOutput` + `parseWholeBufferOpenclawResult`（L400 / L525） |
//! | 运行骨架 | [`super::cli_core`]（spawn / stdout 循环 / 终态归因） |
//!
//! # 传输形态
//!
//! `openclaw agent` **只从 argv 收 prompt**（`--message <prompt>`），stdin 关掉
//! （[`PromptTransport::Argv`]）。会话续跑走 `--session-id <id>`：不续跑时上游现场生成
//! `multica-<UnixNano>`，本 crate 同款（同一 run 内稳定，见 [`generated_session_id`]）。
//!
//! # 与上游的差异
//!
//! 1. **没有 gateway 模式**：上游按 `opts.OpenclawMode != "gateway"` 决定要不要加
//!    `--local`（gateway 模式让 openclaw 去连远端 Gateway）。本 crate 没有这个开关，
//!    恒为本机（embedded）模式 ⇒ 恒有 `--local`，且它仍在封锁表里，
//!    `extra_args` 无法改写。
//! 2. **没有 `SystemPrompt`**：上游把 `opts.SystemPrompt + "\n\n" + prompt` 拼进
//!    `--message`。`LaunchRequest` 没有系统提示字段（属 M4 口径）。
//! 3. **`--timeout` 来自 `LaunchRequest::timeout`**：上游用 `opts.Timeout`
//!    （`> 0` 才加，单位秒）。零值/未给 ⇒ 不加，交给 openclaw 自己的默认值。
//! 4. **没有最低版本闸门**：上游 `checkOpenclawVersion` 在启动前挡掉 `< 2026.5.5`
//!    （那些版本把 `--json` 输出写到 stderr）。本 crate 只有 `probe_version()` 的
//!    semver 解析，"探测值 → 拒绝启动"这条通路不存在（`docs/33` §11）。
//! 5. **没有 idle-grace 提前收尾**：上游 `readOpenclawStdout`（`openclaw_stdout.go`）
//!    在"buffer 已成完整结果 且 stdout 静默 ≥2s"时提前收尾并 kill 掉那个"交完结果却
//!    不退出"的进程。理由与影响写在解码器模块文档（差异 2）。
//! 6. **`extra_args` 过滤多了一条**：`--local` / `--json` / `--session-id` / `--message`
//!    / `--model` / `--system-prompt` 都是守护进程管理的参数（封锁表逐字抄上游），
//!    此外 crate 统一剔除位置参数与 `@文件`（`docs/33` §4 已记录的通用偏差）。

mod decode;

pub(crate) use decode::OpenclawDecoder;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::cli_core::{
    args::non_empty, filter_extra_args, ArgPolicy, ArgValueMode, CliCapabilities, CliCoreConfig,
    CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字。
pub(crate) const LABEL: &str = "openclaw";

/// 守护进程管理、不允许被 `extra_args` 覆盖的参数 —— 上游 `openclawBlockedArgs`。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("--local", ArgValueMode::Standalone),        // 本机执行模式
    ("--json", ArgValueMode::Standalone),         // 守护进程要的 JSON 输出
    ("--session-id", ArgValueMode::WithValue),    // 会话续跑由守护进程管
    ("--message", ArgValueMode::WithValue),       // prompt 由守护进程给
    ("--model", ArgValueMode::WithValue),         // 上游注释：agent 不认 --model
    ("--system-prompt", ArgValueMode::WithValue), // 上游注释：instructions 塞进 --message
];

/// 用户可透传的取值参数（只影响"过滤时要不要把值留下"）。
const MODES: &[(&str, ArgValueMode)] = &[
    ("--agent", ArgValueMode::WithValue),
    ("--channel", ArgValueMode::WithValue),
    ("--timeout", ArgValueMode::WithValue),
];

const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    // prompt 走 argv，用户的位置参数会被 CLI 当成 prompt 的补充 ⇒ 必须剔除。
    strip_prompt_like: true,
};

/// 组装 argv（上游 `buildOpenclawArgs`）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args = vec!["agent".to_owned()];
    // 差异 1：没有 gateway 开关 ⇒ 恒为本机模式。
    args.push("--local".to_owned());
    args.push("--json".to_owned());
    args.push("--session-id".to_owned());
    args.push(session_id(request));
    // 差异 3：`> 0` 才加（单位秒）。
    if let Some(timeout) = request.timeout.filter(|timeout| !timeout.is_zero()) {
        args.push("--timeout".to_owned());
        args.push(timeout.as_secs().to_string());
    }

    let custom = filter_extra_args(&request.extra_args, POLICY);
    // 上游：用户自己给了 `--agent` 就以用户的为准（向后兼容旧配置）。
    if let Some(model) = non_empty(request.model.as_deref()) {
        if !custom.iter().any(|arg| is_flag(arg, "--agent")) {
            args.push("--agent".to_owned());
            args.push(model.to_owned());
        }
    }
    args.extend(custom);

    // 差异 2：`SystemPrompt` 不在 `LaunchRequest` 里，prompt 直接就是 `--message` 的值。
    args.push("--message".to_owned());
    args.push(request.prompt.clone());
    args
}

/// `arg` 是不是 `--flag` / `--flag=value`（上游 `customArgsContains`）。
fn is_flag(arg: &str, flag: &str) -> bool {
    arg == flag || arg.starts_with(&format!("{flag}="))
}

/// 会话 id：续跑用请求里的，否则现场生成 `multica-<UnixNano>`（上游同款）。
fn session_id(request: &LaunchRequest) -> String {
    match non_empty(request.resume_session.as_deref()) {
        Some(resume) => resume.to_owned(),
        None => generated_session_id(),
    }
}

/// 上游 `fmt.Sprintf("multica-%d", time.Now().UnixNano())`。
fn generated_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|delta| delta.as_nanos())
        .unwrap_or_default();
    format!("multica-{nanos}")
}

/// openclaw adapter。
#[derive(Debug)]
pub struct Openclaw {
    config: CliCoreConfig,
}

impl Openclaw {
    /// 默认配置：可执行文件 `openclaw`（走 `PATH`）。
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

impl Default for Openclaw {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Openclaw {
    fn kind(&self) -> AgentType {
        AgentType::Openclaw
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::Argv,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::JsonLine,
                streaming: true,
                // openclaw 不上报推理增量（上游没有对应事件类型）。
                thinking: false,
                tool_events: true,
                usage_reporting: true,
                resume: true,
            },
            // prompt 走 argv：这个开关对 `Argv` 不生效（stdin 本来就是 null）。
            prompt_write_is_fatal: false,
            build_args,
        }
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }

    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder> {
        Box::new(OpenclawDecoder::new(request))
    }
}

impl TestableAdapter for Openclaw {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "openclaw 2026.5.5\n".to_owned(),
            expected_version: Some("2026.5.5".to_owned()),
            // 现役格式：前导日志行 + 一个跨行的结果 blob（快路径）。
            success_stdout: concat!(
                "openclaw: 准备就绪\n",
                "{\n",
                "  \"payloads\": [\n",
                "    { \"text\": \"ok\" }\n",
                "  ],\n",
                "  \"meta\": {\n",
                "    \"durationMs\": 812,\n",
                "    \"agentMeta\": {\n",
                "      \"sessionId\": \"openclaw-ses-1\",\n",
                "      \"model\": \"deepseek-chat\",\n",
                "      \"usage\": { \"input_tokens\": 10, \"output_tokens\": 5 }\n",
                "    }\n",
                "  }\n",
                "}\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: concat!(
                "openclaw: 无法解析的横幅\n",
                r#"{"type":"future.chunk","payload":{"x":1}}"#,
                "\n",
                r#"{"type":"text","text":"ok"}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "openclaw exploded".to_owned(),
            // prompt 走 `--message`（argv），stdin 是 null。
            prompt_via_stdin: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::adapter::RuntimeAdapter;

    fn session_of(args: &[String]) -> String {
        let index = args
            .iter()
            .position(|arg| arg == "--session-id")
            .expect("argv 里必须有 --session-id");
        args[index + 1].clone()
    }

    #[test]
    fn argv_is_agent_with_local_json_and_the_prompt_last() {
        let args = build_args(&LaunchRequest::new("干点活"));
        assert_eq!(&args[..1], ["agent"]);
        assert_eq!(&args[1..4], ["--local", "--json", "--session-id"]);
        assert!(!args.iter().any(|arg| arg == "--agent"));
        assert!(!args.iter().any(|arg| arg == "--timeout"));
        assert_eq!(args[args.len() - 2..], ["--message", "干点活"]);
    }

    #[test]
    fn the_session_id_is_generated_with_the_multica_prefix_or_taken_from_a_resume() {
        let generated = session_of(&build_args(&LaunchRequest::new("p")));
        let nanos = generated.strip_prefix("multica-").expect("前缀");
        assert!(nanos.parse::<u128>().is_ok(), "应是纳秒时间戳：{nanos}");

        let resumed = session_of(&build_args(
            &LaunchRequest::new("p").with_resume_session("oc-existing"),
        ));
        assert_eq!(resumed, "oc-existing");
    }

    #[test]
    fn the_model_becomes_agent_unless_the_user_already_passed_one() {
        let args = build_args(&LaunchRequest::new("p").with_model("my-agent"));
        let index = args
            .iter()
            .position(|arg| arg == "--agent")
            .expect("注入 --agent");
        assert_eq!(args[index + 1], "my-agent");

        // 用户自己给了 `--agent` ⇒ 不注入（且用户的那个在 prompt 之前）。
        let args = build_args(
            &LaunchRequest::new("p")
                .with_model("my-agent")
                .with_extra_args(["--agent", "user-agent"]),
        );
        assert_eq!(
            args.iter().filter(|arg| *arg == "--agent").count(),
            1,
            "{args:?}"
        );
        assert!(args.contains(&"user-agent".to_owned()));

        // `--agent=value` 也算"用户给了"。
        let args = build_args(
            &LaunchRequest::new("p")
                .with_model("my-agent")
                .with_extra_args(["--agent=user-agent"]),
        );
        assert!(!args.contains(&"my-agent".to_owned()), "{args:?}");
    }

    #[test]
    fn the_timeout_flag_is_seconds_and_only_when_non_zero() {
        let args =
            build_args(&LaunchRequest::new("p").with_timeout(std::time::Duration::from_secs(90)));
        let index = args
            .iter()
            .position(|arg| arg == "--timeout")
            .expect("有超时就必须有 --timeout");
        assert_eq!(args[index + 1], "90");

        let args = build_args(&LaunchRequest::new("p").with_timeout(std::time::Duration::ZERO));
        assert!(!args.iter().any(|arg| arg == "--timeout"));
    }

    #[test]
    fn daemon_managed_flags_cannot_be_smuggled_in_and_the_prompt_cannot_be_duplicated() {
        let args = build_args(&LaunchRequest::new("真正的 prompt").with_extra_args([
            "--local",
            "--json",
            "--session-id",
            "hijacked",
            "--message",
            "伪 prompt",
            "--model",
            "fake",
            "--system-prompt",
            "fake system",
            "裸位置参数",
            "@notes.md",
            "--channel",
            "slack",
        ]));
        assert_eq!(
            args.iter().filter(|arg| *arg == "--local").count(),
            1,
            "{args:?}"
        );
        assert_eq!(args.iter().filter(|arg| *arg == "--json").count(), 1);
        assert_eq!(args.iter().filter(|arg| *arg == "--message").count(), 1);
        assert!(!args.contains(&"hijacked".to_owned()));
        assert!(!args.contains(&"伪 prompt".to_owned()));
        assert!(!args.contains(&"fake".to_owned()));
        assert!(!args.contains(&"fake system".to_owned()));
        assert!(!args.contains(&"裸位置参数".to_owned()));
        assert!(!args.contains(&"@notes.md".to_owned()));
        // 用户的取值参数（含值）留下来。
        let index = args
            .iter()
            .position(|arg| arg == "--channel")
            .expect("--channel 该留下");
        assert_eq!(args[index + 1], "slack");
    }

    #[test]
    fn the_reported_surface_matches_the_whitelist_entry() {
        let adapter = Openclaw::new();
        let caps = adapter.capabilities();
        assert_eq!(caps.protocol, ProtocolFamily::JsonLine);
        assert_eq!(caps.launch_header, AgentType::Openclaw.launch_header());
        assert!(!caps.thinking, "openclaw 没有推理增量");
        assert!(caps.tool_events);
    }

    crate::adapter_conformance!(Openclaw);
}
