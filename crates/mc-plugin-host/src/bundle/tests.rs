//! `bundle.rs` 的测试面（拆出以守门 ⑩：`bundle.rs` 本体 800 行硬上限）。
//!
//! 写入规则与 `bundle.rs` 同片（M6-1）。测试是**门 ⑩ 之外的部分**：
//! 上游 409 行的 bundle 规则每条都要有正反例，所以这些用例不是可选的。

use super::*;
use std::io::Write;
use zip::write::SimpleFileOptions;

const MANIFEST: &str = r#"{
        "manifest_version": 1,
        "key": "com.example.panel",
        "name": "Example Panel",
        "version": "1.2.0",
        "author": { "name": "Example" },
        "scopes": ["issues:read"],
        "contributes": {
            "surfaces": [{ "key": "panel", "type": "issue_panel", "name": "Panel", "entry": "panel.js" }]
        }
    }"#;

const PANEL_JS: &str =
    "const root = document.getElementById(\"root\");\nroot.textContent = \"ok\";\n";

/// 打一个 zip：`(路径, 内容)` 列表，路径带 `/` 结尾就是目录条目。
fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        if name.ends_with('/') {
            writer
                .add_directory(*name, options)
                .expect("目录条目应当能写");
        } else {
            writer.start_file(*name, options).expect("条目应当能写");
            writer.write_all(content).expect("内容应当能写");
        }
    }
    writer.finish().expect("zip 应当能收口").into_inner()
}

#[test]
fn accepts_a_minimal_package() {
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, MANIFEST.as_bytes()),
        ("panel.js", PANEL_JS.as_bytes()),
    ]);
    let bundle = parse_bundle(&archive).expect("最小包应当合法");
    assert_eq!(bundle.manifest.key, "com.example.panel");
    assert_eq!(bundle.manifest_raw, MANIFEST.as_bytes());
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.file("panel.js"), Some(PANEL_JS.as_bytes()));
    assert_eq!(bundle.total_size(), PANEL_JS.len());
}

#[test]
fn accepts_one_leading_directory() {
    let archive = zip_bytes(&[
        ("my-plugin/", &[][..]),
        ("my-plugin/multica.plugin.json", MANIFEST.as_bytes()),
        ("my-plugin/panel.js", PANEL_JS.as_bytes()),
    ]);
    let bundle = parse_bundle(&archive).expect("一层目录前缀应当被允许");
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.file("panel.js"), Some(PANEL_JS.as_bytes()));
}

#[test]
fn extra_entries_are_dropped() {
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, MANIFEST.as_bytes()),
        ("panel.js", PANEL_JS.as_bytes()),
        ("unused.js", b"/* nothing references this */"),
        (".env", b"SECRET=1"),
    ]);
    let bundle = parse_bundle(&archive).expect("多余条目应当只是被丢掉");
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.file("unused.js"), None);
    assert_eq!(bundle.file(".env"), None);
}

#[test]
fn rejects_a_package_without_a_manifest() {
    let archive = zip_bytes(&[("panel.js", PANEL_JS.as_bytes())]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::MissingManifest { .. })
    ));
}

#[test]
fn rejects_two_manifest_directories_but_a_root_manifest_wins() {
    // 没有根 manifest、且有两个不同的候选目录 ⇒ 没有唯一答案，拒。
    let archive = zip_bytes(&[
        ("one/multica.plugin.json", MANIFEST.as_bytes()),
        ("one/panel.js", PANEL_JS.as_bytes()),
        ("two/multica.plugin.json", MANIFEST.as_bytes()),
        ("two/panel.js", PANEL_JS.as_bytes()),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::MultipleManifests)
    ));

    // 有根 manifest 时根**赢**（上游 `manifestPrefix` 见到根 manifest 立刻返回，与顺序无关）。
    let archive = zip_bytes(&[
        ("other/multica.plugin.json", MANIFEST.as_bytes()),
        (MANIFEST_FILENAME, MANIFEST.as_bytes()),
        ("panel.js", PANEL_JS.as_bytes()),
    ]);
    let bundle = parse_bundle(&archive).expect("根 manifest 应当胜出");
    assert_eq!(bundle.file("panel.js"), Some(PANEL_JS.as_bytes()));
}

