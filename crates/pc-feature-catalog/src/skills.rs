#![forbid(unsafe_code)]
//! `skills-catalog` —— Skills catalog data access for Paperclip.
//!
//! Direct port of `paperclip/server/src/services/skills-catalog.ts` (356 LOC).
//!
//! ## 设计目标
//!
//! - **纯数据逻辑**：manifest 解析、过滤、排序、引用解析、文件路径/语言推断
//! - **无 DB / 无网络 IO**：文件 IO 暴露在调用层（`std::fs` 同步读取），便于测试和嵌入
//! - **serde-friendly**：所有公开类型 derive `Serialize` / `Deserialize`，可以直接从
//!   manifest JSON 反序列化
//!
//! ## 公共 API（与 Node 1:1 对齐）
//!
//! | Rust | Node |
//! |---|---|
//! | [`CatalogManifestUnavailableError`] | `CatalogManifestUnavailableError` |
//! | [`is_catalog_manifest_unavailable_error`] | `isCatalogManifestUnavailableError` |
//! | [`CatalogSkill`] / [`CatalogSkillFileEntry`] / [`CatalogSkillFileDetail`] / [`CatalogSkillListQuery`] / [`CatalogSkillSource`] / [`CatalogManifestFile`] | 同名 types |
//! | [`list_catalog_skills`] | `listCatalogSkills` |
//! | [`list_catalog_skills_or_empty`] | `listCatalogSkillsOrEmpty` |
//! | [`resolve_catalog_skill_reference`] | `resolveCatalogSkillReference` |
//! | [`get_catalog_skill_or_throw`] | `getCatalogSkillOrThrow` |
//! | [`read_catalog_skill_file`] | `readCatalogSkillFile` |
//! | [`copy_catalog_skill_file`] | `copyCatalogSkillFile` |
//! | [`get_catalog_package_metadata`] | `getCatalogPackageMetadata` |
//! | [`is_markdown_path`] / [`infer_language_from_path`] / [`normalize_portable_path`] | internal helpers |
//!
//! ## 设计取舍
//!
//! - `teamsCatalogService` 类 DB-coupled factory 不在本 crate；调用方持有自己的 DB 句柄，
//!   调用这些纯函数完成读/写。
//! - manifest caching 用 `OnceLock` 而非 Node 的 module-level mutable cache，避免全局可变状态。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ============================================================================
// Types
// ============================================================================

/// Catalog skill manifest file format（Node `CatalogManifestFile` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogManifestFile {
    pub package_name: String,
    pub package_version: String,
    pub skills: Vec<CatalogSkill>,
}

/// Catalog skill —— manifest 中每个 skill 的完整定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogSkill {
    pub id: String,
    pub key: String,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub recommended_for_roles: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub path: String,
    #[serde(default)]
    pub files: Vec<CatalogSkillFileEntry>,
    #[serde(default, rename = "source")]
    pub source: Option<CatalogSkillSource>,
    /// 由 `attach_package_metadata` 注入（Node `getCatalogSkills` spread）。
    #[serde(default, rename = "packageName")]
    pub package_name: Option<String>,
    #[serde(default, rename = "packageVersion")]
    pub package_version: Option<String>,
}

/// Skill 文件条目 —— manifest 中的 `files[]`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogSkillFileEntry {
    pub path: String,
    pub sha256: String,
    #[serde(default)]
    pub kind: String,
}

/// Skill 来源（GitHub 等）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogSkillSource {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

/// `listCatalogSkills` 的查询参数。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CatalogSkillListQuery {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
}

/// `readCatalogSkillFile` 的返回值。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogSkillFileDetail {
    pub catalog_skill_id: String,
    pub path: String,
    pub kind: String,
    pub content: String,
    pub language: Option<String>,
    pub markdown: bool,
}

/// Catalog manifest 不可用错误。
///
/// 与 Node `CatalogManifestUnavailableError` 1:1 对齐：name = `"CatalogManifestUnavailableError"`。
#[derive(Debug, Clone)]
pub struct CatalogManifestUnavailableError {
    pub message: String,
}

