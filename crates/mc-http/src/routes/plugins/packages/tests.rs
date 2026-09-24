//! `packages.rs` 的单元测试（拆成独立文件只为门 ⑩ 的 800 行/文件上限；
//! 本文件的被测面仍是 `packages.rs`，无独立语义）。
//!
//! 这里**不碰数据库、不造 zip**：包体一律走 M6-1 的本地目录通道
//! （`parse_bundle_from_dir` + 内存 map），这样「包发布路径的 JS 校验」正反例可以在
//! 无真库、无 `dev-dependencies`（`Cargo.toml` 是 M6-0 冻结面）的条件下跑。

use super::*;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use uuid::Uuid;

/// 一个最小的合法 manifest（surface 指向 `panel.js`）。
fn manifest_json(surface_type: &str) -> String {
    format!(
        r#"{{
          "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
          "version": "1.0.0", "author": {{"name": "Example"}},
          "scopes": ["issues:read"],
          "contributes": {{
            "surfaces": [
              {{"key": "panel", "type": "{surface_type}", "name": "Panel", "entry": "panel.js"}}
            ]
          }}
        }}"#
    )
}

/// 把一份「目录」喂给 M6-1 的本地通道解析器：不碰文件系统、不造 zip。
fn dir_bundle(files: &[(&str, Vec<u8>)]) -> Result<Bundle, BundleError> {
    let map: HashMap<String, Vec<u8>> = files
        .iter()
        .map(|(path, content)| ((*path).to_string(), content.clone()))
        .collect();
    parse_bundle_from_dir(move |entry| Ok(map.get(entry).cloned()))
}

fn demo_bundle(script: &[u8]) -> Result<Bundle, BundleError> {
    dir_bundle(&[
        (
            "multica.plugin.json",
            manifest_json("issue_panel").into_bytes(),
        ),
        ("panel.js", script.to_vec()),
    ])
}

fn version_row(version: &str) -> PackageVersionRow {
    PackageVersionRow {
        id: Uuid::nil(),
        package_id: Uuid::nil(),
        workspace_id: Uuid::nil(),
        version: version.to_owned(),
        manifest: sqlx::types::Json(serde_json::json!({})),
        digest: "0".repeat(64),
        size_bytes: 0,
        published_by: None,
        created_at: DateTime::<Utc>::default(),
    }
}

#[test]
fn router_is_constructible() {
    // 静态段（`/packages/local`）与参数段（`/packages/:packageId`）在同一前缀下共存，
    // 冲突的话 router() 会直接 panic。
    let _ = router();
}

/// DoD：包发布路径的 JS 校验**正例**（合法 surface 过 `publish` 的校验半段）。
#[test]
fn publish_validation_accepts_a_classic_surface_script() {
    let bundle = demo_bundle(b"console.log('hi');\n").expect("bundle parses");
    assert!(require_supported(&bundle.manifest).is_ok());
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.file("panel.js"), Some(&b"console.log('hi');\n"[..]));
}

/// DoD：JS 校验**反例**（模块专用语法）—— 与 zip 通道同源（M6-1 的 `js` 扫描器）。
#[test]
fn publish_validation_rejects_module_syntax() {
    let error = demo_bundle(b"export function boot() {}\n").expect_err("module syntax");
    assert!(matches!(error, BundleError::SurfaceModuleSyntax { .. }));
    // 路由层把它折成 400 + 上游前缀。
    let rendered = PluginError::invalid(format!("local plugin package is invalid: {error}"));
    assert_eq!(rendered.status, StatusCode::BAD_REQUEST);
    assert!(rendered
        .message
        .starts_with("local plugin package is invalid: "));
    assert!(rendered.message.contains("panel.js"));
}

/// 反例（词法层面）：空白入口 / 非 UTF-8 入口 / manifest 点名的文件不在包里。
#[test]
fn publish_validation_rejects_broken_entries() {
    let empty = demo_bundle(b"   \n").expect_err("empty surface");
    assert!(matches!(empty, BundleError::SurfaceEmpty { .. }));

    let not_utf8 = demo_bundle(&[0xff, 0xfe]).expect_err("non-utf8 surface");
    assert!(matches!(not_utf8, BundleError::SurfaceNotUtf8 { .. }));

    let missing = dir_bundle(&[(
        "multica.plugin.json",
        manifest_json("issue_panel").into_bytes(),
    )])
    .expect_err("entry missing");
    assert!(matches!(missing, BundleError::MissingEntry { .. }));
}

/// 反例：manifest 自己非法（版本号不是 v1）—— 错误码仍是 400 家族。
#[test]
fn publish_validation_rejects_an_unknown_manifest_version() {
    let raw =
        manifest_json("issue_panel").replace("\"manifest_version\": 1", "\"manifest_version\": 2");
    let error = dir_bundle(&[
        ("multica.plugin.json", raw.into_bytes()),
        ("panel.js", b"console.log(1);\n".to_vec()),
    ])
    .expect_err("manifest v2 is rejected");
    assert!(
        error.code().starts_with("plugin_manifest_invalid"),
        "unexpected code {}",
        error.code()
    );
}

