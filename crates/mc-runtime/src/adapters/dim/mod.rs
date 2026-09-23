//! `dim`（`DimCode` CLI）adapter —— 上游 `server/pkg/agent/dim.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `dim.go` L192（`["acp"] ++ filtered custom_args`） |
//! | `BLOCKED` | `dimBlockedArgs`（L21） |
//! | [`FLAVOR`] | `dimBackend.Execute` 的会话 / 配置链 / `set_model` / effort 四段 |
//! | [`super::acp_core::AcpDecoder`] | 复用的 `hermesClient` |
//!
//! # 命令面
//!
//! `dim acp`。这个 provider 有一个别的 ACP provider 都没有的特性：**建会话后必须
//! 先下发两条固定配置**。Dim 的 ACP server 在 `session/new` 时把权限钉在只读预设上，
//! 不抬到 `permission=full-access` 的话每一次写文件、每一次 spawn 进程都会被静默拒绝
//! —— 任务看起来"跑完了"，实际什么都没改。
//!
//! 这就是 [`AcpFlavor::session_configs`] 的由来。两条配置**在新建与恢复会话上都要
//! 重下发**（上游注释：恢复后的会话可能继承上一轮"半配置"的状态，而
//! `set_config_option` 是幂等的），链上任一步失败都是致命的。
//!
//! # 差异点（相对 [`super::acp_core`] 的默认值）
//!
//! * 恢复会话用 `session/load`（dim 0.3.10+ 才在进程退出后释放会话锁）；
//! * 会话帧后先走固定配置链 `permission=full-access` → `mode=agent`，再
//!   `set_model`（失败致命），最后 effort；
//! * 推理等级走 `session/set_config_option {configId: "thought_level"}`，失败**不致命**。
//!
//! # 有意不搬的部分
//!
//! * **版本闸门**：上游要求 dim ≥ 0.3.10，否则拒绝恢复会话（老版本把会话永久绑在
//!   创建它的进程上，跨进程 `session/load` 必失败）。本 crate 的
//!   [`crate::adapter::RuntimeAdapter`] 契约里没有"探测到版本后改变行为"这一档
//!   （版本探测是独立的一次 RPC，不与 run 耦合），所以只做一次 `session/load`、
//!   失败即失败的归因（`ResumeRejected` 的分类与重试留给 M4）。
//! * `session/load` 的"锁还没释放 → 重试"（`dimSessionLoadRetryDelay`）：同属重试
//!   编排。见 `docs/33` §11。
//! * `session/close` 的 best-effort 收尾：本 crate 的会话生命周期与进程一致
//!   （进程退出即释放），没有跨 run 的会话托管。

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
pub(crate) const LABEL: &str = "dim";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`dimBlockedArgs`）。
///
/// `--auth-setup` / `--remote` 会把 CLI 切进"登录 / 远程"两种**不会启动 ACP
/// server** 的模式；`--help` / `-h` 同理（输出完就退出）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("acp", ArgValueMode::Standalone),
    ("--auth-setup", ArgValueMode::Standalone),
    ("--remote", ArgValueMode::Standalone),
    ("--help", ArgValueMode::Standalone),
    ("-h", ArgValueMode::Standalone),
];

/// 已知取值参数（`acp` 子命令没有别的 daemon 认识的取值 flag）。
const MODES: &[(&str, ArgValueMode)] = &[];

/// `extra_args` 过滤策略（prompt 走 ACP 的 `session/prompt`，不进 argv）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 建会话后**必须依次下发**的固定配置（上游 `dim.go` L510 的两元素表）。
///
/// 顺序即语义：先把只读预设抬成 `full-access`，再把模式钉成 `agent`。
const SESSION_CONFIGS: &[(&str, &str)] = &[("permission", "full-access"), ("mode", "agent")];

/// 本 provider 的 ACP 差异表。
pub static FLAVOR: AcpFlavor = AcpFlavor {
    kind: AgentType::Dim,
    label: LABEL,
    resume: AcpResume::Load,
    auth: AcpAuth::None,
    prompt_fields: AcpPromptFields::Prompt,
    // Dim 的 effort 旋钮叫 `thought_level`（auto/high/max）。
    thinking_config: Some("thought_level"),
    tool_aliases: AcpToolAliases::Kimi,
    model_selection: AcpModelSelection::SetModel,
    session_meta_key: None,
    session_configs: SESSION_CONFIGS,
    resume_params: AcpResumeParams::SessionAndCwd,
};

/// 组装 argv（`["acp"] ++ filtered`）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["acp".to_owned()];
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// dim adapter。
#[derive(Debug)]
pub struct Dim {
    config: CliCoreConfig,
}

impl Dim {
    /// 默认配置：可执行文件 `dim`（走 `PATH`）。
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

impl Default for Dim {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpProvider for Dim {
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

impl TestableAdapter for Dim {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "dim 0.3.10\n".to_owned(),
            expected_version: Some("0.3.10".to_owned()),
            // 固定配置链有两条 ⇒ 回放里要多两条 `id=50/51` 的应答，否则逐帧推进
            // 会在第 50 帧上死等（见 `acp_core::conformance_config_stdout`）。
            success_stdout: super::acp_core::conformance_config_stdout(
                "dim-ses-1",
                "ok",
                SESSION_CONFIGS.len(),
            ),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(15),
            junk_stdout: super::acp_core::conformance_junk_stdout(LABEL, "ok"),
            expected_junk_output: "ok".to_owned(),
            expected_error: "dim exploded".to_owned(),
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
            .with_model("dim-pro")
            .with_extra_args(["--verbose"]);
        assert_eq!(
            build_args(&request),
            vec!["acp", "--verbose"],
            "模型 / cwd 都不进 argv"
        );
    }

    #[test]
    fn non_acp_modes_cannot_be_activated_through_extra_args() {
        let request = LaunchRequest::new("p").with_extra_args(["--auth-setup", "--remote", "-h"]);
        assert_eq!(build_args(&request), vec!["acp"]);
    }

    /// 配置链的顺序与取值是**行为契约**：改顺序就等于先要 agent 模式再放开权限。
    #[test]
    fn the_session_config_chain_is_order_and_value_pinned() {
        assert_eq!(SESSION_CONFIGS.len(), 2);
        assert_eq!(SESSION_CONFIGS[0], ("permission", "full-access"));
        assert_eq!(SESSION_CONFIGS[1], ("mode", "agent"));
        assert_eq!(FLAVOR.session_configs.len(), SESSION_CONFIGS.len());
    }

    crate::adapter_conformance!(Dim);
}
