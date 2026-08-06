//! `workspace_file_classify` 域（Round 269）。
//!
//! 与原 `paperclip/server/src/services/workspace-file-resources.ts` 顶部常量 + 路径工具
//! 1:1 对齐：
//! - 大小上限（text / media / list limits / scanned entries / path bytes）
//! - 拒绝段集合 DENIED_SEGMENTS（node_modules、.git 等）
//! - TEXT_EXTENSIONS / IMAGE_CONTENT_TYPES / VIDEO_CONTENT_TYPES 映射
//! - 本地项目工作区来源类型 LOCAL_PROJECT_WORKSPACE_SOURCE_TYPES
//! - 路径 normalize / relative-path / isInsideRoot
//!
//! 设计目标：高内聚低耦合。
//! - **高内聚**：纯函数 + 静态常量。零 IO，零 DB。
//! - **低耦合**：仅依赖 std 集合与字符串类型。允许上层（pc-repos / pc-http / pc-server）
//!   在不直接引入 workspace-file-resources.ts 的前提下复用同样的"黑名单 + 大小限制"。
//!
//! 与 Node 版差异说明：
//! - Rust 中使用 `OnceLock<HashSet<&str>>` / `OnceLock<HashMap<&str, &str>>` 替代 `Set` /
//!   `Map` 单例初始化。
//! - `path.sep`/`path.posix.sep` 在 Rust 中通过 `MAIN_SEPARATOR` 与 '/' 区分。
//! - 工具函数 `normalizeWorkspaceRelativePath` 抛出 `WorkspaceFileError`，由调用方
//!   决定是否 wrap 成 HTTP 4xx（与 Node 中 `unprocessable(...)` 等价）。

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf, MAIN_SEPARATOR};
use std::sync::OnceLock;
use thiserror::Error;

// ============================================================================
// 常量
// ============================================================================

/// 文本文件预览上限（Node `WORKSPACE_FILE_TEXT_MAX_BYTES = 512 * 1024`）。
pub const WORKSPACE_FILE_TEXT_MAX_BYTES: u64 = 512 * 1024;

/// 多媒体文件预览上限（Node `WORKSPACE_FILE_MEDIA_MAX_BYTES = 10 * 1024 * 1024`）。
pub const WORKSPACE_FILE_MEDIA_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// 列表默认 limit（Node `WORKSPACE_FILE_LIST_DEFAULT_LIMIT = 25`）。
pub const WORKSPACE_FILE_LIST_DEFAULT_LIMIT: u32 = 25;

/// 列表最大 limit（Node `WORKSPACE_FILE_LIST_MAX_LIMIT = 100`）。
pub const WORKSPACE_FILE_LIST_MAX_LIMIT: u32 = 100;

/// 列表扫描上限（Node `WORKSPACE_FILE_LIST_MAX_SCANNED_ENTRIES = 5_000`）。
pub const WORKSPACE_FILE_LIST_MAX_SCANNED_ENTRIES: u32 = 5_000;

/// 相对路径字节上限（Node `MAX_RELATIVE_PATH_BYTES = 4096`）。
pub const MAX_RELATIVE_PATH_BYTES: usize = 4096;

/// 文本嗅探字节数（Node `TEXT_SNIFF_BYTES = 4096`）。
pub const TEXT_SNIFF_BYTES: usize = 4096;

/// 文件树最大深度（Node `MAX_LIST_DEPTH = 20`）。
pub const MAX_LIST_DEPTH: u32 = 20;

/// git status 调用最大 buffer（Node `GIT_STATUS_MAX_BUFFER_BYTES = 1MB`）。
pub const GIT_STATUS_MAX_BUFFER_BYTES: usize = 1024 * 1024;

// ============================================================================
// 黑名单 + MIME 映射（懒加载单例）
// ============================================================================

fn denied_segments() -> &'static HashSet<&'static str> {
    static CELL: OnceLock<HashSet<&'static str>> = OnceLock::new();
    CELL.get_or_init(|| {
        [
            ".git",
            ".paperclip",
            "node_modules",
            ".pnpm-store",
            ".yarn",
            ".cache",
            ".turbo",
            ".next",
            ".vite",
            ".vercel",
            "dist",
            "build",
            "coverage",
            "runtime-services",
            ".runtime",
        ]
        .into_iter()
        .collect()
    })
}