impl std::fmt::Display for CatalogManifestUnavailableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CatalogManifestUnavailableError {}

impl CatalogManifestUnavailableError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// 与 Node `isCatalogManifestUnavailableError(error)` 1:1 对齐。
pub fn is_catalog_manifest_unavailable_error<E: std::error::Error + 'static>(
    error: &E,
) -> bool {
    use std::any::Any;
    let any: &dyn Any = error;
    any.downcast_ref::<CatalogManifestUnavailableError>().is_some()
}

// ============================================================================
// Path normalization (inline port of normalizePortablePath)
// ============================================================================

/// 规范化路径字符串。
///
/// 与 Node `normalizePortablePath` 1:1 对齐：
///
/// | 输入 | 输出 |
/// |---|---|
/// | `"foo/bar"` | `"foo/bar"` |
/// | `"foo\\bar"` | `"foo/bar"` |
/// | `"./foo"` | `"foo"` |
/// | `"/foo"` | `"foo"`（前导 / 全去掉） |
/// | `"foo/./bar"` | `"foo/bar"` |
/// | `"foo/../bar"` | `"bar"` |
pub fn normalize_portable_path(input: &str) -> String {
    let normalized = input.replace('\\', "/");
    let without_dot_slash = normalized.strip_prefix("./").unwrap_or(&normalized);
    let trimmed = without_dot_slash.trim_start_matches('/');

    let mut parts: Vec<&str> = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            if !parts.is_empty() {
                parts.pop();
            }
            continue;
        }
        parts.push(segment);
    }
    parts.join("/")
}

// ============================================================================
// Markdown / language inference
// ============================================================================

/// 与 Node `isMarkdownPath` 1:1 对齐：`skill.md` 或 `.md` 结尾返回 `true`。
pub fn is_markdown_path(file_path: &str) -> bool {
    let file_name = basename_posix(file_path).to_lowercase();
    file_name == "skill.md" || file_name.ends_with(".md")
}

/// 与 Node `inferLanguageFromPath` 1:1 对齐。
pub fn infer_language_from_path(file_path: &str) -> Option<&'static str> {
    let file_name = basename_posix(file_path).to_lowercase();
    if file_name == "skill.md" || file_name.ends_with(".md") {
        Some("markdown")
    } else if file_name.ends_with(".ts") {
        Some("typescript")
    } else if file_name.ends_with(".tsx") {
        Some("tsx")
    } else if file_name.ends_with(".js") {
        Some("javascript")
    } else if file_name.ends_with(".jsx") {
        Some("jsx")
    } else if file_name.ends_with(".json") {
        Some("json")
    } else if file_name.ends_with(".yml") || file_name.ends_with(".yaml") {
        Some("yaml")
    } else if file_name.ends_with(".sh") {
        Some("bash")
    } else if file_name.ends_with(".py") {
        Some("python")
    } else if file_name.ends_with(".html") {
        Some("html")
    } else if file_name.ends_with(".css") {
        Some("css")
    } else {
        None
    }
}

fn basename_posix(file_path: &str) -> &str {
    // path.posix.basename — 简单实现
    match file_path.rfind('/') {
        Some(idx) => &file_path[idx + 1..],
        None => file_path,
    }
}

// ============================================================================
// Internal: skill resolution, source roots
// ============================================================================

/// Source root path —— 与 Node `sourceRootPath` 1:1 对齐。
pub fn source_root_path(source: &CatalogSkillSource) -> String {
    source.path.as_deref().map(normalize_portable_path).unwrap_or_default()
}

/// Resolve catalog source path —— 与 Node `resolveCatalogSourcePath` 1:1 对齐。
pub fn resolve_catalog_source_path(source: &CatalogSkillSource, relative_path: &str) -> String {
    let root = source_root_path(source);
    if root.is_empty() {
        relative_path.to_string()
    } else {
        format!("{root}/{relative_path}")
    }
}

