//! Node `cleanupSourceResolvedRunProcess` 的 Rust 端口。
//!
//! 该模块只负责本地进程生命周期，不修改 run/issue 数据；fold 的数据库状态由
//! `fold_source_resolved_stale_run` 负责，避免进程 I/O 与数据库事务耦合。

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::{sleep, Instant};
use uuid::Uuid;

pub const SESSIONED_LOCAL_ADAPTERS: &[&str] = &[
    "claude_local",
    "codex_local",
    "cursor",
    "gemini_local",
    "hermes_local",
    "opencode_local",
    "pi_local",
];

#[derive(Debug, Clone)]
pub struct CleanupSourceResolvedRunProcessInput {
    pub run_id: Uuid,
    pub adapter_type: String,
    pub pid: Option<i32>,
    pub process_group_id: Option<i32>,
    pub grace_after_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupSourceResolvedRunProcessResult {
    pub attempted: bool,
    pub outcome: String,
    pub adapter_type: String,
    pub pid: Option<i32>,
    pub process_group_id: Option<i32>,
    pub error: Option<String>,
}

pub async fn cleanup_source_resolved_run_process(
    input: CleanupSourceResolvedRunProcessInput,
) -> CleanupSourceResolvedRunProcessResult {
    let base = |attempted: bool, outcome: &str, error: Option<String>| {
        CleanupSourceResolvedRunProcessResult {
            attempted,
            outcome: outcome.to_owned(),
            adapter_type: input.adapter_type.clone(),
            pid: input.pid,
            process_group_id: input.process_group_id,
            error,
        }
    };

    if !SESSIONED_LOCAL_ADAPTERS.contains(&input.adapter_type.as_str()) {
        return base(false, "skipped_non_local_adapter", None);
    }
    if !has_valid_pid(input.pid) && !has_valid_pid(input.process_group_id) {
        return base(false, "no_process_metadata", None);
    }

    let was_alive =
        is_alive(input.pid).await || is_process_group_alive(input.process_group_id).await;
    if !was_alive {
        return base(false, "not_running", None);
    }

    let target = if has_valid_pid(input.process_group_id) {
        ProcessTarget::Group(input.process_group_id.unwrap())
    } else {
        ProcessTarget::Pid(input.pid.unwrap())
    };
    if let Err(error) = send_signal(target, false).await {
        return base(true, "failed", Some(error));
    }

    let deadline = Instant::now() + Duration::from_millis(input.grace_after_ms);
    while Instant::now() < deadline {
        if !target.is_alive().await {
            return base(true, "terminated", None);
        }
        sleep(Duration::from_millis(50)).await;
    }
    if !target.is_alive().await {
        return base(true, "terminated", None);
    }

    if let Err(error) = send_signal(target, true).await {
        return base(true, "failed", Some(error));
    }
    let outcome = if target.is_alive().await {
        "termination_sent_still_running"
    } else {
        "terminated"
    };
    base(true, outcome, None)
}

#[derive(Debug, Clone, Copy)]
enum ProcessTarget {
    Pid(i32),
    Group(i32),
}

impl ProcessTarget {
    async fn is_alive(self) -> bool {
        match self {
            Self::Pid(pid) => is_alive(Some(pid)).await,
            Self::Group(group_id) => is_process_group_alive(Some(group_id)).await,
        }
    }
}

fn has_valid_pid(value: Option<i32>) -> bool {
    matches!(value, Some(value) if value > 0)
}

async fn is_alive(pid: Option<i32>) -> bool {
    let Some(pid) = pid.filter(|pid| *pid > 0) else {
        return false;
    };
    #[cfg(unix)]
    {
        command_succeeded("kill", ["-0", &pid.to_string()]).await
    }
    #[cfg(windows)]
    {
        command_succeeded("tasklist", ["/FI", &format!("PID eq {pid}")]).await
    }
}

async fn is_process_group_alive(process_group_id: Option<i32>) -> bool {
    let Some(group_id) = process_group_id.filter(|group_id| *group_id > 0) else {
        return false;
    };
    #[cfg(unix)]
    {
        let target = format!("-{group_id}");
        command_succeeded("kill", ["-0", target.as_str()]).await
    }
    #[cfg(windows)]
    {
        let _ = group_id;
        false
    }
}

async fn send_signal(target: ProcessTarget, force: bool) -> Result<(), String> {
    #[cfg(unix)]
    {
        let signal = if force { "-KILL" } else { "-TERM" };
        let target = match target {
            ProcessTarget::Pid(pid) => pid.to_string(),
            ProcessTarget::Group(group_id) => format!("-{group_id}"),
        };
        let status = Command::new("kill")
            .args([signal, target.as_str()])
            .status()
            .await
            .map_err(|error| error.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("kill exited with status {status}"))
        }
    }
    #[cfg(windows)]
    {
        let ProcessTarget::Pid(pid) = target else {
            return Err("process group termination is unsupported on Windows".to_owned());
        };
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .await
            .map_err(|error| error.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("taskkill exited with status {status}"))
        }
    }
}

async fn command_succeeded<const N: usize>(program: &str, args: [&str; N]) -> bool {
    Command::new(program)
        .args(args)
        .stderr(Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}
