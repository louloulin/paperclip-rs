#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Paperclip on-disk config.json schema.
//!
//! R557: Direct port of `paperclip/packages/shared/src/config-schema.ts` (205 LOC).
//!
//! Models the persisted server config schema (written by `onboard` /
//! `configure` / `doctor` to `~/.paperclip/instances/default/config.json`).
//! Distinct from `pc-config`, which is the runtime env-based `Config` struct
//! used by the server process.
//!
//! Field names mirror the JSON contract 1:1 with Node (camelCase), so a JSON
//! file produced by the Node code parses identically into the Rust structs.

use pc_feature_catalog::{self, FeatureTier};
use pc_network_bind::{
    validate_configured_bind_mode, BindMode, DeploymentExposure, DeploymentMode,
    ValidateConfiguredBindModeInput,
};
use serde::{Deserialize, Serialize};

// ---------- enums ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigMetaSource {
    Onboard,
    Configure,
    Doctor,
}

impl ConfigMetaSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Onboard => "onboard",
            Self::Configure => "configure",
            Self::Doctor => "doctor",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    Claude,
    Openai,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DatabaseMode {
    EmbeddedPostgres,
    Postgres,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoggingMode {
    File,
    Cloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthBaseUrlMode {
    Auto,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageProvider {
    LocalDisk,
    S3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretProvider {
    LocalEncrypted,
    AwsSecretsManager,
}

// ---------- $meta ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigMeta {
    pub version: u32, // zod literal 1
    pub updated_at: String,
    pub source: ConfigMetaSource,
}

// ---------- llm ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmConfig {
    pub provider: LlmProvider,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

// ---------- database ----------

pub const DEFAULT_EMBEDDED_POSTGRES_DATA_DIR: &str = "~/.paperclip/instances/default/db";
pub const DEFAULT_EMBEDDED_POSTGRES_PORT: u16 = 54329;
pub const DEFAULT_BACKUP_DIR: &str = "~/.paperclip/instances/default/data/backups";
pub const DEFAULT_BACKUP_INTERVAL_MINUTES: u32 = 60;
pub const DEFAULT_BACKUP_RETENTION_DAYS: u32 = 7;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseBackupConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_backup_interval_minutes")]
    pub interval_minutes: u32,
    #[serde(default = "default_backup_retention_days")]
    pub retention_days: u32,
    #[serde(default = "default_backup_dir")]
    pub dir: String,
}

impl Default for DatabaseBackupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_minutes: DEFAULT_BACKUP_INTERVAL_MINUTES,
            retention_days: DEFAULT_BACKUP_RETENTION_DAYS,
            dir: DEFAULT_BACKUP_DIR.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseConfig {
    #[serde(default = "default_database_mode")]
    pub mode: DatabaseMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_string: Option<String>,
    #[serde(default = "default_embedded_postgres_data_dir")]
    pub embedded_postgres_data_dir: String,
    #[serde(default = "default_embedded_postgres_port")]
    pub embedded_postgres_port: u16,
    #[serde(default)]
    pub backup: DatabaseBackupConfig,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            mode: DatabaseMode::EmbeddedPostgres,
            connection_string: None,
            embedded_postgres_data_dir: DEFAULT_EMBEDDED_POSTGRES_DATA_DIR.to_string(),
            embedded_postgres_port: DEFAULT_EMBEDDED_POSTGRES_PORT,
            backup: DatabaseBackupConfig::default(),
        }
    }
}

// ---------- logging ----------

pub const DEFAULT_LOG_DIR: &str = "~/.paperclip/instances/default/logs";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingConfig {
    pub mode: LoggingMode,
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            mode: LoggingMode::File,
            log_dir: DEFAULT_LOG_DIR.to_string(),
        }
    }
}

// ---------- server ----------

