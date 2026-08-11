//! SSH connector / shell trait 抽象。
//!
//! R628 复刻 paperclip Node
//!   `EnvironmentCustomImageSshShell` (write/resize/close + onData/onClose/onError)
//!   `EnvironmentCustomImageSshConnector` (connect → Shell)
//!
//! 设计：
//! - `#[async_trait]` 让 trait dyn-compatible
//! - `close` / `into_data_stream` 接收 `Box<Self>`（消费所有权）
//! - 数据流通过 `mpsc::Receiver` 暴露，caller 消费
//! - 与 `pc-adapter-openclaw-gateway::wire_client` 同款 mockable 模式
//!
//! 与 Node 上游差异：
//! - Node 用 `ssh2` 包；Rust 留待 R629 选 `russh` 或 `ssh2-rs`
//! - Node 回调函数；Rust 改 mpsc channel（idiomatic async，更易测试）

use std::sync::Arc;
use tokio::sync::mpsc;

/// 终端尺寸（列/行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalDimensions {
    pub cols: u16,
    pub rows: u16,
}

/// 已建立的 SSH shell handle（持有方独占 write/resize/close）。
///
/// stdout 流由 `TerminalSshConnector::connect` 单独返回 `mpsc::Receiver<ShellEvent>`，
/// 避免 shell 所有权和数据流耦合（handler 同时持有 handle 和 receiver）。
#[async_trait::async_trait]
pub trait TerminalSshShell: Send {
    /// 写一段 stdin 到远端 PTY。
    async fn write(&mut self, data: &str) -> Result<(), String>;

    /// 调整 PTY 尺寸。
    async fn resize(&mut self, dims: TerminalDimensions) -> Result<(), String>;

    /// 显式关闭 shell + SSH 连接。Box<Self> 转移所有权。
    async fn close(self: Box<Self>) -> Result<(), String>;
}

/// Shell 生命周期事件（通过 mpsc 发送给 caller）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// SSH stdout 数据
    Data(String),
    /// Shell 正常关闭（EOF）
    Closed,
    /// SSH 流错误（network / protocol / host key mismatch 等）
    Error(String),
}

/// SSH 连接参数（由 setup session 决定，从 DB 取出）。
#[derive(Debug, Clone)]
pub struct SshConnectionParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 初始 term type（固定 `"xterm-256color"` 与 Node 一致）
    pub term: String,
    pub initial_dims: TerminalDimensions,
}

/// Connector 注入点：把 SSH 连接参数 + host key 验证 callback 转成实际 Shell。
///
/// 真实实现留待 R629（`russh` 或 `ssh2`）。
/// 当前轮次：trait 定义 + FakeSshConnector 用于单测。
#[async_trait::async_trait]
pub trait TerminalSshConnector: Send + Sync {
    /// 建立 SSH shell + 启动 stdout mpsc stream。
    ///
    /// 返回 `(handle, data_rx)`：handle 由 caller 持有用于 write/resize/close；
    /// data_rx 持续 drain ShellEvent 流（Data/Closed/Error）。
    ///
    /// `verify_host_key_sha256` 由 caller 注入：返回 true 表示接受该 host key
    /// （pin 现有或写入新 pin），返回 false 表示拒绝（host key 变化 → 401）。
    async fn connect(
        &self,
        params: SshConnectionParams,
        verify_host_key_sha256: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Result<(Box<dyn TerminalSshShell>, mpsc::Receiver<ShellEvent>), String>;
}

// ============================================================================
// FakeSshShell (单测用)
// ============================================================================

pub struct FakeSshShell {
    pub writes: Vec<String>,
    pub resizes: Vec<TerminalDimensions>,
    pub closed: bool,
}

#[async_trait::async_trait]
impl TerminalSshShell for FakeSshShell {
    async fn write(&mut self, data: &str) -> Result<(), String> {
        if self.closed {
            return Err("shell closed".into());
        }
        self.writes.push(data.into());
        Ok(())
    }

    async fn resize(&mut self, dims: TerminalDimensions) -> Result<(), String> {
        if self.closed {
            return Err("shell closed".into());
        }
        self.resizes.push(dims);
        Ok(())
    }