fn text_extensions() -> &'static HashSet<&'static str> {
    static CELL: OnceLock<HashSet<&'static str>> = OnceLock::new();
    CELL.get_or_init(|| {
        [
            ".c", ".cc", ".conf", ".cpp", ".css", ".csv", ".go", ".h", ".html", ".htm", ".java",
            ".js", ".json", ".jsx", ".log", ".md", ".mjs", ".py", ".rb", ".rs", ".sh", ".sql",
            ".svg", ".toml", ".ts", ".tsx", ".txt", ".xml", ".yaml", ".yml",
        ]
        .into_iter()
        .collect()
    })
}

fn image_content_types() -> &'static HashMap<&'static str, &'static str> {
    static CELL: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    CELL.get_or_init(|| {
        [
            (".gif", "image/gif"),
            (".jpeg", "image/jpeg"),
            (".jpg", "image/jpeg"),
            (".png", "image/png"),
            (".webp", "image/webp"),
        ]
        .into_iter()
        .collect()
    })
}

fn video_content_types() -> &'static HashMap<&'static str, &'static str> {
    static CELL: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    CELL.get_or_init(|| {
        [
            (".m4v", "video/mp4"),
            (".mov", "video/quicktime"),
            (".mp4", "video/mp4"),
            (".webm", "video/webm"),
        ]
        .into_iter()
        .collect()
    })
}

fn local_project_workspace_source_types() -> &'static HashSet<&'static str> {
    static CELL: OnceLock<HashSet<&'static str>> = OnceLock::new();
    CELL.get_or_init(|| {
        ["local_path", "non_git_path", "git_repo", "git_worktree"]
            .into_iter()
            .collect()
    })
}

// ============================================================================
// 分类谓词
// ============================================================================

/// 段是否在拒绝列表中？例如 `node_modules`, `.git`, `dist`。
pub fn is_denied_segment(segment: &str) -> bool {
    denied_segments().contains(segment)
}

/// 扩展名（小写、含前置 `.`）是否在 TEXT_EXTENSIONS 中？
pub fn is_text_extension(ext_lc: &str) -> bool {
    text_extensions().contains(ext_lc)
}

/// 后缀名查 MIME；图像 / 视频分别查各自表。
pub fn image_content_type_for_ext(ext_lc: &str) -> Option<&'static str> {
    image_content_types().get(ext_lc).copied()
}

pub fn video_content_type_for_ext(ext_lc: &str) -> Option<&'static str> {
    video_content_types().get(ext_lc).copied()
}

