//! Codex session resume 决策（对齐 Node `codex-local/src/server/execute.ts`
//! L981-1004 的 `canResumeSession` + 日志分支）。
//!
//! ```ts
//! const canResumeSession =
//!   runtimeSessionId.length > 0 &&
//!   (runtimeSessionCwd.length === 0 ||
//!     path.resolve(runtimeSessionCwd) === path.resolve(effectiveExecutionCwd)) &&
//!   adapterExecutionTargetSessionMatches(runtimeRemoteExecution, runtimeExecutionTarget);
//! const sessionId = canResumeSession && !forceFreshSession ? runtimeSessionId : null;
//! if (executionTargetIsRemote && runtimeSessionId && !canResumeSession) {
//!   onLog("stdout", `...does not match the current remote execution identity...`);
//! } else if (runtimeSessionId && !canResumeSession) {
//!   onLog("stdout", `...was saved for cwd "${runtimeSessionCwd}"...`);
//! }
//! ```
//!
//! # 设计范围
//!
//! 本模块是**纯决策函数**，无 I/O：
//! - `decide_codex_session_resume` — 完整 resume 决策 + 日志行
//! - `session_cwd_matches_execution_target` — cwd 匹配（path.resolve 语义）
//! - `remote_execution_matches_target` — identity 匹配（复用 pc-acpx）
//!
//! `force_fresh_session`（fallback 模式）在调用方传入，决策结果可以直接
//! 用于 `session_id = can_resume && !force_fresh ? runtime : None`。

use pc_acpx::execution_target::{
    adapter_execution_target_session_matches, AdapterExecutionTarget,
};
use std::path::Path;

/// session resume 决策的输入参数。
#[derive(Debug, Clone, PartialEq)]
pub struct CodexSessionResumeInput<'a> {
    /// 持久化的 runtime session id（来自 runtime.sessionParams.sessionId）
    pub runtime_session_id: &'a str,
    /// 持久化的 runtime session cwd（来自 runtime.sessionParams.cwd）
    pub runtime_session_cwd: &'a str,
    /// 持久化的 remoteExecution 对象（来自 runtime.sessionParams.remoteExecution）
    pub runtime_remote_execution: Option<&'a serde_json::Value>,
    /// 当前有效执行 cwd（本地或受管远程目录）
    pub effective_execution_cwd: &'a str,
    /// 执行目标是否为远程（SSH / Sandbox）
    pub execution_target_is_remote: bool,
    /// 当前 execution target（用于 identity 匹配）
    pub execution_target: Option<&'a AdapterExecutionTarget>,
    /// fallback 模式是否强制 fresh session
    pub force_fresh_session: bool,
}

/// session resume 决策结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodexSessionResumeDecision {
    /// 是否可以 resume（与 Node `canResumeSession` 一致）
    pub can_resume: bool,
    /// 应当传给 CLI 的 session id（None 表示 fresh session；
    /// 已考虑 force_fresh_session）
    pub session_id: Option<String>,
    /// 决策过程中产生的 stdout 日志（与 Node onLog("stdout", ...) 一致）
    pub log_lines: Vec<String>,
}

/// 判断 cwd 是否匹配执行目标（对齐 Node `path.resolve` 比较）。
///
/// Node：`runtimeSessionCwd.length === 0 || resolve(cwd) === resolve(effectiveCwd)`。
#[must_use]
pub fn session_cwd_matches_execution_target(
    runtime_session_cwd: &str,
    effective_execution_cwd: &str,
) -> bool {
    if runtime_session_cwd.trim().is_empty() {
        return true;
    }
    normalize_like_resolve(runtime_session_cwd) == normalize_like_resolve(effective_execution_cwd)
}

/// `runtimeRemoteExecution` 与当前 execution target 是否匹配。
/// 对齐 Node `adapterExecutionTargetSessionMatches(runtimeRemoteExecution,
/// runtimeExecutionTarget)`。
#[must_use]
pub fn remote_execution_matches_target(
    runtime_remote_execution: Option<&serde_json::Value>,
    execution_target: Option<&AdapterExecutionTarget>,
) -> bool {
    let Some(saved) = runtime_remote_execution else {
        // Node parseObject(null) = {}；sessionMatches({}, local target) = true
        return execution_target.is_none()
            || matches!(
                execution_target,
                Some(AdapterExecutionTarget::Local(_))
            );
    };
    adapter_execution_target_session_matches(saved, execution_target)
}