pub const DEFAULT_SERVER_HOST: &str = "127.0.0.1";
pub const DEFAULT_SERVER_PORT: u16 = 3100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default = "default_deployment_mode")]
    pub deployment_mode: DeploymentMode,
    #[serde(default = "default_deployment_exposure")]
    pub exposure: DeploymentExposure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind: Option<BindMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_bind_host: Option<String>,
    #[serde(default = "default_server_host")]
    pub host: String,
    #[serde(default = "default_server_port")]
    pub port: u16,
    #[serde(default)]
    pub allowed_hostnames: Vec<String>,
    #[serde(default = "default_true")]
    pub serve_ui: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            deployment_mode: DeploymentMode::LocalTrusted,
            exposure: DeploymentExposure::Private,
            bind: None,
            custom_bind_host: None,
            host: DEFAULT_SERVER_HOST.to_string(),
            port: DEFAULT_SERVER_PORT,
            allowed_hostnames: Vec::new(),
            serve_ui: true,
        }
    }
}

// ---------- auth ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfig {
    #[serde(default = "default_auth_base_url_mode")]
    pub base_url_mode: AuthBaseUrlMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
    #[serde(default)]
    pub disable_sign_up: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            base_url_mode: AuthBaseUrlMode::Auto,
            public_base_url: None,
            disable_sign_up: false,
        }
    }
}

// ---------- storage ----------

pub const DEFAULT_STORAGE_LOCAL_BASE_DIR: &str = "~/.paperclip/instances/default/data/storage";
pub const DEFAULT_S3_BUCKET: &str = "paperclip";
pub const DEFAULT_S3_REGION: &str = "us-east-1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocalDiskConfig {
    #[serde(default = "default_storage_local_base_dir")]
    pub base_dir: String,
}