/// 综合预览 MIME：image > video > text。
///
/// 与 Node `previewMimeTypeForExtension` 行为一致：空 / 未知后缀返回 None。
pub fn preview_mime_for_extension(ext_lc: &str) -> Option<PreviewMime> {
    if image_content_type_for_ext(ext_lc).is_some() {
        Some(PreviewMime::Image)
    } else if video_content_type_for_ext(ext_lc).is_some() {
        Some(PreviewMime::Video)
    } else if is_text_extension(ext_lc) {
        Some(PreviewMime::Text)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewMime {
    Image,
    Video,
    Text,
}

impl PreviewMime {
    /// 该 kind 的字节上限（image/video → media 上限；text → text 上限）。
    pub fn cap_bytes(self) -> u64 {
        match self {
            PreviewMime::Image | PreviewMime::Video => WORKSPACE_FILE_MEDIA_MAX_BYTES,
            PreviewMime::Text => WORKSPACE_FILE_TEXT_MAX_BYTES,
        }
    }

    /// 对应 `WorkspaceFilePreviewKind` 字符串字面量。
    pub fn as_kind_str(self) -> &'static str {
        match self {
            PreviewMime::Image => "image",
            PreviewMime::Video => "video",
            PreviewMime::Text => "text",
        }
    }
}

/// 类型是否属于本地项目工作区来源（local_path / non_git_path / git_repo / git_worktree）？
pub fn is_local_project_workspace_source(source_type: &str) -> bool {
    local_project_workspace_source_types().contains(source_type)
}

// ============================================================================
// 路径工具
// ============================================================================

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorkspaceFileError {
    #[error("Workspace file path is required")]
    PathRequired,
    #[error("Workspace file path is too long")]
    PathTooLong,
    #[error("Workspace file path is invalid: {0}")]
    InvalidPath(String),
}

/// 规范化工作区相对路径：trim 空格、限制字节数（UTF-8）、拆段、拒绝 list/绝对路径。
///
/// 与 Node `normalizeWorkspaceRelativePath(input)` 1:1 对齐：
/// - 空字符串 / 全空白 → `PathRequired`
/// - UTF-8 字节数 > MAX_RELATIVE_PATH_BYTES → `PathTooLong`
/// - 含 `\` 段 / 非 posix 分隔符 → `InvalidPath("backslash ...")`
pub fn normalize_workspace_relative_path(input: &str) -> Result<NormalizedPath, WorkspaceFileError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceFileError::PathRequired);
    }
    if trimmed.as_bytes().len() > MAX_RELATIVE_PATH_BYTES {
        return Err(WorkspaceFileError::PathTooLong);
    }
    if trimmed.contains('\\') {
        return Err(WorkspaceFileError::InvalidPath(
            "backslash not allowed".to_string(),
        ));
    }
    if trimmed.starts_with('/') {
        return Err(WorkspaceFileError::InvalidPath(
            "absolute path not allowed".to_string(),
        ));
    }
    // 拆分 posix 段，丢弃空段（兼容 "a//b"、"a/./b"）
    let mut segments = Vec::new();
    for seg in trimmed.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(WorkspaceFileError::InvalidPath(
                "parent segment not allowed".to_string(),
            ));
        }
        segments.push(seg.to_string());
    }
    Ok(NormalizedPath {
        relative_path: segments.join("/"),
        segments,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedPath {
    pub relative_path: String,
    pub segments: Vec<String>,
}

impl NormalizedPath {
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
    pub fn depth(&self) -> usize {
        self.segments.len()
    }
}

/// 计算 `target_real` 相对于 `root_real` 的 posix 风格相对路径。
///
/// 与 Node `relativePathFromReal(root, target)` 等价：用 `path.relative` 后用 `/` 重接。
pub fn relative_path_from_real(root_real: &Path, target_real: &Path) -> String {
    let rel = pathdiff_relative(root_real, target_real);
    rel.to_string_lossy().split(MAIN_SEPARATOR).collect::<Vec<_>>().join("/")
}

fn pathdiff_relative(root: &Path, target: &Path) -> PathBuf {
    let root_clean = clean_path(root);
    let target_clean = clean_path(target);
    let mut root_parts: Vec<&str> = root_clean.iter().map(|c| c.to_str().unwrap_or("")).collect();
    let mut target_parts: Vec<&str> = target_clean.iter().map(|c| c.to_str().unwrap_or("")).collect();

    // 跳过共同前缀
    while !root_parts.is_empty() && !target_parts.is_empty() && root_parts[0] == target_parts[0] {
        root_parts.remove(0);
        target_parts.remove(0);
    }
    let mut result = PathBuf::new();
    for _ in &root_parts {
        result.push("..");
    }
    for seg in &target_parts {
        result.push(seg);
    }
    result
}

/// `target_real` 是否在 `root_real` 之内？
///
/// 与 Node `isInsideRoot(root, target)` 等价：相对路径不包含 `..` 且非 absolute。
pub fn is_inside_root(root_real: &Path, target_real: &Path) -> bool {
    let rel = pathdiff_relative(root_real, target_real);
    if rel.as_os_str().is_empty() {
        return true;
    }
    let rel_str = rel.to_string_lossy();
    !rel_str.starts_with("..") && !rel.starts_with(Path::new("/")) && !rel_components_have_parent(&rel)
}

fn rel_components_have_parent(p: &Path) -> bool {
    p.components().any(|c| matches!(c, Component::ParentDir))
}

fn clean_path(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => cleaned.push(prefix.as_os_str()),
            Component::RootDir => cleaned.push(Path::new(MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::Normal(segment) => cleaned.push(segment),
        }
    }
    cleaned
}

const MAIN_SEPARATOR_STR: &str = if cfg!(windows) { "\\" } else { "/" };

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_node() {
        assert_eq!(WORKSPACE_FILE_TEXT_MAX_BYTES, 512 * 1024);
        assert_eq!(WORKSPACE_FILE_MEDIA_MAX_BYTES, 10 * 1024 * 1024);
        assert_eq!(WORKSPACE_FILE_LIST_DEFAULT_LIMIT, 25);
        assert_eq!(WORKSPACE_FILE_LIST_MAX_LIMIT, 100);
        assert_eq!(WORKSPACE_FILE_LIST_MAX_SCANNED_ENTRIES, 5_000);
        assert_eq!(MAX_RELATIVE_PATH_BYTES, 4096);
        assert_eq!(TEXT_SNIFF_BYTES, 4096);
        assert_eq!(MAX_LIST_DEPTH, 20);
        assert_eq!(GIT_STATUS_MAX_BUFFER_BYTES, 1024 * 1024);
    }

    #[test]
    fn denied_segments_match_node() {
        for s in [
            ".git", ".paperclip", "node_modules", ".pnpm-store", ".yarn", ".cache", ".turbo",
            ".next", ".vite", ".vercel", "dist", "build", "coverage",
            "runtime-services", ".runtime",
        ] {
            assert!(is_denied_segment(s), "missing denied: {s}");
        }
        assert!(!is_denied_segment("src"));
        assert!(!is_denied_segment("lib"));
    }

    #[test]
    fn text_extensions_match_node() {
        for ext in [".ts", ".tsx", ".rs", ".py", ".md", ".json", ".yaml", ".html", ".sql"] {
            assert!(is_text_extension(ext), "missing text ext: {ext}");
        }
        assert!(!is_text_extension(".exe"));
        assert!(!is_text_extension(".bin"));
    }

    #[test]
    fn image_mime_lookup() {
        assert_eq!(image_content_type_for_ext(".png"), Some("image/png"));
        assert_eq!(image_content_type_for_ext(".jpg"), Some("image/jpeg"));
        assert_eq!(image_content_type_for_ext(".webp"), Some("image/webp"));
        // 注意：调用方应先 lowercase；测试中模拟正常路径
        assert_eq!(image_content_type_for_ext(".gif"), Some("image/gif"));
        assert_eq!(image_content_type_for_ext(".txt"), None);
    }

    #[test]
    fn video_mime_lookup() {
        assert_eq!(video_content_type_for_ext(".mp4"), Some("video/mp4"));
        assert_eq!(video_content_type_for_ext(".webm"), Some("video/webm"));
        assert_eq!(video_content_type_for_ext(".mov"), Some("video/quicktime"));
        assert_eq!(video_content_type_for_ext(".m4v"), Some("video/mp4"));
        assert_eq!(video_content_type_for_ext(".png"), None);
    }

    #[test]
    fn preview_mime_combines_image_video_text() {
        assert_eq!(preview_mime_for_extension(".png"), Some(PreviewMime::Image));
        assert_eq!(preview_mime_for_extension(".mp4"), Some(PreviewMime::Video));
        assert_eq!(preview_mime_for_extension(".ts"), Some(PreviewMime::Text));
        assert_eq!(preview_mime_for_extension(".exe"), None);
        assert_eq!(preview_mime_for_extension(""), None);
    }

    #[test]
    fn preview_mime_caps_match_node() {
        assert_eq!(PreviewMime::Image.cap_bytes(), WORKSPACE_FILE_MEDIA_MAX_BYTES);
        assert_eq!(PreviewMime::Video.cap_bytes(), WORKSPACE_FILE_MEDIA_MAX_BYTES);
        assert_eq!(PreviewMime::Text.cap_bytes(), WORKSPACE_FILE_TEXT_MAX_BYTES);
    }

    #[test]
    fn preview_mime_kind_str() {
        assert_eq!(PreviewMime::Image.as_kind_str(), "image");
        assert_eq!(PreviewMime::Video.as_kind_str(), "video");
        assert_eq!(PreviewMime::Text.as_kind_str(), "text");
    }

    #[test]
    fn local_project_source_types_match_node() {
        for s in ["local_path", "non_git_path", "git_repo", "git_worktree"] {
            assert!(is_local_project_workspace_source(s), "missing: {s}");
        }
        assert!(!is_local_project_workspace_source("adapter_managed"));
        assert!(!is_local_project_workspace_source("cloud_sandbox"));
    }

    #[test]
    fn normalize_rejects_empty_and_whitespace() {
        assert_eq!(normalize_workspace_relative_path(""), Err(WorkspaceFileError::PathRequired));
        assert_eq!(normalize_workspace_relative_path("   "), Err(WorkspaceFileError::PathRequired));
    }

    #[test]
    fn normalize_rejects_too_long() {
        let too_long = "a".repeat(MAX_RELATIVE_PATH_BYTES + 1);
        assert_eq!(
            normalize_workspace_relative_path(&too_long),
            Err(WorkspaceFileError::PathTooLong)
        );
    }

    #[test]
    fn normalize_rejects_backslash() {
        assert!(matches!(
            normalize_workspace_relative_path("a\\b"),
            Err(WorkspaceFileError::InvalidPath(_))
        ));
    }

    #[test]
    fn normalize_rejects_absolute_path() {
        assert!(matches!(
            normalize_workspace_relative_path("/abs"),
            Err(WorkspaceFileError::InvalidPath(_))
        ));
    }

    #[test]
    fn normalize_rejects_parent_segment() {
        assert!(matches!(
            normalize_workspace_relative_path("a/../b"),
            Err(WorkspaceFileError::InvalidPath(_))
        ));
        assert!(matches!(
            normalize_workspace_relative_path("../escape"),
            Err(WorkspaceFileError::InvalidPath(_))
        ));
    }

    #[test]
    fn normalize_collapses_empty_and_dot_segments() {
        let n = normalize_workspace_relative_path("a//b/./c").expect("ok");
        assert_eq!(n.relative_path, "a/b/c");
        assert_eq!(n.segments, vec!["a", "b", "c"]);
        assert_eq!(n.depth(), 3);
    }

    #[test]
    fn normalize_splits_segments_correctly() {
        let n = normalize_workspace_relative_path("src/lib/index.ts").expect("ok");
        assert_eq!(n.segments, vec!["src", "lib", "index.ts"]);
    }

    #[test]
    fn normalize_root_returns_empty_segments() {
        let n = normalize_workspace_relative_path("").unwrap_err();
        let _ = n;
        let n = normalize_workspace_relative_path("   ").unwrap_err();
        let _ = n;
        // "." 也是空段（被 ignore），结果 segments=[]
        let n = normalize_workspace_relative_path("./").expect("ok");
        assert!(n.is_empty());
        assert_eq!(n.depth(), 0);
    }

    #[test]
    fn relative_path_from_real_basic() {
        let root = PathBuf::from("/work/proj");
        let target = PathBuf::from("/work/proj/src/lib/main.rs");
        let rel = relative_path_from_real(&root, &target);
        assert_eq!(rel, "src/lib/main.rs");
    }

    #[test]
    fn relative_path_from_real_returns_root_dot_for_same() {
        let root = PathBuf::from("/work/proj");
        let target = root.clone();
        let rel = relative_path_from_real(&root, &target);
        assert_eq!(rel, "");
    }

    #[test]
    fn is_inside_root_detects_within() {
        let root = PathBuf::from("/work/proj");
        assert!(is_inside_root(&root, &PathBuf::from("/work/proj/src")));
        assert!(is_inside_root(&root, &root));
        assert!(!is_inside_root(&root, &PathBuf::from("/other")));
        assert!(!is_inside_root(&root, &PathBuf::from("/work/other")));
    }

    #[test]
    fn preview_mime_priority_is_image_video_text() {
        // 同一个 ext 不可能同时是 image 和 video；该测试主要保证优先级。
        assert_eq!(preview_mime_for_extension(".jpg"), Some(PreviewMime::Image));
        assert_eq!(preview_mime_for_extension(".mp4"), Some(PreviewMime::Video));
        assert_eq!(preview_mime_for_extension(".json"), Some(PreviewMime::Text));
    }
}

// ============================================================================
// Round 270 追加：扩展至 deny 规则 + content_type 推断 + 文本嗅探
// ============================================================================

/// 文件路径被策略拒绝的原因（Node `denyReasonForPathSegments` 1:1 对齐）。
///
/// 返回 `None` 表示允许访问；返回 `Some(reason)` 时调用方应拒绝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    DeniedPathSegment,
    DeniedSecret,
}

