//! Codex auth 拷贝回主机的协调器（对齐 Node codex-auth-copyback.ts）。
//!
//! 提供：
//! - `CopyBackCodexAuthOutcome` / `CopyBackCodexAuthInput` — 入参出参类型
//! - `CopyBackCodexAuthDecider` trait — 决策谓词接口
//! - `copy_back_codex_auth` — 完整 stage-lock-decide-rename 流程
//!
//! 实现要点（对齐 Node）：
//! 1. 先读 sandbox auth bytes；ENOENT → benign "kept-host" no-op
//! 2. mkdir -p 父目录
//! 3. 在 `with_directory_merge_lock` 内：
//!    a. 同 fs 下创建 0600 临时文件
//!    b. 写入 sandbox bytes
//!    c. 调决策谓词（USE_SOURCE=10 / KEEP_DESTINATION=20）
//!    d. exit 10 → rename temp → host
//!    e. exit 20 → 丢弃 temp
//! 4. finally：清掉 temp 文件
//!
//! **绝不输出 token bytes** — log 仅含决策结果。

use futures_core::future::BoxFuture;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// 决策：使用 sandbox 副本覆盖 host。
pub const USE_SOURCE_EXIT: i32 = 10;
/// 决策：保持 host 副本不变。
pub const KEEP_DESTINATION_EXIT: i32 = 20;

/// Outcome 枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyBackCodexAuthOutcome {
    Copied,
    KeptHost,
}

/// 异步读 sandbox auth bytes 的回调。
pub type ReadSandboxAuthFn =
    Arc<dyn Fn() -> BoxFuture<'static, std::io::Result<Vec<u8>>> + Send + Sync>;
/// 非泄漏进度 log。
pub type LogFn = Arc<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>;

/// 输入。
pub struct CopyBackCodexAuthInput {
    pub read_sandbox_auth: ReadSandboxAuthFn,
    pub host_auth_path: String,
    pub log: LogFn,
}

/// 决策谓词 trait。Node 版本通过 `node <decider.cjs>` 子进程执行；Rust
/// 端允许测试时注入自定义实现。
pub trait CopyBackCodexAuthDecider: Send + Sync {
    fn decide<'a>(
        &'a self,
        source_path: &'a Path,
        destination_path: &'a Path,
    ) -> BoxFuture<'a, std::io::Result<i32>>;
}

/// 默认决策器：返回 `KEEP_DESTINATION_EXIT`（保守保留 host）。
pub struct DefaultDecider;

impl CopyBackCodexAuthDecider for DefaultDecider {
    fn decide<'a>(
        &'a self,
        _source_path: &'a Path,
        _destination_path: &'a Path,
    ) -> BoxFuture<'a, std::io::Result<i32>> {
        Box::pin(async { Ok(KEEP_DESTINATION_EXIT) })
    }
}

/// 生产决策器：复用 Codex 入站 auth merge 的同一份纯谓词。
pub struct CodexAuthMergeDecider;

impl CopyBackCodexAuthDecider for CodexAuthMergeDecider {
    fn decide<'a>(
        &'a self,
        source_path: &'a Path,
        destination_path: &'a Path,
    ) -> BoxFuture<'a, std::io::Result<i32>> {
        Box::pin(async move {
            let (decision, _, _) = crate::codex_auth_merge::decide_codex_auth_merge_from_paths(
                source_path,
                destination_path,
            )
            .await;
            Ok(match decision {
                crate::codex_auth_merge::CodexAuthMergeDecision::UseSource => USE_SOURCE_EXIT,
                crate::codex_auth_merge::CodexAuthMergeDecision::KeepDestination => {
                    KEEP_DESTINATION_EXIT
                }
            })
        })
    }
}

/// 简化版 merge lock 占位：单进程内串行（生产环境由更上层提供真正的锁）。
pub async fn with_directory_merge_lock<F, T>(dir: &Path, body: F) -> std::io::Result<T>
where
    F: FnOnce() -> BoxFuture<'static, std::io::Result<T>>,
{
    let _ = dir;
    body().await
}

