//! sidecar 落盘的**这半套**：拒绝覆盖的写入、目录创建、skill slug 分配、frontmatter 切片。
//!
//! - **上游**：`execenv/sidecar_manifest.go`（469 行）里与本 slice 六个注入口相关的那几件，
//!   加上 `execenv/context.go` 的 `sanitizeSkillName` / `frontmatterParts`。
//! - **写者**：M6-9（本 slice）。
//!
//! ## 为什么不整块移植 `sidecar_manifest.go`
//!
//! 该文件是**任务环境准备**那条腿的写账本（记录 prepare 期间新建的每个目录/文件，好让
//! `CleanupSidecars` 能按「纯删除」收尾），它的写集归 execenv 的任务准备切片，不在
//! M6-9 的动作清单里。本 slice 只需要其中三件**语义独立**的东西：
//!
//! 1. **拒绝覆盖**（上游 `recordWriteFile` 的 `errPathPreExists`）：目标已存在 —— 无论是
//!    普通文件、符号链接还是目录 —— 一律**不碰**并报错。这条不是洁癖：先覆盖再在退出时
//!    「不删」（因为不是我们建的）就等于**毁两次**用户数据（写坏一次、留在盘上第二次），
//!    所以「拒绝覆盖」是消除这半个失败模式的唯一办法。
//! 2. **slug 分配**（上游 `sanitizeSkillName` / `skillSlugCandidate` /
//!    `allocateCollisionFreeSkillDir`）：`skill_visibility`、`runtime_skill_policy`、
//!    `codex_user_skills` 三个消费者必须**同意**同一个 slug 序列，否则「列在清单里的名字」
//!    与「盘上的目录」会分叉（上游为一个 bug 专门把这对函数抽出来共用）。
//! 3. **frontmatter 切片**（上游 `frontmatterParts` / `frontmatterBodyStart`）：判断
//!    `disable-model-invocation` 必须先知道 frontmatter 块的边界。
//!
//! 其余的（manifest 落盘、`CleanupSidecars`、`ensureSkillFrontmatter` 的合成）**不做**，
//! 逐条登记在 `docs/32` §9.9。
//!
//! ## 与上游的差异
//!
//! 本 slice 没有 manifest 参数：写入**不记账**。因此它只用于「本来就不会被回收」的
//! sidecar（`cursor-data/` 之类随 env root 一起被 GC 删掉的东西）。等任务准备切片落地后，
//! 这三个函数应改为接收 manifest（或直接换回上游那套 `record*`）。

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// sidecar 写入的失败。
#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error("execenv: {op} {path}: {source}")]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// 目标已存在（上游 `errPathPreExists` 的等价物）。
    #[error("execenv: refuse to overwrite pre-existing path {0}")]
    PathPreExists(PathBuf),
    #[error("execenv: {0}")]
    Invalid(String),
}

impl SidecarError {
    fn io(op: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            op,
            path: path.to_path_buf(),
            source,
        }
    }

    /// 是不是「目标已存在」这一类（调用方据此翻成人话，例如
    /// `managed mcp_config would overwrite existing <path>`）。
    #[must_use]
    pub fn is_pre_existing(&self) -> bool {
        matches!(self, Self::PathPreExists(_))
    }
}

/// 本模块的结果别名。
pub type Result<T, E = SidecarError> = std::result::Result<T, E>;

/// `create_dir_all`，但**先探一遍**：绝不跟随一个已存在的**符号链接**目录去写。
///
/// 上游 `recordMkdirAll` 会记录新建的每一级目录；本 slice 只保证「不穿透链接」这一条
/// 安全性质（用户可以在 skills 目录里放链接，我们不该顺着它写到别处）。
pub fn create_dir_all(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(SidecarError::Invalid(format!(
                "refuse to write through symlinked directory {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            return Ok(());
        }
        return Err(SidecarError::PathPreExists(path.to_path_buf()));
    }
    fs::create_dir_all(path).map_err(|err| SidecarError::io("create dir", path, err))
}

/// 写一个**新**文件：目标已存在（含符号链接）就报 [`SidecarError::PathPreExists`]，一个
/// 字节都不碰（上游 `recordWriteFile` 的核心不变式）。
pub fn write_new_file(path: &Path, data: &[u8]) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(SidecarError::PathPreExists(path.to_path_buf()));
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    // 0600：sidecar 里可能有 token（`mcp-auth.json` 的链接目标、approvals）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|err| SidecarError::io("create file", path, err))?;
    file.write_all(data)
        .map_err(|err| SidecarError::io("write file", path, err))?;
    file.sync_all()
        .map_err(|err| SidecarError::io("sync file", path, err))
}

/// 删除一个**符号链接**（若目标不存在则静默；上游 `removeCursorMcpAuthFile`）。
///
/// `remove_file` 只摘链接本身，绝不删它指向的东西。
pub fn remove_file_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(SidecarError::io("remove file", path, err)),
    }
}

