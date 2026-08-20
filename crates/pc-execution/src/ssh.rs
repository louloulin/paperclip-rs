#![forbid(unsafe_code)]

//! SSH session abstraction.
//!
//! Mirrors the SSH portion of Node `workspace-runtime.ts::restoreRemoteWorkspace`.
//! The trait abstracts the underlying SSH impl so unit tests can use a mock
//! and the real `ssh2` crate can be plugged in later.

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::mpsc;
use uuid::Uuid;

/// SSH authentication credentials (matches Node `SshAuth`).
#[derive(Debug, Clone)]
pub enum SshAuth {
    /// Password-based authentication.
    Password(String),
    /// Public-key authentication with key + optional passphrase.
    PublicKey {
        private_key: String,
        passphrase: Option<String>,
    },
}

/// Connection configuration.
#[derive(Debug, Clone)]
pub struct SshSessionConfig {
    /// Remote host (hostname or IP).
    pub host: String,
    /// SSH port (default 22).
    pub port: u16,
    /// Username.
    pub username: String,
    /// Authentication.
    pub auth: SshAuth,
    /// Connection timeout in seconds.
    pub timeout_seconds: u64,
}

impl SshSessionConfig {
    pub fn new(host: impl Into<String>, port: u16, username: impl Into<String>, auth: SshAuth) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
            auth,
            timeout_seconds: 30,
        }
    }
}

/// Established SSH connection handle.
#[derive(Debug, Clone)]
pub struct SshConnection {
    pub session_id: Uuid,
    pub host: String,
    pub port: u16,
    pub username: String,
}

/// Event emitted while a remote command is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteEvent {
    Stdout(String),
    Stderr(String),
    /// Process exited with this code (0 = success).
    Exit(i32),
    /// Error during execution.
    Error(String),
}

/// Stream of `RemoteEvent`s for a running command.
#[derive(Debug)]
pub struct EventStream {
    pub receiver: mpsc::Receiver<RemoteEvent>,
}

/// SSH error.
#[derive(Debug, Error)]
pub enum SshError {
    #[error("connection refused: {0}")]
    ConnectionRefused(String),
    #[error("authentication failed: {0}")]
    Authentication(String),
    #[error("host unreachable: {0}")]
    Unreachable(String),
    #[error("timeout after {0}s")]
    Timeout(u64),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("session closed")]
    SessionClosed,
    #[error("io error: {0}")]
    Io(String),
}

/// SSH session trait. Implementations handle the actual SSH protocol.
#[async_trait]
pub trait SshSession: Send + Sync {
    /// Open an SSH connection to the configured host.
    async fn connect(&self, config: &SshSessionConfig) -> Result<SshConnection, SshError>;

    /// Execute a command on the open connection, streaming events via `mpsc`.
    async fn exec(
        &self,
        connection: &SshConnection,
        command: &str,
        env: &[(String, String)],
    ) -> Result<EventStream, SshError>;

    /// Close the connection gracefully.
    async fn close(&self, connection: SshConnection) -> Result<(), SshError>;
}

/// No-op SshSession (used when remote features are disabled).
pub struct NoopSshSession;

#[async_trait]
impl SshSession for NoopSshSession {
    async fn connect(&self, _config: &SshSessionConfig) -> Result<SshConnection, SshError> {
        Err(SshError::InvalidConfig(
            "NoopSshSession cannot connect".into(),
        ))
    }

    async fn exec(
        &self,
        _connection: &SshConnection,
        _command: &str,
        _env: &[(String, String)],
    ) -> Result<EventStream, SshError> {
        Err(SshError::SessionClosed)
    }

    async fn close(&self, _connection: SshConnection) -> Result<(), SshError> {
        Ok(())
    }
}

/// Recording SshSession for tests — captures all calls.
#[derive(Default)]
pub struct RecordingSshSession {
    pub connections: std::sync::Mutex<Vec<SshConnection>>,
    pub commands: std::sync::Mutex<Vec<String>>,
    pub envs: std::sync::Mutex<Vec<Vec<(String, String)>>>,
}

#[async_trait]
impl SshSession for RecordingSshSession {
    async fn connect(&self, config: &SshSessionConfig) -> Result<SshConnection, SshError> {
        let conn = SshConnection {
            session_id: Uuid::new_v4(),
            host: config.host.clone(),
            port: config.port,
            username: config.username.clone(),
        };
        self.connections.lock().expect("lock").push(conn.clone());
        Ok(conn)
    }

    async fn exec(
        &self,
        connection: &SshConnection,
        command: &str,
        env: &[(String, String)],
    ) -> Result<EventStream, SshError> {
        self.commands.lock().expect("lock").push(command.to_string());
        self.envs.lock().expect("lock").push(env.to_vec());
        let (tx, rx) = mpsc::channel(16);
        let _ = tx
            .send(RemoteEvent::Exit(0))
            .await;
        Ok(EventStream { receiver: rx })
    }

    async fn close(&self, _connection: SshConnection) -> Result<(), SshError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_auth_password_construct() {
        let auth = SshAuth::Password("secret".into());
        match auth {
            SshAuth::Password(p) => assert_eq!(p, "secret"),
            _ => panic!("expected Password variant"),
        }
    }

    #[test]
    fn ssh_auth_public_key_with_passphrase() {
        let auth = SshAuth::PublicKey {
            private_key: "KEY".into(),
            passphrase: Some("pass".into()),
        };
        match auth {
            SshAuth::PublicKey { private_key, passphrase } => {
                assert_eq!(private_key, "KEY");
                assert_eq!(passphrase.as_deref(), Some("pass"));
            }
            _ => panic!("expected PublicKey variant"),
        }
    }

    #[test]
    fn ssh_session_config_new() {
        let cfg = SshSessionConfig::new(
            "host.example",
            22,
            "user",
            SshAuth::Password("x".into()),
        );
        assert_eq!(cfg.host, "host.example");
        assert_eq!(cfg.port, 22);
        assert_eq!(cfg.username, "user");
        assert_eq!(cfg.timeout_seconds, 30);
    }

    #[tokio::test]
    async fn noop_session_connect_fails() {
        let session = NoopSshSession;
        let cfg = SshSessionConfig::new("h", 22, "u", SshAuth::Password("p".into()));
        let result = session.connect(&cfg).await;
        assert!(matches!(result, Err(SshError::InvalidConfig(_))));
    }

    #[tokio::test]
    async fn recording_session_captures_calls() {
        let session = RecordingSshSession::default();
        let cfg = SshSessionConfig::new("h", 22, "u", SshAuth::Password("p".into()));
        let conn = session.connect(&cfg).await.unwrap();
        let _stream = session.exec(&conn, "ls", &[]).await.unwrap();
        let conns = session.connections.lock().unwrap();
        let cmds = session.commands.lock().unwrap();
        assert_eq!(conns.len(), 1);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], "ls");
    }

    #[test]
    fn ssh_error_variants_display() {
        let errors = vec![
            SshError::ConnectionRefused("h".into()),
            SshError::Authentication("bad".into()),
            SshError::Unreachable("x".into()),
            SshError::Timeout(30),
            SshError::InvalidConfig("bad cfg".into()),
            SshError::SessionClosed,
            SshError::Io("io".into()),
        ];
        for err in errors {
            let _ = format!("{err}");
        }
    }
}