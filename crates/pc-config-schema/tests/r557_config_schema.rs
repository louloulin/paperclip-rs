//! R557 — pc-config-schema 综合测试。

#![allow(clippy::doc_markdown)]

use pc_config_schema::{
    parse_paperclip_config, validate_paperclip_config, AuthBaseUrlMode, AuthConfig, ConfigMeta,
    ConfigMetaSource, DatabaseBackupConfig, DatabaseConfig, DatabaseMode, LlmConfig, LlmProvider,
    LoggingConfig, LoggingMode, PaperclipConfig, PaperclipConfigError, SecretProvider,
    SecretsConfig, SecretsLocalEncryptedConfig, ServerConfig, StorageConfig,
    StorageLocalDiskConfig, StorageProvider, StorageS3Config, TelemetryConfig, UpdatesConfig,
    DEFAULT_BACKUP_DIR, DEFAULT_BACKUP_INTERVAL_MINUTES, DEFAULT_BACKUP_RETENTION_DAYS,
    DEFAULT_EMBEDDED_POSTGRES_DATA_DIR, DEFAULT_EMBEDDED_POSTGRES_PORT, DEFAULT_LOG_DIR,
    DEFAULT_S3_BUCKET, DEFAULT_S3_REGION, DEFAULT_SECRETS_KEY_FILE_PATH, DEFAULT_SERVER_HOST,
    DEFAULT_SERVER_PORT, DEFAULT_STORAGE_LOCAL_BASE_DIR,
};
use pc_network_bind::{DeploymentExposure, DeploymentMode};
use serde_json::json;

fn base_meta() -> ConfigMeta {
    ConfigMeta {
        version: 1,
        updated_at: "2026-05-10T00:00:00.000Z".into(),
        source: ConfigMetaSource::Configure,
    }
}

#[test]
fn r557_defaults_match_node() {
    let value = json!({
        "$meta": {
            "version": 1,
            "updatedAt": "2026-05-10T00:00:00.000Z",
            "source": "configure",
        },
        "database": { "mode": "embedded-postgres" },
        "logging": { "mode": "file" },
        "server": {},
    });
    let parsed = parse_paperclip_config(&value).unwrap();
    assert_eq!(
        parsed.database.embedded_postgres_data_dir,
        DEFAULT_EMBEDDED_POSTGRES_DATA_DIR
    );
    assert_eq!(parsed.database.backup.dir, DEFAULT_BACKUP_DIR);
    assert_eq!(parsed.logging.log_dir, DEFAULT_LOG_DIR);
    assert_eq!(
        parsed.storage.local_disk.base_dir,
        DEFAULT_STORAGE_LOCAL_BASE_DIR
    );
    assert_eq!(
        parsed.secrets.local_encrypted.key_file_path,
        DEFAULT_SECRETS_KEY_FILE_PATH
    );
}

#[test]
fn r557_database_backup_defaults() {
    let cfg = DatabaseBackupConfig::default();
    assert!(cfg.enabled);
    assert_eq!(cfg.interval_minutes, DEFAULT_BACKUP_INTERVAL_MINUTES);
    assert_eq!(cfg.retention_days, DEFAULT_BACKUP_RETENTION_DAYS);
    assert_eq!(cfg.dir, DEFAULT_BACKUP_DIR);
}

#[test]
fn r557_database_defaults() {
    let cfg = DatabaseConfig::default();
    assert!(matches!(cfg.mode, DatabaseMode::EmbeddedPostgres));
    assert!(cfg.connection_string.is_none());
    assert_eq!(
        cfg.embedded_postgres_data_dir,
        DEFAULT_EMBEDDED_POSTGRES_DATA_DIR
    );
    assert_eq!(cfg.embedded_postgres_port, DEFAULT_EMBEDDED_POSTGRES_PORT);
    assert!(cfg.backup.enabled);
}

