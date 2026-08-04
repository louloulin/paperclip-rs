//! folder 模块的纯规则单测（不依赖数据库）。

use super::*;
use crate::folder::hierarchy::descendant_ids_from_rows;
use crate::RepoError;
use crate::folder::slug::{
    is_reserved_root_slug, normalize_folder_slug, MAX_FOLDER_DEPTH, RESERVED_CHILD_ROOT_SYSTEM_KEYS,
};
use crate::folder::view::build_folder_views;

fn row(id: &str, parent: Option<&str>, slug: &str, name: &str) -> FolderRow {
    FolderRow {
        id: Uuid::parse_str(id).unwrap(),
        company_id: Uuid::nil(),
        kind: "skill".to_string(),
        parent_id: parent.map(Uuid::parse_str).transpose().unwrap(),
        name: name.to_string(),
        slug: slug.to_string(),
        system_key: None,
        color: None,
        position: 0,
        created_at: pc_core::Timestamp::now(),
        updated_at: pc_core::Timestamp::now(),
    }
}

#[test]
fn folder_kind_strings_round_trip() {
    for k in [FolderKind::Routine, FolderKind::Skill] {
        assert_eq!(FolderKind::parse(k.as_str()), Some(k));
    }
    assert_eq!(FolderKind::parse("nope"), None);
}

#[test]
fn folder_patch_double_option_logic() {
    let p = FolderPatch::default();
    let has_new = p.parent_id.is_some();
    assert!(!has_new);
    let p2 = FolderPatch {
        parent_id: Some(None),
        ..Default::default()
    };
    assert!(p2.parent_id.is_some());
    assert!(p2.parent_id.flatten().is_none());
}

#[test]
fn new_folder_validation() {
    let bad = NewFolder {
        company_id: Uuid::new_v4(),
        kind: FolderKind::Routine,
        parent_id: None,
        name: "".into(),
        slug: "abc".into(),
        system_key: None,
        color: None,
        position: 0,
    };
    assert!(bad.name.trim().is_empty());
}

#[test]
fn normalize_folder_slug_basic() {
    assert_eq!(normalize_folder_slug("Hello World"), "hello-world");
    assert_eq!(normalize_folder_slug("  Multi   Space  "), "multi-space");
    assert_eq!(normalize_folder_slug("café"), "caf");
    assert_eq!(normalize_folder_slug("a---b"), "a-b");
    assert_eq!(normalize_folder_slug("!!!"), "folder");
    assert_eq!(normalize_folder_slug(""), "folder");
    assert_eq!(normalize_folder_slug("My Folder 123"), "my-folder-123");
}

#[test]
fn reserved_root_slug_matches_node_contract() {
    assert!(is_reserved_root_slug(FolderKind::Skill, None, "bundled"));
    assert!(is_reserved_root_slug(FolderKind::Skill, None, "my"));
    assert!(is_reserved_root_slug(FolderKind::Skill, None, "projects"));
    assert!(!is_reserved_root_slug(FolderKind::Skill, None, "misc"));
    // routine kind 不参与 reserved
    assert!(!is_reserved_root_slug(FolderKind::Routine, None, "bundled"));
    // 即使 slug 是 bundled，如果 parent 存在也不算 reserved
    let parent = Uuid::new_v4();
    assert!(!is_reserved_root_slug(
        FolderKind::Skill,
        Some(parent),
        "bundled"
    ));
}

#[test]
fn max_folder_depth_matches_node() {
    assert_eq!(MAX_FOLDER_DEPTH, 4);
}

#[test]
fn reserved_child_root_system_keys_matches_node() {
    for k in ["my", "projects"] {
        assert!(RESERVED_CHILD_ROOT_SYSTEM_KEYS.contains(&k));
    }
}

