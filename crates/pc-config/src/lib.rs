//! Paperclip 配置加载。
//!
//! 单一职责：从环境变量（+ 可选 .env 文件）构建强类型 `Config`。
//! 不持有 IO 资源，不发起网络请求。
//!
//! 测试友好：`build_with<F>` 接受 env lookup 函数，避免并行测试共享进程 env。

use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::info;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required env var: {0}")]
    MissingEnv(&'static str),
    #[error("invalid env var {0}: {1}")]
    InvalidEnv(&'static str, String),
    #[error(".env load error: {0}")]
    Dotenv(#[from] dotenvy::Error),
}

/// Paperclip 运行时配置（Phase A 最小集）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub storage: StorageConfig,
    pub secrets: SecretsConfig,
    pub mode: RunMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub shutdown_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub run_migrations: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub session_cookie_name: String,
    pub session_ttl_secs: u64,
    pub api_key_header: String,
    pub csrf_header: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StorageKind {
    LocalDisk,
    S3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub kind: StorageKind,
    pub local_path: Option<String>,
    pub s3_bucket: Option<String>,
    pub s3_region: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretsKind {
    LocalEncrypted,
    AwsSecretsManager,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretsConfig {
    pub kind: SecretsKind,
    pub master_key: Option<String>,
    pub aws_region: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunMode {
    Development,
    Production,
    Test,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_env_with_dotenv(None)
    }

    pub fn from_env_with_dotenv(dotenv_path: Option<&Path>) -> Result<Self, ConfigError> {
        if let Some(p) = dotenv_path {
            dotenvy::from_path(p).ok();
        } else {
            dotenvy::dotenv().ok();
        }
        Self::build()
    }

    fn build() -> Result<Self, ConfigError> {
        Self::build_with(|k| std::env::var(k).ok())
    }

    /// 测试入口：接受 env lookup 函数。
    pub fn build_with<F: Fn(&str) -> Option<String>>(lookup: F) -> Result<Self, ConfigError> {
        let lookup = &lookup;
        let mode = match lookup("PAPERCLIP_MODE")
            .unwrap_or_else(|| "development".into())
            .to_lowercase()
            .as_str()
        {
            "production" | "prod" => RunMode::Production,
            "test" => RunMode::Test,
            _ => RunMode::Development,
        };

        let server = ServerConfig {
            host: lookup("PAPERCLIP_HOST").unwrap_or_else(|| "127.0.0.1".into()),
            port: parse_or(lookup, "PAPERCLIP_PORT", 3100)?,
            shutdown_timeout_secs: parse_or(lookup, "PAPERCLIP_SHUTDOWN_TIMEOUT_SECS", 10)?,
        };

        let database = DatabaseConfig {
            url: lookup("PAPERCLIP_DATABASE_URL")
                .ok_or(ConfigError::MissingEnv("PAPERCLIP_DATABASE_URL"))?,
            max_connections: parse_or(lookup, "PAPERCLIP_DB_MAX_CONNECTIONS", 16)?,
            min_connections: parse_or(lookup, "PAPERCLIP_DB_MIN_CONNECTIONS", 1)?,
            run_migrations: parse_or(lookup, "PAPERCLIP_DB_RUN_MIGRATIONS", true)?,
        };

        let auth = AuthConfig {
            session_cookie_name: lookup("PAPERCLIP_SESSION_COOKIE")
                .unwrap_or_else(|| "paperclip.session".into()),
            session_ttl_secs: parse_or(lookup, "PAPERCLIP_SESSION_TTL_SECS", 60 * 60 * 24 * 30)?,
            api_key_header: lookup("PAPERCLIP_API_KEY_HEADER")
                .unwrap_or_else(|| "x-paperclip-agent-key".into()),
            csrf_header: lookup("PAPERCLIP_CSRF_HEADER")
                .unwrap_or_else(|| "x-paperclip-csrf".into()),
        };

        let storage = StorageConfig {
            kind: match lookup("PAPERCLIP_STORAGE")
                .unwrap_or_else(|| "local-disk".into())
                .as_str()
            {
                "s3" => StorageKind::S3,
                _ => StorageKind::LocalDisk,
            },
            local_path: lookup("PAPERCLIP_STORAGE_LOCAL_PATH"),
            s3_bucket: lookup("PAPERCLIP_STORAGE_S3_BUCKET"),
            s3_region: lookup("PAPERCLIP_STORAGE_S3_REGION"),
        };

        let secrets = SecretsConfig {
            kind: match lookup("PAPERCLIP_SECRETS")
                .unwrap_or_else(|| "local-encrypted".into())
                .as_str()
            {
                "aws-secrets-manager" | "aws_sm" => SecretsKind::AwsSecretsManager,
                _ => SecretsKind::LocalEncrypted,
            },
            master_key: lookup("PAPERCLIP_MASTER_KEY"),
            aws_region: lookup("PAPERCLIP_AWS_REGION"),
        };

        let cfg = Config {
            server,
            database,
            auth,
            storage,
            secrets,
            mode,
        };
        info!(host = %cfg.server.host, port = cfg.server.port, mode = ?cfg.mode, "config loaded");
        Ok(cfg)
    }
}

fn parse_or<T: std::str::FromStr>(
    lookup: &dyn Fn(&str) -> Option<String>,
    key: &'static str,
    default: T,
) -> Result<T, ConfigError>
where
    T::Err: std::fmt::Display,
{
    match lookup(key) {
        Some(s) => s
            .parse::<T>()
            .map_err(|e| ConfigError::InvalidEnv(key, e.to_string())),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_with<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn build_with_minimal_env() {
        let l = lookup_with(&[("PAPERCLIP_DATABASE_URL", "postgres://localhost/test")]);
        let cfg = Config::build_with(&l).unwrap();
        assert_eq!(cfg.server.port, 3100);
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.database.url, "postgres://localhost/test");
        assert_eq!(cfg.mode, RunMode::Development);
        assert!(matches!(cfg.storage.kind, StorageKind::LocalDisk));
    }

    #[test]
    fn missing_required_url_returns_error() {
        let l = lookup_with(&[]);
        let err = Config::build_with(&l).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::MissingEnv("PAPERCLIP_DATABASE_URL")
        ));
    }

    #[test]
    fn invalid_port_returns_error() {
        let l = lookup_with(&[
            ("PAPERCLIP_DATABASE_URL", "postgres://localhost/test"),
            ("PAPERCLIP_PORT", "not-a-number"),
        ]);
        let err = Config::build_with(&l).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidEnv(_, _)));
    }

    #[test]
    fn production_mode_recognized() {
        let l = lookup_with(&[
            ("PAPERCLIP_DATABASE_URL", "postgres://localhost/test"),
            ("PAPERCLIP_MODE", "production"),
        ]);
        let cfg = Config::build_with(&l).unwrap();
        assert_eq!(cfg.mode, RunMode::Production);
    }

    #[test]
    fn s3_storage_kind_recognized() {
        let l = lookup_with(&[
            ("PAPERCLIP_DATABASE_URL", "postgres://localhost/test"),
            ("PAPERCLIP_STORAGE", "s3"),
            ("PAPERCLIP_STORAGE_S3_BUCKET", "test-bucket"),
        ]);
        let cfg = Config::build_with(&l).unwrap();
        assert!(matches!(cfg.storage.kind, StorageKind::S3));
        assert_eq!(cfg.storage.s3_bucket.as_deref(), Some("test-bucket"));
    }
}
