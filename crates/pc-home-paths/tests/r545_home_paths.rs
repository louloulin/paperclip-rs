#![allow(clippy::doc_markdown)]
//! R545 — pc-home-paths 综合测试集。
//!
//! 覆盖：
//! 1. Env trait mock — 完全确定性的 env 注入
//! 2. expand_home_prefix / resolve_home_aware_path — ~/~/foo/绝对路径
//! 3. resolve_paperclip_home_dir — override / env / 默认 fallback
//! 4. resolve_paperclip_instance_id — override / env / default + 验证
//! 5. 单一 root + config + env + 子目录解析器
//! 6. resolve_paperclip_instance_paths 聚合器
//! 7. 错误：invalid instance id
//! 8. 常量稳定性

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pc_home_paths::{
    expand_home_prefix, resolve_default_backup_dir, resolve_default_embedded_postgres_dir,
    resolve_default_logs_dir, resolve_default_secrets_key_file_path, resolve_default_storage_dir,
    resolve_home_aware_path, resolve_paperclip_config_path_for_instance,
    resolve_paperclip_env_path_for_config, resolve_paperclip_home_dir,
    resolve_paperclip_instance_config_path, resolve_paperclip_instance_id,
    resolve_paperclip_instance_paths, resolve_paperclip_instance_root, Env, PaperclipInstanceInput,
    DEFAULT_PAPERCLIP_INSTANCE_ID, PAPERCLIP_CONFIG_BASENAME, PAPERCLIP_ENV_FILENAME,
};

// ============================================================================
// Mock Env
// ============================================================================

#[derive(Debug, Clone, Default)]
struct MockEnv {
    home: Option<PathBuf>,
    vars: HashMap<String, String>,
}

impl MockEnv {
    fn with_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: Some(home.into()),
            vars: HashMap::new(),
        }
    }
    fn with_var(mut self, name: &str, value: &str) -> Self {
        self.vars.insert(name.to_string(), value.to_string());
        self
    }
}

impl Env for MockEnv {
    fn home_dir(&self) -> Option<PathBuf> {
        self.home.clone()
    }
    fn var(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned()
    }
}

fn input<'a>(
    home_dir: Option<&'a str>,
    instance_id: Option<&'a str>,
) -> PaperclipInstanceInput<'a> {
    PaperclipInstanceInput {
        home_dir,
        instance_id,
    }
}

// ============================================================================
// expand_home_prefix / resolve_home_aware_path
// ============================================================================

#[test]
fn r545_expand_home_prefix_bare_tilde() {
    let env = MockEnv::with_home("/home/alice");
    assert_eq!(expand_home_prefix(&env, "~"), PathBuf::from("/home/alice"));
}

#[test]
fn r545_expand_home_prefix_tilde_slash() {
    let env = MockEnv::with_home("/home/alice");
    assert_eq!(
        expand_home_prefix(&env, "~/projects"),
        PathBuf::from("/home/alice/projects")
    );
    assert_eq!(
        expand_home_prefix(&env, "~/deep/nested/dir"),
        PathBuf::from("/home/alice/deep/nested/dir")
    );
}

#[test]
fn r545_expand_home_prefix_absolute_passthrough() {
    let env = MockEnv::with_home("/home/alice");
    assert_eq!(
        expand_home_prefix(&env, "/etc/hosts"),
        PathBuf::from("/etc/hosts")
    );
}

#[test]
fn r545_expand_home_prefix_relative_passthrough() {
    let env = MockEnv::with_home("/home/alice");
    assert_eq!(
        expand_home_prefix(&env, "local/path"),
        PathBuf::from("local/path")
    );
}

#[test]
fn r545_expand_home_prefix_tilde_in_middle_is_literal() {
    // "~something" without the slash is NOT a home expansion.
    let env = MockEnv::with_home("/home/alice");
    assert_eq!(
        expand_home_prefix(&env, "~something"),
        PathBuf::from("~something")
    );
}