impl DenyReason {
    pub fn as_code(self) -> &'static str {
        match self {
            DenyReason::DeniedPathSegment => "denied_path_segment",
            DenyReason::DeniedSecret => "denied_secret",
        }
    }

    pub fn as_str(self) -> &'static str {
        self.as_code()
    }
}

/// 检查路径段是否被拒绝；返回 None 表示允许。
///
/// 与 Node `denyReasonForPathSegments(segments)` 1:1 对齐：
/// - 任何段（lowercased）在 DENIED_SEGMENTS → `DeniedPathSegment`
/// - 文件名是 `.env` / `.env.*` / `*.pem` / `*.key` / `*.p12` / `*.pfx` → `DeniedSecret`
/// - 文件名是 `id_rsa`/`id_ed25519`/`.npmrc`/`.pypirc`/`.netrc`/`kubeconfig` → `DeniedSecret`
/// - 包含 `.aws` 或 `.ssh` 段 → `DeniedSecret`
/// - 文件名 `config.json` 且父目录 `.docker` → `DeniedSecret`
/// - 文件名 `config` 且父目录 `.kube` → `DeniedSecret`
pub fn deny_reason_for_segments(segments: &[&str]) -> Option<DenyReason> {
    let lower: Vec<String> = segments.iter().map(|s| s.to_lowercase()).collect();
    let lower_slice: Vec<&str> = lower.iter().map(|s| s.as_str()).collect();
    if lower_slice.iter().any(|s| is_denied_segment(s)) {
        return Some(DenyReason::DeniedPathSegment);
    }
    let file_name = *lower_slice.last()?;
    if file_name == ".env" || file_name.starts_with(".env.") {
        return Some(DenyReason::DeniedSecret);
    }
    if file_name.ends_with(".pem")
        || file_name.ends_with(".key")
        || file_name.ends_with(".p12")
        || file_name.ends_with(".pfx")
    {
        return Some(DenyReason::DeniedSecret);
    }
    const SECRET_FILES: &[&str] = &[
        "id_rsa", "id_ed25519", ".npmrc", ".pypirc", ".netrc", "kubeconfig",
    ];
    if SECRET_FILES.contains(&file_name) {
        return Some(DenyReason::DeniedSecret);
    }
    if lower_slice.contains(&".aws") || lower_slice.contains(&".ssh") {
        return Some(DenyReason::DeniedSecret);
    }
    if lower_slice.len() >= 2 {
        let parent = lower_slice[lower_slice.len() - 2];
        if parent == ".docker" && file_name == "config.json" {
            return Some(DenyReason::DeniedSecret);
        }
        if parent == ".kube" && file_name == "config" {
            return Some(DenyReason::DeniedSecret);
        }
    }
    None
}

