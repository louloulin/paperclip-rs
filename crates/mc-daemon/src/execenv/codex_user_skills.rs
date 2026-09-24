//! 把用户级的 `~/.codex/skills/` **链接**进每任务的 `CODEX_HOME`。
//!
//! - **上游**：`execenv/codex_user_skills.go`（106 行）。
//! - **写者**：M6-9（本 slice）。
//!
//! Codex 是唯一一个把 HOME 重定向到每任务目录（`CODEX_HOME`）的 runtime，所以不补这一步
//! 它就**永远看不见**用户自己的 `~/.codex/skills/` 内容。
//!
//! ## 链接，绝不拷贝
//!
//! 拷贝会把整棵 skill 树记到**每一个**任务目录上（一个装了几个 npm 系 skill 的用户就是
//! 100 MB），而且 `hydrateCodexSkills` 每次任务启动都在关键路径上重做一遍，只要 issue 还
//! 开着就没有任何 GC 路径回收它。同一个 per-task home 里另外两个共享资源（sessions、
//! 插件缓存）也早就用链接 —— 理由一样：skill 目录对 CLI 是只读输入，拷贝顺手买来的
//! 「写隔离」并没有人要。
//!
//! ## workspace skill 优先
//!
//! 名字（按 [`sanitize_skill_name`] 归一后）与某个 workspace skill 相同的用户 skill 在这里
//! **跳过**；随后 `writeSkillFiles` 把 workspace 版本写进干净的槽位。它**不会**写穿一个
//! 不是自己建的链接（`allocateCollisionFreeSkillDir` 用 `Lstat` 探测，留下来的链接算占用），
//! 于是 workspace skill 落到 `-multica` 兄弟目录。
//!
//! ## 与上游的差异（逐条登记在 `docs/32` §9.9）
//!
//! - `resolveSharedCodexHome` 与 `createDirLink` 在 `codex_home.go` / `codex_home_link*.go`
//!   里（不在本 slice 写集）。本文件用**同一语义的窄实现**：前者 `$CODEX_HOME`（绝对值，
//!   失败回落 `~/.codex`，再回落 `$TMPDIR/.codex`），后者在 unix 上是 `symlink`。
//! - **Windows 未接**：上游 `createDirLink` 在 Windows 上建 junction；本 slice 的
//!   `create_dir_link` 在非 unix 平台返回可区分错误（Windows 面整波为登记缺口）。
//! - **每 skill 失败只记日志并跳过**（上游同）：一个坏掉的用户 skill 不该拦下整个任务；
//!   返回 `Err` 只留给「影响所有 skill」的失败（列共享目录、建目标目录）。
//! - 本 slice 的端口不加 `slog` 依赖，用 `tracing`。

use std::fs;
use std::path::{Path, PathBuf};

use crate::execenv::sidecar::{create_dir_all, sanitize_skill_name, SidecarError};
use crate::skill::{env_or, user_home, SkillForEnv};

/// 用户共享的 Codex home（上游 `resolveSharedCodexHome`）。
#[must_use]
pub fn resolve_shared_codex_home() -> PathBuf {
    if let Some(value) = crate::skill::non_empty_env("CODEX_HOME") {
        let path = PathBuf::from(value);
        if let Ok(absolute) = fs::canonicalize(&path) {
            return absolute;
        }
        if path.is_absolute() {
            return path;
        }
    }
    match user_home() {
        Ok(home) => home.join(".codex"),
        // 最后一条退路（上游 `os.TempDir()/.codex`）。
        Err(_) => std::env::temp_dir().join(".codex"),
    }
}

/// 把用户 skill 链接进 `codex_home/skills`（上游 `seedUserCodexSkills`）。
///
/// 返回「链接了几个」便于调用方观测；`Err` 只表示「影响所有 skill」的失败。
pub fn seed_user_codex_skills(
    codex_home: &Path,
    workspace_skills: &[SkillForEnv],
) -> Result<usize, SidecarError> {
    seed_user_codex_skills_from(&resolve_shared_codex_home(), codex_home, workspace_skills)
}