#[test]
fn r545_expand_home_prefix_missing_home_falls_back_to_dot() {
    let env = MockEnv::default();
    // "~" → "." (current dir fallback)
    assert_eq!(expand_home_prefix(&env, "~"), PathBuf::from("."));
    // "~/x" → "./x" (PathBuf::join keeps the "." prefix)
    assert_eq!(expand_home_prefix(&env, "~/x"), PathBuf::from("./x"));
}

#[test]
fn r545_resolve_home_aware_path_matches_expand() {
    let env = MockEnv::with_home("/home/alice");
    assert_eq!(
        resolve_home_aware_path(&env, "~/cfg"),
        expand_home_prefix(&env, "~/cfg")
    );
}

// ============================================================================
// resolve_paperclip_home_dir
// ============================================================================

#[test]
fn r545_resolve_home_dir_override_wins() {
    let env = MockEnv::with_home("/home/alice").with_var("PAPERCLIP_HOME", "/svc/paperclip");
    assert_eq!(
        resolve_paperclip_home_dir(&env, Some("/explicit/home")),
        PathBuf::from("/explicit/home")
    );
}

#[test]
fn r545_resolve_home_dir_env_wins_over_default() {
    let env = MockEnv::with_home("/home/alice").with_var("PAPERCLIP_HOME", "/svc/paperclip");
    assert_eq!(
        resolve_paperclip_home_dir(&env, None),
        PathBuf::from("/svc/paperclip")
    );
}

#[test]
fn r545_resolve_home_dir_trims_override_whitespace() {
    let env = MockEnv::with_home("/home/alice");
    assert_eq!(
        resolve_paperclip_home_dir(&env, Some("   /trimmed   ")),
        PathBuf::from("/trimmed")
    );
}

#[test]
fn r545_resolve_home_dir_empty_override_falls_through_to_env() {
    let env = MockEnv::with_home("/home/alice").with_var("PAPERCLIP_HOME", "/svc/paperclip");
    assert_eq!(
        resolve_paperclip_home_dir(&env, Some("   ")),
        PathBuf::from("/svc/paperclip")
    );
}

#[test]
fn r545_resolve_home_dir_trims_env_whitespace() {
    let env = MockEnv::with_home("/home/alice").with_var("PAPERCLIP_HOME", "   /svc/paperclip   ");
    assert_eq!(
        resolve_paperclip_home_dir(&env, None),
        PathBuf::from("/svc/paperclip")
    );
}

#[test]
fn r545_resolve_home_dir_default_falls_back_to_dot_paperclip() {
    let env = MockEnv::with_home("/home/alice");
    assert_eq!(
        resolve_paperclip_home_dir(&env, None),
        PathBuf::from("/home/alice/.paperclip")
    );
}

#[test]
fn r545_resolve_home_dir_expands_tilde_in_override() {
    let env = MockEnv::with_home("/home/alice");
    assert_eq!(
        resolve_paperclip_home_dir(&env, Some("~/svc")),
        PathBuf::from("/home/alice/svc")
    );
}

// ============================================================================
// resolve_paperclip_instance_id
// ============================================================================

#[test]
fn r545_resolve_instance_id_override_wins() {
    let env = MockEnv::default().with_var("PAPERCLIP_INSTANCE_ID", "env-instance");
    assert_eq!(
        resolve_paperclip_instance_id(&env, Some("override-instance")).unwrap(),
        "override-instance"
    );
}

#[test]
fn r545_resolve_instance_id_env_wins_over_default() {
    let env = MockEnv::default().with_var("PAPERCLIP_INSTANCE_ID", "env-instance");
    assert_eq!(
        resolve_paperclip_instance_id(&env, None).unwrap(),
        "env-instance"
    );
}

#[test]
fn r545_resolve_instance_id_default_when_unset() {
    let env = MockEnv::default();
    assert_eq!(
        resolve_paperclip_instance_id(&env, None).unwrap(),
        DEFAULT_PAPERCLIP_INSTANCE_ID
    );
}