/// 便捷：在归一化段后调用；段需先经过 `normalize_workspace_relative_path`。
pub fn is_denied_segments(segments: &[&str]) -> bool {
    deny_reason_for_segments(segments).is_some()
}

/// 路径文件的 MIME 类型（Node `contentTypeForPath` 1:1 对齐）。
///
/// 优先级：image > video > pdf > svg > html > text > unknown。
pub fn content_type_for_path(file_path: &str) -> Option<String> {
    let ext = extname_lower(file_path);
    if let Some(m) = image_content_type_for_ext(&ext) {
        return Some(m.to_string());
    }
    if let Some(m) = video_content_type_for_ext(&ext) {
        return Some(m.to_string());
    }
    match ext.as_str() {
        ".pdf" => return Some("application/pdf".to_string()),
        ".svg" => return Some("image/svg+xml".to_string()),
        ".html" | ".htm" => return Some("text/html".to_string()),
        _ => {}
    }
    if is_text_extension(&ext) {
        return Some("text/plain; charset=utf-8".to_string());
    }
    None
}

/// 文件名后缀（小写，含 `.`）；无后缀返回空字符串。
pub fn extname_lower(file_path: &str) -> String {
    let p = Path::new(file_path);
    p.extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{}", s.to_lowercase()))
        .unwrap_or_default()
}