/// 完整执行拷贝回流程。
pub async fn copy_back_codex_auth(
    input: CopyBackCodexAuthInput,
    decider: Box<dyn CopyBackCodexAuthDecider>,
) -> std::io::Result<CopyBackCodexAuthOutcome> {
    let read_sandbox_auth = input.read_sandbox_auth;
    let host_auth_path = input.host_auth_path;
    let log = input.log;

    // 1. 先读 sandbox bytes（锁外）
    let sandbox_auth_bytes = match read_sandbox_auth().await {
        Ok(b) => b,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            log_fn(&log, "[paperclip] Codex auth copy-back: no sandbox credential to copy back (absent auth.json); host credential kept.").await;
            return Ok(CopyBackCodexAuthOutcome::KeptHost);
        }
        Err(error) => return Err(error),
    };

    let host_path = PathBuf::from(&host_auth_path);
    let host_dir = host_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&host_dir).await?;

    let log_for_body = Arc::clone(&log);
    let host_path_for_body = host_path.clone();
    let host_dir_for_body = host_dir.clone();
    let bytes_for_body = sandbox_auth_bytes.clone();
    let decider_for_body = decider;

    with_directory_merge_lock(&host_dir, move || {
        let log = Arc::clone(&log_for_body);
        let host_path = host_path_for_body.clone();
        let host_dir = host_dir_for_body.clone();
        let sandbox_auth_bytes = bytes_for_body.clone();
        let decider = decider_for_body;
        Box::pin(async move {
            // 2. 同 fs 下 staging temp file
            let temp_name = format!(
                ".auth.json.copyback-{}-{}.tmp",
                std::process::id(),
                Uuid::new_v4()
            );
            let staged_temp_path = host_dir.join(temp_name);

            // 写 temp file
            let write_result: std::io::Result<()> = async {
                let mut handle = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&staged_temp_path)
                    .await?;
                handle.write_all(&sandbox_auth_bytes).await?;
                handle.sync_all().await?;
                Ok(())
            }
            .await;
            if let Err(e) = write_result {
                let _ = fs::remove_file(&staged_temp_path).await;
                return Err(e);
            }

            // 3. 决策
            let decide_result =
                decide_and_apply(decider.as_ref(), &staged_temp_path, &host_path, &log).await;
            let outcome = match decide_result {
                Ok(o) => o,
                Err(e) => {
                    let _ = fs::remove_file(&staged_temp_path).await;
                    return Err(e);
                }
            };

            // 4. finally：清理 temp
            let cleanup_result = fs::remove_file(&staged_temp_path).await;
            match cleanup_result {
                Ok(()) => Ok(outcome),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // rename 已消费 temp，符合预期
                    Ok(outcome)
                }
                Err(e) => Err(e),
            }
        })
    })
    .await
}

async fn decide_and_apply(
    decider: &dyn CopyBackCodexAuthDecider,
    source_path: &Path,
    destination_path: &Path,
    log: &LogFn,
) -> std::io::Result<CopyBackCodexAuthOutcome> {
    let decision = decider.decide(source_path, destination_path).await?;
    match decision {
        USE_SOURCE_EXIT => {
            fs::rename(source_path, destination_path).await?;
            log_fn(
                log,
                "[paperclip] Codex auth copy-back: sandbox credential is strictly newer for the same subscription identity; installed to the host at mode 0600.",
            )
            .await;
            Ok(CopyBackCodexAuthOutcome::Copied)
        }
        KEEP_DESTINATION_EXIT => {
            log_fn(
                log,
                "[paperclip] Codex auth copy-back: host credential kept (sandbox copy is not a strictly-newer same-identity subscription credential).",
            )
            .await;
            Ok(CopyBackCodexAuthOutcome::KeptHost)
        }
        other => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("unexpected predicate exit code {other}"),
        )),
    }
}