/// 同 [`seed_user_codex_skills`]，但显式给共享目录（测试与「共享 home 已解析」的调用方用）。
pub fn seed_user_codex_skills_from(
    shared_skills_dir: &Path,
    codex_home: &Path,
    workspace_skills: &[SkillForEnv],
) -> Result<usize, SidecarError> {
    let shared_skills_dir = shared_skills_dir.join("skills");
    match fs::metadata(&shared_skills_dir) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(0),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => {
            return Err(SidecarError::Io {
                op: "stat shared skills dir",
                path: shared_skills_dir,
                source: err,
            });
        }
    }

    let reserved: Vec<String> = workspace_skills
        .iter()
        .map(|skill| sanitize_skill_name(&skill.name))
        .collect();

    let entries = fs::read_dir(&shared_skills_dir).map_err(|err| SidecarError::Io {
        op: "read shared skills dir",
        path: shared_skills_dir.clone(),
        source: err,
    })?;
    let mut entries: Vec<fs::DirEntry> = entries.filter_map(std::result::Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);

    let target_skills_dir = codex_home.join("skills");
    let mut linked = 0usize;
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.is_empty() || name.starts_with('.') {
            continue;
        }
        if reserved.contains(&sanitize_skill_name(&name)) {
            tracing::info!(name = %name, "execenv: codex user-skill yields to workspace skill");
            continue;
        }
        let source = shared_skills_dir.join(&name);
        // lark-cli 这类安装器把每个 skill 做成指向共享 `~/.agents/skills/<name>/` 的链接；
        // 解析它，让每任务的链接指向真目录（而不是另一个链接）。
        let resolved = match fs::canonicalize(&source) {
            Ok(resolved) => resolved,
            Err(err) => {
                tracing::warn!(name = %name, error = %err, "execenv: codex user-skill resolve failed");
                continue;
            }
        };
        match fs::metadata(&resolved) {
            Ok(metadata) if metadata.is_dir() => {}
            _ => continue,
        }
        // `hydrateCodexSkills` 每次 seed 前会清空 skills 目录，所以父目录通常不存在；
        // 惰性创建，于是「一个合格用户 skill 都没有」的任务连 skills 目录都不会有。
        create_dir_all(&target_skills_dir)?;
        let destination = target_skills_dir.join(&name);
        // 只摘链接本身，绝不删链接指向的东西（`RemoveAll` 对符号链接就是 unlink）。
        if let Err(err) = remove_link_or_entry(&destination) {
            tracing::warn!(name = %name, error = %err, "execenv: codex user-skill clean dst failed");
            continue;
        }
        match create_dir_link(&resolved, &destination) {
            Ok(()) => linked += 1,
            Err(err) => {
                tracing::warn!(name = %name, error = %err, "execenv: codex user-skill link failed");
            }
        }
    }
    Ok(linked)
}

/// 摘掉目标位置上的**链接或空目录**（上游 `os.RemoveAll(dst)` 的窄化版）。
///
/// 对符号链接：`remove_file` 只会摘链接。对真目录：**不删**（上游 `RemoveAll` 会，
/// 但那正是「拷贝顺手买来的写隔离」的另一面；本 slice 只处理自己上一轮留下的链接）。
fn remove_link_or_entry(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                fs::remove_file(path)
            } else if metadata.is_dir() && fs::read_dir(path)?.next().is_none() {
                fs::remove_dir(path)
            } else {
                // 真目录（或有内容的目录）不是我们的：留给它。
                Ok(())
            }
        }
    }
}

/// 建一个指向目录的链接（上游 `createDirLink`；unix 是 `symlink`，Windows 是 junction）。
#[cfg(unix)]
pub fn create_dir_link(source: &Path, destination: &Path) -> Result<(), SidecarError> {
    std::os::unix::fs::symlink(source, destination).map_err(|err| SidecarError::Io {
        op: "create dir link",
        path: destination.to_path_buf(),
        source: err,
    })
}

/// 非 unix 平台：明确失败（Windows junction 面是整波登记缺口）。
#[cfg(not(unix))]
pub fn create_dir_link(source: &Path, destination: &Path) -> Result<(), SidecarError> {
    Err(SidecarError::Invalid(format!(
        "directory links are not implemented on this platform: {} -> {}",
        destination.display(),
        source.display()
    )))
}