/// 从 skills 列表中按 id/key/slug 解析单个 skill。
///
/// 与 Node `resolveCatalogSkillReference` 1:1 对齐：返回 `{ skill, ambiguous }`。
pub fn resolve_catalog_skill_reference(
    skills: &[CatalogSkill],
    reference: &str,
) -> CatalogSkillReference {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return CatalogSkillReference {
            skill: None,
            ambiguous: false,
        };
    }

    if let Some(skill) = skills
        .iter()
        .find(|s| s.id == trimmed || s.key == trimmed)
    {
        return CatalogSkillReference {
            skill: Some(skill.clone()),
            ambiguous: false,
        };
    }

    let slug_matches: Vec<&CatalogSkill> =
        skills.iter().filter(|s| s.slug == trimmed).collect();
    if slug_matches.len() == 1 {
        return CatalogSkillReference {
            skill: Some(slug_matches[0].clone()),
            ambiguous: false,
        };
    }
    if slug_matches.len() > 1 {
        return CatalogSkillReference {
            skill: None,
            ambiguous: true,
        };
    }

    CatalogSkillReference {
        skill: None,
        ambiguous: false,
    }
}

/// `resolveCatalogSkillReference` 的返回值（1:1 对齐）。
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogSkillReference {
    pub skill: Option<CatalogSkill>,
    pub ambiguous: bool,
}

/// 与 Node `getCatalogSkillOrThrow` 1:1 对齐。
pub fn get_catalog_skill_or_throw(
    skills: &[CatalogSkill],
    reference: &str,
) -> Result<CatalogSkill, CatalogError> {
    let result = resolve_catalog_skill_reference(skills, reference);
    if result.ambiguous {
        return Err(CatalogError::Ambiguous(format!(
            "Catalog skill slug \"{reference}\" is ambiguous. Use an id or key."
        )));
    }
    match result.skill {
        Some(s) => Ok(s),
        None => Err(CatalogError::NotFound("Catalog skill not found".to_string())),
    }
}

// ============================================================================
// Listing / searching / filtering
// ============================================================================

/// 过滤 + 搜索 + 排序 —— 与 Node `listCatalogSkills` 1:1 对齐。
pub fn list_catalog_skills(
    manifest: &CatalogManifestFile,
    query: &CatalogSkillListQuery,
) -> Vec<CatalogSkill> {
    let normalized_query = query
        .q
        .as_deref()
        .map(|q| q.trim().to_lowercase())
        .unwrap_or_default();

    let mut out: Vec<CatalogSkill> = manifest
        .skills
        .iter()
        .filter(|skill| query.kind.as_ref().is_none_or(|k| &skill.kind == k))
        .filter(|skill| {
            query
                .category
                .as_ref()
                .is_none_or(|c| &skill.category == c)
        })
        .filter(|skill| {
            normalized_query.is_empty() || search_text(skill).contains(&normalized_query)
        })
        .map(|skill| attach_package_metadata(skill, manifest))
        .collect();

    out.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then(left.key.to_lowercase().cmp(&right.key.to_lowercase()))
    });
    out
}

fn attach_package_metadata(skill: &CatalogSkill, manifest: &CatalogManifestFile) -> CatalogSkill {
    let mut skill = skill.clone();
    skill.package_name = Some(manifest.package_name.clone());
    skill.package_version = Some(manifest.package_version.clone());
    skill
}

/// `searchText` —— 把可搜字段 join + lowercase。
fn search_text(skill: &CatalogSkill) -> String {
    [
        skill.id.as_str(),
        skill.key.as_str(),
        skill.slug.as_str(),
        skill.name.as_str(),
        skill.description.as_str(),
        skill.category.as_str(),
        skill.kind.as_str(),
    ]
    .into_iter()
    .chain(skill.recommended_for_roles.iter().map(String::as_str))
    .chain(skill.tags.iter().map(String::as_str))
    .collect::<Vec<&str>>()
    .join("\n")
    .to_lowercase()
}

