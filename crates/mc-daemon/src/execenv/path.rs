//! env root 的路径守卫：先把**已存在的那一段** canonical 化，再判它是否仍在 root 之内。
//!
//! 上游 `isolation.go` 的隔离面只有一条规则，但它是这一层最贵的一条：execenv 是唯一
//! 允许把宿主目录、环境变量、skill 文件写进用户机器的层，所以任何路径拼接都不得直接
//! 相信调用方给的字符串。本模块把它拆成两步：
//!
//! 1. **语法层**（[`EnvRoot::join_checked`]）：拒绝绝对路径与任何 `..` 段；
//! 2. **语义层**（[`EnvRoot::assert_inside`]）：沿路径逐段走，**每一段符号链接都
//!    `canonicalize` 一次**并复核前缀——因此 `sub -> /etc` 这种留在 root 内的链接
//!    指向 root 外时会被拒，而 `codex-home -> <root>/shared` 这种**在 root 内**的链接
//!    仍然放行（上游确实依赖这种链接，见 `codex_home_link.go`）。
//!
//! 两处容易写漏的地方，本实现显式覆盖（各有单测）：
//!
//! * **悬空符号链接**：`sub -> /outside/does-not-exist` 时 `canonicalize(sub)` 会失败。
//!   只做「canonicalize 最深的已存在祖先」的实现会退化成「根目录在 root 内」⇒ 放行，
//!   随后写入就落到了 root 外。这里对符号链接的 `canonicalize` 失败**直接报错**。
//! * **尚不存在的尾部**：只有真正不存在的段才允许不解析（首次创建目录的常规路径）。

use std::path::{Component, Path, PathBuf};

use super::{ExecEnvError, Result};

/// 一个已 canonical 化的 env root。所有对外写入都必须经它派生出路径。
#[derive(Debug, Clone)]
pub struct EnvRoot {
    root: PathBuf,
}

impl EnvRoot {
    /// 打开一个**已存在**的目录作为 env root，并立即 canonical 化（解析符号链接、
    /// 去掉 `..`）。canonical 后的 root 是后面所有前缀比较的基准。
    pub fn open(root: &Path) -> Result<Self> {
        let canonical = root
            .canonicalize()
            .map_err(|err| ExecEnvError::io("canonicalize env root", root, err))?;
        if !canonical.is_dir() {
            return Err(ExecEnvError::io(
                "env root is not a directory",
                root,
                std::io::Error::new(std::io::ErrorKind::NotADirectory, "not a directory"),
            ));
        }
        Ok(Self { root: canonical })
    }

    /// 创建（若不存在）并打开 env root。
    pub fn create(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)
            .map_err(|err| ExecEnvError::io("create env root", root, err))?;
        Self::open(root)
    }

    #[must_use]
    pub fn canonical_root(&self) -> &Path {
        self.root.as_path()
    }

    /// 把 root 内的相对路径解析成绝对路径，并在**解析之后**复核它仍在 root 内。
    pub fn join_checked(&self, rel: &Path) -> Result<PathBuf> {
        if rel.is_absolute() {
            return Err(ExecEnvError::InvalidRelativePath {
                path: rel.to_path_buf(),
            });
        }
        let mut candidate = self.root.clone();
        for component in rel.components() {
            match component {
                Component::Normal(segment) => candidate.push(segment),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(ExecEnvError::InvalidRelativePath {
                        path: rel.to_path_buf(),
                    });
                }
            }
        }
        self.assert_inside(&candidate)?;
        Ok(candidate)
    }

    /// 语义层守卫：逐段解析符号链接，任何一段把它带出 root 就报错。
    pub fn assert_inside(&self, candidate: &Path) -> Result<()> {
        let rel = candidate
            .strip_prefix(&self.root)
            .map_err(|_| ExecEnvError::PathEscape {
                root: self.root.clone(),
                candidate: candidate.to_path_buf(),
            })?;
        let mut cursor = self.root.clone();
        for component in rel.components() {
            let Component::Normal(segment) = component else {
                continue;
            };
            cursor.push(segment);
            match std::fs::symlink_metadata(&cursor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    // 悬空链接在这里失败 ⇒ 绝不因为「目标不存在」而放行。
                    let target = cursor.canonicalize().map_err(|err| {
                        ExecEnvError::io("canonicalize symlink under env root", &cursor, err)
                    })?;
                    if !target.starts_with(&self.root) {
                        return Err(ExecEnvError::PathEscape {
                            root: self.root.clone(),
                            candidate: candidate.to_path_buf(),
                        });
                    }
                    cursor = target;
                }
                Ok(_) => {}
                // 这一层还不存在 ⇒ 后面的段都是新建，交给调用方的 create_dir_all。
                Err(_) => break,
            }
        }
        if !cursor.starts_with(&self.root) {
            return Err(ExecEnvError::PathEscape {
                root: self.root.clone(),
                candidate: candidate.to_path_buf(),
            });
        }
        Ok(())
    }

    /// 解析 + 建目录（含中间目录），返回绝对路径。
    pub fn ensure_dir(&self, rel: &Path) -> Result<PathBuf> {
        let path = self.join_checked(rel)?;
        std::fs::create_dir_all(&path)
            .map_err(|err| ExecEnvError::io("create env dir", &path, err))?;
        Ok(path)
    }
}
