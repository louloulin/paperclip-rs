#![forbid(unsafe_code)]
//! `systemd-notify(1)` 调用封装（原 `pc-systemd-notify` crate 已下沉）。
//!
//! 对应 Node `server/src/services/systemd-notify.ts`（8 行）。
//!
//! 设计目标：1:1 复刻
//! - `systemdNotify(args)` —— 当 `NOTIFY_SOCKET` 环境变量非空时调用 `systemd-notify`
//!   并把退出码转成 boolean；否则直接返回 `false`
//! - `windowsHide: true` 在 Rust 端用 `Command::new(...).spawn()` 默认即可（无
//!   console window 概念）
//! - 用 `Arc<dyn Fn>` 注入命令执行器 + `NOTIFY_SOCKET` getter，便于测试

use std::process::Command;
use std::sync::Arc;

/// 命令执行 trait：抽象 `systemd-notify` 子进程调用。
///
/// 测试中可注入 fake executor 验证 args 转发与返回值的语义。
pub trait NotifyExecutor: Send + Sync {
    fn run(&self, args: &[&str]) -> bool;
}

/// 默认实现：用 `Command::new("systemd-notify")` 执行。
#[derive(Default)]
pub struct ChildProcessNotifyExecutor;

impl NotifyExecutor for ChildProcessNotifyExecutor {
    fn run(&self, args: &[&str]) -> bool {
        Command::new("systemd-notify")
            .args(args)
            .spawn()
            .and_then(|mut c| c.wait())
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// 时钟 / env trait 对象 —— 用于在测试中控制 `NOTIFY_SOCKET`。
pub type NotifySocketGetter = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// `systemd-notify(1)` 调用。
///
/// 与 Node `systemdNotify(args)` 1:1 对齐：
/// - `NOTIFY_SOCKET` 不存在 / 空字符串 → 返回 `false`（不调用）
/// - 否则调用 `systemd-notify <args>`，exit=0 → `true`，否则 → `false`
pub async fn systemd_notify(
    args: &[&str],
    executor: Arc<dyn NotifyExecutor>,
    notify_socket: NotifySocketGetter,
) -> bool {
    if notify_socket().is_none() {
        return false;
    }
    executor.run(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingExecutor {
        calls: Mutex<Vec<Vec<String>>>,
        success: Mutex<bool>,
    }

    impl NotifyExecutor for RecordingExecutor {
        fn run(&self, args: &[&str]) -> bool {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|s| s.to_string()).collect());
            *self.success.lock().unwrap()
        }
    }

    #[tokio::test]
    async fn r703_returns_false_when_notify_socket_unset() {
        let exec = Arc::new(RecordingExecutor::default());
        let get: NotifySocketGetter = Arc::new(|| None);
        let r = systemd_notify(&["READY=1"], exec, get).await;
        assert!(!r);
    }

    #[tokio::test]
    async fn r703_returns_false_when_notify_socket_empty() {
        let exec = Arc::new(RecordingExecutor::default());
        let get: NotifySocketGetter = Arc::new(|| Some("".to_string()));
        let r = systemd_notify(&["READY=1"], exec, get).await;
        assert!(!r);
    }

    #[tokio::test]
    async fn r703_returns_false_when_notify_socket_whitespace() {
        let exec = Arc::new(RecordingExecutor::default());
        let get: NotifySocketGetter = Arc::new(|| Some("   ".to_string()));
        let r = systemd_notify(&["READY=1"], exec, get).await;
        assert!(!r);
    }

    #[tokio::test]
    async fn r703_forwards_args_to_executor() {
        let exec = Arc::new(RecordingExecutor {
            calls: Mutex::new(Vec::new()),
            success: Mutex::new(true),
        });
        let get: NotifySocketGetter = Arc::new(|| Some("/run/systemd/notify".to_string()));
        let r = systemd_notify(&["READY=1", "STATUS=Working..."], exec.clone(), get).await;
        assert!(r);
        let calls = exec.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], vec!["READY=1", "STATUS=Working..."]);
    }

    #[tokio::test]
    async fn r703_returns_false_when_executor_fails() {
        let exec = Arc::new(RecordingExecutor {
            calls: Mutex::new(Vec::new()),
            success: Mutex::new(false),
        });
        let get: NotifySocketGetter = Arc::new(|| Some("/run/systemd/notify".to_string()));
        let r = systemd_notify(&["READY=1"], exec, get).await;
        assert!(!r);
    }

    #[tokio::test]
    async fn r703_empty_args_list_works() {
        let exec = Arc::new(RecordingExecutor {
            calls: Mutex::new(Vec::new()),
            success: Mutex::new(true),
        });
        let get: NotifySocketGetter = Arc::new(|| Some("/run/systemd/notify".to_string()));
        let r = systemd_notify(&[], exec.clone(), get).await;
        assert!(r);
        assert_eq!(exec.calls.lock().unwrap()[0].len(), 0);
    }
}
