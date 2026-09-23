//! `qwenpaw`（`QwenPaw` v2.x CLI）adapter —— 上游 `server/pkg/agent/qwenpaw.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `qwenpaw.go` L77 起（`["acp"] ++ ExtraArgs ++ CustomArgs`） |
//! | `BLOCKED` | `qwenpawBlockedArgs`（L18） |
//! | [`FLAVOR`] | `qwenpawBackend.Execute` 的会话建/恢复两段（L212 起） |
//! | [`super::acp_core::AcpDecoder`] | 复用的 `hermesClient` |
//!
//! # 命令面
//!
//! `qwenpaw acp`。`QwenPaw` 的默认会话是**非 Coding Mode**，要靠
//! `session/new` / `session/load` 的 `_meta["qwenpaw.coding_project_dir"] = cwd`
//! 才切进 Coding Mode（上游注释把这条列为 v2.0.1 契约的第一项）。因此差异表里
//! [`AcpFlavor::session_meta_key`] 只有这一家不是 `None`，且上游只在 `cwd != "."`
//! 时才带 —— 本 crate 同款。
//!
//! # 差异点（相对 [`super::acp_core`] 的默认值）
//!
//! * 恢复会话用 `session/load`；
//! * **不发** `session/set_model`：`set_model` 写的是 agent 作用域的
//!   `agent.json`（不是会话作用域），调一次就等于改掉用户共享的 agent 配置
//!   ⇒ [`AcpModelSelection::Unsupported`]，用量归属回落到 `"unknown"`；
//! * 会话帧带 `_meta`（见上）。
//!
//! # 有意不搬的部分
//!
//! 上游用 `opts.QwenpawWorkspace` 往 argv 追加 `--workspace <per-task 目录>`
//! （skill 隔离），并在封锁表里挡住用户自己传的同名参数。本 crate 的
//! [`LaunchRequest`] 没有"per-task workspace"这个字段，**不合成** —— 用 `cwd`
//! 冒充会指向错误目录。于是 `--workspace` 在本片既不注入也不可注入，缺口记在
//! `docs/33` §11（属于 skills/execenv 切片）。
//!
//! v1.x 的 `QwenPaw` 说另一套协议、v2.0.1 以下不受支持：本片不做版本闸门
//! （`docs/33` §11 的统一口径）。v2.1.0-beta.1 才在 `session/new` 里加 `models`
//! 字段，选择不支持就没人消费那份目录，故也不解析。

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
pub(crate) const LABEL: &str = "qwenpaw";

/// Coding Mode 的 `_meta` 键（`qwenpaw.coding_project_dir`）。
const CODING_PROJECT_DIR_META_KEY: &str = "qwenpaw.coding_project_dir";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`qwenpawBlockedArgs`）。
///
/// `--workspace` 是 daemon 自己做 per-task 隔离用的：用户覆盖它等于把任务挪出
/// 隔离目录（本片不注入，见模块头）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("acp", ArgValueMode::Standalone),
    ("--workspace", ArgValueMode::WithValue),
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
    kind: AgentType::QwenPaw,
    label: LABEL,
    // `session/load`（不是 `session/resume`）。
    resume: AcpResume::Load,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    // QwenPaw 没有 effort 旋钮（上游不调 `applyACPEffortOption`）。
    thinking_config: None,
    tool_aliases: AcpToolAliases::Kimi,
    // `set_model` 会写 agent 作用域配置 ⇒ 不支持。
    model_selection: AcpModelSelection::Unsupported,
    session_meta_key: Some(CODING_PROJECT_DIR_META_KEY),
    session_configs: &[],
    resume_params: AcpResumeParams::SessionAndCwd,
};

/// 组装 argv（`["acp"] ++ filtered`；`--workspace` 由 daemon 侧注入，本片无此字段）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["acp".to_owned()];
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// qwenpaw adapter。
#[derive(Debug)]
pub struct Qwenpaw {
    config: CliCoreConfig,
}

impl Qwenpaw {
    /// 默认配置：可执行文件 `qwenpaw`（走 `PATH`）。
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

impl Default for Qwenpaw {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for Qwenpaw {
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

impl TestableAdapter for Qwenpaw {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "qwenpaw 2.0.1\n".to_owned(),
            expected_version: Some("2.0.1".to_owned()),
            success_stdout: super::acp_core::conformance_success_stdout(
                "qwenpaw-ses-1",
                "ok",
                false,
            ),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "qwenpaw exploded".to_owned(),
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
            .with_cwd("/work/task")
            .with_model("qwen3-coder")
            .with_extra_args(["--debug"]);
        assert_eq!(
            build_args(&request),
            vec!["acp", "--debug"],
            "cwd / 模型都不进 argv"
        );
    }

    #[test]
    fn workspace_cannot_be_injected_through_extra_args() {
        let request = LaunchRequest::new("p").with_extra_args(["--workspace", "/etc", "pos"]);
        assert_eq!(build_args(&request), vec!["acp"], "封锁参数不注入也不透传");
    }

    #[test]
    fn the_session_meta_key_is_only_attached_for_a_real_cwd() {
        // `_meta` 的落点由 `acp_core::client::queue_session_frame` 决定，
        // 这里只钉住"哪一家带键、键名是什么"。
        assert_eq!(FLAVOR.session_meta_key, Some(CODING_PROJECT_DIR_META_KEY));
        assert_eq!(
            FLAVOR.model_selection,
            AcpModelSelection::Unsupported,
            "QwenPaw 的 set_model 会改用户共享配置，must not be called"
        );
        assert_eq!(FLAVOR.usage_label(), "unknown");
    }

    crate::adapter_conformance!(Qwenpaw);
}
