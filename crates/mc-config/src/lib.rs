//! Multica 配置加载。
//!
//! 单一职责：从环境变量（+ 可选 .env 文件）构建强类型 `Config`。
//! 不持有 IO 资源，不发起网络请求。
//!
//! 测试友好：`build_with<F>` 接受 env lookup 函数，避免并行测试共享进程 env。
//!
//! 环境变量前缀：`MULTICA_*`（首选），兼容 `PAPERCLIP_*`（迁移期）。
//!
//! 章节：
//! - `server`    — 监听地址 / 端口 / 外部 URL
//! - `database`  — PostgreSQL URL / 连接池大小 / 是否自动迁移
//! - `auth`      — cookie / API key / CSRF 头部 / session TTL
//! - `storage`   — local-disk / s3 provider
//! - `secrets`   — 本地加密密钥 / AWS Secrets Manager
//! - `instance`  — telemetry / instance id
//! - `feature_flags` — 默认开关
//! - `runtime`   — runtime host (daemon)
//! - `channel`   — channel 默认行为
//! - `plugin`    — plugin IPC

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;
use url::Url;

pub mod home_paths;

pub use home_paths::{
    expand_home_prefix, resolve_home_aware_path, HomePathError, MulticaHomePaths,
    DEFAULT_MULTICA_INSTANCE_ID, MULTICA_CONFIG_BASENAME, MULTICA_ENV_FILENAME,
};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required env var: {0}")]
    MissingEnv(&'static str),

    #[error("invalid env var {0}: {1}")]
    InvalidEnv(&'static str, String),

    #[error(".env load error: {0}")]
    Dotenv(#[from] dotenvy::Error),

    #[error("invalid URL: {0}")]
    InvalidUrl(#[from] url::ParseError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Multica 运行时配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub storage: StorageConfig,
    pub secrets: SecretsConfig,
    pub instance: InstanceConfig,
    pub feature_flags: FeatureFlagsConfig,
    pub runtime: RuntimeConfig,
    pub channel: ChannelConfig,
    pub plugin: PluginConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub external_url: Option<Url>,
    pub mode: RunMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunMode {
    Development,
    Production,
    Test,
}

impl RunMode {
    pub fn from_env_or_default(value: Option<&str>) -> Self {
        match value.unwrap_or("production").to_ascii_lowercase().as_str() {
            "development" | "dev" => Self::Development,
            "test" => Self::Test,
            _ => Self::Production,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub run_migrations: bool,
    pub statement_timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub session_cookie_name: String,
    pub api_key_header: String,
    pub csrf_header: String,
    /// 滑窗 session TTL（秒）。默认 30 天；`/api/auth/refresh` 在窗口内续期。
    pub session_ttl_secs: u64,
    pub pat_ttl_secs: u64,
    pub require_csrf: bool,
    pub require_email_verified: bool,
    /// 邮件验证码 TTL（秒）。默认 600 = 10 分钟。
    pub verification_code_ttl_secs: u64,
    /// 单 IP / 全局每秒允许的 `send-code` 请求数。默认 20。
    pub send_code_per_min: u32,
    /// 单邮箱每分钟允许的 `send-code` 请求数（防爆破）。默认 5。
    pub send_code_per_email_per_min: u32,
    /// 单 workspace 每小时最大邀请条数（对应上游 multica
    /// `RATE_LIMIT_INVITATION_PER_WORKSPACE_PER_HOUR`）。默认 50。
    /// 由 M1 sub-issue C 追加。
    pub invitation_per_workspace_per_hour: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub default_provider: String,
    pub local_disk_root: PathBuf,
    pub s3_bucket: Option<String>,
    pub s3_region: Option<String>,
    pub s3_endpoint: Option<Url>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretsConfig {
    pub local_keyring: bool,
    pub aws_secrets_manager_prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub instance_id: String,
    pub telemetry_enabled: bool,
    pub telemetry_endpoint: Option<Url>,
    pub maintenance_interval_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlagsConfig {
    pub default_enabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub default_runtime: String,
    pub max_concurrent_tasks_per_agent: u32,
    pub lease_secs: u64,
    pub retry_max: u32,
    pub allow_local_daemon: bool,
    pub allow_cloud_runtime: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub default_reply_strategy: String,
    pub rate_limit_per_minute: u32,
    pub media_max_size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub host_workspace_root: PathBuf,
    pub spawn_timeout_secs: u64,
    pub healthcheck_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 3500,
                external_url: None,
                mode: RunMode::Production,
            },
            database: DatabaseConfig {
                url: "postgres://multica:multica@127.0.0.1:5432/multica".into(),
                max_connections: 20,
                min_connections: 2,
                run_migrations: true,
                statement_timeout_ms: None,
            },
            auth: AuthConfig {
                session_cookie_name: "multica_session".into(),
                api_key_header: "X-Multica-Api-Key".into(),
                csrf_header: "X-Multica-Csrf".into(),
                session_ttl_secs: 60 * 60 * 24 * 30,
                pat_ttl_secs: 60 * 60 * 24 * 365,
                require_csrf: true,
                require_email_verified: false,
                verification_code_ttl_secs: 600,
                send_code_per_min: 20,
                send_code_per_email_per_min: 5,
                invitation_per_workspace_per_hour: 50,
            },
            storage: StorageConfig {
                default_provider: "local_disk".into(),
                local_disk_root: PathBuf::from(".multica-storage"),
                s3_bucket: None,
                s3_region: None,
                s3_endpoint: None,
            },
            secrets: SecretsConfig {
                local_keyring: true,
                aws_secrets_manager_prefix: None,
            },
            instance: InstanceConfig {
                instance_id: DEFAULT_MULTICA_INSTANCE_ID.into(),
                telemetry_enabled: false,
                telemetry_endpoint: None,
                maintenance_interval_secs: 60,
            },
            feature_flags: FeatureFlagsConfig {
                default_enabled: Vec::new(),
            },
            runtime: RuntimeConfig {
                default_runtime: "claude-code".into(),
                max_concurrent_tasks_per_agent: 4,
                lease_secs: 5 * 60,
                retry_max: 3,
                allow_local_daemon: true,
                allow_cloud_runtime: false,
            },
            channel: ChannelConfig {
                default_reply_strategy: "thread".into(),
                rate_limit_per_minute: 60,
                media_max_size_bytes: 25 * 1024 * 1024,
            },
            plugin: PluginConfig {
                host_workspace_root: PathBuf::from("./plugins"),
                spawn_timeout_secs: 15,
                healthcheck_interval_secs: 30,
            },
        }
    }
}

impl Config {
    /// 从当前进程环境构建 `Config`。
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::build_with(|name| std::env::var(name).ok())
    }

    /// 测试 / 嵌入式场景下，使用传入的 lookup 函数读取 env。
    /// 允许并行测试而不会污染全局 env。
    pub fn build_with<F>(mut lookup: F) -> Result<Self, ConfigError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        // Optional: 加载 .env（若存在），不强制。
        let _ = dotenvy::dotenv();

        let mut config = Config::default();

        if let Some(host) = lookup("MULTICA_HOST").or_else(|| lookup("PAPERCLIP_HOST")) {
            config.server.host = host;
        }
        if let Some(port) = lookup("MULTICA_PORT")
            .or_else(|| lookup("PAPERCLIP_PORT"))
            .and_then(|s| s.parse::<u16>().ok())
        {
            config.server.port = port;
        }
        if let Some(url) = lookup("MULTICA_EXTERNAL_URL").or_else(|| lookup("PAPERCLIP_EXTERNAL_URL"))
        {
            config.server.external_url = Some(Url::parse(&url)?);
        }
        if let Some(mode) = lookup("MULTICA_RUN_MODE").or_else(|| lookup("PAPERCLIP_RUN_MODE")) {
            config.server.mode = RunMode::from_env_or_default(Some(&mode));
        }

        // Database
        let db_url = lookup("MULTICA_DATABASE_URL")
            .or_else(|| lookup("PAPERCLIP_DATABASE_URL"))
            .or_else(|| lookup("DATABASE_URL"))
            .ok_or(ConfigError::MissingEnv("MULTICA_DATABASE_URL"))?;
        config.database.url = db_url;
        if let Some(max) = lookup("MULTICA_DB_MAX_CONNECTIONS")
            .and_then(|s| s.parse().ok())
        {
            config.database.max_connections = max;
        }
        if let Some(min) = lookup("MULTICA_DB_MIN_CONNECTIONS")
            .and_then(|s| s.parse().ok())
        {
            config.database.min_connections = min;
        }
        if let Some(run) = lookup("MULTICA_DB_RUN_MIGRATIONS")
            .or_else(|| lookup("PAPERCLIP_DB_RUN_MIGRATIONS"))
        {
            config.database.run_migrations = parse_bool(&run).unwrap_or(true);
        }

        // Auth
        if let Some(c) = lookup("MULTICA_SESSION_COOKIE_NAME") {
            config.auth.session_cookie_name = c;
        }
        if let Some(h) = lookup("MULTICA_API_KEY_HEADER") {
            config.auth.api_key_header = h;
        }
        if let Some(h) = lookup("MULTICA_CSRF_HEADER") {
            config.auth.csrf_header = h;
        }
        if let Some(ttl) = lookup("MULTICA_SESSION_TTL_SECS")
            .and_then(|s| s.parse().ok())
        {
            config.auth.session_ttl_secs = ttl;
        }
        if let Some(ttl) = lookup("MULTICA_AUTH_VERIFICATION_CODE_TTL_SECS")
            .and_then(|s| s.parse().ok())
        {
            config.auth.verification_code_ttl_secs = ttl;
        }
        if let Some(n) = lookup("MULTICA_AUTH_SEND_CODE_PER_MIN")
            .and_then(|s| s.parse().ok())
        {
            config.auth.send_code_per_min = n;
        }
        if let Some(n) = lookup("MULTICA_AUTH_SEND_CODE_PER_EMAIL_PER_MIN")
            .and_then(|s| s.parse().ok())
        {
            config.auth.send_code_per_email_per_min = n;
        }
        // Invitation rate limit（M1 sub-issue C 追加）。
        if let Some(n) = lookup("MULTICA_INVITATION_PER_WORKSPACE_PER_HOUR")
            .and_then(|s| s.parse().ok())
        {
            config.auth.invitation_per_workspace_per_hour = n;
        }

        // Storage
        if let Some(p) = lookup("MULTICA_STORAGE_ROOT") {
            config.storage.local_disk_root = PathBuf::from(p);
        }

        // Instance
        if let Some(id) = lookup("MULTICA_INSTANCE_ID") {
            config.instance.instance_id = id;
        }
        if let Some(t) = lookup("MULTICA_TELEMETRY_ENABLED") {
            config.instance.telemetry_enabled = parse_bool(&t).unwrap_or(false);
        }
        if let Some(url) = lookup("MULTICA_TELEMETRY_ENDPOINT") {
            config.instance.telemetry_endpoint = Some(Url::parse(&url)?);
        }

        // Runtime
        if let Some(p) = lookup("MULTICA_DEFAULT_RUNTIME") {
            config.runtime.default_runtime = p;
        }
        if let Some(n) = lookup("MULTICA_MAX_CONCURRENT_TASKS").and_then(|s| s.parse().ok()) {
            config.runtime.max_concurrent_tasks_per_agent = n;
        }

        // Channel
        if let Some(n) = lookup("MULTICA_CHANNEL_RATE_LIMIT")
            .and_then(|s| s.parse().ok())
        {
            config.channel.rate_limit_per_minute = n;
        }

        // Plugin
        if let Some(p) = lookup("MULTICA_PLUGIN_ROOT") {
            config.plugin.host_workspace_root = PathBuf::from(p);
        }

        info!(
            host = %config.server.host,
            port = config.server.port,
            mode = ?config.server.mode,
            "multica config loaded"
        );

        Ok(config)
    }
}

/// Parse "true" / "false" / "1" / "0" / "yes" / "no" / "on" / "off" into Option<bool>.
fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let c = Config::default();
        assert_eq!(c.server.host, "127.0.0.1");
        assert_eq!(c.server.port, 3500);
        assert_eq!(c.auth.session_cookie_name, "multica_session");
        assert_eq!(c.runtime.default_runtime, "claude-code");
    }

    #[test]
    fn build_with_minimal_env() {
        let lookup = |name: &str| match name {
            "MULTICA_DATABASE_URL" => Some("postgres://u:p@host:5432/db".to_string()),
            _ => None,
        };
        let c = Config::build_with(lookup).unwrap();
        assert_eq!(c.database.url, "postgres://u:p@host:5432/db");
    }

    #[test]
    fn parse_bool_recognises_truthy_and_falsy() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("1"), Some(true));
        assert_eq!(parse_bool("on"), Some(true));
        assert_eq!(parse_bool("false"), Some(false));
        assert_eq!(parse_bool("off"), Some(false));
        assert_eq!(parse_bool("xyz"), None);
    }

    #[test]
    fn run_mode_parses() {
        assert_eq!(RunMode::from_env_or_default(Some("dev")), RunMode::Development);
        assert_eq!(RunMode::from_env_or_default(Some("test")), RunMode::Test);
        assert_eq!(RunMode::from_env_or_default(Some("prod")), RunMode::Production);
        assert_eq!(RunMode::from_env_or_default(None), RunMode::Production);
    }

    #[test]
    fn missing_database_url_is_error() {
        let lookup = |_: &str| None;
        let err = Config::build_with(lookup).unwrap_err();
        match err {
            ConfigError::MissingEnv(name) => assert_eq!(name, "MULTICA_DATABASE_URL"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn paperclip_env_alias_works() {
        let lookup = |name: &str| match name {
            "PAPERCLIP_DATABASE_URL" => Some("postgres://alias@host/db".into()),
            _ => None,
        };
        let c = Config::build_with(lookup).unwrap();
        assert_eq!(c.database.url, "postgres://alias@host/db");
    }
}