#[test]
fn r545_resolve_instance_id_rejects_invalid_chars() {
    let env = MockEnv::default();
    let err = resolve_paperclip_instance_id(&env, Some("has/slash")).unwrap_err();
    assert!(matches!(
        err,
        pc_home_paths::HomePathError::InvalidInstanceId(_)
    ));
}

#[test]
fn r545_resolve_instance_id_rejects_empty_after_trim() {
    let env = MockEnv::default();
    let result = resolve_paperclip_instance_id(&env, Some("   ")).unwrap();
    // Empty override + empty env → falls through to DEFAULT.
    assert_eq!(result, DEFAULT_PAPERCLIP_INSTANCE_ID);
}

#[test]
fn r545_resolve_instance_id_rejects_path_traversal() {
    let env = MockEnv::default();
    assert!(resolve_paperclip_instance_id(&env, Some("../escape")).is_err());
    assert!(resolve_paperclip_instance_id(&env, Some("a b")).is_err());
}

#[test]
fn r545_resolve_instance_id_trims_env_value() {
    let env = MockEnv::default().with_var("PAPERCLIP_INSTANCE_ID", "  trimmed-id  ");
    assert_eq!(
        resolve_paperclip_instance_id(&env, None).unwrap(),
        "trimmed-id"
    );
}

// ============================================================================
// root + config + env
// ============================================================================

#[test]
fn r545_resolve_instance_root_uses_home_and_id() {
    let env = MockEnv::with_home("/home/alice");
    let root = resolve_paperclip_instance_root(&env, input(None, Some("dev"))).unwrap();
    assert_eq!(root, PathBuf::from("/home/alice/.paperclip/instances/dev"));
}

#[test]
fn r545_resolve_instance_root_default_id() {
    let env = MockEnv::with_home("/home/alice");
    let root = resolve_paperclip_instance_root(&env, input(None, None)).unwrap();
    assert_eq!(
        root,
        PathBuf::from("/home/alice/.paperclip/instances/default")
    );
}

#[test]
fn r545_resolve_instance_root_honours_paperclip_home_env() {
    let env = MockEnv::with_home("/home/alice").with_var("PAPERCLIP_HOME", "/svc/paperclip");
    let root = resolve_paperclip_instance_root(&env, input(None, Some("prod"))).unwrap();
    assert_eq!(root, PathBuf::from("/svc/paperclip/instances/prod"));
}

#[test]
fn r545_resolve_config_path_is_under_root() {
    let env = MockEnv::with_home("/home/alice");
    let config = resolve_paperclip_instance_config_path(&env, input(None, Some("dev"))).unwrap();
    assert_eq!(
        config,
        PathBuf::from("/home/alice/.paperclip/instances/dev/config.json")
    );
    assert_eq!(config.file_name().unwrap(), PAPERCLIP_CONFIG_BASENAME);
}

#[test]
fn r545_resolve_config_path_for_instance_alias_matches() {
    let env = MockEnv::with_home("/home/alice");
    let a = resolve_paperclip_instance_config_path(&env, input(None, Some("dev"))).unwrap();
    let b = resolve_paperclip_config_path_for_instance(&env, input(None, Some("dev"))).unwrap();
    assert_eq!(a, b);
}

#[test]
fn r545_resolve_env_path_is_next_to_config() {
    let config = PathBuf::from("/home/alice/.paperclip/instances/dev/config.json");
    let env_path = resolve_paperclip_env_path_for_config(&config);
    assert_eq!(
        env_path,
        PathBuf::from("/home/alice/.paperclip/instances/dev/.env")
    );
    assert_eq!(env_path.file_name().unwrap(), PAPERCLIP_ENV_FILENAME);
}

#[test]
fn r545_resolve_env_path_handles_relative_config() {
    let env_path = resolve_paperclip_env_path_for_config(Path::new("config.json"));
    assert_eq!(env_path, PathBuf::from(".env"));
}

// ============================================================================
// Sub-directory resolvers
// ============================================================================

