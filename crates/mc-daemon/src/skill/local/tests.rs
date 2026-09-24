//! 本模块的用例（上游对应的 `*_test.go`）。
//!
//! 拆成独立文件的两个理由：① 上游就是把测试放同一个包的 `_test.go` 里（本 slice 沿用它
//! 的分工）；② 门 ⑩ 是**逐文件** 800 行硬上限，实现与用例放在一个文件里会让实现本身被
//! 用例的行数挤出上限。

use super::*;
use crate::skill::{ROOT_PLUGIN, ROOT_PROVIDER, ROOT_UNIVERSAL};
use std::time::{SystemTime, UNIX_EPOCH};

struct TestHome(PathBuf);

impl TestHome {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mc-daemon-local-skill-{name}-{nanos:x}"));
        fs::create_dir_all(&dir).expect("create home");
        Self(dir)
    }

    fn path(&self) -> &Path {
        self.0.as_path()
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, content).expect("write");
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 环境变量在这些用例里被改写，所以必须串行跑（`HOME` 是进程级的）。
fn with_home<T>(home: &Path, action: impl FnOnce() -> T) -> T {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = std::env::var_os("HOME");
    std::env::set_var("HOME", home);
    let out = action();
    match previous {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    out
}

#[test]
fn provider_roots_match_the_upstream_switch() {
    let home = Path::new("/home/tester");
    for (provider, want) in [
        ("claude", Some("/home/tester/.claude/skills")),
        ("codebuddy", Some("/home/tester/.codebuddy/skills")),
        ("codex", Some("/home/tester/.codex/skills")),
        ("copilot", Some("/home/tester/.copilot/skills")),
        ("opencode", Some("/home/tester/.config/opencode/skills")),
        ("codearts", Some("/home/tester/.codeartsdoer/skills")),
        ("deveco", Some("/home/tester/.config/deveco/skills")),
        ("openclaw", Some("/home/tester/.openclaw/skills")),
        ("pi", Some("/home/tester/.pi/agent/skills")),
        ("cursor", Some("/home/tester/.cursor/skills")),
        ("kimi", Some("/home/tester/.kimi/skills")),
        ("reasonix", Some("/home/tester/.reasonix/skills")),
        ("dsh", Some("/home/tester/.dsh/skills")),
        ("kiro", Some("/home/tester/.kiro/skills")),
        ("qoder", Some("/home/tester/.qoder/skills")),
        ("qoderclicn", Some("/home/tester/.qoder-cn/skills")),
        ("traecli", Some("/home/tester/.traecli/skills")),
        (
            "antigravity",
            Some("/home/tester/.gemini/antigravity-cli/skills"),
        ),
        ("grok", Some("/home/tester/.grok/skills")),
        ("qwen", Some("/home/tester/.qwen/skills")),
        ("mcode", Some("/home/tester/.minimax/skills")),
        ("omp", Some("/home/tester/.omp/agent/skills")),
        // 上游要走 Hermes home 解析器，本 slice 明确不支持。
        ("hermes", None),
        ("unknown-runtime", None),
    ] {
        assert_eq!(
            provider_root(provider, home).as_deref(),
            want.map(Path::new),
            "provider={provider}"
        );
    }
}

#[test]
fn roots_are_provider_first_and_universal_last() {
    let home = TestHome::new("roots");
    with_home(home.path(), || {
        let roots = local_skill_roots_for_provider("claude")
            .expect("home")
            .expect("supported");
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].path, home.path().join(".claude").join("skills"));
        assert_eq!(roots[0].kind, ROOT_PROVIDER);
        assert_eq!(roots[1].path, home.path().join(".agents").join("skills"));
        assert_eq!(roots[1].kind, ROOT_UNIVERSAL);

        assert_eq!(
            local_skill_roots_for_provider("redacted").expect("home"),
            None
        );
    });
}

