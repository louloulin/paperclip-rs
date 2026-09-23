//! 每 task 临时目录：创建、按「先内容后标记」删除、以及只回收死者的 GC。
//!
//! 上游 `task_temp.go` 与 `reclaimable.go` 的三条不变量，本模块逐条照做：
//!
//! 1. **名字前缀**（`multica-task-`）：GC 只碰带前缀的目录，因此共享的 `/tmp`
//!    （里面还有别人的文件）永远不在回收面内；
//! 2. **删除顺序**：先删内容、再删标记、最后删目录本身，且**在第一个删不掉的内容上就停**。
//!    这不是 `remove_dir_all` 能替代的：`remove_dir_all` 遇错继续走，会把 `.task_lock`
//!    一起删掉，于是下一个 GC 看到的是一个「没有标记的目录」——与「尚未加锁的遗留物」
//!    无法区分，只能靠年龄兜底。留着标记 = 这次清理失败不花任何代价，下一轮再试。
//!    同样的道理要求**目录删不掉时把标记放回去**，否则「清理走得最远」的那种失败
//!    反而成为唯一会永久泄漏的一种；
//! 3. **只回收死者**：有标记的目录必须**能锁上**才算死者；锁不上就是有人在用（包括
//!    同机另一个 daemon 的 task —— 任何进程内 active 集合都看不见它）。没有标记的
//!    遗留物只在「有内容且超龄」时回收，因为一个刚刚创建、还没发布标记的目录里
//!    不可能有 task 内容。

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::lock::ExecutionLock;
use super::{ExecEnvError, Result, ENV_ROOT_LOCK_FILE};

/// daemon 在每个 task 临时目录上盖的 basename 前缀（上游 `TaskTempDirPrefix`）。
pub const TASK_TEMP_DIR_PREFIX: &str = "multica-task-";

/// daemon 拥有的一次 task 临时目录。创建即加锁；丢弃即释放锁并尽力清空目录
/// （取消路径也走这里，见 `super::guard`）。
#[derive(Debug)]
pub struct TaskTempDir {
    dir: PathBuf,
    lock: Option<ExecutionLock>,
}

