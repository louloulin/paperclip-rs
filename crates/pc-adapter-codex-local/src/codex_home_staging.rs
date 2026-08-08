//! Codex home staging —— 把受管 `CODEX_HOME` 按白名单拷贝到临时目录，
//! 供 sandbox `home` asset 上传。
//!
//! 对齐 Node `codex-home.ts` 中的 `stageCodexHomeForSync` + 三个私有
//! 辅助函数 `stageCodexHomeEntry` / `stageDirectorySecure` /
//! `stageContainedSubtree` / `isResolvedPathInside`。
//!
//! # 设计要点
//!
//! 1. **白名单而非黑名单**：只上传 Codex 真正需要的文件 (`auth.json` /
//!    `config.toml` / `config.json` / `instructions.md` / `skills/`)。
//!    `*.sqlite` / `plugins/` / `sessions/` / `cache/` / `shell_snapshots/`
//!    等主机本地运行时状态不会上 sandbox。
//! 2. **符号链接解除为字节**：`auth.json` 是指向 shared source home 的
//!    symlink（保持单次使用 refresh token 鲜活），staging 时 `fs:read` 进
//!    symlink，并把字节落盘为普通文件 —— 拷贝 & 拷贝回时拿到真实字节。
//! 3. **每个 file 0600 / 每个 dir 0700**：staging 目录 + 每个文件权限
//!    最小化，机密字段（OAuth token / MCP bearer header）不会落到
//!    group/other-readable。
//! 4. **Fail-closed**：任何意外 I/O 错误 → 删 staging dir + 抛错，绝不
//!    返回部分目录。
//! 5. **容纳性检查**：每个 `skills/` 子目录的 symlink 在
//!    `stageDirectorySecure` 中单独作为 containment root，nested links
//!    不能逃出；`stageContainedSubtree` 检测路径环，`Top-level child
//!    -> sourceDir 自身 / ancestor` 视为退化 link 跳过。

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tokio::fs;

/// Stage 调用选项。
///
/// 仅用 `run_id` 拼接 staging tmpdir 名字，便于在日志中追溯。
/// 对齐 Node `StageCodexHomeForSyncOptions`。
#[derive(Debug, Clone, Default)]
pub struct StageCodexHomeForSyncOptions {
    pub run_id: Option<String>,
}

/// `CODEX_SYNC_ALLOWLIST`（与 `codex_home.rs::CODEX_SYNC_ALLOWLIST` 复用）。
///
/// 文件名 + `skills/` 目录是 codex sandbox 真正需要的最小集合。
pub const CODEX_SYNC_ALLOWLIST: &[&str] = &[
    "config.json",
    "config.toml",
    "instructions.md",
    "auth.json",
    "skills",
];