/// skill 名 → 安全目录名（上游 `sanitizeSkillName`）。
///
/// 逐字三条：小写、`[^a-z0-9]+` → `-`、去掉首尾 `-`；全空 ⇒ `skill`。
/// ⚠️ 它**不是单射**：`"A B"` 与 `"A-B"` 都归到 `a-b` —— 这正是
/// [`skill_slug_candidate`] 与 [`allocate_collision_free_skill_dir`] 必须与其配对的原因。
#[must_use]
pub fn sanitize_skill_name(name: &str) -> String {
    let lowered = name.trim().to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut pending_dash = false;
    for ch in lowered.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch);
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        "skill".to_string()
    } else {
        out
    }
}

/// 第 `attempt` 个候选 slug（上游 `skillSlugCandidate`）：`base`、「`base-multica`」、
/// 之后是「`base-multica-<n>`」。
///
/// ⚠️ **两个调用方必须同意这个序列**：探测文件系统的
/// [`allocate_collision_free_skill_dir`] 与在内存里给一批 skill 去重的
/// [`crate::execenv::skill_visibility::resolve_skill_slugs`]。两边一旦分叉，skill 会被
/// 「按一个名字列出、按另一个名字写入」。
#[must_use]
pub fn skill_slug_candidate(base_slug: &str, attempt: usize) -> String {
    match attempt {
        0 => base_slug.to_string(),
        1 => format!("{base_slug}-multica"),
        other => format!("{base_slug}-multica-{other}"),
    }
}

/// 在 `skills_parent` 下挑一个**当前不存在**的目录（上游 `allocateCollisionFreeSkillDir`）。
///
/// 上限 64 次（上游 `maxAttempts`）：同一个 slug 撞上千次是上游 bug 而不是现实状态，
/// 报错逼调用方把问题暴露出来，而不是无限循环。
pub fn allocate_collision_free_skill_dir(
    skills_parent: &Path,
    base_slug: &str,
) -> Result<(String, PathBuf)> {
    const MAX_ATTEMPTS: usize = 64;
    for attempt in 0..MAX_ATTEMPTS {
        let candidate = skill_slug_candidate(base_slug, attempt);
        let path = skills_parent.join(&candidate);
        match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok((candidate, path));
            }
            Err(err) => return Err(SidecarError::io("stat candidate", &path, err)),
            // 已存在（含链接）⇒ 换下一个候选。
            Ok(_) => {}
        }
    }
    Err(SidecarError::Invalid(format!(
        "allocate collision-free skill dir under {}: exhausted {MAX_ATTEMPTS} attempts for base {base_slug:?}",
        skills_parent.display()
    )))
}

/// `SKILL.md` 的开头 `---` 行之后的偏移（上游 `frontmatterBodyStart`）。
#[must_use]
pub fn frontmatter_body_start(content: &str) -> Option<usize> {
    if content.starts_with("---\n") {
        return Some(4);
    }
    if content.starts_with("---\r\n") {
        return Some(5);
    }
    None
}