#[test]
fn r557_logging_defaults() {
    let cfg = LoggingConfig::default();
    assert!(matches!(cfg.mode, LoggingMode::File));
    assert_eq!(cfg.log_dir, DEFAULT_LOG_DIR);
}

#[test]
fn r557_server_defaults() {
    let cfg = ServerConfig::default();
    assert!(matches!(cfg.deployment_mode, DeploymentMode::LocalTrusted));
    assert!(matches!(cfg.exposure, DeploymentExposure::Private));
    assert!(cfg.bind.is_none());
    assert!(cfg.custom_bind_host.is_none());
    assert_eq!(cfg.host, DEFAULT_SERVER_HOST);
    assert_eq!(cfg.port, DEFAULT_SERVER_PORT);
    assert!(cfg.allowed_hostnames.is_empty());
    assert!(cfg.serve_ui);
}

#[test]
fn r557_auth_defaults() {
    let cfg = AuthConfig::default();
    assert!(matches!(cfg.base_url_mode, AuthBaseUrlMode::Auto));
    assert!(cfg.public_base_url.is_none());
    assert!(!cfg.disable_sign_up);
}

#[test]
fn r557_storage_defaults() {
    let cfg = StorageConfig::default();
    assert!(matches!(cfg.provider, StorageProvider::LocalDisk));
    assert_eq!(cfg.local_disk.base_dir, DEFAULT_STORAGE_LOCAL_BASE_DIR);
    assert_eq!(cfg.s3.bucket, DEFAULT_S3_BUCKET);
    assert_eq!(cfg.s3.region, DEFAULT_S3_REGION);
    assert!(cfg.s3.prefix.is_empty());
    assert!(!cfg.s3.force_path_style);
}

#[test]
fn r557_secrets_defaults() {
    let cfg = SecretsConfig::default();
    assert!(matches!(cfg.provider, SecretProvider::LocalEncrypted));
    assert!(!cfg.strict_mode);
    assert_eq!(
        cfg.local_encrypted.key_file_path,
        DEFAULT_SECRETS_KEY_FILE_PATH
    );
}

#[test]
fn r557_telemetry_defaults() {
    let cfg = TelemetryConfig::default();
    assert!(cfg.enabled);
}

#[test]
fn r557_updates_defaults() {
    let cfg = UpdatesConfig::default();
    assert!(cfg.check_enabled);
}

#[test]
fn r557_round_trip_json() {
    let cfg = PaperclipConfig {
        meta: base_meta(),
        llm: Some(LlmConfig {
            provider: LlmProvider::Claude,
            api_key: Some("sk-test".into()),
        }),
        database: DatabaseConfig::default(),
        logging: LoggingConfig::default(),
        server: ServerConfig::default(),
        telemetry: TelemetryConfig::default(),
        updates: None,
        auth: AuthConfig::default(),
        storage: StorageConfig::default(),
        secrets: SecretsConfig::default(),
    };
    let json = serde_json::to_value(&cfg).unwrap();
    let parsed = parse_paperclip_config(&json).unwrap();
    assert_eq!(parsed, cfg);
}

#[test]
fn r557_meta_version_must_be_1() {
    let value = json!({
        "$meta": { "version": 2, "updatedAt": "2026-08-11T00:00:00Z", "source": "configure" },
        "database": { "mode": "embedded-postgres" },
        "logging": { "mode": "file" },
        "server": {},
    });
    let err = parse_paperclip_config(&value).unwrap_err();
    // version=2 parses as u32 successfully but fails semantic validation.
    match err {
        PaperclipConfigError::Semantic { path, .. } => {
            assert_eq!(path, "$meta.version");
        }
        PaperclipConfigError::Json(_) => panic!("expected Semantic error"),
    }
}

