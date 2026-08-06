//! `workspace_file_list_response_builder` — workspace file list 响应的纯构造器。
//!
//! 与 Node `matchesSearch` / `listItemFromStat` / `listItemFromDirectory` /
//! `unavailableFileList` / `availableFileList` 1:1 对齐。
//!
//! 设计目标：纯函数模块，无 IO/DB；构造 typed response struct。
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workspace_file_classify::{
    content_type_for_path, preview_kind_for_content_type, PreviewKind,
};

// ============================================================================
// Shared types (mirrors @paperclipai/shared)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceFileWorkspaceKind {
    ExecutionWorkspace,
    ProjectWorkspace,
}

impl Default for WorkspaceFileWorkspaceKind {
    fn default() -> Self {
        Self::ExecutionWorkspace
    }
}

impl WorkspaceFileWorkspaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExecutionWorkspace => "execution_workspace",
            Self::ProjectWorkspace => "project_workspace",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceFileSelector {
    Auto,
    Execution,
    Project,
}

impl Default for WorkspaceFileSelector {
    fn default() -> Self {
        Self::Auto
    }
}

impl WorkspaceFileSelector {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Execution => "execution",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceFileListMode {
    All,
    Recent,
    Changed,
}

impl Default for WorkspaceFileListMode {
    fn default() -> Self {
        Self::All
    }
}

impl WorkspaceFileListMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Recent => "recent",
            Self::Changed => "changed",
        }
    }
}

/// `WorkspaceFilePreviewKind` 字符串字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceFilePreviewKind {
    Text,
    Image,
    Video,
    Pdf,
    Unsupported,
}

impl Default for WorkspaceFilePreviewKind {
    fn default() -> Self {
        Self::Unsupported
    }
}

impl WorkspaceFilePreviewKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Video => "video",
            Self::Pdf => "pdf",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Self::Text),
            "image" => Some(Self::Image),
            "video" => Some(Self::Video),
            "pdf" => Some(Self::Pdf),
            "unsupported" => Some(Self::Unsupported),
            _ => None,
        }
    }
}

// ============================================================================
// Item capabilities
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItemCapabilities {
    pub preview: bool,
    pub download: bool,
    pub list_children: bool,
}

// ============================================================================
// List item types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFileListFileItem {
    pub kind: String, // "file"
    pub provider: String,
    pub title: String,
    pub relative_path: String,
    pub display_path: String,
    pub workspace_label: String,
    pub workspace_kind: WorkspaceFileWorkspaceKind,
    pub workspace_id: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub content_type: Option<String>,
    pub byte_size: Option<i64>,
    pub modified_at: Option<String>,
    pub preview_kind: WorkspaceFilePreviewKind,
    pub capabilities: ListItemCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFileListDirectoryItem {
    pub kind: String, // "directory"
    pub provider: String,
    pub title: String,
    pub relative_path: String,
    pub display_path: String,
    pub workspace_label: String,
    pub workspace_kind: WorkspaceFileWorkspaceKind,
    pub workspace_id: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub content_type: Option<String>,
    pub byte_size: Option<i64>,
    pub modified_at: Option<String>,
    pub preview_kind: WorkspaceFilePreviewKind,
    pub capabilities: ListItemCapabilities,
}

// ============================================================================
// Candidate (minimal)
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceCandidate {
    pub provider: String,
    pub label: String,
    pub workspace_kind: WorkspaceFileWorkspaceKind,
    pub workspace_id: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
}