impl Default for StorageLocalDiskConfig {
    fn default() -> Self {
        Self {
            base_dir: DEFAULT_STORAGE_LOCAL_BASE_DIR.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageS3Config {
    #[serde(default = "default_s3_bucket")]
    pub bucket: String,
    #[serde(default = "default_s3_region")]
    pub region: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub force_path_style: bool,
}

impl Default for StorageS3Config {
    fn default() -> Self {
        Self {
            bucket: DEFAULT_S3_BUCKET.to_string(),
            region: DEFAULT_S3_REGION.to_string(),
            endpoint: None,
            prefix: String::new(),
            force_path_style: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageConfig {
    #[serde(default = "default_storage_provider")]
    pub provider: StorageProvider,
    #[serde(default)]
    pub local_disk: StorageLocalDiskConfig,
    #[serde(default)]
    pub s3: StorageS3Config,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            provider: StorageProvider::LocalDisk,
            local_disk: StorageLocalDiskConfig::default(),
            s3: StorageS3Config::default(),
        }
    }
}

// ---------- secrets ----------

pub const DEFAULT_SECRETS_KEY_FILE_PATH: &str = "~/.paperclip/instances/default/secrets/master.key";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsLocalEncryptedConfig {
    #[serde(default = "default_secrets_key_file_path")]
    pub key_file_path: String,
}

impl Default for SecretsLocalEncryptedConfig {
    fn default() -> Self {
        Self {
            key_file_path: DEFAULT_SECRETS_KEY_FILE_PATH.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsConfig {
    #[serde(default = "default_secret_provider")]
    pub provider: SecretProvider,
    #[serde(default)]
    pub strict_mode: bool,
    #[serde(default)]
    pub local_encrypted: SecretsLocalEncryptedConfig,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            provider: SecretProvider::LocalEncrypted,
            strict_mode: false,
            local_encrypted: SecretsLocalEncryptedConfig::default(),
        }
    }
}

// ---------- telemetry + updates ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatesConfig {
    #[serde(default = "default_true")]
    pub check_enabled: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self {
            check_enabled: true,
        }
    }
}

// ---------- top-level paperclipConfig ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperclipConfig {
    #[serde(rename = "$meta")]
    pub meta: ConfigMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmConfig>,
    pub database: DatabaseConfig,
    pub logging: LoggingConfig,
    pub server: ServerConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updates: Option<UpdatesConfig>,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub secrets: SecretsConfig,
}

// ---------- defaults ----------

fn default_true() -> bool {
    true
}
fn default_database_mode() -> DatabaseMode {
    DatabaseMode::EmbeddedPostgres
}
fn default_embedded_postgres_data_dir() -> String {
    DEFAULT_EMBEDDED_POSTGRES_DATA_DIR.to_string()
}
fn default_embedded_postgres_port() -> u16 {
    DEFAULT_EMBEDDED_POSTGRES_PORT
}
fn default_backup_interval_minutes() -> u32 {
    DEFAULT_BACKUP_INTERVAL_MINUTES
}
fn default_backup_retention_days() -> u32 {
    DEFAULT_BACKUP_RETENTION_DAYS
}
fn default_backup_dir() -> String {
    DEFAULT_BACKUP_DIR.to_string()
}
fn default_log_dir() -> String {
    DEFAULT_LOG_DIR.to_string()
}
fn default_deployment_mode() -> DeploymentMode {
    DeploymentMode::LocalTrusted
}
fn default_deployment_exposure() -> DeploymentExposure {
    DeploymentExposure::Private
}
fn default_server_host() -> String {
    DEFAULT_SERVER_HOST.to_string()
}
fn default_server_port() -> u16 {
    DEFAULT_SERVER_PORT
}
fn default_auth_base_url_mode() -> AuthBaseUrlMode {
    AuthBaseUrlMode::Auto
}
fn default_storage_local_base_dir() -> String {
    DEFAULT_STORAGE_LOCAL_BASE_DIR.to_string()
}
fn default_s3_bucket() -> String {
    DEFAULT_S3_BUCKET.to_string()
}
fn default_s3_region() -> String {
    DEFAULT_S3_REGION.to_string()
}
fn default_storage_provider() -> StorageProvider {
    StorageProvider::LocalDisk
}
fn default_secret_provider() -> SecretProvider {
    SecretProvider::LocalEncrypted
}
fn default_secrets_key_file_path() -> String {
    DEFAULT_SECRETS_KEY_FILE_PATH.to_string()
}

// ---------- validation ----------

/// Error returned by `parse_paperclip_config`. Combines serde errors and
/// semantic cross-field validation errors.
#[derive(Debug, thiserror::Error)]
pub enum PaperclipConfigError {
    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("semantic error: {message} (path: {path})")]
    Semantic { message: String, path: String },
}

/// Parse a JSON value into `PaperclipConfig`, then run the cross-field
/// semantic checks (zod `superRefine` mirror).
pub fn parse_paperclip_config(
    value: &serde_json::Value,
) -> Result<PaperclipConfig, PaperclipConfigError> {
    let config: PaperclipConfig = serde_json::from_value(value.clone())?;
    validate_paperclip_config(&config)?;
    Ok(config)
}

/// Run semantic checks (cross-field). Returns the first failing issue.
pub fn validate_paperclip_config(config: &PaperclipConfig) -> Result<(), PaperclipConfigError> {
    if config.meta.version != 1 {
        return Err(PaperclipConfigError::Semantic {
            message: format!("$meta.version must be 1, got {}", config.meta.version),
            path: "$meta.version".into(),
        });
    }

    if config.server.deployment_mode == DeploymentMode::LocalTrusted
        && config.server.exposure != DeploymentExposure::Private
    {
        return Err(PaperclipConfigError::Semantic {
            message: "server.exposure must be private when deploymentMode is local_trusted".into(),
            path: "server.exposure".into(),
        });
    }

    // Bind mode validation (delegated to pc-network-bind).
    let bind_errors = validate_configured_bind_mode(&ValidateConfiguredBindModeInput {
        deployment_mode: config.server.deployment_mode,
        deployment_exposure: config.server.exposure,
        bind: config.server.bind,
        host: Some(config.server.host.as_str()),
        custom_bind_host: config.server.custom_bind_host.as_deref(),
    });
    if let Some(message) = bind_errors.into_iter().next() {
        let path = if message.contains("customBindHost") {
            "server.customBindHost"
        } else {
            "server.bind"
        };
        return Err(PaperclipConfigError::Semantic {
            message,
            path: path.into(),
        });
    }

    if config.auth.base_url_mode == AuthBaseUrlMode::Explicit
        && config.auth.public_base_url.is_none()
    {
        return Err(PaperclipConfigError::Semantic {
            message: "auth.publicBaseUrl is required when auth.baseUrlMode is explicit".into(),
            path: "auth.publicBaseUrl".into(),
        });
    }

    if config.server.exposure == DeploymentExposure::Public
        && config.auth.base_url_mode != AuthBaseUrlMode::Explicit
    {
        return Err(PaperclipConfigError::Semantic {
            message:
                "auth.baseUrlMode must be explicit when deploymentMode=authenticated and exposure=public"
                    .into(),
            path: "auth.baseUrlMode".into(),
        });
    }

    if config.server.exposure == DeploymentExposure::Public && config.auth.public_base_url.is_none()
    {
        return Err(PaperclipConfigError::Semantic {
            message: "auth.publicBaseUrl is required when deploymentMode=authenticated and exposure=public".into(),
            path: "auth.publicBaseUrl".into(),
        });
    }

    Ok(())
}

// ============================================================================
// Feature catalog integration (R-INTEGRATION-1)
//
// Delegates to pc-feature-catalog so any config-level feature flag reference
// (current or future) can be validated against the static catalog of known
// flags. Mirrors the pc-network-bind delegation pattern used in
// validate_paperclip_config.
//
// This module is pure delegation — no business logic lives here. The catalog
// itself (titles, descriptions, tiers, defaults) is owned by pc-feature-catalog.
// ============================================================================

/// Error returned when an unknown feature key is supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFeatureKeyError {
    pub key: String,
    pub known_keys: Vec<&'static str>,
}