#[test]
fn rejects_a_deeply_nested_manifest() {
    let archive = zip_bytes(&[("a/b/multica.plugin.json", MANIFEST.as_bytes())]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::ManifestTooDeep)
    ));
}

#[test]
fn rejects_a_missing_entry_the_manifest_declares() {
    let archive = zip_bytes(&[(MANIFEST_FILENAME, MANIFEST.as_bytes())]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::MissingEntry { entry }) if entry == "panel.js"
    ));
}

#[test]
fn rejects_traversal_and_absolute_entries() {
    for name in [
        "../evil.js",
        "/etc/passwd",
        "a/../b.js",
        "a//b.js",
        "./panel.js",
    ] {
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", PANEL_JS.as_bytes()),
            (name, b"x"),
        ]);
        assert!(
            matches!(parse_bundle(&archive), Err(BundleError::NotRelative { .. })),
            "{name} 应当被拒"
        );
    }
}

#[test]
fn rejects_backslashes_because_zip_paths_are_slashed() {
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, MANIFEST.as_bytes()),
        ("panel.js", PANEL_JS.as_bytes()),
        ("sub\\evil.js", b"x"),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::Backslash { .. })
    ));
}

#[test]
fn rejects_empty_and_oversized_archives() {
    assert!(matches!(parse_bundle(&[]), Err(BundleError::Empty)));
    let archive = vec![0u8; MAX_BUNDLE_SIZE + 1];
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::ArchiveTooLarge { .. })
    ));
}

#[test]
fn rejects_a_non_zip_body() {
    let error = parse_bundle(b"this is not a zip archive").expect_err("非 zip 必须被拒");
    assert!(matches!(error, BundleError::NotZip { .. }));
    assert_eq!(error.code(), "plugin_package_invalid");
}

#[test]
fn rejects_too_many_entries() {
    let mut entries: Vec<(String, &[u8])> =
        vec![(MANIFEST_FILENAME.to_owned(), MANIFEST.as_bytes())];
    let filler = "x";
    for index in 0..MAX_BUNDLE_ENTRIES {
        entries.push((format!("filler-{index}.bin"), filler.as_bytes()));
    }
    let owned: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, content)| (name.as_str(), *content))
        .collect();
    let archive = zip_bytes(&owned);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::TooManyEntries { .. })
    ));
}

#[test]
fn rejects_an_oversized_file_but_ignores_oversized_unreferenced_ones() {
    // 被 manifest 引用的文件按单文件上限拒。
    let big = vec![b'/'; MAX_BUNDLE_FILE_SIZE + 1];
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, MANIFEST.as_bytes()),
        ("panel.js", big.as_slice()),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::FileTooLarge { .. })
    ));

    // 没被引用的文件压根不解压 ⇒ 它多大都只是被丢掉（上游「惰性读」的原意）。
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, MANIFEST.as_bytes()),
        ("panel.js", PANEL_JS.as_bytes()),
        ("big.js", big.as_slice()),
    ]);
    let bundle = parse_bundle(&archive).expect("未引用的大文件应当只是被丢掉");
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.file("big.js"), None);
}