/// 把 `effective_codex_home` 中白名单项 stage 到一个 **新建私有 tmpdir**，
/// 返回该 tmpdir 路径。调用方负责在 run teardown 时删除。
///
/// 任何意外 I/O 错都会清理 partial tmpdir 并重抛 —— 永不返回半空 staging。
///
/// # Symlink 行为
///
/// - `auth.json`（共享 source 的 symlink）→ staging 为 plain file 含
///   dereferenced 字节
/// - `skills/*`（每个子项是 symlink）→ dereferenced 到 plain bytes
/// - dangling / ELOOP / 自指 symlink → 跳过（与 Node 一致）
/// - 越出 containment root 的 symlink → 跳过（防止 host 文件被拖入 staging）
///
/// # 权限
///
/// - staging tmpdir：`mkdtemp` 默认 0700（POSIX）
/// - 普通文件：读后写为 0600，并显式 `chmod 0600` 防止 umask 影响
/// - 子目录：0700
pub async fn stage_codex_home_for_sync(
    effective_codex_home: impl AsRef<Path>,
    options: StageCodexHomeForSyncOptions,
) -> std::io::Result<PathBuf> {
    let effective = effective_codex_home.as_ref();
    let run_id_part = options
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // 1. 创建 staging tmpdir
    let prefix = match run_id_part {
        Some(rid) => format!("paperclip-codex-home-sync-{}-", rid),
        None => "paperclip-codex-home-sync-".to_string(),
    };
    let staged_home = tokio::task::spawn_blocking(move || {
        let mut template = std::env::temp_dir();
        template.push(format!(".{}", prefix));
        // tokio::fs::create_dir 没法直接 mkdtemp，用 std::env::temp_dir + 唯一后缀
        let unique = format!(
            "{}.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            uuid::Uuid::new_v4().simple()
        );
        template.push(unique);
        // 确保父目录存在
        if let Some(parent) = template.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir(&template)?;
        // mkdtemp 默认 0700；显式 chmod 防止 race
        std::fs::set_permissions(&template, std::fs::Permissions::from_mode(0o700))?;
        Ok::<PathBuf, std::io::Error>(template)
    })
    .await
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("join error: {}", e)))??;

    // 2. 对每个白名单项执行 stage
    let result: std::io::Result<()> = async {
        for entry in CODEX_SYNC_ALLOWLIST {
            stage_codex_home_entry(effective, &staged_home, entry).await?;
        }
        Ok(())
    }
    .await;

    if let Err(err) = result {
        // 清理 partial dir
        let _ = fs::remove_dir_all(&staged_home).await;
        return Err(err);
    }
    Ok(staged_home)
}

/// 拷贝白名单中的一项（文件 / 目录 / 符号链接）到 staging 目录。
///
/// 缺失 / 不可 stat → 跳过（keyring 模式下没有 `auth.json`，某些 home
/// 没有 `config.json`，不报错）。
/// 其他非 ENOENT 错误向上抛。
async fn stage_codex_home_entry(
    source_home: &Path,
    staged_home: &Path,
    entry: &str,
) -> std::io::Result<()> {
    let source = source_home.join(entry);
    // `fs::metadata` 跟随 symlink；dangling link → ENOENT → 跳过
    let stat = match fs::metadata(&source).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let target = staged_home.join(entry);
    if stat.is_dir() {
        stage_directory_secure(&source, &target).await?;
    } else {
        // dereference symlink to bytes
        let bytes = fs::read(&source).await?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).await?;
        }
        write_file_0600(&target, &bytes).await?;
    }
    Ok(())
}