#[test]
fn lists_skills_with_frontmatter_and_file_counts() {
    let home = TestHome::new("list");
    home.write(
        ".claude/skills/deploy/SKILL.md",
        "---\nname: Deploy helper\ndescription: ships things\n---\nbody\n",
    );
    home.write(".claude/skills/deploy/scripts/run.sh", "echo hi\n");
    home.write(".claude/skills/deploy/LICENSE", "ignore me\n");
    // 嵌套布局（opencode 的 `release/reporter/SKILL.md`）。
    home.write(
        ".agents/skills/release/reporter/SKILL.md",
        "no frontmatter\n",
    );

    with_home(home.path(), || {
        let ProviderSkills::Supported(skills) = list_runtime_local_skills("claude").expect("list")
        else {
            panic!("claude must be supported");
        };
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].key, "deploy");
        assert_eq!(skills[0].name, "Deploy helper");
        assert_eq!(skills[0].description, "ships things");
        assert_eq!(skills[0].root, ROOT_PROVIDER);
        assert!(skills[0].can_disable);
        // 支持文件 1 个（LICENSE 被忽略）+ SKILL.md 本身。
        assert_eq!(skills[0].file_count, 2);
        assert!(skills[0].source_path.starts_with('~'));

        assert_eq!(skills[1].key, "release/reporter");
        assert_eq!(skills[1].name, "reporter");
        assert_eq!(skills[1].root, ROOT_UNIVERSAL);
    });
}

#[test]
fn provider_root_wins_a_same_key_collision() {
    let home = TestHome::new("dedupe");
    home.write(
        ".claude/skills/shared/SKILL.md",
        "---\nname: from provider\n---\n",
    );
    home.write(
        ".agents/skills/shared/SKILL.md",
        "---\nname: from universal\n---\n",
    );

    with_home(home.path(), || {
        let ProviderSkills::Supported(skills) = list_runtime_local_skills("claude").expect("list")
        else {
            panic!("supported");
        };
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "from provider");
    });
}

#[test]
fn a_directory_without_skill_md_is_not_a_skill() {
    let home = TestHome::new("no-main");
    home.write(
        ".claude/skills/outer/inner/SKILL.md",
        "---\nname: nested\n---\n",
    );

    with_home(home.path(), || {
        let ProviderSkills::Supported(skills) = list_runtime_local_skills("claude").expect("list")
        else {
            panic!("supported");
        };
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].key, "outer/inner");
    });
}

#[test]
fn unsupported_provider_is_not_an_empty_list() {
    let home = TestHome::new("unsupported");
    with_home(home.path(), || {
        assert_eq!(
            list_runtime_local_skills("nope").expect("list"),
            ProviderSkills::Unsupported
        );
        assert_eq!(
            load_runtime_local_skill_bundle("nope", "a").expect("load"),
            None
        );
    });
}

#[test]
fn collects_supporting_files_sorted_and_skips_binaries() {
    let home = TestHome::new("collect");
    home.write("skills/a/SKILL.md", "---\nname: a\n---\n");
    home.write("skills/a/b.md", "b\n");
    home.write("skills/a/a.md", "a\n");
    home.write("skills/a/.hidden", "no\n");
    home.write("skills/a/logo.png", "not really png\n");
    home.write("skills/a/nested/c.md", "c\n");
    home.write("skills/a/readme", "plain\n");

    let skill_dir = home.path().join("skills/a");
    let files = collect_local_skill_files(&skill_dir, true).expect("collect");
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, vec!["a.md", "b.md", "nested/c.md", "readme"]);
    assert_eq!(files[0].content, "a\n");

    let discovery = collect_local_skill_files(&skill_dir, false).expect("collect");
    assert_eq!(discovery.len(), files.len());
    assert!(discovery.iter().all(|file| file.content.is_empty()));
}

#[test]
fn collection_skips_nul_bytes_and_invalid_utf8() {
    let home = TestHome::new("binary");
    home.write("skills/b/SKILL.md", "---\nname: b\n---\n");
    let dir = home.path().join("skills/b");
    fs::write(dir.join("nul.txt"), b"ok\0not ok").expect("write nul");
    fs::write(dir.join("bad.txt"), [0xff, 0xfe, 0x00, 0x01]).expect("write bad");

    let files = collect_local_skill_files(&dir, true).expect("collect");
    assert!(files.is_empty(), "unexpected files: {files:?}");
}

#[test]
fn collection_skips_symlinked_children() {
    let home = TestHome::new("symlink");
    home.write("skills/c/SKILL.md", "---\nname: c\n---\n");
    home.write("elsewhere/real.md", "real\n");
    let dir = home.path().join("skills/c");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(home.path().join("elsewhere/real.md"), dir.join("link.md"))
            .expect("symlink file");
        std::os::unix::fs::symlink(home.path().join("elsewhere"), dir.join("linked-dir"))
            .expect("symlink dir");
        let files = collect_local_skill_files(&dir, true).expect("collect");
        assert!(files.is_empty(), "unexpected files: {files:?}");
    }
}