/// 反例：宿主不支持的能力 ⇒ 422（上游 `PluginErrorIncompatible` + `capabilityMessage`）。
#[test]
fn publish_validation_rejects_unsupported_capabilities() {
    let bundle = dir_bundle(&[
        (
            "multica.plugin.json",
            manifest_json("sidebar_panel").into_bytes(),
        ),
        ("panel.js", b"console.log(1);\n".to_vec()),
    ])
    .expect("capabilities are checked after parsing");
    let error = require_supported(&bundle.manifest).expect_err("sidebar_panel unsupported");
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error
        .message
        .starts_with("This plugin declares capabilities that are not enabled yet: "));
    assert!(error.message.contains("surface sidebar_panel"));
}

/// 摘要只跟着**内容**走：同一份内容两次构建得到同一个 digest（`files` 顺序无关）。
#[test]
fn bundle_digest_is_content_addressed() {
    let first = demo_bundle(b"console.log(1);\n").expect("bundle");
    let second = demo_bundle(b"console.log(1);\n").expect("bundle");
    assert_eq!(bundle_digest(&first), bundle_digest(&second));
    assert_eq!(bundle_digest(&first).len(), 64);
    let changed = demo_bundle(b"console.log(2);\n").expect("bundle");
    assert_ne!(bundle_digest(&first), bundle_digest(&changed));
}

/// 上传通道：版本号原样落库（撞车由唯一索引变 409，不在这一层改号）。
#[test]
fn upload_keeps_the_manifest_version() {
    let existing = vec![version_row("1.0.0")];
    assert_eq!(
        resolve_publish_version("1.0.0", &existing, false).expect("upload"),
        "1.0.0"
    );
    assert_eq!(
        resolve_publish_version("2.0.0", &existing, false).expect("upload"),
        "2.0.0"
    );
}

/// 开发通道：撞车改落 `+dev.N`，且 N 取已有后缀的最大值 + 1。
#[test]
fn dev_loop_numbers_the_suffix_monotonically() {
    let free = resolve_publish_version("1.0.0", &[], true).expect("first");
    assert_eq!(free, "1.0.0");

    let taken = vec![version_row("1.0.0")];
    assert_eq!(
        resolve_publish_version("1.0.0", &taken, true).expect("second"),
        "1.0.0+dev.1"
    );

    let mixed = vec![
        version_row("1.0.0+dev.2"),
        version_row("1.0.0"),
        version_row("1.0.0+dev.1"),
        // 别的版本号的后缀不参与计数。
        version_row("0.9.0+dev.7"),
    ];
    assert_eq!(
        resolve_publish_version("1.0.0", &mixed, true).expect("third"),
        "1.0.0+dev.3"
    );
}

/// 反例：版本号已经贴到长度上限 ⇒ 400，不截断、不改号。
#[test]
fn dev_loop_refuses_a_version_with_no_room_for_a_suffix() {
    let long = "a".repeat(MAX_VERSION_LENGTH);
    let existing = vec![version_row(&long)];
    let error = resolve_publish_version(&long, &existing, true).expect_err("no room");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error
        .message
        .contains("leaves no room for a development suffix"));
}

/// 本地通道的目录名：单段、不以 `.` 开头（`..` 也挡在这一层）。
#[test]
fn local_names_are_single_directory_names() {
    assert!(validate_local_name("demo").is_ok());
    assert!(validate_local_name("demo-2").is_ok());
    for bad in ["", "a/b", r"a\b", ".hidden", "..", "../etc"] {
        let error = validate_local_name(bad).expect_err(bad);
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("single directory name"));
    }
}

/// 条目读取：不在目录里 ⇒ `Ok(None)`（交给 M6-1 报 `MissingEntry`），越界 ⇒ 拒。
#[test]
fn local_entries_cannot_escape_their_directory() {
    let root = std::path::Path::new("/tmp/multica-plugin-dir/demo");
    assert!(matches!(read_local_entry(root, "nope.js"), Ok(None)));
    let escaped = read_local_entry(root, "../secret.js").expect_err("escape");
    assert!(matches!(escaped, BundleError::Read { .. }));
    assert_eq!(
        clean_path(&root.join("skills/./a.md")),
        root.join("skills/a.md")
    );
}

/// 路径参数不是 uuid ⇒ 404（不是 400）。
#[test]
fn package_ids_must_be_uuids() {
    let error = parse_package_id("not-a-uuid").expect_err("bad id");
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "plugin package not found");
    assert!(parse_package_id("00000000-0000-0000-0000-000000000000").is_ok());
}