/// `content_type` → 预览 kind（Node `previewKindForKnownContentType` 1:1 对齐）。
///
/// 支持 `image/*` (除 svg 外) / `video/*` / `application/pdf` / `image/svg+xml` / `text/*` / `text/html`。
pub fn preview_kind_for_content_type(content_type: Option<&str>) -> PreviewKind {
    match content_type {
        None => PreviewKind::Unsupported,
        Some(ct) => {
            if ct.starts_with("image/") && ct != "image/svg+xml" {
                PreviewKind::Image
            } else if ct.starts_with("video/") {
                PreviewKind::Video
            } else if ct == "application/pdf" {
                PreviewKind::Pdf
            } else if ct == "text/html" {
                PreviewKind::Unsupported
            } else if ct == "image/svg+xml" || ct.starts_with("text/") {
                PreviewKind::Text
            } else {
                PreviewKind::Unsupported
            }
        }
    }
}

/// `WorkspaceFilePreviewKind` 字符串字面量对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Image,
    Video,
    Pdf,
    Text,
    Unsupported,
}

impl PreviewKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewKind::Image => "image",
            PreviewKind::Video => "video",
            PreviewKind::Pdf => "pdf",
            PreviewKind::Text => "text",
            PreviewKind::Unsupported => "unsupported",
        }
    }
}