/// 归一化路径用于 `path.resolve` 语义比较。
/// 相对路径基于当前工作目录展开，`..` / `.` 折叠。
fn normalize_like_resolve(path_str: &str) -> String {
    let path = Path::new(path_str.trim());
    if path.is_absolute() {
        let mut parts: Vec<&str> = Vec::new();
        for component in path.components() {
            use std::path::Component;
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    parts.pop();
                }
                Component::Normal(s) => parts.push(s.to_str().unwrap_or_default()),
                Component::RootDir | Component::Prefix(_) => {
                    parts.push(component.as_os_str().to_str().unwrap_or_default());
                }
            }
        }
        parts.join("/")
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        let joined = cwd.join(path_str.trim());
        joined
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// 决策是否 resume Codex session（对齐 Node execute.ts L981-1004）。
///
/// 输出：
/// - `can_resume`：是否允许 resume（不含 force_fresh_session）
/// - `session_id`：实际传给 CLI 的 session id（含 force_fresh_session）
/// - `log_lines`：需要写入 stdout 的诊断日志（与 Node 完全一致）
#[must_use]
pub fn decide_codex_session_resume(
    input: &CodexSessionResumeInput<'_>,
) -> CodexSessionResumeDecision {
    let mut decision = CodexSessionResumeDecision::default();

    if input.runtime_session_id.is_empty() {
        return decision;
    }

    let cwd_matches = session_cwd_matches_execution_target(
        input.runtime_session_cwd,
        input.effective_execution_cwd,
    );
    let remote_matches =
        remote_execution_matches_target(input.runtime_remote_execution, input.execution_target);
    let can_resume = cwd_matches && remote_matches;
    decision.can_resume = can_resume;
    decision.session_id = if can_resume && !input.force_fresh_session {
        Some(input.runtime_session_id.to_string())
    } else {
        None
    };

    if input.execution_target_is_remote && !can_resume {
        decision.log_lines.push(format!(
            "[paperclip] Codex session \"{}\" does not match the current remote execution identity and will not be resumed in \"{}\". Starting a fresh remote session.\n",
            input.runtime_session_id,
            input.effective_execution_cwd
        ));
    } else if !can_resume {
        decision.log_lines.push(format!(
            "[paperclip] Codex session \"{}\" was saved for cwd \"{}\" and will not be resumed in \"{}\".\n",
            input.runtime_session_id,
            input.runtime_session_cwd,
            input.effective_execution_cwd
        ));
    }

    decision
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_acpx::execution_target::adapter_execution_target_from_remote_execution;
    use serde_json::json;

    fn ssh_target(remote_cwd: &str) -> AdapterExecutionTarget {
        let value = json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "port": 2222,
            "username": "fixture",
            "remoteWorkspacePath": "/remote/workspace",
            "remoteCwd": remote_cwd,
            "privateKey": "PRIVATE KEY",
            "knownHosts": "[127.0.0.1]:2222 ssh-ed25519 AAAA",
            "strictHostKeyChecking": true,
        });
        adapter_execution_target_from_remote_execution(&value, None).expect("valid ssh target")
    }

    fn managed_remote() -> String {
        format!(
            "{}/.paperclip-runtime/runs/run-ssh-resume/workspace",
            "/remote/workspace"
        )
    }

    fn matching_remote_execution(remote_cwd: &str) -> serde_json::Value {
        json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "port": 2222,
            "username": "fixture",
            "remoteCwd": remote_cwd,
        })
    }

    #[test]
    fn session_cwd_matches_when_empty() {
        assert!(session_cwd_matches_execution_target("", "/remote/workspace"));
        assert!(session_cwd_matches_execution_target("  ", "/remote/workspace"));
    }

    #[test]
    fn session_cwd_matches_when_equal() {
        assert!(session_cwd_matches_execution_target(
            "/remote/workspace",
            "/remote/workspace"
        ));
    }

    #[test]
    fn session_cwd_mismatches_when_different() {
        assert!(!session_cwd_matches_execution_target(
            "/remote/workspace-other",
            "/remote/workspace"
        ));
    }

    #[test]
    fn remote_matches_ssh_identity() {
        let managed = managed_remote();
        let target = ssh_target(&managed);
        let saved = matching_remote_execution(&managed);
        assert!(remote_execution_matches_target(Some(&saved), Some(&target)));
    }

    #[test]
    fn remote_mismatches_ssh_identity() {
        let target = ssh_target("/remote/workspace");
        let saved = matching_remote_execution("/remote/other");
        assert!(!remote_execution_matches_target(Some(&saved), Some(&target)));
    }

    #[test]
    fn remote_none_matches_local_target() {
        let target: Option<AdapterExecutionTarget> = None;
        assert!(remote_execution_matches_target(None, target.as_ref()));
    }

    #[test]
    fn remote_some_against_local_target_mismatches() {
        let managed = managed_remote();
        let saved = matching_remote_execution(&managed);
        let target: Option<AdapterExecutionTarget> = None;
        assert!(!remote_execution_matches_target(Some(&saved), target.as_ref()));
    }

    #[test]
    fn decide_allows_resume_with_matching_identity() {
        let managed = managed_remote();
        let target = ssh_target(&managed);
        let saved = matching_remote_execution(&managed);
        let input = CodexSessionResumeInput {
            runtime_session_id: "session-123",
            runtime_session_cwd: &managed,
            runtime_remote_execution: Some(&saved),
            effective_execution_cwd: &managed,
            execution_target_is_remote: true,
            execution_target: Some(&target),
            force_fresh_session: false,
        };
        let decision = decide_codex_session_resume(&input);
        assert!(decision.can_resume);
        assert_eq!(decision.session_id.as_deref(), Some("session-123"));
        assert!(decision.log_lines.is_empty());
    }

    #[test]
    fn decide_denies_resume_without_matching_identity() {
        // Node 测试："does not resume saved Codex sessions for remote SSH
        // execution without a matching remote identity"
        let target = ssh_target("/remote/workspace");
        let input = CodexSessionResumeInput {
            runtime_session_id: "session-123",
            runtime_session_cwd: "/remote/workspace",
            runtime_remote_execution: None,
            effective_execution_cwd: "/remote/workspace",
            execution_target_is_remote: true,
            execution_target: Some(&target),
            force_fresh_session: false,
        };
        let decision = decide_codex_session_resume(&input);
        assert!(!decision.can_resume);
        assert_eq!(decision.session_id, None);
        assert_eq!(decision.log_lines.len(), 1);
        assert!(decision.log_lines[0].contains("does not match the current remote execution identity"));
        assert!(decision.log_lines[0].contains("Starting a fresh remote session"));
    }

    #[test]
    fn decide_denies_cwd_mismatch_logs_saved_cwd() {
        let input = CodexSessionResumeInput {
            runtime_session_id: "session-123",
            runtime_session_cwd: "/remote/old",
            runtime_remote_execution: None,
            effective_execution_cwd: "/remote/workspace",
            execution_target_is_remote: false,
            execution_target: None,
            force_fresh_session: false,
        };
        let decision = decide_codex_session_resume(&input);
        assert!(!decision.can_resume);
        assert_eq!(decision.session_id, None);
        assert_eq!(decision.log_lines.len(), 1);
        assert!(decision.log_lines[0].contains("was saved for cwd"));
        assert!(decision.log_lines[0].contains("\"/remote/old\""));
    }

    #[test]
    fn decide_force_fresh_session_overrides_resume() {
        let managed = managed_remote();
        let target = ssh_target(&managed);
        let saved = matching_remote_execution(&managed);
        let input = CodexSessionResumeInput {
            runtime_session_id: "session-123",
            runtime_session_cwd: &managed,
            runtime_remote_execution: Some(&saved),
            effective_execution_cwd: &managed,
            execution_target_is_remote: true,
            execution_target: Some(&target),
            force_fresh_session: true,
        };
        let decision = decide_codex_session_resume(&input);
        assert!(decision.can_resume);
        assert_eq!(decision.session_id, None);
        assert!(decision.log_lines.is_empty());
    }

    #[test]
    fn decide_empty_session_id_returns_fresh() {
        let input = CodexSessionResumeInput {
            runtime_session_id: "",
            runtime_session_cwd: "",
            runtime_remote_execution: None,
            effective_execution_cwd: "/remote/workspace",
            execution_target_is_remote: false,
            execution_target: None,
            force_fresh_session: false,
        };
        let decision = decide_codex_session_resume(&input);
        assert!(!decision.can_resume);
        assert_eq!(decision.session_id, None);
        assert!(decision.log_lines.is_empty());
    }
}
