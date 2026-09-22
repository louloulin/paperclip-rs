//! Multica home paths: 解析 `$MULTICA_HOME` 与 instance root。
//!
//! 与 paperclip-rs `pc-config::home_paths` 行为等价，但 env 前缀替换为 `MULTICA_*`。

use std::path::{Path, PathBuf};

pub const DEFAULT_MULTICA_INSTANCE_ID: &str = "default";
pub const MULTICA_CONFIG_BASENAME: &str = "config";
pub const MULTICA_ENV_FILENAME: &str = ".env";

#[derive(Debug, thiserror::Error)]
pub enum HomePathError {
    #[error("invalid home path: {0}")]
    InvalidPath(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// 多实例目录布局：
///
/// ```text
/// $MULTICA_HOME/
///   ├── config.toml        # 默认配置（可选）
///   ├── .env               # 默认环境变量（可选）
///   ├── instances/
///   │   ├── default/
///   │   │   ├── config.toml
///   │   │   ├── .env
///   │   │   ├── data/
///   │   │   └── state/
///   │   └── <other>/
///   └── logs/
/// ```
#[derive(Debug, Clone)]
pub struct MulticaHomePaths {
    pub home: PathBuf,
    pub instance_id: String,
}

impl MulticaHomePaths {
    /// 从 env 读取 `MULTICA_HOME`（fallback `~/.multica`）+ `MULTICA_INSTANCE_ID`（fallback `default`）。
    pub fn from_env() -> Result<Self, HomePathError> {
        let home = match std::env::var("MULTICA_HOME").ok().filter(|s| !s.is_empty()) {
            Some(p) => PathBuf::from(p),
            None => dirs::home_dir()
                .ok_or_else(|| HomePathError::InvalidPath("no $HOME".into()))?
                .join(".multica"),
        };
        let instance_id = std::env::var("MULTICA_INSTANCE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_MULTICA_INSTANCE_ID.into());
        Ok(Self { home, instance_id })
    }

    pub fn instance_root(&self) -> PathBuf {
        self.home.join("instances").join(&self.instance_id)
    }

    pub fn config_path(&self) -> PathBuf {
        self.instance_root().join(format!("{MULTICA_CONFIG_BASENAME}.toml"))
    }

    pub fn env_path(&self) -> PathBuf {
        self.instance_root().join(MULTICA_ENV_FILENAME)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.instance_root().join("data")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.instance_root().join("state")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.home.join("logs")
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for d in [
            self.instance_root(),
            self.data_dir(),
            self.state_dir(),
            self.log_dir(),
        ] {
            std::fs::create_dir_all(&d)?;
        }
        Ok(())
    }
}

/// `~` 前缀展开。
pub fn expand_home_prefix(path: &Path) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        if let Ok(s) = path.strip_prefix("~") {
            return home.join(s);
        }
    }
    path.to_path_buf()
}

/// 解析一个 home-aware 路径：先 expand `~`，再相对 home。
pub fn resolve_home_aware_path(path: &Path, home: &Path) -> PathBuf {
    let expanded = expand_home_prefix(path);
    if expanded.is_absolute() {
        expanded
    } else {
        home.join(expanded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_instance_root() {
        let p = MulticaHomePaths {
            home: PathBuf::from("/tmp/multica"),
            instance_id: "default".into(),
        };
        assert_eq!(
            p.instance_root(),
            PathBuf::from("/tmp/multica/instances/default")
        );
        assert_eq!(
            p.config_path(),
            PathBuf::from("/tmp/multica/instances/default/config.toml")
        );
    }

    #[test]
    fn expand_home_prefix_handles_tilde() {
        let p = PathBuf::from("~/foo/bar");
        let resolved = expand_home_prefix(&p);
        // Should be absolute and not start with ~
        assert!(resolved.is_absolute() || !resolved.starts_with("~"));
    }

    #[test]
    fn resolve_home_aware_path_relative() {
        let p = PathBuf::from("subdir");
        let resolved = resolve_home_aware_path(&p, Path::new("/tmp/multica"));
        assert_eq!(resolved, PathBuf::from("/tmp/multica/subdir"));
    }
}