// ============================================================================
// List response
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFileListResponseWorkspace {
    pub provider: String,
    pub workspace_label: String,
    pub workspace_kind: WorkspaceFileWorkspaceKind,
    pub workspace_id: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFileListResponseQuery {
    pub workspace: WorkspaceFileSelector,
    pub mode: WorkspaceFileListMode,
    pub path: Option<String>,
    pub q: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

/// `ListItem`：file 或 directory 的 union。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ListItem {
    File(WorkspaceFileListFileItem),
    Directory(WorkspaceFileListDirectoryItem),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkspaceFileListResponse {
    Available {
        kind: String, // "workspace_file_list"
        workspace: Option<WorkspaceFileListResponseWorkspace>,
        query: WorkspaceFileListResponseQuery,
        items: Vec<ListItem>,
        scanned_count: u32,
        truncated: bool,
    },
    Unavailable {
        kind: String, // "workspace_file_list"
        unavailable_reason: String,
        workspace: Option<WorkspaceFileListResponseWorkspace>,
        query: WorkspaceFileListResponseQuery,
        items: Vec<ListItem>,
        scanned_count: u32,
        truncated: bool,
    },
}

// ============================================================================
// matchesSearch
// ============================================================================

/// `matchesSearch(relativePath, normalizedQuery)`：搜索匹配判断。
///
/// 与 Node 1:1 对齐：
/// - normalizedQuery 为 null/空 → 全部匹配
/// - 否则 lowercase + includes
pub fn matches_search(relative_path: &str, normalized_query: Option<&str>) -> bool {
    match normalized_query {
        None => true,
        Some(q) if q.is_empty() => true,
        Some(q) => relative_path.to_lowercase().contains(q),
    }
}

// ============================================================================
// listItemFromStat
// ============================================================================

/// `StatLite`：文件 stat minimal subset。
#[derive(Debug, Clone, Default)]
pub struct StatLite {
    pub size: i64,
    pub mtime: chrono::DateTime<chrono::Utc>,
}

/// `listItemFromStat(input)`：构造文件 list item。
///
/// 与 Node 1:1 对齐：
/// - contentType 由 contentTypeForPath 派生
/// - previewKind 由 previewKindForKnownContentType 派生，未知 → "unsupported"
/// - previewable = previewKind !== "unsupported" && size <= previewCapForKind(previewKind)
/// - contentType: null + text previewKind → "text/plain; charset=utf-8"，否则 "application/octet-stream"
pub fn list_item_from_stat(input: ListItemFromStatInput) -> Option<ListItem> {
    let content_type = content_type_for_path(&input.relative_path);
    let preview_kind_pk = preview_kind_for_content_type(content_type.as_deref());
    let preview_kind = match preview_kind_pk {
        crate::workspace_file_classify::PreviewKind::Image => WorkspaceFilePreviewKind::Image,
        crate::workspace_file_classify::PreviewKind::Video => WorkspaceFilePreviewKind::Video,
        crate::workspace_file_classify::PreviewKind::Pdf => WorkspaceFilePreviewKind::Pdf,
        crate::workspace_file_classify::PreviewKind::Text => WorkspaceFilePreviewKind::Text,
        crate::workspace_file_classify::PreviewKind::Unsupported => {
            WorkspaceFilePreviewKind::Unsupported
        }
    };
    let cap = preview_cap_for_kind(preview_kind);
    let previewable =
        preview_kind != WorkspaceFilePreviewKind::Unsupported && input.stat.size <= cap;

    let final_content_type = if let Some(ct) = content_type {
        Some(ct)
    } else if preview_kind == WorkspaceFilePreviewKind::Text {
        Some("text/plain; charset=utf-8".to_string())
    } else {
        Some("application/octet-stream".to_string())
    };

    let display_path = match &input.candidate.project_name {
        Some(pn) => format!("{}/{}", pn, input.relative_path),
        None => input.relative_path.clone(),
    };

    let item = WorkspaceFileListFileItem {
        kind: "file".to_string(),
        provider: input.candidate.provider.clone(),
        title: posix_basename(&input.relative_path),
        relative_path: input.relative_path.clone(),
        display_path,
        workspace_label: input.candidate.label.clone(),
        workspace_kind: input.candidate.workspace_kind,
        workspace_id: input.candidate.workspace_id.clone(),
        project_id: input.candidate.project_id.clone(),
        project_name: input.candidate.project_name.clone(),
        content_type: final_content_type,
        byte_size: Some(input.stat.size),
        modified_at: Some(
            input
                .stat
                .mtime
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
        preview_kind,
        capabilities: ListItemCapabilities {
            preview: previewable,
            download: true,
            list_children: false,
        },
    };

    Some(ListItem::File(item))
}

/// `ListItemFromStatInput`。
#[derive(Debug, Clone)]
pub struct ListItemFromStatInput {
    pub candidate: WorkspaceCandidate,
    pub relative_path: String,
    pub stat: StatLite,
}

/// `previewCapForKind(kind)`：返回预览上限字节数。
///
/// 与 Node 1:1 对齐：
/// - image / video / pdf → WORKSPACE_FILE_MEDIA_MAX_BYTES
/// - 其它 → WORKSPACE_FILE_TEXT_MAX_BYTES
pub fn preview_cap_for_kind(kind: WorkspaceFilePreviewKind) -> i64 {
    use crate::workspace_file_classify::{
        WORKSPACE_FILE_MEDIA_MAX_BYTES, WORKSPACE_FILE_TEXT_MAX_BYTES,
    };
    match kind {
        WorkspaceFilePreviewKind::Image
        | WorkspaceFilePreviewKind::Video
        | WorkspaceFilePreviewKind::Pdf => WORKSPACE_FILE_MEDIA_MAX_BYTES as i64,
        WorkspaceFilePreviewKind::Text | WorkspaceFilePreviewKind::Unsupported => {
            WORKSPACE_FILE_TEXT_MAX_BYTES as i64
        }
    }
}

// ============================================================================
// listItemFromDirectory
// ============================================================================

/// `listItemFromDirectory(input)`：构造目录 list item。
///
/// 与 Node 1:1 对齐：
/// - displayPath: projectName ? `${projectName} / ${relativePath}/` : `${relativePath}/`
/// - previewKind: "unsupported"
/// - capabilities: preview=false, download=false, listChildren=true
pub fn list_item_from_directory(input: ListItemFromDirectoryInput) -> ListItem {
    let display_path = match &input.candidate.project_name {
        Some(pn) => format!("{}/{}/", pn, input.relative_path),
        None => format!("{}/", input.relative_path),
    };

    let item = WorkspaceFileListDirectoryItem {
        kind: "directory".to_string(),
        provider: input.candidate.provider.clone(),
        title: posix_basename(&input.relative_path),
        relative_path: input.relative_path.clone(),
        display_path,
        workspace_label: input.candidate.label.clone(),
        workspace_kind: input.candidate.workspace_kind,
        workspace_id: input.candidate.workspace_id.clone(),
        project_id: input.candidate.project_id.clone(),
        project_name: input.candidate.project_name.clone(),
        content_type: None,
        byte_size: None,
        modified_at: input
            .stat
            .map(|s| s.mtime.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        preview_kind: WorkspaceFilePreviewKind::Unsupported,
        capabilities: ListItemCapabilities {
            preview: false,
            download: false,
            list_children: true,
        },
    };

    ListItem::Directory(item)
}

/// `ListItemFromDirectoryInput`。
#[derive(Debug, Clone)]
pub struct ListItemFromDirectoryInput {
    pub candidate: WorkspaceCandidate,
    pub relative_path: String,
    pub stat: Option<StatLite>,
}

// ============================================================================
// unavailableFileList
// ============================================================================

/// `UnavailableFileListInput`。
#[derive(Debug, Clone)]
pub struct UnavailableFileListInput {
    pub selector: WorkspaceFileSelector,
    pub mode: WorkspaceFileListMode,
    pub path: Option<String>,
    pub q: Option<String>,
    pub limit: u32,
    pub offset: u32,
    pub candidate: Option<WorkspaceCandidate>,
    pub reason: String,
}

/// `unavailableFileList(input)`：构造 unavailable list response。
pub fn build_unavailable_file_list(input: UnavailableFileListInput) -> WorkspaceFileListResponse {
    WorkspaceFileListResponse::Unavailable {
        kind: "workspace_file_list".to_string(),
        unavailable_reason: input.reason,
        workspace: input.candidate.map(candidate_to_response_workspace),
        query: WorkspaceFileListResponseQuery {
            workspace: input.selector,
            mode: input.mode,
            path: input.path,
            q: input.q,
            limit: input.limit,
            offset: input.offset,
        },
        items: vec![],
        scanned_count: 0,
        truncated: false,
    }
}

// ============================================================================
// availableFileList
// ============================================================================

/// `AvailableFileListInput`。
#[derive(Debug, Clone)]
pub struct AvailableFileListInput {
    pub selector: WorkspaceFileSelector,
    pub mode: WorkspaceFileListMode,
    pub path: Option<String>,
    pub q: Option<String>,
    pub limit: u32,
    pub offset: u32,
    pub candidate: WorkspaceCandidate,
    pub items: Vec<ListItem>,
    pub scanned_count: u32,
    pub truncated: bool,
}

/// `availableFileList(input)`：构造 available list response。
pub fn build_available_file_list(input: AvailableFileListInput) -> WorkspaceFileListResponse {
    WorkspaceFileListResponse::Available {
        kind: "workspace_file_list".to_string(),
        workspace: Some(candidate_to_response_workspace(input.candidate)),
        query: WorkspaceFileListResponseQuery {
            workspace: input.selector,
            mode: input.mode,
            path: input.path,
            q: input.q,
            limit: input.limit,
            offset: input.offset,
        },
        items: input.items,
        scanned_count: input.scanned_count,
        truncated: input.truncated,
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn candidate_to_response_workspace(c: WorkspaceCandidate) -> WorkspaceFileListResponseWorkspace {
    WorkspaceFileListResponseWorkspace {
        provider: c.provider,
        workspace_label: c.label,
        workspace_kind: c.workspace_kind,
        workspace_id: c.workspace_id,
        project_id: c.project_id,
        project_name: c.project_name,
    }
}

fn posix_basename(p: &str) -> String {
    // 找最后一个 `/`
    p.rsplit('/').next().unwrap_or(p).to_string()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(y: i32, m: u32, d: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    fn candidate() -> WorkspaceCandidate {
        WorkspaceCandidate {
            provider: "local_fs".into(),
            label: "ws-label".into(),
            workspace_kind: WorkspaceFileWorkspaceKind::ExecutionWorkspace,
            workspace_id: "ws-1".into(),
            project_id: Some("p-1".into()),
            project_name: Some("paperclip".into()),
        }
    }

    // ----- matchesSearch -----

    #[test]
    fn matches_search_none_matches_all() {
        assert!(matches_search("foo/bar.txt", None));
    }

    #[test]
    fn matches_search_empty_matches_all() {
        assert!(matches_search("foo/bar.txt", Some("")));
    }

    #[test]
    fn matches_search_case_insensitive() {
        assert!(matches_search("src/INDEX.ts", Some("index")));
    }

    #[test]
    fn matches_search_mismatch() {
        assert!(!matches_search("foo/bar.txt", Some("baz")));
    }

    // ----- previewCapForKind -----

    #[test]
    fn preview_cap_image_video_pdf_use_media() {
        let media = crate::workspace_file_classify::WORKSPACE_FILE_MEDIA_MAX_BYTES as i64;
        assert_eq!(preview_cap_for_kind(WorkspaceFilePreviewKind::Image), media);
        assert_eq!(preview_cap_for_kind(WorkspaceFilePreviewKind::Video), media);
        assert_eq!(preview_cap_for_kind(WorkspaceFilePreviewKind::Pdf), media);
    }

    #[test]
    fn preview_cap_text_unsupported_use_text() {
        let text = crate::workspace_file_classify::WORKSPACE_FILE_TEXT_MAX_BYTES as i64;
        assert_eq!(preview_cap_for_kind(WorkspaceFilePreviewKind::Text), text);
        assert_eq!(
            preview_cap_for_kind(WorkspaceFilePreviewKind::Unsupported),
            text
        );
    }

    // ----- listItemFromStat -----

    #[test]
    fn list_item_from_stat_file_text() {
        let item = list_item_from_stat(ListItemFromStatInput {
            candidate: candidate(),
            relative_path: "src/main.rs".into(),
            stat: StatLite {
                size: 100,
                mtime: dt(2025, 1, 1),
            },
        })
        .unwrap();
        match item {
            ListItem::File(f) => {
                assert_eq!(f.kind, "file");
                assert_eq!(f.title, "main.rs");
                assert_eq!(f.relative_path, "src/main.rs");
                assert!(f.display_path.contains("paperclip"));
                assert!(f.display_path.contains("src/main.rs"));
                assert_eq!(f.preview_kind, WorkspaceFilePreviewKind::Text);
                assert!(f.capabilities.preview);
                assert!(f.capabilities.download);
                assert!(!f.capabilities.list_children);
                assert_eq!(f.byte_size, Some(100));
                assert!(f.content_type.is_some());
            }
            _ => panic!("expected File"),
        }
    }

    #[test]
    fn list_item_from_stat_image_oversized_not_previewable() {
        let item = list_item_from_stat(ListItemFromStatInput {
            candidate: candidate(),
            relative_path: "img/big.png".into(),
            stat: StatLite {
                size: 999_999_999,
                mtime: dt(2025, 1, 1),
            },
        })
        .unwrap();
        match item {
            ListItem::File(f) => {
                assert_eq!(f.preview_kind, WorkspaceFilePreviewKind::Image);
                assert!(!f.capabilities.preview); // too big
                assert!(f.capabilities.download);
            }
            _ => panic!("expected File"),
        }
    }

    #[test]
    fn list_item_from_stat_unknown_extension_octet_stream() {
        let item = list_item_from_stat(ListItemFromStatInput {
            candidate: candidate(),
            relative_path: "weird.xyz".into(),
            stat: StatLite {
                size: 50,
                mtime: dt(2025, 1, 1),
            },
        })
        .unwrap();
        match item {
            ListItem::File(f) => {
                assert_eq!(f.preview_kind, WorkspaceFilePreviewKind::Unsupported);
                assert_eq!(f.content_type.as_deref(), Some("application/octet-stream"));
            }
            _ => panic!("expected File"),
        }
    }

    // ----- listItemFromDirectory -----

    #[test]
    fn list_item_directory_with_project() {
        let item = list_item_from_directory(ListItemFromDirectoryInput {
            candidate: candidate(),
            relative_path: "src".into(),
            stat: Some(StatLite {
                size: 0,
                mtime: dt(2025, 1, 1),
            }),
        });
        match item {
            ListItem::Directory(d) => {
                assert_eq!(d.kind, "directory");
                assert_eq!(d.title, "src");
                assert_eq!(d.relative_path, "src");
                assert!(d.display_path.contains("paperclip"));
                assert!(d.display_path.ends_with('/'));
                assert_eq!(d.content_type, None);
                assert_eq!(d.byte_size, None);
                assert!(!d.capabilities.preview);
                assert!(!d.capabilities.download);
                assert!(d.capabilities.list_children);
            }
            _ => panic!("expected Directory"),
        }
    }

    #[test]
    fn list_item_directory_no_project() {
        let mut c = candidate();
        c.project_name = None;
        let item = list_item_from_directory(ListItemFromDirectoryInput {
            candidate: c,
            relative_path: "src".into(),
            stat: None,
        });
        match item {
            ListItem::Directory(d) => {
                assert_eq!(d.display_path, "src/");
                assert_eq!(d.modified_at, None);
            }
            _ => panic!("expected Directory"),
        }
    }

    // ----- unavailableFileList -----

    #[test]
    fn unavailable_file_list_with_candidate() {
        let r = build_unavailable_file_list(UnavailableFileListInput {
            selector: WorkspaceFileSelector::Execution,
            mode: WorkspaceFileListMode::All,
            path: Some("/foo".into()),
            q: Some("bar".into()),
            limit: 25,
            offset: 0,
            candidate: Some(candidate()),
            reason: "no_root".into(),
        });
        match r {
            WorkspaceFileListResponse::Unavailable {
                unavailable_reason,
                workspace,
                query,
                ..
            } => {
                assert_eq!(unavailable_reason, "no_root");
                assert!(workspace.is_some());
                assert_eq!(query.limit, 25);
                assert_eq!(query.path.as_deref(), Some("/foo"));
                assert_eq!(query.q.as_deref(), Some("bar"));
            }
            _ => panic!("expected Unavailable"),
        }
    }

    #[test]
    fn unavailable_file_list_no_candidate() {
        let r = build_unavailable_file_list(UnavailableFileListInput {
            selector: WorkspaceFileSelector::Auto,
            mode: WorkspaceFileListMode::Recent,
            path: None,
            q: None,
            limit: 25,
            offset: 0,
            candidate: None,
            reason: "no_workspace".into(),
        });
        match r {
            WorkspaceFileListResponse::Unavailable { workspace, .. } => {
                assert!(workspace.is_none());
            }
            _ => panic!("expected Unavailable"),
        }
    }

    // ----- availableFileList -----

    #[test]
    fn available_file_list_response() {
        let r = build_available_file_list(AvailableFileListInput {
            selector: WorkspaceFileSelector::Project,
            mode: WorkspaceFileListMode::All,
            path: None,
            q: None,
            limit: 25,
            offset: 0,
            candidate: candidate(),
            items: vec![],
            scanned_count: 5,
            truncated: false,
        });
        match r {
            WorkspaceFileListResponse::Available {
                scanned_count,
                truncated,
                ..
            } => {
                assert_eq!(scanned_count, 5);
                assert!(!truncated);
            }
            _ => panic!("expected Available"),
        }
    }

    // ----- preview_kind conversion -----

    #[test]
    fn preview_kind_roundtrip() {
        for k in [
            WorkspaceFilePreviewKind::Text,
            WorkspaceFilePreviewKind::Image,
            WorkspaceFilePreviewKind::Video,
            WorkspaceFilePreviewKind::Pdf,
            WorkspaceFilePreviewKind::Unsupported,
        ] {
            assert_eq!(WorkspaceFilePreviewKind::from_str(k.as_str()), Some(k));
        }
    }

    // ----- PreviewKind compat -----

    #[test]
    fn workspace_file_classify_preview_kind_compat() {
        use crate::workspace_file_classify::PreviewKind;
        assert_eq!(
            preview_kind_for_content_type(Some("text/plain")),
            PreviewKind::Text
        );
        assert_eq!(
            preview_kind_for_content_type(Some("image/png")),
            PreviewKind::Image
        );
        assert_eq!(
            preview_kind_for_content_type(Some("video/mp4")),
            PreviewKind::Video
        );
        assert_eq!(
            preview_kind_for_content_type(Some("application/pdf")),
            PreviewKind::Pdf
        );
        assert_eq!(
            preview_kind_for_content_type(Some("image/svg+xml")),
            PreviewKind::Text
        );
        assert_eq!(
            preview_kind_for_content_type(None),
            PreviewKind::Unsupported
        );
    }
}
