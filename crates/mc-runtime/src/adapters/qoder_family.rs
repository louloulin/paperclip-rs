//! Qoder 系两个 CLI（`qoder` / `qoderclicn`）的**共用**参数面。
//!
//! 上游把这两家写在同一个文件里：`qoderBackend` 只有 `defaultExecutable`
//! 一个字段随 `providerType` 变（`qoderclicn` → `qoderclicn`，其余 → `qodercli`），
//! argv、封锁表、resume 方法、prompt 字段、工具名表**全部相同**
//! （`server/pkg/agent/qoder.go`）。
//!
//! 所以本文件只放"两家一模一样"的东西：封锁表 / 取值表 / [`ArgPolicy`] /
//! [`build_args`]；两个 provider 模块（[`super::qoder`] / [`super::qoderclicn`]）
//! 各自提供可执行文件名、[`AcpFlavor`]、`AcpProvider` 与 `TestableAdapter`。
//! 与 [`super::claude_family`] / [`super::opencode_family`] 的分层一致 ——
//! **共享的只抽一份，逐 provider 的（标签、能力、回放脚本）留在各自模块**。
//!
//! # 为什么这两个是**两个** provider 而不是一个
//!
//! 它们是两个独立可执行文件（各自的登录态、各自的安装方式），上游也按两个
//! providerType 注册（`qoder` / `qoderclicn`）。但 `qodercli --acp` 与
//! `qoderclicn --acp` 的**线协议完全相同**，所以差异只落在"跑哪个 binary"上。

use super::cli_core::args::{filter_extra_args, ArgPolicy, ArgValueMode};
use crate::adapter::LaunchRequest;

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`qoderBlockedArgs`）。
///
/// Qoder 进 ACP 模式靠的是**全局** `--acp` flag（不是 `acp` 子命令），
/// `--yolo` 由 daemon 拥有以保证无人值守时不卡在权限确认；老式的 `acp`
/// 子命令也一并封锁，免得 `custom_args` 把进程切成另一种模式。
pub(crate) const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("--acp", ArgValueMode::Standalone),
    ("acp", ArgValueMode::Standalone),
    ("--yolo", ArgValueMode::Standalone),
];

/// 已知取值参数（`qoderBlockedArgs` 里没有吃值的项）。
pub(crate) const MODES: &[(&str, ArgValueMode)] = &[];

/// `extra_args` 过滤策略（prompt 走 JSON-RPC 的 `session/prompt`，不进 argv）。
pub(crate) const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 组装 argv（上游 `qoder.go` L100：`["--yolo", "--acp"] ++ custom_args`）。
///
/// 两个 binary 的 argv 形状完全一样；顺序也照上游：`--yolo` 在前。
pub(crate) fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["--yolo".to_owned(), "--acp".to_owned()];
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_shape_is_shared_and_flags_are_locked() {
        let request = LaunchRequest::new("p")
            .with_model("qwen3-coder")
            .with_resume_session("ses-old")
            .with_extra_args(["--acp", "--yolo", "acp", "--verbose"]);
        assert_eq!(
            build_args(&request),
            vec!["--yolo", "--acp", "--verbose"],
            "模型 / 恢复会话不进 argv；协议开关不能被重复或换成 acp 子命令"
        );
    }
}
