//! `.task_lock`：回答「拥有这个目录的那次执行还活着吗」的执行锁。
//!
//! 上游（`envlock_unix.go` 的 `flock`、`task_temp.go` 的 `.task_lock`）用内核建议锁而不是
//! 标记文件，理由只有一条但很硬：**锁随持有进程的死亡（或文件关闭）由内核释放**，
//! 所以「上一个执行还在不在」这个问题不需要心跳、不需要 PID 表、也不需要一条
//! 「过期状态清理」路径。GC 与恢复路径都建在这条性质上（[`super::temp::prune_task_temp_dirs`]）。
//!
//! 两个上游细节本模块照做：
//!
//! 1. **先锁私有名，再 rename 发布**：先锁 `.task_lock.claiming`，锁定成功后才把它
//!    `rename` 成 `.task_lock`。反过来（先建 `.task_lock` 再锁）会留一个窗口：并发的
//!    GC 看见一个「有标记但能锁上」的目录，判定持有者已死，把它从正要开工的 task
//!    脚下删掉。同目录内的 `rename` 是原子的，而锁挂在**打开的文件描述**上而非名字上,
//!    所以「可见」与「已持有」在一步内同时成立。
//! 2. **非阻塞**：拿不到就是「活着」，交给调用方决定跳过还是报错，绝不等待。

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use super::{ExecEnvError, Result, ENV_ROOT_LOCK_FILE};

/// 已持有的执行锁。丢弃它（或进程死亡）即释放。
#[derive(Debug)]
pub struct ExecutionLock {
    file: File,
    marker: PathBuf,
}

impl ExecutionLock {
    /// 为**新建**目录取执行锁：先锁私有名 `.task_lock.claiming`，成功后原子 `rename`
    /// 发布为 `.task_lock`。`Ok(None)` = 另一个进程正卡在 claim 阶段（不是错误）。
    pub fn acquire_fresh(dir: &Path) -> Result<Option<Self>> {
        let claim = dir.join(format!("{ENV_ROOT_LOCK_FILE}.claiming"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&claim)
            .map_err(|err| ExecEnvError::io("open lock claim file", &claim, err))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                drop(file);
                remove_quietly(&claim);
                return Ok(None);
            }
            Err(TryLockError::Error(err)) => {
                drop(file);
                remove_quietly(&claim);
                return Err(ExecEnvError::io("lock claim file", &claim, err));
            }
        }
        let marker = dir.join(ENV_ROOT_LOCK_FILE);
        if let Err(err) = std::fs::rename(&claim, &marker) {
            drop(file);
            remove_quietly(&claim);
            return Err(ExecEnvError::io("publish lock marker", &marker, err));
        }
        Ok(Some(Self { file, marker }))
    }

    /// 探测**已发布**的 `.task_lock`：`Ok(None)` = 标记不存在，或仍被存活的持有者持有。
    ///
    /// GC 用的是这一条，而不是 [`Self::acquire_fresh`]：后者给自己另开一个 claim 文件，
    /// 锁的是**别的 inode**，因此对「已发布的标记是否还被持有」没有任何判断力。
    /// 反过来，`acquire_fresh` 也**不能**用在这里——探测者绝不能创建一个标记。
    pub fn probe(dir: &Path) -> Result<Option<Self>> {
        let marker = dir.join(ENV_ROOT_LOCK_FILE);
        let file = match OpenOptions::new().read(true).write(true).open(&marker) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(ExecEnvError::io("open lock marker", &marker, err)),
        };
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { file, marker })),
            Err(TryLockError::WouldBlock) => {
                drop(file);
                Ok(None)
            }
            Err(TryLockError::Error(err)) => {
                drop(file);
                Err(ExecEnvError::io("probe lock marker", &marker, err))
            }
        }
    }

    /// 已发布的标记路径（`.task_lock`）。
    #[must_use]
    pub fn marker(&self) -> &Path {
        self.marker.as_path()
    }

    /// 显式释放。丢弃 `self` 同样释放（`Drop`），这里只是让调用点的意图显式。
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for ExecutionLock {
    fn drop(&mut self) {
        // 关闭文件描述符就已经释放内核锁；显式 unlock 让「释放」与「关闭」解耦。
        let _ = self.file.unlock();
    }
}

fn remove_quietly(path: &Path) {
    if let Err(err) = std::fs::remove_file(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %path.display(), error = %err, "remove lock claim file failed");
        }
    }
}
