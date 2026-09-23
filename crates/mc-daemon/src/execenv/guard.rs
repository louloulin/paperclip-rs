//! prepare 的事务语义：上游用 `defer` 保证「成功、失败、取消」三条路径都清理，
//! Rust 侧用 RAII（`Drop`）实现同一件事。
//!
//! 三条路径在这里各有去处：
//!
//! | 路径 | 本模块的机制 |
//! |------|--------------|
//! | 准备成功、执行结束 | [`PreparedEnv::cleanup`]（显式） |
//! | 准备**中途失败** | `prepare` 内任何一步返回 `Err` ⇒ 局部 `PreparedEnv` 被丢弃 ⇒ `Drop` 回滚 |
//! | 取消 / 任务被 kill 之外的异常退出 | task 目录持有者进程死亡 ⇒ 内核释放 [`super::ExecutionLock`]，下一轮 GC 回收（`super::temp`） |
//!
//! 失败回滚不靠「记住删了哪几个目录」而是靠**整个 task 临时目录自包含**：所有中间产物
//! 都写在它的子树里，所以回滚 = 释放锁 + 删这一个目录，不需要反向步骤清单，
//! 也就不会漏掉某一步写得比较隐晦的中间文件。

use std::path::{Path, PathBuf};

use super::path::EnvRoot;
use super::temp::TaskTempDir;
use super::Result;

/// 一次已准备好的执行环境。丢弃它就是清理（除非已经 [`PreparedEnv::cleanup`]）。
#[derive(Debug)]
pub struct PreparedEnv {
    env_root: PathBuf,
    temp: Option<TaskTempDir>,
}

impl PreparedEnv {
    /// 在 `base`（通常是 daemon 的 task 临时根，如 `/tmp`）下准备一个隔离的执行环境。
    ///
    /// `layout` 是 env root 内需要预建的**相对**目录（如 `workspace`、`codex-home`）。
    /// 任何一步失败 —— 包括 `layout` 里出现越界路径 —— 都会让整个目录被回滚掉。
    pub fn prepare(base: &Path, layout: &[&str]) -> Result<Self> {
        let temp = TaskTempDir::create(base)?;
        let root = EnvRoot::open(temp.path())?;
        for rel in layout {
            root.ensure_dir(Path::new(rel))?;
        }
        Ok(Self {
            env_root: root.canonical_root().to_path_buf(),
            temp: Some(temp),
        })
    }

    /// canonical 化之后的 env root：后续所有写入都要从这里派生路径。
    #[must_use]
    pub fn env_root(&self) -> &Path {
        self.env_root.as_path()
    }

    /// task 临时目录（含锁）。锁一直持有到清理为止。
    #[must_use]
    pub fn temp_dir(&self) -> Option<&Path> {
        self.temp.as_ref().map(TaskTempDir::path)
    }

    /// 显式清理：放锁 + 删除整个 task 临时目录。幂等。
    pub fn cleanup(mut self) -> Result<()> {
        match self.temp.take() {
            Some(temp) => temp.cleanup(),
            None => Ok(()),
        }
    }
}

impl Drop for PreparedEnv {
    fn drop(&mut self) {
        if let Some(temp) = self.temp.take() {
            let dir = temp.path().to_path_buf();
            if let Err(err) = temp.cleanup() {
                // 清理失败时 TaskTempDir 的 Drop 已经打过一条 warn（标记保留、下轮重试）。
                tracing::warn!(
                    dir = %dir.display(),
                    error = %err,
                    "prepared env cleanup failed"
                );
            }
        }
    }
}