// ============================================================================
// Errors for non-manifest operations
// ============================================================================

/// Skills catalog 错误（manifest 错误单独定义）。
#[derive(Debug, Clone, PartialEq)]
pub enum CatalogError {
    NotFound(String),
    Ambiguous(String),
    Unprocessable(String),
    UnsupportedMediaType(String),
    Io(String),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(msg) => f.write_str(msg),
            Self::Ambiguous(msg) => f.write_str(msg),
            Self::Unprocessable(msg) => f.write_str(msg),
            Self::UnsupportedMediaType(msg) => f.write_str(msg),
            Self::Io(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<io::Error> for CatalogError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

// ============================================================================
// File reading / copying
// ============================================================================

/// Resolve a single file path inside the catalog package root.
///
/// 与 Node `resolveCatalogSourceFile` 的本地分支 1:1 对齐。
///
/// - `source == None` → 读取 `package_root / skill.path / relative_path`
/// - `source.kind == "github"` → 通过 caller 提供的 fetcher（参数未在此 API 暴露）
pub fn resolve_local_skill_file_path(
    package_root: &Path,
    skill: &CatalogSkill,
    relative_path: &str,
) -> Result<PathBuf, CatalogError> {
    let skill_root = package_root.join(&skill.path);
    let absolute = join_canonical(&skill_root, relative_path);
    // Containment check: ensure absolute path stays inside skill_root
    if absolute != skill_root && !starts_with(&absolute, &skill_root) {
        return Err(CatalogError::NotFound("Catalog skill file not found".into()));
    }
    Ok(absolute)
}

fn join_canonical(base: &Path, relative: &str) -> PathBuf {
    let normalized = normalize_portable_path(relative);
    if normalized.is_empty() {
        return base.to_path_buf();
    }
    base.join(&normalized)
}

fn starts_with(path: &Path, prefix: &Path) -> bool {
    path.starts_with(prefix)
}

/// 与 Node `readCatalogSkillFile` 1:1 对齐（仅本地 source 分支）。
///
/// `default_relative_path` 默认 `"SKILL.md"`。
pub fn read_catalog_skill_file(
    package_root: &Path,
    manifest: &CatalogManifestFile,
    reference: &str,
    relative_path: Option<&str>,
) -> Result<CatalogSkillFileDetail, CatalogError> {
    let skill = get_catalog_skill_or_throw(&manifest.skills, reference)?;
    let raw = relative_path.unwrap_or("SKILL.md");
    let normalized_path = normalize_portable_path(if raw.is_empty() { "SKILL.md" } else { raw });

    let file_entry = skill
        .files
        .iter()
        .find(|f| f.path == normalized_path)
        .ok_or_else(|| CatalogError::NotFound("Catalog skill file not found".into()))?;

    if file_entry.kind == "asset" {
        return Err(CatalogError::UnsupportedMediaType(
            "Catalog asset previews are not supported.".into(),
        ));
    }

    let absolute = resolve_local_skill_file_path(package_root, &skill, &normalized_path)?;
    let bytes = fs::read(&absolute)?;
    let content = String::from_utf8(bytes)
        .map_err(|e| CatalogError::Unprocessable(format!("Non-UTF8 catalog file: {e}")))?;

    Ok(CatalogSkillFileDetail {
        catalog_skill_id: skill.id.clone(),
        path: normalized_path.clone(),
        kind: file_entry.kind.clone(),
        content,
        language: infer_language_from_path(&normalized_path).map(|s| s.to_string()),
        markdown: is_markdown_path(&normalized_path),
    })
}

/// 与 Node `copyCatalogSkillFile` 1:1 对齐（仅本地 source 分支）。
pub fn copy_catalog_skill_file(
    package_root: &Path,
    manifest: &CatalogManifestFile,
    reference: &str,
    relative_path: &str,
    target_path: &Path,
) -> Result<(), CatalogError> {
    let skill = get_catalog_skill_or_throw(&manifest.skills, reference)?;
    let normalized_path = normalize_portable_path(if relative_path.is_empty() {
        "SKILL.md"
    } else {
        relative_path
    });

    let _file_entry = skill
        .files
        .iter()
        .find(|f| f.path == normalized_path)
        .ok_or_else(|| CatalogError::NotFound("Catalog skill file not found".into()))?;

    let absolute = resolve_local_skill_file_path(package_root, &skill, &normalized_path)?;
    let bytes = fs::read(&absolute)?;
    if let Some(parent) = target_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(target_path, bytes)?;
    Ok(())
}

// ============================================================================
// Manifest metadata
// ============================================================================

/// 与 Node `getCatalogPackageMetadata` 1:1 对齐。
pub fn get_catalog_package_metadata(manifest: &CatalogManifestFile) -> CatalogPackageMetadata {
    CatalogPackageMetadata {
        package_name: manifest.package_name.clone(),
        package_version: manifest.package_version.clone(),
    }
}

/// 与 Node `getCatalogPackageMetadata` 返回值 1:1 对齐。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogPackageMetadata {
    pub package_name: String,
    pub package_version: String,
}

// ============================================================================
// Manifest parsing (with caching via OnceLock)
// ============================================================================

/// Manifest cache entry.
#[derive(Debug, Clone)]
struct CachedManifest {
    manifest: CatalogManifestFile,
    mtime_ms: u64,
    size: u64,
}

/// Per-path manifest cache. Avoids reading the same file repeatedly.
///
/// 与 Node 的 `cachedCatalogManifest` module-level cache 1:1 对齐。
#[derive(Debug, Default)]
pub struct ManifestCache {
    inner: std::sync::Mutex<Option<CachedManifest>>,
}

impl ManifestCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a manifest, using the cache when the file's mtime+size match.
    pub fn load(&self, manifest_path: &Path) -> Result<CatalogManifestFile, CatalogManifestUnavailableError> {
        if !manifest_path.exists() {
            return Err(CatalogManifestUnavailableError::new(format!(
                "Skills catalog manifest not found at {}. Run pnpm --filter @paperclipai/skills-catalog build:manifest.",
                manifest_path.display()
            )));
        }
        let meta = fs::metadata(manifest_path).map_err(|e| {
            CatalogManifestUnavailableError::new(format!(
                "Skills catalog manifest not found at {}. ({})",
                manifest_path.display(),
                e
            ))
        })?;
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let size = meta.len();

        let mut guard = self.inner.lock().expect("ManifestCache poisoned");
        if let Some(cached) = guard.as_ref() {
            if cached.mtime_ms == mtime_ms && cached.size == size {
                return Ok(cached.manifest.clone());
            }
        }

        let raw = fs::read_to_string(manifest_path).map_err(|e| {
            CatalogManifestUnavailableError::new(format!(
                "Skills catalog manifest not found at {}. ({})",
                manifest_path.display(),
                e
            ))
        })?;
        let manifest: CatalogManifestFile = serde_json::from_str(&raw).map_err(|e| {
            CatalogManifestUnavailableError::new(format!(
                "Failed to parse skills catalog manifest at {}: {}",
                manifest_path.display(),
                e
            ))
        })?;

        *guard = Some(CachedManifest {
            manifest: manifest.clone(),
            mtime_ms,
            size,
        });
        Ok(manifest)
    }
}