#[test]
fn rejects_a_package_whose_referenced_files_exceed_the_total() {
    let manifest = r#"{
            "manifest_version": 1,
            "key": "com.example.big",
            "name": "Big Panel",
            "version": "1.0.0",
            "author": { "name": "Example" },
            "scopes": ["issues:read"],
            "contributes": {
                "surfaces": [
                    { "key": "one", "type": "issue_panel", "name": "One", "entry": "one.js" },
                    { "key": "two", "type": "issue_panel", "name": "Two", "entry": "two.js" },
                    { "key": "three", "type": "issue_panel", "name": "Three", "entry": "three.js" },
                    { "key": "four", "type": "issue_panel", "name": "Four", "entry": "four.js" },
                    { "key": "five", "type": "issue_panel", "name": "Five", "entry": "five.js" }
                ]
            }
        }"#;
    let chunk = 900 * 1024;
    let error = parse_bundle_from_dir(|entry| {
        if entry == MANIFEST_FILENAME {
            return Ok(Some(manifest.as_bytes().to_vec()));
        }
        let mut content = vec![b' '; chunk];
        content.extend_from_slice(b"a = 1;\n");
        Ok(Some(content))
    })
    .expect_err("累计超过 4 MiB 必须被拒");
    assert!(matches!(error, BundleError::TotalTooLarge { .. }));
    assert_eq!(
        error.to_string(),
        "plugin package files exceed 4194304 bytes"
    );
}

#[test]
fn rejects_a_manifest_that_fails_its_own_validation() {
    let broken = MANIFEST.replace("\"version\": \"1.2.0\"", "\"version\": \"1.2\"");
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, broken.as_bytes()),
        ("panel.js", PANEL_JS.as_bytes()),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::Manifest(ManifestError::Invalid(_)))
    ));
}

#[test]
fn surface_entry_rules_are_the_do_d_counterexamples() {
    // 1) 空白 surface 入口
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, MANIFEST.as_bytes()),
        ("panel.js", b"   \n\t"),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::SurfaceEmpty { .. })
    ));

    // 2) 非 UTF-8
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, MANIFEST.as_bytes()),
        ("panel.js", &[0xff, 0xfe, 0x00]),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::SurfaceNotUtf8 { .. })
    ));

    // 3) 词法上就不是 JS（未闭合字符串）
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, MANIFEST.as_bytes()),
        ("panel.js", b"const a = 'oops;"),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::SurfaceInvalidJs { .. })
    ));

    // 4) 模块专用语法
    for source in [
        "import { open } from \"@multica/plugin-sdk\";",
        "export default function panel() {}",
        "await boot();",
        "console.log(import.meta.url);",
    ] {
        let archive = zip_bytes(&[
            (MANIFEST_FILENAME, MANIFEST.as_bytes()),
            ("panel.js", source.as_bytes()),
        ]);
        assert!(
            matches!(
                parse_bundle(&archive),
                Err(BundleError::SurfaceModuleSyntax { .. })
            ),
            "{source} 应当被拒"
        );
    }
}

#[test]
fn skill_resources_are_text_and_bounded() {
    let manifest = MANIFEST.replace(
            "\"surfaces\": [{ \"key\": \"panel\", \"type\": \"issue_panel\", \"name\": \"Panel\", \"entry\": \"panel.js\" }]",
            "\"resources\": [{ \"type\": \"skill\", \"key\": \"guide\", \"entry\": \"skills/guide/SKILL.md\" }]",
        );

    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, manifest.as_bytes()),
        ("skills/guide/SKILL.md", b"# Guide\n\nSteps.\n"),
    ]);
    let bundle = parse_bundle(&archive).expect("skill 包应当合法");
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(
        bundle.file("skills/guide/SKILL.md"),
        Some(&b"# Guide\n\nSteps.\n"[..])
    );

    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, manifest.as_bytes()),
        ("skills/guide/SKILL.md", b"  \n"),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::SkillEmpty { .. })
    ));

    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, manifest.as_bytes()),
        ("skills/guide/SKILL.md", &[0xff, 0x00]),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::SkillNotUtf8 { .. })
    ));

    let oversized = vec![b'#'; MAX_SKILL_BYTES + 1];
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, manifest.as_bytes()),
        ("skills/guide/SKILL.md", oversized.as_slice()),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::SkillTooLarge { .. })
    ));
}