#[test]
fn descendant_ids_returns_self_and_children() {
    let a = row("11111111-1111-1111-1111-111111111111", None, "a", "A");
    let b = row(
        "22222222-2222-2222-2222-222222222222",
        Some("11111111-1111-1111-1111-111111111111"),
        "b",
        "B",
    );
    let c = row(
        "33333333-3333-3333-3333-333333333333",
        Some("22222222-2222-2222-2222-222222222222"),
        "c",
        "C",
    );
    let rows = vec![a.clone(), b.clone(), c.clone()];
    let result = descendant_ids_from_rows(&rows, a.id).unwrap();
    assert_eq!(result.len(), 3);
    assert!(result.contains(&a.id));
    assert!(result.contains(&b.id));
    assert!(result.contains(&c.id));
}

#[test]
fn descendant_ids_returns_error_for_missing_root() {
    let rows = vec![row(
        "11111111-1111-1111-1111-111111111111",
        None,
        "a",
        "A",
    )];
    let err = descendant_ids_from_rows(&rows, Uuid::new_v4());
    assert!(matches!(err, Err(RepoError::Invalid(_))));
}

#[test]
fn descendant_ids_returns_error_for_cycle() {
    // b -> c -> b 形成环，从 b 出发应检测到环。
    let b = row(
        "22222222-2222-2222-2222-222222222222",
        Some("33333333-3333-3333-3333-333333333333"),
        "b",
        "B",
    );
    let c = row(
        "33333333-3333-3333-3333-333333333333",
        Some("22222222-2222-2222-2222-222222222222"),
        "c",
        "C",
    );
    let rows = vec![b.clone(), c];
    let err = descendant_ids_from_rows(&rows, b.id);
    assert!(matches!(err, Err(RepoError::Invalid(_))));
}

#[test]
fn build_folder_views_computes_path_and_depth() {
    let a = row("11111111-1111-1111-1111-111111111111", None, "alpha", "Alpha");
    let b = row(
        "22222222-2222-2222-2222-222222222222",
        Some("11111111-1111-1111-1111-111111111111"),
        "beta",
        "Beta",
    );
    let c = row(
        "33333333-3333-3333-3333-333333333333",
        Some("22222222-2222-2222-2222-222222222222"),
        "gamma",
        "Gamma",
    );
    let rows = vec![a.clone(), b.clone(), c.clone()];
    let views = build_folder_views(&rows).unwrap();
    let va = views.get(&a.id).unwrap();
    let vb = views.get(&b.id).unwrap();
    let vc = views.get(&c.id).unwrap();
    assert_eq!(va.path, "alpha");
    assert_eq!(va.depth, 1);
    assert_eq!(vb.path, "alpha/beta");
    assert_eq!(vb.depth, 2);
    assert_eq!(vc.path, "alpha/beta/gamma");
    assert_eq!(vc.depth, 3);
}

#[test]
fn build_folder_views_detects_cycle() {
    // b -> c -> b 形成环，build_folder_views 应检测到。
    let b = row(
        "22222222-2222-2222-2222-222222222222",
        Some("33333333-3333-3333-3333-333333333333"),
        "b",
        "B",
    );
    let c = row(
        "33333333-3333-3333-3333-333333333333",
        Some("22222222-2222-2222-2222-222222222222"),
        "c",
        "C",
    );
    let rows = vec![b, c];
    let err = build_folder_views(&rows);
    assert!(matches!(err, Err(RepoError::Invalid(_))));
}

#[test]
fn build_folder_views_detects_dangling_parent() {
    let bad = row(
        "11111111-1111-1111-1111-111111111111",
        Some("99999999-9999-9999-9999-999999999999"),
        "a",
        "A",
    );
    let err = build_folder_views(&[bad]);
    assert!(matches!(err, Err(RepoError::Invalid(_))));
}

#[test]
fn move_folder_item_kind_round_trip() {
    for k in [MoveFolderItemKind::Routine, MoveFolderItemKind::Skill] {
        assert_eq!(MoveFolderItemKind::parse(k.as_str()), Some(k));
    }
    assert_eq!(MoveFolderItemKind::parse("nope"), None);
}