impl TaskTempDir {
    /// 在 `base` 下新建一个已加锁的 task 临时目录。
    pub fn create(base: &Path) -> Result<Self> {
        fs::create_dir_all(base)
            .map_err(|err| ExecEnvError::io("create task temp base", base, err))?;
        let dir = unique_task_temp_dir(base)?;
        fs::create_dir(&dir).map_err(|err| ExecEnvError::io("create task temp dir", &dir, err))?;
        let Some(lock) = ExecutionLock::acquire_fresh(&dir)? else {
            // 目录名刚由本进程生成，理论上不可达；真到了这里也不能留下半成品。
            remove_quietly(&dir);
            return Err(ExecEnvError::LockBusy { path: dir });
        };
        Ok(Self {
            dir,
            lock: Some(lock),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.as_path()
    }

    /// 显式清理（幂等）：释放锁 → 删除内容与标记 → 删除目录。
    pub fn cleanup(mut self) -> Result<()> {
        self.cleanup_inner()
    }

    fn cleanup_inner(&mut self) -> Result<()> {
        // 必须先放锁：标记随后被删，而 Windows 上删一个自己还开着的文件只是标记删除，
        // 会挡住目录本身的移除。
        drop(self.lock.take());
        remove_task_temp_dir(&self.dir)
    }
}

impl Drop for TaskTempDir {
    fn drop(&mut self) {
        if self.lock.is_none() {
            return; // 已被显式 cleanup
        }
        if let Err(err) = self.cleanup_inner() {
            tracing::warn!(
                dir = %self.dir.display(),
                error = %err,
                "task temp dir cleanup on drop failed; 标记已保留，下一轮 GC 会重试"
            );
        }
    }
}

/// `dir` 里除锁标记外还有别的东西吗（上游 `taskTempDirHoldsContent` 的语义）。
pub fn task_temp_dir_holds_content(dir: &Path) -> Result<bool> {
    let entries =
        fs::read_dir(dir).map_err(|err| ExecEnvError::io("read task temp dir", dir, err))?;
    for entry in entries {
        let entry = entry.map_err(|err| ExecEnvError::io("read task temp entry", dir, err))?;
        if entry.file_name() != OsStr::new(ENV_ROOT_LOCK_FILE) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 按上游顺序删除一个 task 临时目录。已不存在算成功（幂等）。
pub fn remove_task_temp_dir(dir: &Path) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(ExecEnvError::io("read task temp dir", dir, err)),
    };
    for entry in entries {
        let entry = entry.map_err(|err| ExecEnvError::io("read task temp entry", dir, err))?;
        if entry.file_name() == OsStr::new(ENV_ROOT_LOCK_FILE) {
            continue; // 标记最后删，且只删一次
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| ExecEnvError::io("stat task temp entry", &path, err))?;
        // 符号链接按**链接本身**删（不跟随）：跟随会把 root 之外的目标一起拖进来。
        let outcome = if file_type.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if let Err(err) = outcome {
            // 停在这里：标记留着，这一轮算没清理成功，下一轮 GC 继续。
            return Err(ExecEnvError::io("remove task temp content", &path, err));
        }
    }

    let marker = dir.join(ENV_ROOT_LOCK_FILE);
    let had_marker = marker.exists();
    if had_marker {
        fs::remove_file(&marker)
            .map_err(|err| ExecEnvError::io("remove lock marker", &marker, err))?;
    }
    if let Err(err) = fs::remove_dir(dir) {
        if had_marker {
            // 把标记放回去，否则这次「走得最远」的失败会变成永久泄漏。
            if let Err(restore) = fs::write(&marker, b"") {
                tracing::warn!(path = %marker.display(), error = %restore, "restore lock marker failed");
            }
        }
        return Err(ExecEnvError::io("remove task temp dir", dir, err));
    }
    Ok(())
}

/// 一次 GC 的读数（哪三个桶分别装了什么，便于日志与断言）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PruneReport {
    pub removed: Vec<PathBuf>,
    pub kept_in_use: Vec<PathBuf>,
    pub kept_legacy: Vec<PathBuf>,
}

/// 回收 `base` 下**死者**的 task 临时目录。
///
/// * 有 `.task_lock` 且锁不上 ⇒ 有人正在用 ⇒ 留；
/// * 有 `.task_lock` 且能锁上 ⇒ 持有者已死 ⇒ 回收（先放掉刚拿到的锁再删，见
///   [`remove_task_temp_dir`] 的调用约定）；
/// * 无标记的遗留物 ⇒ 只有「有内容且年龄 ≥ `legacy_ttl`」才回收。
///
/// `now` 显式传入而不是内部取时钟，便于确定性测试。
pub fn prune_task_temp_dirs(
    base: &Path,
    legacy_ttl: Duration,
    now: SystemTime,
) -> Result<PruneReport> {
    let mut report = PruneReport::default();
    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(report),
        Err(err) => return Err(ExecEnvError::io("read task temp base", base, err)),
    };
    for entry in entries {
        let entry = entry.map_err(|err| ExecEnvError::io("read task temp entry", base, err))?;
        let path = entry.path();
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(TASK_TEMP_DIR_PREFIX)
        {
            continue;
        }
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if !is_dir {
            continue;
        }
        if path.join(ENV_ROOT_LOCK_FILE).exists() {
            match ExecutionLock::probe(&path)? {
                Some(lock) => {
                    drop(lock); // 删除前必须先放掉自己的锁
                    remove_task_temp_dir(&path)?;
                    report.removed.push(path);
                }
                None => report.kept_in_use.push(path),
            }
            continue;
        }
        let age = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map_or(Duration::ZERO, |modified| {
                now.duration_since(modified).unwrap_or(Duration::ZERO)
            });
        if task_temp_dir_holds_content(&path)? && age >= legacy_ttl {
            remove_task_temp_dir(&path)?;
            report.removed.push(path);
        } else {
            report.kept_legacy.push(path);
        }
    }
    Ok(report)
}

fn unique_task_temp_dir(base: &Path) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|err| ExecEnvError::io("read clock", base, io::Error::other(err)))?
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(base.join(format!(
        "{TASK_TEMP_DIR_PREFIX}{:x}-{nanos:x}-{seq:x}",
        std::process::id()
    )))
}

fn remove_quietly(path: &Path) {
    if let Err(err) = fs::remove_dir_all(path) {
        tracing::warn!(path = %path.display(), error = %err, "remove half-built task temp dir failed");
    }
}