    async fn close(mut self: Box<Self>) -> Result<(), String> {
        self.closed = true;
        Ok(())
    }

}

// ============================================================================
// FakeSshConnector (单测用)
// ============================================================================

#[derive(Clone)]
pub struct FakeSshConnector {
    /// 每次 connect 调用的 host key verifier（用于测试 verify 注入）
    pub verify_returns: bool,
    /// 每次 connect 失败注入的错误（test SSH connect fail）
    pub connect_error: Option<String>,
    /// 给每个新 shell 的预录 data_script
    pub data_script: Vec<ShellEvent>,
}

impl Default for FakeSshConnector {
    fn default() -> Self {
        Self {
            verify_returns: true,
            connect_error: None,
            data_script: Vec::new(),
        }
    }
}

#[async_trait::async_trait]
impl TerminalSshConnector for FakeSshConnector {
    async fn connect(
        &self,
        _params: SshConnectionParams,
        verify_host_key_sha256: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Result<(Box<dyn TerminalSshShell>, mpsc::Receiver<ShellEvent>), String> {
        let accept = verify_host_key_sha256("fake-host-key-sha256");
        if !accept {
            return Err("host key rejected".into());
        }
        if let Some(err) = &self.connect_error {
            return Err(err.clone());
        }
        let (tx, rx) = mpsc::channel(16);
        let script = self.data_script.clone();
        tokio::spawn(async move {
            for ev in script {
                if tx.send(ev).await.is_err() {
                    break;
                }
            }
        });
        let shell: Box<dyn TerminalSshShell> = Box::new(FakeSshShell {
            writes: Vec::new(),
            resizes: Vec::new(),
            closed: false,
        });
        Ok((shell, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dims(cols: u16, rows: u16) -> TerminalDimensions {
        TerminalDimensions { cols, rows }
    }

    fn params() -> SshConnectionParams {
        SshConnectionParams {
            host: "127.0.0.1".into(),
            port: 22,
            username: "root".into(),
            term: "xterm-256color".into(),
            initial_dims: dims(80, 24),
        }
    }

    // ----- TerminalSshShell (FakeSshShell) -----

    #[tokio::test]
    async fn fake_shell_write_appends() {
        let mut shell = FakeSshShell {
            writes: Vec::new(),
            resizes: Vec::new(),
            closed: false,
        };
        shell.write("ls\n").await.unwrap();
        shell.write("pwd\n").await.unwrap();
        assert_eq!(shell.writes, vec!["ls\n", "pwd\n"]);
    }

    #[tokio::test]
    async fn fake_shell_resize_appends() {
        let mut shell = FakeSshShell {
            writes: Vec::new(),
            resizes: Vec::new(),
            closed: false,
        };
        shell.resize(dims(80, 24)).await.unwrap();
        shell.resize(dims(120, 40)).await.unwrap();
        assert_eq!(shell.resizes, vec![dims(80, 24), dims(120, 40)]);
    }

    #[tokio::test]
    async fn fake_shell_close_marks_closed() {
        let shell = Box::new(FakeSshShell {
            writes: Vec::new(),
            resizes: Vec::new(),
            closed: false,
        });
        let r = shell.close().await;
        assert!(r.is_ok());
    }

    // ----- TerminalSshConnector (FakeSshConnector) -----

    #[tokio::test]
    async fn fake_connector_accepts_valid_host_key() {
        let conn = FakeSshConnector {
            verify_returns: true,
            connect_error: None,
            data_script: vec![],
        };
        let verify: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(|_| true);
        let (mut shell, _rx) = conn.connect(params(), verify).await.expect("connect ok");
        shell.write("hi\n").await.unwrap();
    }

    #[tokio::test]
    async fn fake_connector_rejects_bad_host_key() {
        let conn = FakeSshConnector {
            verify_returns: true,
            connect_error: None,
            data_script: vec![],
        };
        let verify: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(|_| false);
        match conn.connect(params(), verify).await {
            Err(e) => assert_eq!(e, "host key rejected"),
            Ok(_) => panic!("expected host key rejection"),
        }
    }

    #[tokio::test]
    async fn fake_connector_propagates_connect_error() {
        let conn = FakeSshConnector {
            verify_returns: true,
            connect_error: Some("connection refused".into()),
            data_script: vec![],
        };
        let verify: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(|_| true);
        match conn.connect(params(), verify).await {
            Err(e) => assert_eq!(e, "connection refused"),
            Ok(_) => panic!("expected connect error"),
        }
    }
}