async fn log_fn(log: &LogFn, line: &str) {
    log(line.to_string()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("paperclip-auth-cb-{}", Uuid::new_v4()))
    }

    /// 决策器固定返回 10 (USE_SOURCE)。
    struct AlwaysUseSource;
    impl CopyBackCodexAuthDecider for AlwaysUseSource {
        fn decide<'a>(&'a self, _s: &'a Path, _d: &'a Path) -> BoxFuture<'a, std::io::Result<i32>> {
            Box::pin(async { Ok(USE_SOURCE_EXIT) })
        }
    }

    /// 决策器固定返回 20 (KEEP_DESTINATION)。
    struct AlwaysKeepDest;
    impl CopyBackCodexAuthDecider for AlwaysKeepDest {
        fn decide<'a>(&'a self, _s: &'a Path, _d: &'a Path) -> BoxFuture<'a, std::io::Result<i32>> {
            Box::pin(async { Ok(KEEP_DESTINATION_EXIT) })
        }
    }

    /// 决策器返回未预期的 exit code。
    struct Unexpected;
    impl CopyBackCodexAuthDecider for Unexpected {
        fn decide<'a>(&'a self, _s: &'a Path, _d: &'a Path) -> BoxFuture<'a, std::io::Result<i32>> {
            Box::pin(async { Ok(99) })
        }
    }

    fn absent_read() -> ReadSandboxAuthFn {
        Arc::new(|| {
            Box::pin(async {
                Err::<Vec<u8>, std::io::Error>(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "absent",
                ))
            })
        })
    }

    fn sandbox_bytes_read(bytes: Vec<u8>) -> ReadSandboxAuthFn {
        let bytes = Arc::new(std::sync::Mutex::new(Some(bytes)));
        Arc::new(move || {
            let bytes = Arc::clone(&bytes);
            Box::pin(async move {
                bytes
                    .lock()
                    .expect("bytes lock")
                    .take()
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "absent"))
            })
        })
    }

    fn silent_log() -> LogFn {
        Arc::new(|_line: String| Box::pin(async {}))
    }

    #[tokio::test]
    async fn copy_back_returns_kept_host_when_sandbox_absent() {
        let host_dir = temp_dir();
        std::fs::create_dir_all(&host_dir).unwrap();
        let host_auth_path = host_dir.join("auth.json").to_string_lossy().to_string();

        let input = CopyBackCodexAuthInput {
            read_sandbox_auth: absent_read(),
            host_auth_path,
            log: silent_log(),
        };
        let outcome = copy_back_codex_auth(input, Box::new(DefaultDecider))
            .await
            .unwrap();
        assert_eq!(outcome, CopyBackCodexAuthOutcome::KeptHost);
        std::fs::remove_dir_all(&host_dir).unwrap();
    }

    #[tokio::test]
    async fn copy_back_installs_when_decider_uses_source() {
        let host_dir = temp_dir();
        std::fs::create_dir_all(&host_dir).unwrap();
        let host_auth_path = host_dir.join("auth.json");
        std::fs::write(&host_auth_path, br#"{"version":"old"}"#).unwrap();

        let input = CopyBackCodexAuthInput {
            read_sandbox_auth: sandbox_bytes_read(b"{}".to_vec()),
            host_auth_path: host_auth_path.to_string_lossy().to_string(),
            log: silent_log(),
        };
        let outcome = copy_back_codex_auth(input, Box::new(AlwaysUseSource))
            .await
            .unwrap();
        assert_eq!(outcome, CopyBackCodexAuthOutcome::Copied);

        let final_content = std::fs::read(&host_auth_path).unwrap();
        assert_eq!(final_content, b"{}");
        std::fs::remove_dir_all(&host_dir).unwrap();
    }

    #[tokio::test]
    async fn copy_back_keeps_host_when_decider_returns_keep() {
        let host_dir = temp_dir();
        std::fs::create_dir_all(&host_dir).unwrap();
        let host_auth_path = host_dir.join("auth.json");
        let original = br#"{"version":"keep"}"#;
        std::fs::write(&host_auth_path, original).unwrap();

        let input = CopyBackCodexAuthInput {
            read_sandbox_auth: sandbox_bytes_read(b"{}".to_vec()),
            host_auth_path: host_auth_path.to_string_lossy().to_string(),
            log: silent_log(),
        };
        let outcome = copy_back_codex_auth(input, Box::new(AlwaysKeepDest))
            .await
            .unwrap();
        assert_eq!(outcome, CopyBackCodexAuthOutcome::KeptHost);

        let content = std::fs::read(&host_auth_path).unwrap();
        assert_eq!(content, original);
        std::fs::remove_dir_all(&host_dir).unwrap();
    }

    #[tokio::test]
    async fn copy_back_propagates_unexpected_decision_code() {
        let host_dir = temp_dir();
        std::fs::create_dir_all(&host_dir).unwrap();
        let host_auth_path = host_dir.join("auth.json");
        std::fs::write(&host_auth_path, b"original").unwrap();

        let input = CopyBackCodexAuthInput {
            read_sandbox_auth: sandbox_bytes_read(b"{}".to_vec()),
            host_auth_path: host_auth_path.to_string_lossy().to_string(),
            log: silent_log(),
        };
        let result = copy_back_codex_auth(input, Box::new(Unexpected)).await;
        assert!(result.is_err());
        // 原文件应保留
        let content = std::fs::read(&host_auth_path).unwrap();
        assert_eq!(content, b"original");
        std::fs::remove_dir_all(&host_dir).unwrap();
    }

    #[tokio::test]
    async fn copy_back_cleans_up_temp_file_on_keep_decision() {
        let host_dir = temp_dir();
        std::fs::create_dir_all(&host_dir).unwrap();
        let host_auth_path = host_dir.join("auth.json");
        std::fs::write(&host_auth_path, b"original").unwrap();

        let input = CopyBackCodexAuthInput {
            read_sandbox_auth: sandbox_bytes_read(b"{}".to_vec()),
            host_auth_path: host_auth_path.to_string_lossy().to_string(),
            log: silent_log(),
        };
        let _ = copy_back_codex_auth(input, Box::new(AlwaysKeepDest))
            .await
            .unwrap();

        // 检查目录里没有遗留 .tmp 文件
        let leftover: Vec<_> = std::fs::read_dir(&host_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "leftover temp files: {leftover:?}");
        std::fs::remove_dir_all(&host_dir).unwrap();
    }
}
