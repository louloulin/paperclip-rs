//! `tree.rs`（tree 面上的目录解析 + 支持文件枚举）的单元测试。
//!
//! 支持文件的**并发下载**与上限算术走 e2e（`tests/skills/import.rs`，配假服务器），
//! 这里只钉「纯函数 + 顺序」那部分：hints / 匹配 / 分区 / 常规路径顺序 / MD 抽取。

use super::*;

#[test]
fn skill_dir_and_extraction_match_the_upstream_forms() {
    let entries = vec![
        GithubTreeEntry {
            path: "SKILL.md".into(),
            kind: "blob".into(),
            size: 10,
        },
        GithubTreeEntry {
            path: "skills/foo/SKILL.md".into(),
            kind: "blob".into(),
            size: 10,
        },
        GithubTreeEntry {
            path: "skills/foo".into(),
            kind: "tree".into(),
            size: 0,
        },
        GithubTreeEntry {
            path: "skills/foo/notes.md".into(),
            kind: "blob".into(),
            size: 3,
        },
    ];
    assert_eq!(
        extract_skill_md_paths(&entries),
        vec!["SKILL.md".to_string(), "skills/foo/SKILL.md".to_string()]
    );
    assert_eq!(skill_dir_from_skill_file_path("SKILL.md"), "");
    assert_eq!(skill_dir_from_skill_file_path("a/b/SKILL.md"), "a/b");
    // tree 只认 blob：目录名叫 SKILL.md 也不算。
    assert!(!extract_skill_md_paths(&[GithubTreeEntry {
        path: "x/SKILL.md".into(),
        kind: "tree".into(),
        size: 0,
    }])
    .contains(&"x/SKILL.md".to_string()));
}

#[test]
fn hints_and_path_matching_mirror_the_upstream_heuristics() {
    // 整串 / 后缀 / 单词三波，长度 < 3 与重复都丢掉（顺序也一致）。
    assert_eq!(
        skill_name_hints("review-helper"),
        vec!["review-helper", "helper", "review"]
    );
    assert_eq!(skill_name_hints("ab"), Vec::<String>::new());
    assert_eq!(
        skill_name_hints("api-gateway-skill"),
        vec![
            "api-gateway-skill",
            "gateway-skill",
            "skill",
            "api",
            "gateway"
        ]
    );
    // 大小写不敏感（上游先 `strings.ToLower`）。
    assert_eq!(
        skill_name_hints("Review-Helper"),
        vec!["review-helper", "helper", "review"]
    );

    assert!(is_likely_skill_path_match("foo", "skills/foo/SKILL.md"));
    assert!(is_likely_skill_path_match(
        "foo-bar",
        "plugins/my-foo-bar/SKILL.md"
    ));
    // ⚠️ 上游第三支 `strings.Contains(hint, base)`：目录基名比提示短/长都可能命中，
    // 但「完全不搭界」必须落到 remaining。
    assert!(!is_likely_skill_path_match(
        "unrelated",
        "skills/foo/SKILL.md"
    ));
}

#[test]
fn partition_keeps_the_preferred_first_and_preserves_order() {
    let paths = vec![
        "skills/other/SKILL.md".to_string(),
        "skills/review-helper/SKILL.md".to_string(),
        "skills/third/SKILL.md".to_string(),
    ];
    let (preferred, remaining) = partition_skill_md_paths("review-helper", &paths);
    assert_eq!(preferred, vec!["skills/review-helper/SKILL.md"]);
    assert_eq!(
        remaining,
        vec!["skills/other/SKILL.md", "skills/third/SKILL.md"]
    );
}

#[test]
fn conventional_paths_follow_the_upstream_order_and_never_include_the_root() {
    assert_eq!(
        conventional_skill_md_paths("foo"),
        vec![
            "skills/foo/SKILL.md",
            ".claude/skills/foo/SKILL.md",
            "plugin/skills/foo/SKILL.md",
            "foo/SKILL.md",
        ]
    );
    // 根 `SKILL.md` 不在列表里：否则「仓库名 == slug」的撞车修复会被绕过。
    assert!(!conventional_skill_md_paths("foo").contains(&"SKILL.md".to_string()));
}