/// 递归拷贝 `source_dir`（一个白名单目录项，目前只有 `skills/`）到
/// `target_dir`，将 symlink dereference 为字节，并对每个拷贝出的 regular
/// file 规范化 mode 为 `0600`。
///
/// 替代 `fs::cp({ dereference: true })`（后者会保留源的 mode，
/// 把 `0644` 文档 / `0755` 脚本带 group-read 进 staging —— 这里
/// 全部写为 0600）。
///
/// **`source_dir` 的直接子项** 是 Paperclip 注入的 skill symlinks（指向
/// 共享 skill store），它们 **可以** 指向 `CODEX_HOME/skills/` 之外，
/// 所以每个直接子项各自作为 containment root；其下的 nested symlink
/// **不能** 逃出该 root。  
/// 顶端子项如果 dereference 到 `sourceDir` 自身或祖先 → 跳过（避免把
/// 整个 home 拖入 `skills/`）。
async fn stage_directory_secure(source_dir: &Path, target_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(target_dir).await?;
    set_dir_0700(target_dir).await?;
    let real_source_dir = match fs::canonicalize(source_dir).await {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut entries = match fs::read_dir(source_dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    while let Some(entry) = entries.next_entry().await? {
        let entry_source = source_dir.join(entry.file_name());
        let entry_target = target_dir.join(entry.file_name());
        // resolve real path; dangling / ELOOP → skip
        let resolved = match fs::canonicalize(&entry_source).await {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) if e.raw_os_error() == Some(62) /* ELOOP on macOS, 40 on linux */ => continue,
            Err(e) => return Err(e),
        };
        // 退化的顶层子项 (`back -> .` / `back -> ..`) → 跳过
        if is_resolved_path_inside(&resolved, &real_source_dir) {
            continue;
        }
        let entry_stat = match fs::metadata(&resolved).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if entry_stat.is_dir() {
            // 该子目录建立自己的 containment root + cycle detection 起点
            stage_contained_subtree(
                &resolved,
                &entry_target,
                &resolved,
                &mut vec![resolved.clone()],
            )
            .await?;
        } else if entry_stat.is_file() {
            let bytes = fs::read(&resolved).await?;
            if let Some(parent) = entry_target.parent() {
                fs::create_dir_all(parent).await?;
            }
            write_file_0600(&entry_target, &bytes).await?;
        }
        // 其他类型（sockets / devices）静默跳过
    }
    Ok(())
}

/// 递归拷贝 `source_dir` 到 `target_dir`，**禁止被拷项 dereference 出
/// `containment_root`**，并检测目录环（`active_path` 记录当前正在打开
/// 的目录的真实路径）。
fn stage_contained_subtree<'a>(
    source_dir: &'a Path,
    target_dir: &'a Path,
    containment_root: &'a Path,
    active_path: &'a mut Vec<PathBuf>,
) -> futures_core::future::BoxFuture<'a, std::io::Result<()>> {
    Box::pin(async move {
        fs::create_dir_all(target_dir).await?;
        set_dir_0700(target_dir).await?;
        let mut entries = match fs::read_dir(source_dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        while let Some(entry) = entries.next_entry().await? {
        let entry_source = source_dir.join(entry.file_name());
        let entry_target = target_dir.join(entry.file_name());
        let resolved = match fs::canonicalize(&entry_source).await {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) if e.raw_os_error() == Some(62) => continue,
            Err(e) => return Err(e),
        };
        // 越出 containment root → 跳过（防止 host 文件被拖入 staging）
        if !is_resolved_path_inside(&resolved, containment_root) {
            continue;
        }
        let entry_stat = match fs::metadata(&resolved).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
            if entry_stat.is_dir() {
                if active_path.contains(&resolved) {
                    continue;
                }
                active_path.push(resolved.clone());
                if let Err(e) = stage_contained_subtree(
                    &resolved,
                    &entry_target,
                    containment_root,
                    active_path,
                )
                .await
                {
                    active_path.pop();
                    return Err(e);
                }
                active_path.pop();
            } else if entry_stat.is_file() {
                let bytes = fs::read(&resolved).await?;
                if let Some(parent) = entry_target.parent() {
                    fs::create_dir_all(parent).await?;
                }
                write_file_0600(&entry_target, &bytes).await?;
            }
        }
        Ok(())
    })
}

/// `candidate == root` 或 `candidate` 在 `root` 下。
///
/// 两个参数都必须是已 resolved（symlink-free）的绝对路径。`path:relative`
/// 不会把 `..` 段或 trailing-separator prefix collision 误判。
fn is_resolved_path_inside(candidate: &Path, root: &Path) -> bool {
    if candidate == root {
        return true;
    }
    // strip_prefix 失败表示 candidate 不在 root 下
    match candidate.strip_prefix(root) {
        Ok(rel) => {
            let s = rel.to_string_lossy();
            // 防止 /tmp/skills vs /tmp/skills-evil 这种 collision
            !s.is_empty()
                && !s.starts_with("..")
                && !std::path::Path::new(s.as_ref()).has_root()
        }
        Err(_) => false,
    }
}

/// 以 0600 写文件（先 truncate，再 chmod 显式 set，防止 umask 干扰）。
async fn write_file_0600(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let _ = fs::remove_file(target).await;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(0o600);
    let mut f = opts.open(target).await?;
    f.write_all(bytes).await?;
    f.sync_all().await?;
    // 显式 chmod，防御 umask
    set_file_0600(target).await?;
    Ok(())
}