#[test]
fn icon_must_exist_and_be_non_empty() {
    let manifest = MANIFEST.replace(
        "\"scopes\": [\"issues:read\"],",
        "\"scopes\": [\"issues:read\"],\n        \"icon\": \"icon.svg\",",
    );
    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, manifest.as_bytes()),
        ("panel.js", PANEL_JS.as_bytes()),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::MissingEntry { entry }) if entry == "icon.svg"
    ));

    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, manifest.as_bytes()),
        ("panel.js", PANEL_JS.as_bytes()),
        ("icon.svg", b""),
    ]);
    assert!(matches!(
        parse_bundle(&archive),
        Err(BundleError::IconEmpty { .. })
    ));

    let archive = zip_bytes(&[
        (MANIFEST_FILENAME, manifest.as_bytes()),
        ("panel.js", PANEL_JS.as_bytes()),
        ("icon.svg", b"<svg/>"),
    ]);
    let bundle = parse_bundle(&archive).expect("带图标的包应当合法");
    assert_eq!(bundle.files.len(), 2);
    assert_eq!(bundle.files[0].path, "icon.svg");
    assert_eq!(bundle.files[1].path, "panel.js");
}

#[test]
fn dir_channel_shares_the_same_judgement() {
    let files: Vec<(&str, &[u8])> = vec![
        (MANIFEST_FILENAME, MANIFEST.as_bytes()),
        ("panel.js", PANEL_JS.as_bytes()),
        ("extra.js", "// 未引用".as_bytes()),
    ];
    let bundle = parse_bundle_from_dir(|entry| {
        Ok(files
            .iter()
            .find(|(name, _)| *name == entry)
            .map(|(_, content)| (*content).to_vec()))
    })
    .expect("目录通道应当合法");
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.manifest_raw, MANIFEST.as_bytes());

    let error = parse_bundle_from_dir(|_| Ok(None)).expect_err("没有 manifest 必须被拒");
    assert_eq!(
        error.to_string(),
        "plugin package must contain multica.plugin.json"
    );

    let error = parse_bundle_from_dir(|entry| {
        Ok(Some(match entry {
            "multica.plugin.json" => MANIFEST.as_bytes().to_vec(),
            "panel.js" => b"export default 1;".to_vec(),
            _ => return Ok(None),
        }))
    })
    .expect_err("模块语法在目录通道也必须被拒");
    assert!(matches!(error, BundleError::SurfaceModuleSyntax { .. }));
}

#[test]
fn error_messages_match_upstream_wording() {
    assert_eq!(
        BundleError::FileTooLarge {
            entry: "panel.js".to_owned(),
            limit: MAX_BUNDLE_FILE_SIZE,
        }
        .to_string(),
        "plugin file \"panel.js\" exceeds 1048576 bytes"
    );
    assert_eq!(
        BundleError::MultipleManifests.to_string(),
        "plugin package contains more than one multica.plugin.json"
    );
    assert_eq!(
            BundleError::SurfaceModuleSyntax {
                entry: "panel.js".to_owned(),
            }
            .to_string(),
            "surface entry \"panel.js\" has top-level import/export/await or import.meta; a surface is a single classic script with no module graph, so bundle its dependencies in"
        );
    assert_eq!(
        BundleError::MissingEntry {
            entry: "skills/guide/SKILL.md".to_owned(),
        }
        .to_string(),
        "plugin package is missing \"skills/guide/SKILL.md\", which the manifest declares"
    );
}

#[test]
fn plain_relative_paths_follow_path_clean() {
    assert!(is_plain_relative("panel.js"));
    assert!(is_plain_relative("skills/guide/SKILL.md"));
    assert!(!is_plain_relative(""));
    assert!(!is_plain_relative("/panel.js"));
    assert!(!is_plain_relative("./panel.js"));
    assert!(!is_plain_relative("a/../panel.js"));
    assert!(!is_plain_relative("a//panel.js"));
    assert!(!is_plain_relative("a/"));
}
