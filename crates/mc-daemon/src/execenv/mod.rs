//! 执行环境（execenv）：在**用户机器**上为一次 task 准备隔离的执行目录。
//!
//! 上游对应 `server/internal/daemon/execenv/`（本机 clone `90e0bdf` 实测：42 个非测试文件 /
//! 17,413 行；含 57 个测试文件后全目录 99 文件 / 47,336 行）。上游按 provider 与平台铺得很开，本片（`LUM-1440`，
//! M3-8-p0）落地的是**隔离与生命周期内核**——即上游硬约束里最贵的那三条：
//! 路径先 canonical 化再判前缀、执行锁回答「持有者还活着吗」、失败/取消都不留残留。
//!
//! | 模块 | 职责 | 上游对应 |
//! |------|------|----------|
//! | [`path`] | env root 的 canonical 化 + 前缀守卫（拒绝 `..` / 绝对路径 / 越界符号链接） | `isolation.go`、`canonical_path.go` |
//! | [`lock`] | `.task_lock` 执行锁：内核在持有者进程死亡时自动释放 | `envlock_unix.go`、`gitroot_lock.go` |
//! | [`temp`] | 每 task 临时目录 + 「先内容后标记」删除 + 只回收死者的 GC | `task_temp.go`、`reclaimable.go` |
//! | [`guard`] | prepare → commit/回滚 的事务语义（RAII，失败与取消同一路径） | `execenv.go` 的 `defer` 语义 |
//!
//! 未落地的子域（provider 配置生成、skill 落盘与剥离、会话/记忆、MCP 接线、Windows 面）
//! 逐条带行数记在 `docs/33-M3-ADAPTERS.md` 的 execenv 章节里，**没有**在代码里静默省略。

pub mod guard;
pub mod lock;
pub mod path;
pub mod temp;

use std::io;
use std::path::{Path, PathBuf};

pub use guard::PreparedEnv;
pub use lock::ExecutionLock;
pub use path::EnvRoot;
pub use temp::{
    prune_task_temp_dirs, remove_task_temp_dir, task_temp_dir_holds_content, PruneReport,
    TaskTempDir, TASK_TEMP_DIR_PREFIX,
};

/// env root 与 task 临时目录的执行锁文件名。两份上游代码（`envRootLockFile` 与
/// `task_temp.go` 的标记）用的是同一个名字，本片沿用，理由写在 [`lock`] 的模块文档里。
pub const ENV_ROOT_LOCK_FILE: &str = ".task_lock";

/// 上游 `reclaimable.go` 的 `codexHomeDirName` / `codexSandboxBinDirName`。
const CODEX_HOME_DIR_NAME: &str = "codex-home";
const CODEX_SANDBOX_BIN_DIR_NAME: &str = ".sandbox-bin";

/// daemon 自有的、可再生成的目录（相对 env root 的**精确相对路径**，不是 basename ——
/// 仓库里完全可能有一个同名叶子目录，按 basename 匹配会误删用户内容）。
#[must_use]
pub fn managed_reclaimable_artifact_subpaths() -> Vec<PathBuf> {
    vec![PathBuf::from(CODEX_HOME_DIR_NAME).join(CODEX_SANDBOX_BIN_DIR_NAME)]
}

/// execenv 的错误面。所有 IO 失败都带上「在做什么」与「对哪个路径」，因为这一层的
/// 失败大多发生在「准备到一半」，排查时路径本身就是证据。
#[derive(Debug, thiserror::Error)]
pub enum ExecEnvError {
    #[error("execenv: {op} {path}: {source}")]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("execenv: path escape — {candidate} 不在 {root} 之内")]
    PathEscape { root: PathBuf, candidate: PathBuf },
    #[error("execenv: 相对路径非法（不得为绝对路径、不得含 `..`）：{path}")]
    InvalidRelativePath { path: PathBuf },
    #[error("execenv: 执行锁已被存活持有者占用：{path}")]
    LockBusy { path: PathBuf },
}

impl ExecEnvError {
    pub(crate) fn io(op: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            op,
            path: path.to_path_buf(),
            source,
        }
    }
}

pub type Result<T, E = ExecEnvError> = std::result::Result<T, E>;