/// 文件内容嗅探：是否像文本？Node `looksLikeText` 1:1 对齐：
/// - 空 buffer → true
/// - 含 NUL 字节（byte 0）→ false
/// - 控制字符（除 \t \n \r）占比 < 2% → true；否则 false
pub fn looks_like_text(buffer: &[u8]) -> bool {
    if buffer.is_empty() {
        return true;
    }
    let mut control_bytes = 0usize;
    for &byte in buffer {
        if byte == 0 {
            return false;
        }
        if byte < 32 && byte != 9 && byte != 10 && byte != 13 {
            control_bytes += 1;
        }
    }
    (control_bytes as f64) / (buffer.len() as f64) < 0.02
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    #[test]
    fn deny_node_modules_is_path_segment() {
        let segs = vec!["src", "node_modules", "foo.js"];
        assert_eq!(deny_reason_for_segments(&segs), Some(DenyReason::DeniedPathSegment));
    }

    #[test]
    fn deny_dot_env_is_secret() {
        // Node 行为：取最后一个 segment 作为 fileName
        let segs = vec![".env"];
        assert_eq!(deny_reason_for_segments(&segs), Some(DenyReason::DeniedSecret));
        let segs = vec!["home", ".env"];
        assert_eq!(deny_reason_for_segments(&segs), Some(DenyReason::DeniedSecret));
        // 隐藏子目录中的 .env.local / .env.production 也命中
        let segs = vec!["home", ".env.production"];
        assert_eq!(deny_reason_for_segments(&segs), Some(DenyReason::DeniedSecret));
    }

    #[test]
    fn deny_pem_and_key_files() {
        for name in ["server.pem", "server.key", "server.p12", "server.pfx"] {
            let segs = vec![name];
            assert_eq!(deny_reason_for_segments(&segs), Some(DenyReason::DeniedSecret), "{name}");
        }
    }

    #[test]
    fn deny_ssh_key_file_names() {
        for name in ["id_rsa", "id_ed25519", ".npmrc", ".pypirc", ".netrc", "kubeconfig"] {
            let segs = vec![name];
            assert_eq!(deny_reason_for_segments(&segs), Some(DenyReason::DeniedSecret), "{name}");
        }
    }

    #[test]
    fn deny_aws_and_ssh_dirs() {
        assert_eq!(
            deny_reason_for_segments(&["home", "user", ".aws", "credentials"]),
            Some(DenyReason::DeniedSecret)
        );
        assert_eq!(
            deny_reason_for_segments(&["home", "user", ".ssh", "id_rsa"]),
            Some(DenyReason::DeniedSecret)
        );
    }

    #[test]
    fn deny_docker_and_kube_config() {
        let segs = vec!["home", ".docker", "config.json"];
        assert_eq!(deny_reason_for_segments(&segs), Some(DenyReason::DeniedSecret));
        let segs = vec!["home", ".kube", "config"];
        assert_eq!(deny_reason_for_segments(&segs), Some(DenyReason::DeniedSecret));
    }

    #[test]
    fn allow_safe_paths() {
        assert_eq!(deny_reason_for_segments(&["src", "main.rs"]), None);
        assert_eq!(deny_reason_for_segments(&["README.md"]), None);
        assert_eq!(deny_reason_for_segments(&["a", "b", "c.txt"]), None);
    }

    #[test]
    fn content_type_for_image_extensions() {
        assert_eq!(content_type_for_path("a.png"), Some("image/png".to_string()));
        assert_eq!(content_type_for_path("a.JPG"), Some("image/jpeg".to_string()));
        assert_eq!(content_type_for_path("a.webp"), Some("image/webp".to_string()));
    }

    #[test]
    fn content_type_for_video_extensions() {
        assert_eq!(content_type_for_path("a.mp4"), Some("video/mp4".to_string()));
        assert_eq!(content_type_for_path("a.MOV"), Some("video/quicktime".to_string()));
    }

    #[test]
    fn content_type_for_pdf_svg_html_text() {
        assert_eq!(content_type_for_path("a.pdf"), Some("application/pdf".to_string()));
        assert_eq!(content_type_for_path("a.svg"), Some("image/svg+xml".to_string()));
        assert_eq!(content_type_for_path("a.html"), Some("text/html".to_string()));
        assert_eq!(content_type_for_path("a.htm"), Some("text/html".to_string()));
        assert_eq!(content_type_for_path("a.rs"), Some("text/plain; charset=utf-8".to_string()));
        assert_eq!(content_type_for_path("a.json"), Some("text/plain; charset=utf-8".to_string()));
    }

    #[test]
    fn content_type_unknown_returns_none() {
        assert_eq!(content_type_for_path("a.bin"), None);
        assert_eq!(content_type_for_path("a"), None);
    }

    #[test]
    fn preview_kind_classifies_known_types() {
        assert_eq!(preview_kind_for_content_type(Some("image/png")), PreviewKind::Image);
        assert_eq!(preview_kind_for_content_type(Some("image/jpeg")), PreviewKind::Image);
        assert_eq!(preview_kind_for_content_type(Some("image/svg+xml")), PreviewKind::Text);
        assert_eq!(preview_kind_for_content_type(Some("video/mp4")), PreviewKind::Video);
        assert_eq!(preview_kind_for_content_type(Some("application/pdf")), PreviewKind::Pdf);
        assert_eq!(preview_kind_for_content_type(Some("text/html")), PreviewKind::Unsupported);
        assert_eq!(preview_kind_for_content_type(Some("text/plain; charset=utf-8")), PreviewKind::Text);
        assert_eq!(preview_kind_for_content_type(Some("application/octet-stream")), PreviewKind::Unsupported);
        assert_eq!(preview_kind_for_content_type(None), PreviewKind::Unsupported);
    }

    #[test]
    fn preview_kind_str_round_trip() {
        assert_eq!(PreviewKind::Image.as_str(), "image");
        assert_eq!(PreviewKind::Video.as_str(), "video");
        assert_eq!(PreviewKind::Pdf.as_str(), "pdf");
        assert_eq!(PreviewKind::Text.as_str(), "text");
        assert_eq!(PreviewKind::Unsupported.as_str(), "unsupported");
    }

    #[test]
    fn looks_like_text_empty_buffer_is_text() {
        assert!(looks_like_text(&[]));
    }

    #[test]
    fn looks_like_text_nul_byte_rejects() {
        assert!(!looks_like_text(&[b'a', 0, b'b']));
    }

    #[test]
    fn looks_like_text_too_much_control_rejects() {
        // 100% 控制字符
        let buf: Vec<u8> = (0..100).map(|_| 1u8).collect();
        assert!(!looks_like_text(&buf));
    }

    #[test]
    fn looks_like_text_lots_of_tabs_newlines_ok() {
        let mut buf = Vec::new();
        for _ in 0..100 {
            buf.push(b'a');
            buf.push(b'\n');
            buf.push(b'\t');
        }
        assert!(looks_like_text(&buf));
    }

    #[test]
    fn extname_lowercase_with_dot() {
        assert_eq!(extname_lower("foo.PNG"), ".png");
        assert_eq!(extname_lower("foo"), "");
        assert_eq!(extname_lower("a.b.c"), ".c");
    }
}
