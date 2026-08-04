//! Paperclip 实例目录路径解析。
//!
//! 对齐 Node `packages/shared/src/home-paths.ts`，统一处理 `PAPERCLIP_HOME`、
//! `PAPERCLIP_INSTANCE_ID` 与实例内各运行时目录。

use std::env;
use std::path::{Component, Path, PathBuf};

pub const DEFAULT_PAPERCLIP_INSTANCE_ID: &str = "default";
pub const PAPERCLIP_CONFIG_BASENAME: &str = "config.json";
pub const PAPERCLIP_ENV_FILENAME: &str = ".env";

#[derive(Debug, thiserror::Error)]
pub enum HomePathError {
    #[error("home directory is unavailable")]
    HomeDirectoryUnavailable,
    #[error("cannot resolve current directory: {0}")]
    CurrentDirectory(#[source] std::io::Error),
    #[error("Invalid PAPERCLIP_INSTANCE_ID '{0}'.")]
    InvalidInstanceId(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperclipHomePaths {
    home_dir: PathBuf,
    instance_id: String,
}

impl PaperclipHomePaths {
    pub fn from_env() -> Result<Self, HomePathError> {
        Self::resolve(None, None)
    }

    pub fn resolve(
        home_override: Option<&str>,
        instance_id_override: Option<&str>,
    ) -> Result<Self, HomePathError> {
        let environment_home = env::var("PAPERCLIP_HOME").ok();
        let environment_instance_id = env::var("PAPERCLIP_INSTANCE_ID").ok();
        let system_home = dirs::home_dir().ok_or(HomePathError::HomeDirectoryUnavailable)?;
        let current_dir = env::current_dir().map_err(HomePathError::CurrentDirectory)?;

        Self::build_with(
            home_override,
            instance_id_override,
            environment_home.as_deref(),
            environment_instance_id.as_deref(),
            &system_home,
            &current_dir,
        )
    }

    pub fn build_with(
        home_override: Option<&str>,
        instance_id_override: Option<&str>,
        environment_home: Option<&str>,
        environment_instance_id: Option<&str>,
        system_home: &Path,
        current_dir: &Path,
    ) -> Result<Self, HomePathError> {
        let home_value = non_empty(home_override).or_else(|| non_empty(environment_home));
        let home_dir = match home_value {
            Some(value) => resolve_path(&expand_home_prefix_with(value, system_home), current_dir),
            None => resolve_path(&system_home.join(".paperclip"), current_dir),
        };
        let instance_id = non_empty(instance_id_override)
            .or_else(|| non_empty(environment_instance_id))
            .unwrap_or(DEFAULT_PAPERCLIP_INSTANCE_ID);

        if !is_valid_path_segment(instance_id) {
            return Err(HomePathError::InvalidInstanceId(instance_id.to_string()));
        }

        Ok(Self {
            home_dir,
            instance_id: instance_id.to_string(),
        })
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn instance_root(&self) -> PathBuf {
        self.home_dir.join("instances").join(&self.instance_id)
    }

    pub fn config_path(&self) -> PathBuf {
        self.instance_root().join(PAPERCLIP_CONFIG_BASENAME)
    }

    pub fn embedded_postgres_dir(&self) -> PathBuf {
        self.instance_root().join("db")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.instance_root().join("logs")
    }

    pub fn secrets_dir(&self) -> PathBuf {
        self.instance_root().join("secrets")
    }

    pub fn secrets_key_file_path(&self) -> PathBuf {
        self.secrets_dir().join("master.key")
    }

    pub fn storage_dir(&self) -> PathBuf {
        self.instance_root().join("data").join("storage")
    }

    pub fn backup_dir(&self) -> PathBuf {
        self.instance_root().join("data").join("backups")
    }
}

pub fn resolve_env_path_for_config(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(PAPERCLIP_ENV_FILENAME)
}

pub fn expand_home_prefix(value: &str) -> Result<PathBuf, HomePathError> {
    let system_home = dirs::home_dir().ok_or(HomePathError::HomeDirectoryUnavailable)?;
    Ok(expand_home_prefix_with(value, &system_home))
}

pub fn resolve_home_aware_path(value: &str) -> Result<PathBuf, HomePathError> {
    let current_dir = env::current_dir().map_err(HomePathError::CurrentDirectory)?;
    Ok(resolve_path(&expand_home_prefix(value)?, &current_dir))
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn expand_home_prefix_with(value: &str, system_home: &Path) -> PathBuf {
    if value == "~" {
        return system_home.to_path_buf();
    }
    if let Some(relative) = value.strip_prefix("~/") {
        return system_home.join(relative);
    }
    PathBuf::from(value)
}

fn resolve_path(path: &Path, current_dir: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir.join(path)
    };
    clean_path(&absolute)
}

fn clean_path(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => cleaned.push(prefix.as_os_str()),
            Component::RootDir => cleaned.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::Normal(segment) => cleaned.push(segment),
        }
    }
    cleaned
}

fn is_valid_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(
        home_override: Option<&str>,
        instance_override: Option<&str>,
        environment_home: Option<&str>,
        environment_instance: Option<&str>,
    ) -> Result<PaperclipHomePaths, HomePathError> {
        PaperclipHomePaths::build_with(
            home_override,
            instance_override,
            environment_home,
            environment_instance,
            Path::new("/Users/tester"),
            Path::new("/workspace/app"),
        )
    }

    #[test]
    fn defaults_to_dot_paperclip_and_default_instance() {
        let resolved = paths(None, None, None, None).unwrap();
        assert_eq!(resolved.home_dir(), Path::new("/Users/tester/.paperclip"));
        assert_eq!(resolved.instance_id(), "default");
        assert_eq!(
            resolved.instance_root(),
            PathBuf::from("/Users/tester/.paperclip/instances/default")
        );
    }

    #[test]
    fn explicit_overrides_win_over_environment() {
        let resolved = paths(
            Some("/explicit/home"),
            Some("explicit-instance"),
            Some("/environment/home"),
            Some("environment-instance"),
        )
        .unwrap();
        assert_eq!(resolved.home_dir(), Path::new("/explicit/home"));
        assert_eq!(resolved.instance_id(), "explicit-instance");
    }

    #[test]
    fn blank_overrides_fall_back_to_trimmed_environment() {
        let resolved = paths(
            Some("  "),
            Some(""),
            Some("  /environment/home  "),
            Some(" env_instance "),
        )
        .unwrap();
        assert_eq!(resolved.home_dir(), Path::new("/environment/home"));
        assert_eq!(resolved.instance_id(), "env_instance");
    }

    #[test]
    fn tilde_home_is_expanded() {
        let root = paths(Some("~/paperclip-data"), None, None, None).unwrap();
        assert_eq!(root.home_dir(), Path::new("/Users/tester/paperclip-data"));

        let exact = paths(Some("~"), None, None, None).unwrap();
        assert_eq!(exact.home_dir(), Path::new("/Users/tester"));
    }

    #[test]
    fn relative_home_is_resolved_and_cleaned() {
        let resolved = paths(Some("./runtime/../paperclip"), None, None, None).unwrap();
        assert_eq!(resolved.home_dir(), Path::new("/workspace/app/paperclip"));
    }

    #[test]
    fn invalid_instance_segments_are_rejected() {
        for invalid in ["../other", "with space", "with.dot", "你好"] {
            let error = paths(None, Some(invalid), None, None).unwrap_err();
            assert!(matches!(error, HomePathError::InvalidInstanceId(_)));
        }
    }

    #[test]
    fn valid_instance_segments_are_accepted() {
        for valid in ["default", "sat-worktree", "instance_42", "ABC123"] {
            assert_eq!(
                paths(None, Some(valid), None, None).unwrap().instance_id(),
                valid
            );
        }
    }

    #[test]
    fn config_and_env_paths_match_node_layout() {
        let resolved = paths(Some("/paperclip"), Some("dev"), None, None).unwrap();
        let config = resolved.config_path();
        assert_eq!(
            config,
            PathBuf::from("/paperclip/instances/dev/config.json")
        );
        assert_eq!(
            resolve_env_path_for_config(&config),
            PathBuf::from("/paperclip/instances/dev/.env")
        );
    }

    #[test]
    fn runtime_directories_match_node_layout() {
        let resolved = paths(Some("/paperclip"), Some("default"), None, None).unwrap();
        let root = PathBuf::from("/paperclip/instances/default");
        assert_eq!(resolved.embedded_postgres_dir(), root.join("db"));
        assert_eq!(resolved.logs_dir(), root.join("logs"));
        assert_eq!(resolved.secrets_dir(), root.join("secrets"));
        assert_eq!(
            resolved.secrets_key_file_path(),
            root.join("secrets/master.key")
        );
        assert_eq!(resolved.storage_dir(), root.join("data/storage"));
        assert_eq!(resolved.backup_dir(), root.join("data/backups"));
    }

    #[test]
    fn parent_segments_do_not_escape_absolute_root() {
        assert_eq!(
            resolve_path(Path::new("/../../paperclip"), Path::new("/workspace")),
            PathBuf::from("/paperclip")
        );
    }

    #[test]
    fn constants_match_node() {
        assert_eq!(DEFAULT_PAPERCLIP_INSTANCE_ID, "default");
        assert_eq!(PAPERCLIP_CONFIG_BASENAME, "config.json");
        assert_eq!(PAPERCLIP_ENV_FILENAME, ".env");
    }
}