/// 把正文切成 `(frontmatter 体, 正文, 有没有 frontmatter)`（上游 `frontmatterParts`）。
///
/// 收尾判据（上游专门集中在一处，避免「合法性检查」与「重新合成」两条码路对块边界
/// 各持一词）：必须是一行**只有** `---`，后面跟 `\n` / `\r\n` / 文件尾。
/// `----` 或 `--- text` 都不算收尾，继续往后找。
///
/// 与上游的差异：上游在找不到收尾时返回 `("", content, false)`（**原样**返回正文），
/// 本实现照此 —— 调用方据此知道「没有可解析的 frontmatter」。
#[must_use]
pub fn frontmatter_parts(content: &str) -> (&str, &str, bool) {
    let Some(start) = frontmatter_body_start(content) else {
        return ("", content, false);
    };
    let rest = &content[start..];
    let bytes = rest.as_bytes();
    let mut search_from = 0usize;
    loop {
        let Some(offset) = rest[search_from..].find("\n---") else {
            return ("", content, false);
        };
        let close_at = search_from + offset;
        let after = &rest[close_at + "\n---".len()..];
        if after.is_empty() || after == "\r" {
            return (&rest[..close_at], "", true);
        }
        if let Some(tail) = after.strip_prefix('\n') {
            return (&rest[..close_at], tail, true);
        }
        if let Some(tail) = after.strip_prefix("\r\n") {
            return (&rest[..close_at], tail, true);
        }
        search_from = close_at + "\n---".len();
        if search_from >= bytes.len() {
            return ("", content, false);
        }
    }
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
            let dir = std::env::temp_dir().join(format!("mc-daemon-sidecar-{name}-{nanos:x}"));
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

    #[test]
    fn sanitize_skill_name_matches_the_upstream_regex() {
        assert_eq!(sanitize_skill_name("PR review"), "pr-review");
        assert_eq!(sanitize_skill_name("  A   B  "), "a-b");
        assert_eq!(sanitize_skill_name("A-B"), "a-b");
        assert_eq!(sanitize_skill_name("--x--"), "x");
        assert_eq!(sanitize_skill_name("部署 skill"), "skill");
        assert_eq!(sanitize_skill_name(""), "skill");
        assert_eq!(sanitize_skill_name("***"), "skill");
        assert_eq!(sanitize_skill_name("a1_b2"), "a1-b2");
    }

    #[test]
    fn slug_candidates_follow_the_shared_sequence() {
        assert_eq!(skill_slug_candidate("a-b", 0), "a-b");
        assert_eq!(skill_slug_candidate("a-b", 1), "a-b-multica");
        assert_eq!(skill_slug_candidate("a-b", 2), "a-b-multica-2");
        assert_eq!(skill_slug_candidate("a-b", 7), "a-b-multica-7");
    }

    #[test]
    fn allocate_skips_every_occupied_candidate_including_symlinks() {
        let dir = TestDir::new("allocate");
        let (slug, path) = allocate_collision_free_skill_dir(dir.path(), "review").expect("first");
        assert_eq!(slug, "review");
        assert_eq!(path, dir.path().join("review"));

        fs::create_dir(dir.path().join("review")).expect("occupy");
        let (slug, path) = allocate_collision_free_skill_dir(dir.path(), "review").expect("second");
        assert_eq!(slug, "review-multica");
        assert_eq!(path, dir.path().join("review-multica"));

        fs::create_dir(dir.path().join("review-multica")).expect("occupy");
        let (slug, _) = allocate_collision_free_skill_dir(dir.path(), "review").expect("third");
        assert_eq!(slug, "review-multica-2");

        // 一个**符号链接**也算被占用（`Lstat` 语义，不穿透）。
        #[cfg(unix)]
        {
            let target = dir.path().join("elsewhere");
            fs::create_dir(&target).expect("target");
            std::os::unix::fs::symlink(&target, dir.path().join("review-multica-2"))
                .expect("symlink");
            let (slug, _) =
                allocate_collision_free_skill_dir(dir.path(), "review").expect("fourth");
            assert_eq!(slug, "review-multica-3");
        }
    }

    #[test]
    fn write_new_file_refuses_to_touch_a_pre_existing_path() {
        let dir = TestDir::new("write-new");
        let target = dir.path().join("mcp.json");
        write_new_file(&target, b"{\"a\":1}").expect("first write");

        let err = write_new_file(&target, b"clobbered").expect_err("must refuse");
        assert!(err.is_pre_existing());
        assert_eq!(fs::read(&target).expect("read"), b"{\"a\":1}");

        // 目录也算「已存在」。
        let dir_target = dir.path().join(".cursor");
        fs::create_dir(&dir_target).expect("dir");
        assert!(write_new_file(&dir_target, b"x")
            .expect_err("refuse")
            .is_pre_existing());
    }

    #[cfg(unix)]
    #[test]
    fn write_new_file_and_create_dir_all_do_not_follow_symlinks() {
        let dir = TestDir::new("symlink-guard");
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).expect("outside");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink dir");

        let err = create_dir_all(&link).expect_err("must refuse symlinked dir");
        assert!(matches!(err, SidecarError::Invalid(_)), "{err:?}");

        let file_target = outside.join("file.json");
        let file_link = dir.path().join("file-link.json");
        std::os::unix::fs::symlink(&file_target, &file_link).expect("symlink file");
        let err = write_new_file(&file_link, b"x").expect_err("must refuse symlink target");
        assert!(err.is_pre_existing());
        assert!(!file_target.exists(), "the link target must not be created");
    }

    #[test]
    fn create_dir_all_is_idempotent_for_real_directories() {
        let dir = TestDir::new("mkdir");
        let nested = dir.path().join("a").join("b");
        create_dir_all(&nested).expect("first");
        create_dir_all(&nested).expect("second is a no-op");
        assert!(nested.is_dir());
    }

    #[test]
    fn remove_file_if_present_is_idempotent() {
        let dir = TestDir::new("remove");
        let target = dir.path().join("mcp-auth.json");
        remove_file_if_present(&target).expect("missing is fine");
        write_new_file(&target, b"{}").expect("write");
        remove_file_if_present(&target).expect("remove");
        assert!(!target.exists());
    }

    #[test]
    fn frontmatter_parts_matches_the_upstream_boundary_rules() {
        assert_eq!(
            frontmatter_parts("no frontmatter"),
            ("", "no frontmatter", false)
        );
        assert_eq!(
            frontmatter_parts("---\nname: a\n---\nbody"),
            ("name: a", "body", true)
        );
        assert_eq!(
            frontmatter_parts("---\r\nname: a\r\n---\r\nbody"),
            ("name: a\r", "body", true)
        );
        // 文件尾就是收尾行。
        assert_eq!(
            frontmatter_parts("---\nname: a\n---"),
            ("name: a", "", true)
        );
        // `----` 不是收尾行；继续找真正的收尾。
        assert_eq!(
            frontmatter_parts("---\na: 1\n----\nmore\n---\nbody"),
            ("a: 1\n----\nmore", "body", true)
        );
        // 没有收尾 ⇒ 整篇当正文。
        assert_eq!(frontmatter_parts("---\na: 1\n"), ("", "---\na: 1\n", false));
    }
}