impl std::fmt::Display for UnknownFeatureKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown feature key: {}", self.key)
    }
}

impl std::error::Error for UnknownFeatureKeyError {}

/// Validate a feature key against the static catalog.
///
/// Returns `Ok(())` if the key is recognized, otherwise an error listing
/// what was supplied and (for diagnostic purposes) the full sorted list of
/// known keys.
pub fn validate_feature_key(key: &str) -> Result<(), UnknownFeatureKeyError> {
    if pc_feature_catalog::lookup_feature(key).is_some() {
        return Ok(());
    }
    Err(UnknownFeatureKeyError {
        key: key.to_string(),
        known_keys: pc_feature_catalog::instance_feature_keys(),
    })
}

/// Sorted list of all known feature keys (delegated).
pub fn known_feature_keys() -> Vec<&'static str> {
    pc_feature_catalog::instance_feature_keys()
}

/// Lookup the feature tier for a known key (delegated).
///
/// Returns `None` if the key is unknown — callers that want strict
/// validation should call [`validate_feature_key`] first.
pub fn feature_tier(key: &str) -> Option<FeatureTier> {
    pc_feature_catalog::lookup_feature(key).map(|e| e.tier)
}

/// True if the catalog has at least one flag of the given tier.
pub fn has_any_feature_of_tier(tier: FeatureTier) -> bool {
    pc_feature_catalog::instance_feature_keys()
        .into_iter()
        .any(|k| pc_feature_catalog::lookup_feature(k).is_some_and(|e| e.tier == tier))
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn defaults_match_node() {
        let cfg = PaperclipConfig {
            meta: ConfigMeta {
                version: 1,
                updated_at: "2026-08-11T00:00:00Z".into(),
                source: ConfigMetaSource::Configure,
            },
            llm: None,
            database: DatabaseConfig::default(),
            logging: LoggingConfig::default(),
            server: ServerConfig::default(),
            telemetry: TelemetryConfig::default(),
            updates: None,
            auth: AuthConfig::default(),
            storage: StorageConfig::default(),
            secrets: SecretsConfig::default(),
        };
        assert_eq!(
            cfg.database.embedded_postgres_data_dir,
            DEFAULT_EMBEDDED_POSTGRES_DATA_DIR
        );
        assert_eq!(cfg.database.embedded_postgres_port, 54329);
        assert_eq!(cfg.database.backup.dir, DEFAULT_BACKUP_DIR);
        assert_eq!(cfg.logging.log_dir, DEFAULT_LOG_DIR);
        assert_eq!(
            cfg.storage.local_disk.base_dir,
            DEFAULT_STORAGE_LOCAL_BASE_DIR
        );
        assert_eq!(
            cfg.secrets.local_encrypted.key_file_path,
            DEFAULT_SECRETS_KEY_FILE_PATH
        );
    }
}