/// Parse a manifest JSON string (no cache, no IO).
pub fn parse_manifest(raw: &str) -> Result<CatalogManifestFile, serde_json::Error> {
    serde_json::from_str(raw)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_manifest() -> CatalogManifestFile {
        CatalogManifestFile {
            package_name: "@paperclipai/skills-catalog".into(),
            package_version: "1.2.3".into(),
            skills: vec![
                CatalogSkill {
                    id: "skill-1".into(),
                    key: "alpha/beta".into(),
                    slug: "alpha-beta".into(),
                    name: "Alpha Beta".into(),
                    description: "Alpha beta skill for testing".into(),
                    category: "productivity".into(),
                    kind: "agent".into(),
                    recommended_for_roles: vec!["engineer".into()],
                    tags: vec!["writing".into(), "review".into()],
                    path: "skills/alpha-beta".into(),
                    files: vec![],
                    source: None,
                    package_name: None,
                    package_version: None,
                },
                CatalogSkill {
                    id: "skill-2".into(),
                    key: "gamma/delta".into(),
                    slug: "shared-delta".into(),
                    name: "Gamma Delta".into(),
                    description: "Another skill".into(),
                    category: "engineering".into(),
                    kind: "agent".into(),
                    recommended_for_roles: vec!["pm".into()],
                    tags: vec!["planning".into()],
                    path: "skills/gamma-delta".into(),
                    files: vec![],
                    source: None,
                    package_name: None,
                    package_version: None,
                },
                CatalogSkill {
                    id: "skill-3".into(),
                    key: "epsilon/zeta".into(),
                    slug: "epsilon-zeta".into(),
                    name: "Epsilon Zeta".into(),
                    description: "Same slug as another".into(),
                    category: "engineering".into(),
                    kind: "team".into(),
                    recommended_for_roles: vec![],
                    tags: vec![],
                    path: "skills/epsilon".into(),
                    files: vec![],
                    source: None,
                    package_name: None,
                    package_version: None,
                },
                CatalogSkill {
                    id: "skill-4".into(),
                    key: "eta/theta".into(),
                    slug: "shared-delta".into(), // intentional collision with skill-2
                    name: "Eta Theta".into(),
                    description: "Same slug, different id".into(),
                    category: "engineering".into(),
                    kind: "team".into(),
                    recommended_for_roles: vec![],
                    tags: vec![],
                    path: "skills/eta".into(),
                    files: vec![],
                    source: None,
                    package_name: None,
                    package_version: None,
                },
            ],
        }
    }

    #[test]
    fn r846_normalize_portable_path_preserves_basic() {
        assert_eq!(normalize_portable_path("foo/bar"), "foo/bar");
    }

    #[test]
    fn r846_normalize_portable_path_collapses_separators() {
        assert_eq!(normalize_portable_path("foo\\bar"), "foo/bar");
        assert_eq!(normalize_portable_path("./foo"), "foo");
        assert_eq!(normalize_portable_path("/foo"), "foo");
        assert_eq!(normalize_portable_path("foo/./bar"), "foo/bar");
        assert_eq!(normalize_portable_path("foo/../bar"), "bar");
        assert_eq!(normalize_portable_path("a/b/../../c"), "c");
    }

    #[test]
    fn r846_infer_language_from_path() {
        assert_eq!(infer_language_from_path("SKILL.md"), Some("markdown"));
        assert_eq!(infer_language_from_path("sub/foo.ts"), Some("typescript"));
        assert_eq!(infer_language_from_path("a/b.PY"), Some("python"));
        assert_eq!(infer_language_from_path("noext"), None);
    }

    #[test]
    fn r846_is_markdown_path() {
        assert!(is_markdown_path("SKILL.md"));
        assert!(is_markdown_path("docs/foo.md"));
        assert!(!is_markdown_path("foo.ts"));
    }

    #[test]
    fn r846_list_filters_by_kind_category_q() {
        let manifest = fixture_manifest();
        let only_teams = list_catalog_skills(
            &manifest,
            &CatalogSkillListQuery {
                kind: Some("team".into()),
                ..Default::default()
            },
        );
        assert_eq!(only_teams.len(), 2);
        assert!(only_teams.iter().all(|s| s.kind == "team"));

        let only_engineering = list_catalog_skills(
            &manifest,
            &CatalogSkillListQuery {
                category: Some("engineering".into()),
                ..Default::default()
            },
        );
        assert_eq!(only_engineering.len(), 3);

        let by_query = list_catalog_skills(
            &manifest,
            &CatalogSkillListQuery {
                q: Some("alpha".into()),
                ..Default::default()
            },
        );
        assert_eq!(by_query.len(), 1);
        assert_eq!(by_query[0].id, "skill-1");
    }

    #[test]
    fn r846_list_sorts_by_name_then_key() {
        let manifest = fixture_manifest();
        let all = list_catalog_skills(&manifest, &CatalogSkillListQuery::default());
        // Names: Alpha Beta, Epsilon Zeta, Eta Theta, Gamma Delta
        // After case-insensitive sort by name: Alpha Beta, Epsilon Zeta, Eta Theta, Gamma Delta
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha Beta", "Epsilon Zeta", "Eta Theta", "Gamma Delta"]);
    }

    #[test]
    fn r846_list_attaches_package_metadata() {
        let manifest = fixture_manifest();
        let all = list_catalog_skills(&manifest, &CatalogSkillListQuery::default());
        for skill in &all {
            assert_eq!(skill.package_name.as_deref(), Some("@paperclipai/skills-catalog"));
            assert_eq!(skill.package_version.as_deref(), Some("1.2.3"));
        }
    }

    #[test]
    fn r846_resolve_reference_id_key_slug_empty() {
        let manifest = fixture_manifest();
        let by_id = resolve_catalog_skill_reference(&manifest.skills, "skill-1");
        assert_eq!(by_id.skill.as_ref().unwrap().id, "skill-1");
        assert!(!by_id.ambiguous);

        let by_key = resolve_catalog_skill_reference(&manifest.skills, "alpha/beta");
        assert_eq!(by_key.skill.as_ref().unwrap().id, "skill-1");

        let empty = resolve_catalog_skill_reference(&manifest.skills, "  ");
        assert!(empty.skill.is_none());
        assert!(!empty.ambiguous);
    }

    #[test]
    fn r846_resolve_reference_slug_ambiguous() {
        let manifest = fixture_manifest();
        // shared-delta collides between skill-2 and skill-4
        let ambiguous = resolve_catalog_skill_reference(&manifest.skills, "shared-delta");
        assert!(ambiguous.skill.is_none());
        assert!(ambiguous.ambiguous);
    }

    #[test]
    fn r846_get_or_throw_errors_on_missing_and_ambiguous() {
        let manifest = fixture_manifest();
        assert!(get_catalog_skill_or_throw(&manifest.skills, "shared-delta").is_err());
        assert!(get_catalog_skill_or_throw(&manifest.skills, "no-such").is_err());
        let found = get_catalog_skill_or_throw(&manifest.skills, "alpha/beta").unwrap();
        assert_eq!(found.id, "skill-1");
    }

    #[test]
    fn r846_get_package_metadata() {
        let manifest = fixture_manifest();
        let meta = get_catalog_package_metadata(&manifest);
        assert_eq!(meta.package_name, "@paperclipai/skills-catalog");
        assert_eq!(meta.package_version, "1.2.3");
    }

    #[test]
    fn r846_parse_manifest_roundtrip() {
        let manifest = fixture_manifest();
        let json = serde_json::to_string(&manifest).unwrap();
        let parsed = parse_manifest(&json).unwrap();
        assert_eq!(parsed.package_name, manifest.package_name);
        assert_eq!(parsed.skills.len(), manifest.skills.len());
    }

    #[test]
    fn r846_resolve_source_path_with_and_without_root() {
        let source_with = CatalogSkillSource {
            kind: "github".into(),
            hostname: None,
            owner: Some("o".into()),
            repo: Some("r".into()),
            commit: Some("c".into()),
            path: Some("sub\\dir".into()),
        };
        assert_eq!(
            resolve_catalog_source_path(&source_with, "SKILL.md"),
            "sub/dir/SKILL.md"
        );
        let source_no_path = CatalogSkillSource {
            path: None,
            ..source_with
        };
        assert_eq!(
            resolve_catalog_source_path(&source_no_path, "SKILL.md"),
            "SKILL.md"
        );
    }
}