#[test]
fn r557_semantic_error_local_trusted_public_exposure() {
    let mut cfg = PaperclipConfig {
        meta: base_meta(),
        llm: None,
        database: DatabaseConfig::default(),
        logging: LoggingConfig::default(),
        server: ServerConfig {
            exposure: DeploymentExposure::Public,
            ..ServerConfig::default()
        },
        telemetry: TelemetryConfig::default(),
        updates: None,
        auth: AuthConfig::default(),
        storage: StorageConfig::default(),
        secrets: SecretsConfig::default(),
    };
    cfg.server.deployment_mode = DeploymentMode::LocalTrusted;
    let err = validate_paperclip_config(&cfg).unwrap_err();
    match err {
        PaperclipConfigError::Semantic { path, .. } => {
            assert_eq!(path, "server.exposure");
        }
        PaperclipConfigError::Json(_) => panic!("expected Semantic error"),
    }
}

#[test]
fn r557_semantic_error_explicit_base_url_required() {
    let cfg = PaperclipConfig {
        meta: base_meta(),
        llm: None,
        database: DatabaseConfig::default(),
        logging: LoggingConfig::default(),
        server: ServerConfig::default(),
        telemetry: TelemetryConfig::default(),
        updates: None,
        auth: AuthConfig {
            base_url_mode: AuthBaseUrlMode::Explicit,
            public_base_url: None,
            disable_sign_up: false,
        },
        storage: StorageConfig::default(),
        secrets: SecretsConfig::default(),
    };
    let err = validate_paperclip_config(&cfg).unwrap_err();
    match err {
        PaperclipConfigError::Semantic { path, message } => {
            assert_eq!(path, "auth.publicBaseUrl");
            assert!(message.contains("publicBaseUrl is required"));
        }
        PaperclipConfigError::Json(_) => panic!("expected Semantic error"),
    }
}

#[test]
fn r557_semantic_error_public_requires_explicit_base_url_mode() {
    let cfg = PaperclipConfig {
        meta: base_meta(),
        llm: None,
        database: DatabaseConfig::default(),
        logging: LoggingConfig::default(),
        server: ServerConfig {
            deployment_mode: DeploymentMode::Authenticated,
            exposure: DeploymentExposure::Public,
            ..ServerConfig::default()
        },
        telemetry: TelemetryConfig::default(),
        updates: None,
        auth: AuthConfig::default(),
        storage: StorageConfig::default(),
        secrets: SecretsConfig::default(),
    };
    let err = validate_paperclip_config(&cfg).unwrap_err();
    match err {
        PaperclipConfigError::Semantic { path, .. } => {
            assert_eq!(path, "auth.baseUrlMode");
        }
        PaperclipConfigError::Json(_) => panic!("expected Semantic error"),
    }
}

#[test]
fn r557_semantic_error_public_requires_public_base_url() {
    let cfg = PaperclipConfig {
        meta: base_meta(),
        llm: None,
        database: DatabaseConfig::default(),
        logging: LoggingConfig::default(),
        server: ServerConfig {
            deployment_mode: DeploymentMode::Authenticated,
            exposure: DeploymentExposure::Public,
            ..ServerConfig::default()
        },
        telemetry: TelemetryConfig::default(),
        updates: None,
        auth: AuthConfig {
            base_url_mode: AuthBaseUrlMode::Explicit,
            public_base_url: Some("https://example.com".into()),
            disable_sign_up: false,
        },
        storage: StorageConfig::default(),
        secrets: SecretsConfig::default(),
    };
    // Valid — should pass
    assert!(validate_paperclip_config(&cfg).is_ok());
}

#[test]
fn r557_semantic_error_bind_mode_local_trusted_requires_loopback() {
    let cfg = PaperclipConfig {
        meta: base_meta(),
        llm: None,
        database: DatabaseConfig::default(),
        logging: LoggingConfig::default(),
        server: ServerConfig {
            deployment_mode: DeploymentMode::LocalTrusted,
            bind: Some(pc_network_bind::BindMode::Lan),
            ..ServerConfig::default()
        },
        telemetry: TelemetryConfig::default(),
        updates: None,
        auth: AuthConfig::default(),
        storage: StorageConfig::default(),
        secrets: SecretsConfig::default(),
    };
    let err = validate_paperclip_config(&cfg).unwrap_err();
    match err {
        PaperclipConfigError::Semantic { path, message } => {
            assert_eq!(path, "server.bind");
            assert!(message.contains("loopback"));
        }
        PaperclipConfigError::Json(_) => panic!("expected Semantic error"),
    }
}