/// `env_or` 的再导出：调用方拿 `$CODEX_HOME` 时不该自己再拼一遍 env 读取。
#[must_use]
pub fn codex_home_or_default() -> PathBuf {
    env_or("CODEX_HOME", resolve_shared_codex_home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("mc-daemon-codex-skills-{name}-{nanos:x}"));
            fs::create_dir_all(&dir).expect("create test dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            self.0.as_path()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn skill(name: &str) -> SkillForEnv {
        SkillForEnv {
            name: name.to_string(),
            content: String::new(),
        }
    }

    fn seed_shared(root: &Path, name: &str) {
        let dir = root.join("skills").join(name);
        fs::create_dir_all(&dir).expect("mkdir skill");
        fs::write(dir.join("SKILL.md"), "---\nname: x\n---\n").expect("write skill");
    }

    #[cfg(unix)]
    #[test]
    fn user_skills_are_linked_not_copied() {
        let shared = TestDir::new("shared");
        let task = TestDir::new("task");
        seed_shared(shared.path(), "alpha");
        seed_shared(shared.path(), "beta");

        let linked = seed_user_codex_skills_from(shared.path(), task.path(), &[skill("alpha")])
            .expect("seed");
        // alpha 被 workspace skill 占名 ⇒ 只链接 beta。
        assert_eq!(linked, 1);
        let destination = task.path().join("skills").join("beta");
        assert!(fs::symlink_metadata(&destination)
            .expect("dst exists")
            .file_type()
            .is_symlink());
        // 链接指向真目录，内容读得到。
        assert!(destination.join("SKILL.md").is_file());
        assert!(!task.path().join("skills").join("alpha").exists());
    }

    #[cfg(unix)]
    #[test]
    fn reseeding_replaces_its_own_link_and_never_touches_a_real_directory() {
        let shared = TestDir::new("reseed");
        let task = TestDir::new("reseed-task");
        seed_shared(shared.path(), "alpha");

        seed_user_codex_skills_from(shared.path(), task.path(), &[]).expect("first");
        // 第二遍：链接已存在，仍然要成功（摘掉自己的链接再建）。
        let linked = seed_user_codex_skills_from(shared.path(), task.path(), &[]).expect("second");
        assert_eq!(linked, 1);

        // 一个**真目录**占了同一个名字 ⇒ 不碰它。
        let target = task.path().join("skills").join("beta");
        fs::create_dir_all(target.join("nested")).expect("real dir");
        fs::write(target.join("keep.txt"), "keep").expect("write");
        seed_shared(shared.path(), "beta");
        let linked = seed_user_codex_skills_from(shared.path(), task.path(), &[]).expect("third");
        assert_eq!(linked, 1);
        assert!(target.join("keep.txt").is_file(), "user dir must survive");
    }

    #[test]
    fn a_missing_shared_directory_is_not_an_error() {
        let shared = TestDir::new("empty-shared");
        let task = TestDir::new("empty-task");
        assert_eq!(
            seed_user_codex_skills_from(shared.path(), task.path(), &[]).expect("seed"),
            0
        );
        assert!(!task.path().join("skills").exists());
    }

    #[test]
    fn dotfiles_and_plain_files_are_skipped() {
        let shared = TestDir::new("dotfiles");
        let task = TestDir::new("dotfiles-task");
        fs::create_dir_all(shared.path().join("skills")).expect("mkdir");
        fs::write(shared.path().join("skills").join(".hidden"), "x").expect("write");
        fs::write(shared.path().join("skills").join("not-a-dir"), "x").expect("write");
        let linked = seed_user_codex_skills_from(shared.path(), task.path(), &[]).expect("seed");
        assert_eq!(linked, 0);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_shared_skills_are_resolved_before_linking() {
        let shared = TestDir::new("symlinked");
        let task = TestDir::new("symlinked-task");
        let real = shared.path().join("real-skills").join("alpha");
        fs::create_dir_all(&real).expect("mkdir real");
        fs::write(real.join("SKILL.md"), "---\nname: a\n---\n").expect("write");
        fs::create_dir_all(shared.path().join("skills")).expect("mkdir skills");
        std::os::unix::fs::symlink(&real, shared.path().join("skills").join("alpha"))
            .expect("symlink");

        let linked = seed_user_codex_skills_from(shared.path(), task.path(), &[]).expect("seed");
        assert_eq!(linked, 1);
        let destination = task.path().join("skills").join("alpha");
        assert_eq!(
            fs::canonicalize(&destination).expect("canonical"),
            fs::canonicalize(&real).expect("canonical real"),
            "the per-task link must point at the real directory, not at another link"
        );
    }

    #[test]
    fn shared_codex_home_falls_back_to_the_home_directory() {
        // `$CODEX_HOME` 未设置时是 `~/.codex`（或临时目录退路）。
        let home = resolve_shared_codex_home();
        assert!(home.is_absolute(), "{home:?}");
    }
}