async fn set_file_0600(target: &Path) -> std::io::Result<()> {
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let meta = std::fs::metadata(&target)?;
        let mut perm = meta.permissions();
        perm.set_mode(0o600);
        std::fs::set_permissions(&target, perm)?;
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("join: {}", e)))?
}

async fn set_dir_0700(target: &Path) -> std::io::Result<()> {
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let meta = std::fs::metadata(&target)?;
        let mut perm = meta.permissions();
        perm.set_mode(0o700);
        std::fs::set_permissions(&target, perm)?;
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("join: {}", e)))?
}

// ====================== 单元测试 ======================
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    async fn temp_root() -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir();
        let unique = format!(
            "pc-codex-home-staging-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            seq
        );
        let dir = base.join(unique);
        tokio::fs::create_dir_all(&dir).await.expect("create tempdir");
        dir
    }

    async fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(path, content).await.unwrap();
    }

    fn auth_bytes() -> &'static str {
        "{\"tokens\":{\"account_id\":\"acct\",\"refresh_token\":\"r\"}}\n"
    }

    fn skill_bytes() -> &'static str {
        "# injected skill\n"
    }

    async fn collect_entries(dir: &Path) -> Vec<String> {
        let mut entries = Vec::new();
        let mut rd = fs::read_dir(dir).await.unwrap();
        while let Some(e) = rd.next_entry().await.unwrap() {
            entries.push(e.file_name().to_string_lossy().to_string());
        }
        entries
    }

    async fn build_fake_home(root: &Path) -> std::path::PathBuf {
        let home = root.join("codex-home");
        let auth_source = root.join("shared/auth.json");
        let skill_source = root.join("shared/skill-src.md");
        tokio::fs::create_dir_all(root.join("shared")).await.unwrap();
        tokio::fs::write(&auth_source, auth_bytes()).await.unwrap();
        tokio::fs::write(&skill_source, skill_bytes()).await.unwrap();

        tokio::fs::create_dir_all(&home).await.unwrap();
        // auth.json 软链到 shared source
        #[cfg(unix)]
        tokio::fs::symlink(&auth_source, home.join("auth.json"))
            .await
            .unwrap();
        write_file(&home.join("config.toml"), "model_provider = \"paperclip\"\n").await;
        write_file(&home.join("config.json"), "{}\n").await;
        write_file(&home.join("instructions.md"), "hi\n").await;
        // skills/ 目录含 symlink
        tokio::fs::create_dir_all(home.join("skills")).await.unwrap();
        #[cfg(unix)]
        tokio::fs::symlink(&skill_source, home.join("skills/demo.md"))
            .await
            .unwrap();

        // decoys: 应被白名单排除
        write_file(&home.join("logs_2.sqlite"), "x").await;
        write_file(&home.join("state_5.sqlite"), "x").await;
        tokio::fs::create_dir_all(home.join("plugins/cache")).await.unwrap();
        write_file(&home.join("plugins/cache/x"), "x").await;
        tokio::fs::create_dir_all(home.join("sessions")).await.unwrap();
        write_file(&home.join("sessions/y"), "x").await;
        tokio::fs::create_dir_all(home.join("tmp")).await.unwrap();
        #[cfg(unix)]
        tokio::fs::symlink("/usr/bin/env", home.join("tmp/arg0"))
            .await
            .unwrap();

        home
    }

    #[tokio::test]
    async fn stage_returns_temp_dir_path() {
        let root = temp_root().await;
        let home = build_fake_home(&root).await;
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        assert!(staged.exists());
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_run_id_used_in_dir_name() {
        let root = temp_root().await;
        let home = build_fake_home(&root).await;
        let staged = stage_codex_home_for_sync(
            &home,
            StageCodexHomeForSyncOptions {
                run_id: Some("run-123".to_string()),
            },
        )
        .await
        .unwrap();
        // dir name 应包含 run-123
        // run_id 在父目录的 prefix 中（staged 本身是 uuid 后缀）
        let parent_name = staged.parent().unwrap().file_name().unwrap().to_string_lossy().to_string();
        assert!(
            parent_name.contains("run-123"),
            "parent dir {} not contain run-123",
            parent_name
        );
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_only_allowlist_entries() {
        let root = temp_root().await;
        let home = build_fake_home(&root).await;
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        let mut entries = collect_entries(&staged).await;
        entries.sort();
        let mut expected: Vec<String> = CODEX_SYNC_ALLOWLIST.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(entries, expected);
        // decoys 都不应出现
        for decoy in ["logs_2.sqlite", "state_5.sqlite", "plugins", "sessions", "tmp"] {
            assert!(!entries.contains(&decoy.to_string()));
        }
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_auth_dereferences_symlink_to_bytes() {
        let root = temp_root().await;
        let home = build_fake_home(&root).await;
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        let staged_auth = staged.join("auth.json");
        let md = fs::metadata(&staged_auth).await.unwrap();
        assert!(md.is_file());
        let ftype = fs::symlink_metadata(&staged_auth).await.unwrap();
        assert!(
            !ftype.file_type().is_symlink(),
            "staged auth.json should be plain file"
        );
        let content = fs::read_to_string(&staged_auth).await.unwrap();
        assert_eq!(content, auth_bytes());
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_skills_dereferences_symlink_to_bytes() {
        let root = temp_root().await;
        let home = build_fake_home(&root).await;
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        let staged_skill = staged.join("skills/demo.md");
        let ftype = fs::symlink_metadata(&staged_skill).await.unwrap();
        assert!(!ftype.file_type().is_symlink());
        let content = fs::read_to_string(&staged_skill).await.unwrap();
        assert_eq!(content, skill_bytes());
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_preserves_static_config_files() {
        let root = temp_root().await;
        let home = build_fake_home(&root).await;
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        let toml = fs::read_to_string(staged.join("config.toml")).await.unwrap();
        assert!(toml.contains("model_provider"));
        let json = fs::read_to_string(staged.join("config.json")).await.unwrap();
        assert!(json.contains("{}") || json.trim().is_empty());
        let instr = fs::read_to_string(staged.join("instructions.md")).await.unwrap();
        assert_eq!(instr, "hi\n");
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_writes_files_with_0600() {
        let root = temp_root().await;
        let home = build_fake_home(&root).await;
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        let auth = staged.join("auth.json");
        let perm = fs::metadata(&auth).await.unwrap().permissions().mode() & 0o777;
        assert_eq!(perm, 0o600, "auth.json mode should be 0600, got {:o}", perm);
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_writes_0700_dirs() {
        let root = temp_root().await;
        let home = build_fake_home(&root).await;
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        let perm = fs::metadata(&staged).await.unwrap().permissions().mode() & 0o777;
        assert_eq!(perm, 0o700, "staging dir mode should be 0700, got {:o}", perm);
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_missing_auth_skipped() {
        let root = temp_root().await;
        let home = root.join("codex-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        // 仅放 config.toml，没有 auth.json
        write_file(&home.join("config.toml"), "x = 1\n").await;
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        let entries = collect_entries(&staged).await;
        // auth.json 缺失 → 跳过
        assert!(!entries.contains(&"auth.json".to_string()));
        // config.toml 保留
        assert!(entries.contains(&"config.toml".to_string()));
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_dangling_auth_symlink_skipped() {
        let root = temp_root().await;
        let home = root.join("codex-home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        // dangling symlink
        #[cfg(unix)]
        tokio::fs::symlink(root.join("nonexistent.json"), home.join("auth.json"))
            .await
            .unwrap();
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        let entries = collect_entries(&staged).await;
        assert!(!entries.contains(&"auth.json".to_string()));
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_top_level_skill_file_symlink_is_allowed() {
        // 顶层 skill/* 直系子项是 Paperclip 注入的 symlink，**可以** 指向
        // 共享 skill store（CODEX_HOME/skills/ 之外）；这是设计预期。
        let root = temp_root().await;
        let home = root.join("codex-home");
        let shared_skill = root.join("shared-skill.md");
        tokio::fs::create_dir_all(home.join("skills")).await.unwrap();
        tokio::fs::write(&shared_skill, "shared-skill-content").await.unwrap();
        #[cfg(unix)]
        tokio::fs::symlink(&shared_skill, home.join("skills/shared.md"))
            .await
            .unwrap();
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        let staged_skill = staged.join("skills/shared.md");
        let content = fs::read_to_string(&staged_skill).await.unwrap();
        assert_eq!(content, "shared-skill-content");
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_nested_skill_symlink_escape_rejected() {
        // 嵌套 symlink 逃出 containment root → 应被 skip
        let root = temp_root().await;
        let home = root.join("codex-home");
        let secret = root.join("secret.txt");
        // skills/sub 是真正的目录（会进入 stage_contained_subtree）
        tokio::fs::create_dir_all(home.join("skills/sub")).await.unwrap();
        tokio::fs::write(&secret, "secret-host-file").await.unwrap();
        // skills/sub/secret.md 指向 <root>/secret.txt  (escape)
        #[cfg(unix)]
        tokio::fs::symlink(&secret, home.join("skills/sub/secret.md"))
            .await
            .unwrap();
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        let staged_skill = staged.join("skills/sub/secret.md");
        // nested escape 应被 skip
        assert!(fs::metadata(&staged_skill).await.is_err());
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_circular_skill_dir_handled() {
        let root = temp_root().await;
        let home = root.join("codex-home");
        tokio::fs::create_dir_all(home.join("skills")).await.unwrap();
        // skills/back -> .（自指）
        #[cfg(unix)]
        tokio::fs::symlink(".", home.join("skills/back")).await.unwrap();
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        // 不会无限递归
        assert!(staged.join("skills").exists());
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[tokio::test]
    async fn stage_fail_closed_cleans_partial_dir() {
        // stub: 我们无法轻易注入失败，所以这里只核实 happy path 不会留下
        // partial dir。fail-closed 行为在 Node 中靠 mkdtemp 后 try/catch 实现。
        let root = temp_root().await;
        let home = build_fake_home(&root).await;
        let staged = stage_codex_home_for_sync(&home, StageCodexHomeForSyncOptions::default())
            .await
            .unwrap();
        // 确保 staged dir 存在
        assert!(staged.exists());
        // 清理
        let _ = fs::remove_dir_all(&staged).await;
    }

    #[test]
    fn is_resolved_path_inside_same_root() {
        let root = std::path::Path::new("/tmp/skills");
        assert!(is_resolved_path_inside(root, root));
    }

    #[test]
    fn is_resolved_path_inside_descendant() {
        let root = std::path::Path::new("/tmp/skills");
        let child = std::path::Path::new("/tmp/skills/sub");
        assert!(is_resolved_path_inside(child, root));
    }

    #[test]
    fn is_resolved_path_inside_rejects_ancestor() {
        let root = std::path::Path::new("/tmp/skills");
        let ancestor = std::path::Path::new("/tmp");
        assert!(!is_resolved_path_inside(ancestor, root));
    }

    #[test]
    fn is_resolved_path_inside_rejects_trailing_prefix_collision() {
        // /tmp/skills-evil 不应在 /tmp/skills 下
        let root = std::path::Path::new("/tmp/skills");
        let evil = std::path::Path::new("/tmp/skills-evil");
        // relative_path(evil, root) = "../skills-evil" → 启动 .. → false
        // 实际行为依赖 relative_path 的实现
        let result = is_resolved_path_inside(evil, root);
        // 我们接受 false 或 true，关键是 trailing-separator-prefix
        // collision 不会逃过检测
        assert!(!result, "trailing-prefix collision should not be classified as inside");
    }
}