#[test]
fn r557_full_valid_config() {
    let value = json!({
        "$meta": {
            "version": 1,
            "updatedAt": "2026-05-10T00:00:00.000Z",
            "source": "configure",
        },
        "llm": { "provider": "claude", "apiKey": "sk-test" },
        "database": {
            "mode": "embedded-postgres",
            "embeddedPostgresPort": 54329,
            "backup": {
                "enabled": true,
                "intervalMinutes": 60,
                "retentionDays": 7,
            },
        },
        "logging": { "mode": "file", "logDir": "/var/log/paperclip" },
        "server": {
            "deploymentMode": "local_trusted",
            "exposure": "private",
            "host": "127.0.0.1",
            "port": 3100,
            "allowedHostnames": ["localhost"],
            "serveUi": true,
        },
        "telemetry": { "enabled": true },
        "updates": { "checkEnabled": true },
        "auth": {
            "baseUrlMode": "auto",
            "disableSignUp": false,
        },
        "storage": {
            "provider": "local_disk",
            "localDisk": { "baseDir": "/var/lib/paperclip/storage" },
            "s3": {
                "bucket": "my-bucket",
                "region": "us-east-1",
                "prefix": "paperclip/",
                "forcePathStyle": false,
            },
        },
        "secrets": {
            "provider": "local_encrypted",
            "strictMode": true,
            "localEncrypted": { "keyFilePath": "/var/lib/paperclip/master.key" },
        },
    });
    let cfg = parse_paperclip_config(&value).unwrap();
    assert_eq!(cfg.llm.as_ref().unwrap().provider, LlmProvider::Claude);
    assert_eq!(cfg.database.backup.interval_minutes, 60);
    assert_eq!(cfg.server.port, 3100);
    assert_eq!(
        cfg.storage.local_disk.base_dir,
        "/var/lib/paperclip/storage"
    );
    assert_eq!(cfg.storage.s3.bucket, "my-bucket");
    assert!(cfg.secrets.strict_mode);
}

#[test]
fn r557_meta_source_round_trip() {
    for src in [
        ConfigMetaSource::Onboard,
        ConfigMetaSource::Configure,
        ConfigMetaSource::Doctor,
    ] {
        let meta = ConfigMeta {
            version: 1,
            updated_at: "2026-08-11T00:00:00Z".into(),
            source: src,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: ConfigMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back, meta);
    }
}

#[test]
fn r557_storage_local_disk_default_serialization() {
    let cfg = StorageLocalDiskConfig::default();
    let json = serde_json::to_value(&cfg).unwrap();
    assert_eq!(json["baseDir"], DEFAULT_STORAGE_LOCAL_BASE_DIR);
}

#[test]
fn r557_storage_s3_default_serialization() {
    let cfg = StorageS3Config::default();
    let json = serde_json::to_value(&cfg).unwrap();
    assert_eq!(json["bucket"], DEFAULT_S3_BUCKET);
    assert_eq!(json["region"], DEFAULT_S3_REGION);
    assert_eq!(json["prefix"], "");
    assert_eq!(json["forcePathStyle"], false);
    assert!(json.get("endpoint").is_none() || json["endpoint"].is_null());
}

#[test]
fn r557_secrets_local_encrypted_default_serialization() {
    let cfg = SecretsLocalEncryptedConfig::default();
    let json = serde_json::to_value(&cfg).unwrap();
    assert_eq!(json["keyFilePath"], DEFAULT_SECRETS_KEY_FILE_PATH);
}