#[test]
fn r545_resolve_subdirectory_paths_match_expected_layout() {
    let env = MockEnv::with_home("/home/alice");
    let inp = input(None, Some("dev"));
    let root = "/home/alice/.paperclip/instances/dev".to_string();
    assert_eq!(
        resolve_default_embedded_postgres_dir(&env, inp).unwrap(),
        PathBuf::from(format!("{root}/db"))
    );
    assert_eq!(
        resolve_default_logs_dir(&env, inp).unwrap(),
        PathBuf::from(format!("{root}/logs"))
    );
    assert_eq!(
        resolve_default_secrets_key_file_path(&env, inp).unwrap(),
        PathBuf::from(format!("{root}/secrets/master.key"))
    );
    assert_eq!(
        resolve_default_storage_dir(&env, inp).unwrap(),
        PathBuf::from(format!("{root}/data/storage"))
    );
    assert_eq!(
        resolve_default_backup_dir(&env, inp).unwrap(),
        PathBuf::from(format!("{root}/data/backups"))
    );
}

// ============================================================================
// Aggregate resolver
// ============================================================================

#[test]
fn r545_aggregate_resolver_returns_full_layout() {
    let env = MockEnv::with_home("/home/alice");
    let p = resolve_paperclip_instance_paths(&env, input(None, Some("dev"))).unwrap();
    assert_eq!(p.home, PathBuf::from("/home/alice/.paperclip"));
    assert_eq!(p.instance_id, "dev");
    assert_eq!(
        p.root,
        PathBuf::from("/home/alice/.paperclip/instances/dev")
    );
    assert_eq!(
        p.config_path,
        PathBuf::from("/home/alice/.paperclip/instances/dev/config.json")
    );
    assert_eq!(
        p.env_path,
        PathBuf::from("/home/alice/.paperclip/instances/dev/.env")
    );
    assert_eq!(
        p.embedded_postgres_dir,
        PathBuf::from("/home/alice/.paperclip/instances/dev/db")
    );
    assert_eq!(
        p.logs_dir,
        PathBuf::from("/home/alice/.paperclip/instances/dev/logs")
    );
    assert_eq!(
        p.secrets_key_file,
        PathBuf::from("/home/alice/.paperclip/instances/dev/secrets/master.key")
    );
    assert_eq!(
        p.storage_dir,
        PathBuf::from("/home/alice/.paperclip/instances/dev/data/storage")
    );
    assert_eq!(
        p.backup_dir,
        PathBuf::from("/home/alice/.paperclip/instances/dev/data/backups")
    );
}

#[test]
fn r545_aggregate_resolver_uses_paperclip_home_env() {
    let env = MockEnv::with_home("/home/alice").with_var("PAPERCLIP_HOME", "/srv/pc");
    let p = resolve_paperclip_instance_paths(&env, input(None, Some("prod"))).unwrap();
    assert_eq!(p.home, PathBuf::from("/srv/pc"));
    assert_eq!(p.instance_id, "prod");
}

#[test]
fn r545_aggregate_resolver_default_instance_id() {
    let env = MockEnv::with_home("/home/alice");
    let p = resolve_paperclip_instance_paths(&env, input(None, None)).unwrap();
    assert_eq!(p.instance_id, DEFAULT_PAPERCLIP_INSTANCE_ID);
    assert_eq!(
        p.root,
        PathBuf::from("/home/alice/.paperclip/instances/default")
    );
}

#[test]
fn r545_aggregate_resolver_propagates_invalid_id_error() {
    let env = MockEnv::default();
    let err = resolve_paperclip_instance_paths(&env, input(None, Some("../escape"))).unwrap_err();
    assert!(matches!(
        err,
        pc_home_paths::HomePathError::InvalidInstanceId(_)
    ));
}

// ============================================================================
// Constants
// ============================================================================

#[test]
fn r545_constants_match_upstream() {
    assert_eq!(DEFAULT_PAPERCLIP_INSTANCE_ID, "default");
    assert_eq!(PAPERCLIP_CONFIG_BASENAME, "config.json");
    assert_eq!(PAPERCLIP_ENV_FILENAME, ".env");
}
