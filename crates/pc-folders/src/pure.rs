#![forbid(unsafe_code)]

//! Folders pure helpers — 1:1 port of paperclip/server/src/services/folders.ts
//!
//! R717: zero-DB helpers extracted from the folders service.

use serde::Serialize;
use serde_json::Value;

pub const MAX_FOLDER_DEPTH: u32 = 4;
pub const RESERVED_ROOT_SLUGS: &[&str] = &["bundled", "my", "projects"];
pub const RESERVED_CHILD_ROOT_SYSTEM_KEYS: &[&str] = &["my", "projects"];
pub const DEFAULT_FOLDER_SLUG: &str = "folder";

pub fn is_postgres_error(error: Option<&Value>, code: &str) -> bool {
    match error {
        Some(Value::Object(map)) => map.get("code").and_then(Value::as_str) == Some(code),
        _ => false,
    }
}

pub fn normalize_name(name: &str) -> String {
    name.trim().to_string()
}

pub fn normalize_color(color: Option<&str>) -> Option<String> {
    let c = color?;
    let trimmed = c.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

pub fn normalize_folder_slug(value: &str) -> String {
    let normalized: String = value
        .chars()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .collect::<String>()
        .to_lowercase();
    let mut out = String::with_capacity(normalized.len());
    let mut last_dash = true;
    for ch in normalized.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        return DEFAULT_FOLDER_SLUG.to_string();
    }
    let mut collapsed = String::with_capacity(trimmed.len());
    let mut prev_dash = false;
    for ch in trimmed.chars() {
        if ch == '-' {
            if !prev_dash { collapsed.push(ch); }
            prev_dash = true;
        } else {
            collapsed.push(ch);
            prev_dash = false;
        }
    }
    collapsed
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderView {
    pub id: String,
    pub parent_id: Option<String>,
    pub slug: String,
    pub name: String,
    pub path: String,
    pub depth: u32,
    pub kind: String,
    pub color: Option<String>,
    pub system_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FolderRowInput {
    pub id: String,
    pub parent_id: Option<String>,
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub color: Option<String>,
    pub system_key: Option<String>,
}

pub fn build_folder_views(rows: &[FolderRowInput]) -> Result<Vec<FolderView>, String> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut by_id: BTreeMap<&str, &FolderRowInput> = BTreeMap::new();
    for row in rows { by_id.insert(row.id.as_str(), row); }
    let mut views: BTreeMap<String, FolderView> = BTreeMap::new();
    let mut visiting: BTreeSet<String> = BTreeSet::new();

    fn resolve(
        row: &FolderRowInput,
        by_id: &BTreeMap<&str, &FolderRowInput>,
        views: &mut BTreeMap<String, FolderView>,
        visiting: &mut BTreeSet<String>,
    ) -> Result<FolderView, String> {
        if let Some(v) = views.get(&row.id) { return Ok(v.clone()); }
        if !visiting.insert(row.id.clone()) {
            return Err("Folder hierarchy contains a cycle".to_string());
        }
        let parent_view = match row.parent_id.as_deref() {
            Some(pid) => match by_id.get(pid) {
                Some(p) => Some(resolve(p, by_id, views, visiting)?),
                None => return Err("Folder hierarchy contains an invalid parent".to_string()),
            },
            None => None,
        };
        visiting.remove(&row.id);
        let path = match parent_view.as_ref() {
            Some(p) => format!("{}/{}", p.path, row.slug),
            None => row.slug.clone(),
        };
        let depth = parent_view.as_ref().map(|p| p.depth + 1).unwrap_or(1);
        let view = FolderView {
            id: row.id.clone(),
            parent_id: row.parent_id.clone(),
            slug: row.slug.clone(),
            name: row.name.clone(),
            path, depth,
            kind: row.kind.clone(),
            color: row.color.clone(),
            system_key: row.system_key.clone(),
        };
        views.insert(row.id.clone(), view.clone());
        Ok(view)
    }

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(resolve(row, &by_id, &mut views, &mut visiting)?);
    }
    Ok(out)
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_postgres_error_match() {
        assert!(is_postgres_error(Some(&json!({"code": "23505"})), "23505"));
        assert!(!is_postgres_error(Some(&json!({"code": "23505"})), "23503"));
        assert!(!is_postgres_error(Some(&json!("oops")), "23505"));
        assert!(!is_postgres_error(None, "23505"));
    }

    #[test]
    fn normalize_name_trims() {
        assert_eq!(normalize_name("  hello  "), "hello");
        assert_eq!(normalize_name(""), "");
    }

    #[test]
    fn normalize_color_variants() {
        assert_eq!(normalize_color(Some("#fff")).as_deref(), Some("#fff"));
        assert_eq!(normalize_color(Some("  #fff  ")).as_deref(), Some("#fff"));
        assert_eq!(normalize_color(Some("")), None);
        assert_eq!(normalize_color(Some("   ")), None);
        assert_eq!(normalize_color(None), None);
    }

    #[test]
    fn slug_basic() {
        assert_eq!(normalize_folder_slug("My Folder"), "my-folder");
        assert_eq!(normalize_folder_slug("Hello World!"), "hello-world");
        assert_eq!(normalize_folder_slug("a---b"), "a-b");
        assert_eq!(normalize_folder_slug("---"), DEFAULT_FOLDER_SLUG);
        assert_eq!(normalize_folder_slug(""), DEFAULT_FOLDER_SLUG);
    }

    #[test]
    fn slug_strips_combining_marks() {
        assert_eq!(normalize_folder_slug("Cafe\u{0301}"), "cafe");
    }

    #[test]
    fn build_views_linear_chain() {
        let rows = vec![
            FolderRowInput { id: "1".into(), parent_id: None, slug: "root".into(), name: "Root".into(), kind: "root".into(), color: None, system_key: None },
            FolderRowInput { id: "2".into(), parent_id: Some("1".into()), slug: "child".into(), name: "Child".into(), kind: "user".into(), color: Some("#abc".into()), system_key: None },
            FolderRowInput { id: "3".into(), parent_id: Some("2".into()), slug: "grand".into(), name: "Grand".into(), kind: "user".into(), color: None, system_key: None },
        ];
        let views = build_folder_views(&rows).unwrap();
        assert_eq!(views.len(), 3);
        let grand = views.iter().find(|v| v.id == "3").unwrap();
        assert_eq!(grand.path, "root/child/grand");
        assert_eq!(grand.depth, 3);
    }

    #[test]
    fn build_views_detects_cycle() {
        let rows = vec![
            FolderRowInput { id: "1".into(), parent_id: Some("2".into()), slug: "a".into(), name: "A".into(), kind: "user".into(), color: None, system_key: None },
            FolderRowInput { id: "2".into(), parent_id: Some("1".into()), slug: "b".into(), name: "B".into(), kind: "user".into(), color: None, system_key: None },
        ];
        assert!(build_folder_views(&rows).unwrap_err().contains("cycle"));
    }

    #[test]
    fn build_views_detects_invalid_parent() {
        let rows = vec![
            FolderRowInput { id: "1".into(), parent_id: Some("missing".into()), slug: "a".into(), name: "A".into(), kind: "user".into(), color: None, system_key: None },
        ];
        assert!(build_folder_views(&rows).unwrap_err().contains("invalid parent"));
    }

    #[test]
    fn build_views_orphans() {
        let rows = vec![
            FolderRowInput { id: "1".into(), parent_id: None, slug: "x".into(), name: "X".into(), kind: "root".into(), color: None, system_key: Some("my".into()) },
        ];
        let views = build_folder_views(&rows).unwrap();
        assert_eq!(views[0].depth, 1);
        assert_eq!(views[0].system_key.as_deref(), Some("my"));
    }

    #[test]
    fn constants_exposed() {
        assert_eq!(MAX_FOLDER_DEPTH, 4);
        assert!(RESERVED_ROOT_SLUGS.contains(&"bundled"));
        assert!(RESERVED_CHILD_ROOT_SYSTEM_KEYS.contains(&"my"));
    }
}