#[test]
fn main_file_over_one_mib_is_rejected() {
    let home = TestHome::new("big-main");
    let dir = home.path().join("skills/big");
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(
        dir.join(SKILL_MAIN_FILE),
        vec![b'x'; usize::try_from(MAX_LOCAL_SKILL_FILE_SIZE + 1).expect("fits in usize")],
    )
    .expect("write");
    let err = read_local_skill_main_file(&dir).expect_err("must reject");
    assert!(err.to_string().contains("exceeds"), "{err}");
}

#[test]
fn too_many_supporting_files_is_rejected() {
    let home = TestHome::new("too-many");
    let dir = home.path().join("skills/many");
    fs::create_dir_all(&dir).expect("mkdir");
    for index in 0..=MAX_LOCAL_SKILL_FILE_COUNT {
        fs::write(dir.join(format!("f{index:03}.md")), "x").expect("write");
    }
    let err = collect_local_skill_files(&dir, false).expect_err("must reject");
    assert!(err.to_string().contains("256"), "{err}");
}

#[test]
fn load_matches_what_the_listing_surfaced() {
    let home = TestHome::new("load");
    home.write(
        ".claude/skills/deploy/SKILL.md",
        "---\nname: Deploy helper\ndescription: d\n---\nbody\n",
    );
    home.write(".claude/skills/deploy/scripts/run.sh", "echo hi\n");

    with_home(home.path(), || {
        let bundle = load_runtime_local_skill_bundle("claude", "deploy")
            .expect("load")
            .expect("supported");
        assert_eq!(bundle.name, "Deploy helper");
        assert_eq!(bundle.description, "d");
        assert!(bundle.content.ends_with("body\n"));
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.files[0].path, "scripts/run.sh");
        assert_eq!(bundle.files[0].content, "echo hi\n");

        // 目录没有 SKILL.md ⇒ 与列表一致地「这个根没有它」。
        fs::create_dir_all(home.path().join(".claude/skills/empty")).expect("mkdir");
        let err = load_runtime_local_skill_bundle("claude", "empty").expect_err("not found");
        assert!(matches!(err, SkillExecError::NotFound), "{err:?}");

        let err = load_runtime_local_skill_bundle("claude", "../escape").expect_err("key");
        assert!(matches!(err, SkillExecError::InvalidSkillKey(_)), "{err:?}");
    });
}

#[test]
fn plugin_root_prefix_is_applied_when_present() {
    // 插件根本 slice 不产出，但前缀逻辑由同一条码路承担 —— 直接构造一个根来钉它。
    let home = TestHome::new("plugin-prefix");
    home.write("plugin-skills/design/SKILL.md", "---\nname: nope\n---\n");
    let root = LocalSkillRoot::plugin(
        home.path().join("plugin-skills"),
        "paper-desktop:",
        "paper-desktop",
    );
    let mut skills = Vec::new();
    let mut visited = Vec::new();
    enumerate_local_skills(
        "claude",
        &root,
        &root.path,
        &root.path,
        0,
        &mut visited,
        &mut skills,
    );
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].key, "paper-desktop:design");
    // 插件根的展示名回落成 key（上游 plugin != "" 分支）。
    assert_eq!(skills[0].name, "paper-desktop:design");
    assert_eq!(skills[0].root, ROOT_PLUGIN);
    assert_eq!(skills[0].plugin, "paper-desktop");
}

#[test]
fn symlink_cycles_do_not_loop_forever() {
    let home = TestHome::new("cycle");
    let dir = home.path().join("skills/loop");
    fs::create_dir_all(&dir).expect("mkdir");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&dir, dir.join("self")).expect("symlink cycle");
        let root = LocalSkillRoot::new(home.path().join("skills"), ROOT_PROVIDER);
        let mut skills = Vec::new();
        let mut visited = Vec::new();
        // 只要不挂死就算过（`visited` 用解析后的真实路径）。
        enumerate_local_skills(
            "claude",
            &root,
            &root.path,
            &root.path,
            0,
            &mut visited,
            &mut skills,
        );
        assert!(skills.is_empty());
    }
}
