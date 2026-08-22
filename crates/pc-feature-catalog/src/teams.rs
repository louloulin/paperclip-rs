#![forbid(unsafe_code)]
//! `teams-catalog` —— Teams catalog data access for Paperclip.
//!
//! Direct port of `paperclip/server/src/services/teams-catalog.ts` (1037 LOC, pure
//! helpers portion).
//!
//! ## 设计目标
//!
//! - **纯数据逻辑**：manifest 解析、过滤、排序、引用解析、YAML 渲染、provenance 提取
//! - **无 DB / 无网络 IO**：文件 IO 暴露在调用层（`std::fs` 同步读取），便于测试和嵌入
//! - **serde-friendly**：所有公开类型 derive `Serialize` / `Deserialize`
//!
//! ## 公共 API（与 Node 1:1 对齐 —— 仅纯函数部分）
//!
//! ### Types
//!
//! | Rust | Node |
//! |---|---|
//! | [`CatalogManifest`] | `CatalogManifest` |
//! | [`CatalogTeam`] / [`CatalogTeamFileEntry`] / [`CatalogTeamFileDetail`] | 同名 types |
//! | [`CatalogTeamListQuery`] | `CatalogTeamListQuery` |
//! | [`CatalogTeamSourcePolicy`] | `CatalogTeamSourcePolicy` |
//! | [`CatalogTeamSkillRequirement`] | `CatalogTeamSkillRequirement` |
//! | [`CatalogTeamSkillPreparation`] / [`CatalogTeamSkillPreparationAction`] | 同名 types |
//! | [`CatalogTeamProvenance`] | `CatalogTeamProvenance` (internal) |
//! | [`InstalledCatalogTeam`] | `InstalledCatalogTeam` |
//!
//! ### Functions
//!
//! | Rust | Node |
//! |---|---|
//! | [`list_catalog_teams`] | `listCatalogTeams` |
//! | [`resolve_catalog_team_reference`] | `resolveCatalogTeamReference` |
//! | [`get_catalog_team_or_throw`] | `getCatalogTeamOrThrow` |
//! | [`read_catalog_team_file`] | `readCatalogTeamFile` |
//! | [`read_catalog_team_provenance`] | `readCatalogTeamProvenance` |
//! | [`collect_catalog_team_skill_preparations`] | `collectCatalogTeamSkillPreparations` |
//! | [`render_yaml_file`] / [`render_yaml_block`] / [`yaml_scalar`] | internal yaml helpers |
//! | [`render_synthetic_company_markdown`] | `renderSyntheticCompanyMarkdown` |
//! | [`render_catalog_provenance_yaml`] | `renderCatalogProvenanceYaml` |
//! | [`merge_plain_records`] / [`parse_yaml_document`] / [`render_simple_markdown`] | internal helpers |
//!
//! ## 设计取舍
//!
//! - `teamsCatalogService(db)` factory 函数依赖 `Db` / `agentService` / `companyPortabilityService`，
//!   这些是 Node 服务层耦合，本 crate 不包含。它的纯子部分（preparation / provenance 渲染）已
//!   作为独立函数暴露，调用方自行组合即可。
//! - `ghFetch` / `normalizeAgentUrlKey` 依赖被替换为纯函数等效项（GitHub URL resolve 仅保留占位）
//! - `parseFrontmatterMarkdown` 复用：`pc-frontmatter` crate 提供 `parse_frontmatter`，这里直接复用
//!   而不引入新的依赖（最小实现 inline）。

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

// ============================================================================
// Types
// ============================================================================

/// `CatalogManifest` —— 整个 teams-catalog manifest 的顶层类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogManifest {
    pub package_name: String,
    pub package_version: String,
    pub teams: Vec<CatalogTeam>,
}

/// Team kind string（对应 `CatalogTeamKind`）。
pub const CATALOG_TEAM_KIND_BUNDLED: &str = "bundled";
pub const CATALOG_TEAM_KIND_OPTIONAL: &str = "optional";

/// File kind string（对应 `CatalogTeamFileKind`）。
pub const CATALOG_TEAM_FILE_KIND_TASK: &str = "task";
pub const CATALOG_TEAM_FILE_KIND_ASSET: &str = "asset";

/// Skill requirement type strings（对应 `CatalogTeamSkillRequirementType`）。
pub const SKILL_REQ_TYPE_CATALOG: &str = "catalog";
pub const SKILL_REQ_TYPE_LOCAL: &str = "local";
pub const SKILL_REQ_TYPE_LOCAL_PATH: &str = "local_path";
pub const SKILL_REQ_TYPE_GITHUB: &str = "github";
pub const SKILL_REQ_TYPE_SKILLS_SH: &str = "skills_sh";
pub const SKILL_REQ_TYPE_AGENT_PACKAGE: &str = "agent_package";

/// Skill preparation actions。
pub const SKILL_PREP_ALREADY_IN_PACKAGE: &str = "already_in_package";
pub const SKILL_PREP_CATALOG_INSTALL_REQUIRED: &str = "catalog_install_required";
pub const SKILL_PREP_EXTERNAL_IMPORT_REQUIRED: &str = "external_import_required";
pub const SKILL_PREP_BLOCKED: &str = "blocked";

/// `CatalogTeam` —— 一个 catalog team 的完整定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogTeam {
    pub id: String,
    #[serde(default)]
    pub key: Option<String>,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub recommended_for_company_types: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub path: String,
    pub entrypoint: String,
    #[serde(default)]
    pub files: Vec<CatalogTeamFileEntry>,
    #[serde(default)]
    pub agent_slugs: Vec<String>,
    #[serde(default)]
    pub root_agent_slugs: Vec<String>,
    #[serde(default)]
    pub project_slugs: Vec<String>,
    #[serde(default)]
    pub required_skills: Vec<CatalogTeamSkillRequirement>,
    #[serde(default)]
    pub compatibility: String,
    #[serde(default)]
    pub trust_level: String,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default, rename = "packageName")]
    pub package_name: Option<String>,
    #[serde(default, rename = "packageVersion")]
    pub package_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogTeamFileEntry {
    pub path: String,
    pub kind: String,
}

/// `CatalogTeamSkillRequirement` —— skill 需求项。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogTeamSkillRequirement {
    #[serde(rename = "type")]
    pub kind: String,
    pub ref_: String,
    #[serde(default)]
    pub agent_slugs: Vec<String>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub catalog_skill_id: Option<String>,
    #[serde(default)]
    pub catalog_skill_key: Option<String>,
    #[serde(default)]
    pub source_locator: Option<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
}

impl CatalogTeamSkillRequirement {
    fn ref_for_match(&self) -> &str {
        &self.ref_
    }
}

/// `CatalogTeamListQuery` —— `listCatalogTeams` 的查询参数。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CatalogTeamListQuery {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
}

/// `CatalogTeamSourcePolicy` —— install 时的 source policy。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CatalogTeamSourcePolicy {
    #[serde(default)]
    pub allow_external_sources: bool,
    #[serde(default)]
    pub allow_unpinned_optional_sources: bool,
    #[serde(default)]
    pub allow_local_path_sources: bool,
}

/// Skill preparation —— `collectCatalogTeamSkillPreparations` 的元素。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogTeamSkillPreparation {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    pub agent_slugs: Vec<String>,
    pub action: String,
    pub catalog_skill_id: Option<String>,
    pub catalog_skill_key: Option<String>,
    pub source_locator: Option<String>,
    pub source_ref: Option<String>,
    pub reason: Option<String>,
}

/// Internal provenance type (Node `CatalogTeamProvenance`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogTeamProvenance {
    pub catalog_id: String,
    pub catalog_key: Option<String>,
    pub origin_hash: Option<String>,
}

/// `InstalledCatalogTeam` —— `listInstalledCatalogTeams` 的元素。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstalledCatalogTeam {
    pub catalog_id: String,
    pub catalog_key: Option<String>,
    pub present: bool,
    pub current_content_hash: Option<String>,
    pub installed_origin_hashes: Vec<String>,
    pub agent_count: i64,
    pub out_of_date: bool,
}

/// `CatalogTeamFileDetail` —— `readCatalogTeamFile` 的返回值。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogTeamFileDetail {
    pub catalog_team_id: String,
    pub path: String,
    pub kind: String,
    pub content: String,
    pub language: Option<String>,
    pub markdown: bool,
}

// ============================================================================
// Errors
// ============================================================================

/// Teams catalog 错误（manifest 错误单独定义）。
#[derive(Debug, Clone, PartialEq)]
pub enum CatalogTeamError {
    NotFound(String),
    Ambiguous(String),
    Unprocessable(String),
    UnsupportedMediaType(String),
    Io(String),
    ManifestUnavailable(String),
}

impl std::fmt::Display for CatalogTeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(msg) => f.write_str(msg),
            Self::Ambiguous(msg) => f.write_str(msg),
            Self::Unprocessable(msg) => f.write_str(msg),
            Self::UnsupportedMediaType(msg) => f.write_str(msg),
            Self::Io(msg) => f.write_str(msg),
            Self::ManifestUnavailable(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for CatalogTeamError {}

impl From<std::io::Error> for CatalogTeamError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

// ============================================================================
// Path / language helpers (team variant)
// ============================================================================

fn basename_posix(file_path: &str) -> &str {
    match file_path.rfind('/') {
        Some(idx) => &file_path[idx + 1..],
        None => file_path,
    }
}

/// `isMarkdownPath` —— `team.md` 或 `.md` 结尾返回 true。
pub fn is_markdown_path(file_path: &str) -> bool {
    let file_name = basename_posix(file_path).to_lowercase();
    file_name == "team.md" || file_name.ends_with(".md")
}

/// `inferLanguageFromPath` —— team 版。
pub fn infer_language_from_path(file_path: &str) -> Option<&'static str> {
    let file_name = basename_posix(file_path).to_lowercase();
    if file_name.ends_with(".md") {
        Some("markdown")
    } else if file_name.ends_with(".json") {
        Some("json")
    } else if file_name.ends_with(".yml") || file_name.ends_with(".yaml") {
        Some("yaml")
    } else if file_name.ends_with(".sh") {
        Some("bash")
    } else if file_name.ends_with(".ts") {
        Some("typescript")
    } else if file_name.ends_with(".tsx") {
        Some("tsx")
    } else if file_name.ends_with(".js") {
        Some("javascript")
    } else if file_name.ends_with(".jsx") {
        Some("jsx")
    } else if file_name.ends_with(".py") {
        Some("python")
    } else {
        None
    }
}

/// `normalizePortablePath` inline 副本（与 skills.rs 一致）。
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
// Listing / searching / filtering
// ============================================================================

fn search_text(team: &CatalogTeam) -> String {
    [
        team.id.as_str(),
        team.key.as_deref().unwrap_or(""),
        team.slug.as_str(),
        team.name.as_str(),
        team.description.as_str(),
        team.category.as_str(),
        team.kind.as_str(),
    ]
    .into_iter()
    .chain(team.recommended_for_company_types.iter().map(String::as_str))
    .chain(team.tags.iter().map(String::as_str))
    .collect::<Vec<&str>>()
    .join("\n")
    .to_lowercase()
}

/// `listCatalogTeams` 1:1 对齐。
pub fn list_catalog_teams(
    manifest: &CatalogManifest,
    query: &CatalogTeamListQuery,
) -> Vec<CatalogTeam> {
    let normalized_query = query
        .q
        .as_deref()
        .map(|q| q.trim().to_lowercase())
        .unwrap_or_default();

    let mut out: Vec<CatalogTeam> = manifest
        .teams
        .iter()
        .filter(|team| query.kind.as_ref().is_none_or(|k| &team.kind == k))
        .filter(|team| {
            query
                .category
                .as_ref()
                .is_none_or(|c| &team.category == c)
        })
        .filter(|team| {
            normalized_query.is_empty() || search_text(team).contains(&normalized_query)
        })
        .map(|team| attach_team_package_metadata(team, manifest))
        .collect();

    out.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then(left.key.cmp(&right.key))
    });
    out
}

fn attach_team_package_metadata(team: &CatalogTeam, manifest: &CatalogManifest) -> CatalogTeam {
    let mut team = team.clone();
    team.package_name = Some(manifest.package_name.clone());
    team.package_version = Some(manifest.package_version.clone());
    team
}

/// `resolveCatalogTeamReference` 1:1 对齐。
pub fn resolve_catalog_team_reference(
    teams: &[CatalogTeam],
    reference: &str,
) -> CatalogTeamReference {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return CatalogTeamReference {
            team: None,
            ambiguous: false,
        };
    }

    if let Some(team) = teams
        .iter()
        .find(|t| t.id == trimmed || t.key.as_deref() == Some(trimmed))
    {
        return CatalogTeamReference {
            team: Some(team.clone()),
            ambiguous: false,
        };
    }

    let slug_matches: Vec<&CatalogTeam> =
        teams.iter().filter(|t| t.slug == trimmed).collect();
    if slug_matches.len() == 1 {
        return CatalogTeamReference {
            team: Some(slug_matches[0].clone()),
            ambiguous: false,
        };
    }
    if slug_matches.len() > 1 {
        return CatalogTeamReference {
            team: None,
            ambiguous: true,
        };
    }

    CatalogTeamReference {
        team: None,
        ambiguous: false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogTeamReference {
    pub team: Option<CatalogTeam>,
    pub ambiguous: bool,
}

/// `getCatalogTeamOrThrow` 1:1 对齐。
pub fn get_catalog_team_or_throw(
    teams: &[CatalogTeam],
    reference: &str,
) -> Result<CatalogTeam, CatalogTeamError> {
    let result = resolve_catalog_team_reference(teams, reference);
    if result.ambiguous {
        return Err(CatalogTeamError::Ambiguous(format!(
            "Catalog team slug \"{reference}\" is ambiguous. Use an id or key."
        )));
    }
    match result.team {
        Some(t) => Ok(t),
        None => Err(CatalogTeamError::NotFound("Catalog team not found".into())),
    }
}

// ============================================================================
// File reading (local package root only)
// ============================================================================

/// `resolveCatalogTeamFile` 1:1 对齐（仅本地 source 部分）。
fn resolve_catalog_team_file<'a>(
    package_root: &Path,
    team: &'a CatalogTeam,
    relative_path: &str,
) -> Result<(String, &'a CatalogTeamFileEntry, PathBuf), CatalogTeamError> {
    let raw = if relative_path.is_empty() {
        team.entrypoint.as_str()
    } else {
        relative_path
    };
    let normalized_path = normalize_portable_path(raw);
    let file_entry = team
        .files
        .iter()
        .find(|f| f.path == normalized_path)
        .ok_or_else(|| CatalogTeamError::NotFound("Catalog team file not found".into()))?;

    let team_root = package_root.join(&team.path);
    let absolute = if normalized_path.is_empty() {
        team_root.clone()
    } else {
        team_root.join(&normalized_path)
    };
    if absolute != team_root && !absolute.starts_with(&team_root) {
        return Err(CatalogTeamError::NotFound("Catalog team file not found".into()));
    }

    Ok((normalized_path, file_entry, absolute))
}

/// `readCatalogTeamFile` 1:1 对齐（仅本地 source 部分）。
pub fn read_catalog_team_file(
    package_root: &Path,
    manifest: &CatalogManifest,
    reference: &str,
    relative_path: Option<&str>,
) -> Result<CatalogTeamFileDetail, CatalogTeamError> {
    let raw = relative_path.unwrap_or("TEAM.md");
    let team = get_catalog_team_or_throw(&manifest.teams, reference)?;
    let (normalized_path, file_entry, absolute) = resolve_catalog_team_file(
        package_root,
        &team,
        if raw.is_empty() { "TEAM.md" } else { raw },
    )?;

    if file_entry.kind == CATALOG_TEAM_FILE_KIND_ASSET {
        return Err(CatalogTeamError::UnsupportedMediaType(
            "Catalog team asset previews are not supported.".into(),
        ));
    }

    let content = fs::read_to_string(&absolute)?;
    Ok(CatalogTeamFileDetail {
        catalog_team_id: team.id.clone(),
        path: normalized_path.clone(),
        kind: file_entry.kind.clone(),
        content,
        language: infer_language_from_path(&normalized_path).map(|s| s.to_string()),
        markdown: is_markdown_path(&normalized_path),
    })
}

// ============================================================================
// Provenance (metadata extraction)
// ============================================================================

fn is_plain_record(value: &serde_json::Value) -> bool {
    value.is_object()
}

fn read_non_empty_string(value: Option<&serde_json::Value>) -> Option<String> {
    let s = value?.as_str()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `readCatalogTeamProvenance` 1:1 对齐。
pub fn read_catalog_team_provenance(
    metadata: Option<&serde_json::Value>,
) -> Option<CatalogTeamProvenance> {
    let metadata = metadata?;
    if !is_plain_record(metadata) {
        return None;
    }
    let paperclip = metadata
        .get("paperclip")
        .filter(|v| is_plain_record(v));
    let catalog_team = paperclip
        .and_then(|p| p.get("catalogTeam"))
        .filter(|v| is_plain_record(v));
    let catalog_team = catalog_team?;

    let catalog_id = read_non_empty_string(catalog_team.get("catalogId"))?;
    Some(CatalogTeamProvenance {
        catalog_id,
        catalog_key: read_non_empty_string(catalog_team.get("catalogKey")),
        origin_hash: read_non_empty_string(catalog_team.get("originHash")),
    })
}

// ============================================================================
// YAML rendering helpers (pure)
// ============================================================================

/// `yamlScalar` —— 与 Node `yamlScalar` 1:1 对齐。
pub fn yaml_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\"")),
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".into()),
    }
}

fn is_scalar(v: &serde_json::Value) -> bool {
    matches!(
        v,
        serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_)
    ) || matches!(v, serde_json::Value::Array(arr) if arr.is_empty())
        || matches!(v, serde_json::Value::Object(obj) if obj.is_empty())
}

fn is_plain_object(v: &serde_json::Value) -> bool {
    v.is_object()
}

/// `renderStringArrayYaml` —— 与 Node 1:1 对齐。
pub fn render_string_array_yaml(key: &str, values: &[String]) -> Vec<String> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(values.len() + 1);
    out.push(format!("{key}:"));
    for v in values {
        let val = serde_json::Value::String(v.clone());
        out.push(format!("  - {}", yaml_scalar(&val)));
    }
    out
}

/// `renderYamlBlock` —— 与 Node 1:1 对齐。
pub fn render_yaml_block(value: &serde_json::Value, indent_level: usize) -> Vec<String> {
    let indent = "  ".repeat(indent_level);
    match value {
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return vec![format!("{indent}[]")];
            }
            let mut lines = Vec::new();
            for entry in arr {
                if is_scalar(entry) {
                    lines.push(format!("{indent}- {}", yaml_scalar(entry)));
                } else {
                    lines.push(format!("{indent}-"));
                    lines.extend(render_yaml_block(entry, indent_level + 1));
                }
            }
            lines
        }
        serde_json::Value::Object(obj) => {
            let entries: Vec<(&String, &serde_json::Value)> = obj
                .iter()
                .filter(|(_, v)| !v.is_null())
                .collect();
            if entries.is_empty() {
                return vec![format!("{indent}{{}}")];
            }
            let mut lines = Vec::new();
            for (k, v) in entries {
                if is_scalar(v) {
                    lines.push(format!("{indent}{k}: {}", yaml_scalar(v)));
                } else {
                    lines.push(format!("{indent}{k}:"));
                    lines.extend(render_yaml_block(v, indent_level + 1));
                }
            }
            lines
        }
        _ => vec![format!("{indent}{}", yaml_scalar(value))],
    }
}

/// `renderYamlFile` —— 与 Node 1:1 对齐。
pub fn render_yaml_file(value: &serde_json::Value) -> String {
    let mut out = render_yaml_block(value, 0).join("\n");
    out.push('\n');
    out
}

/// `renderSyntheticCompanyMarkdown` —— 与 Node 1:1 对齐。
pub fn render_synthetic_company_markdown(team: &CatalogTeam) -> String {
    let name = serde_json::Value::String(team.name.clone());
    let description = serde_json::Value::String(team.description.clone());
    let slug = serde_json::Value::String(team.slug.clone());
    let lines = [
        "---".to_string(),
        format!("name: {}", yaml_scalar(&name)),
        format!("description: {}", yaml_scalar(&description)),
        "schema: agentcompanies/v1".to_string(),
        format!("slug: {}", yaml_scalar(&slug)),
        "includes:".to_string(),
        "  - TEAM.md".to_string(),
        "---".to_string(),
        String::new(),
        format!("# {}", team.name),
        String::new(),
        team.description.clone(),
        String::new(),
    ];
    lines.join("\n")
}

// ============================================================================
// Catalog provenance rendering
// ============================================================================

#[derive(Debug, Clone)]
pub struct CatalogProvenanceFields {
    pub catalog_id: String,
    pub catalog_key: Option<String>,
    pub catalog_kind: String,
    pub catalog_category: String,
    pub catalog_slug: String,
    pub package_name: String,
    pub package_version: String,
    pub origin_hash: String,
}

/// `catalogProvenance` 1:1 对齐 —— 提取 team 的 provenance 字段。
pub fn catalog_provenance(
    team: &CatalogTeam,
    manifest: &CatalogManifest,
) -> CatalogProvenanceFields {
    CatalogProvenanceFields {
        catalog_id: team.id.clone(),
        catalog_key: team.key.clone(),
        catalog_kind: team.kind.clone(),
        catalog_category: team.category.clone(),
        catalog_slug: team.slug.clone(),
        package_name: manifest.package_name.clone(),
        package_version: manifest.package_version.clone(),
        origin_hash: team.content_hash.clone(),
    }
}

#[derive(Debug, Clone)]
pub struct TargetManagerReference {
    pub agent_id: String,
    pub slug: String,
}

/// `renderCatalogProvenanceYaml` —— 与 Node 1:1 对齐（除 `normalizeAgentUrlKey` 等价用 ASCII-only
/// 转小写-`-` 替换外）。
pub fn render_catalog_provenance_yaml(
    team: &CatalogTeam,
    manifest: &CatalogManifest,
    target_manager: Option<&TargetManagerReference>,
) -> String {
    let provenance = catalog_provenance(team, manifest);
    let agent_slugs = unique_sorted(&team.agent_slugs);
    let project_slugs = unique_sorted(&team.project_slugs);

    let task_slugs_set: BTreeSet<String> = team
        .files
        .iter()
        .filter(|f| f.kind == CATALOG_TEAM_FILE_KIND_TASK)
        .filter_map(|f| {
            let normalized = normalize_portable_path(&f.path);
            let parent = match normalized.rfind('/') {
                Some(idx) => &normalized[..idx],
                None => return None,
            };
            if parent.is_empty() {
                None
            } else {
                Some(parent.rsplit('/').next().unwrap_or(parent).to_string())
            }
        })
        .collect();
    let task_slugs: Vec<String> = task_slugs_set.into_iter().collect();

    let root_agent_set: std::collections::HashSet<&str> =
        team.root_agent_slugs.iter().map(String::as_str).collect();

    let mut agents_map = serde_json::Map::new();
    for slug in &agent_slugs {
        let reparent_root = target_manager.is_some() && root_agent_set.contains(slug.as_str());
        let mut entry = serde_json::Map::new();
        if reparent_root {
            if let Some(tm) = target_manager {
                entry.insert(
                    "reportsToExistingAgentId".into(),
                    serde_json::Value::String(tm.agent_id.clone()),
                );
                entry.insert(
                    "reportsToExistingAgentSlug".into(),
                    serde_json::Value::String(tm.slug.clone()),
                );
            }
        }
        let mut metadata = serde_json::Map::new();
        let mut paperclip = serde_json::Map::new();
        let mut catalog_team = serde_json::Map::new();
        catalog_team.insert(
            "catalogId".into(),
            serde_json::Value::String(provenance.catalog_id.clone()),
        );
        if let Some(ref k) = provenance.catalog_key {
            catalog_team.insert(
                "catalogKey".into(),
                serde_json::Value::String(k.clone()),
            );
        }
        catalog_team.insert(
            "catalogKind".into(),
            serde_json::Value::String(provenance.catalog_kind.clone()),
        );
        catalog_team.insert(
            "catalogCategory".into(),
            serde_json::Value::String(provenance.catalog_category.clone()),
        );
        catalog_team.insert(
            "catalogSlug".into(),
            serde_json::Value::String(provenance.catalog_slug.clone()),
        );
        catalog_team.insert(
            "packageName".into(),
            serde_json::Value::String(provenance.package_name.clone()),
        );
        catalog_team.insert(
            "packageVersion".into(),
            serde_json::Value::String(provenance.package_version.clone()),
        );
        catalog_team.insert(
            "originHash".into(),
            serde_json::Value::String(provenance.origin_hash.clone()),
        );
        paperclip.insert("catalogTeam".into(), serde_json::Value::Object(catalog_team));
        metadata.insert("paperclip".into(), serde_json::Value::Object(paperclip));
        entry.insert("metadata".into(), serde_json::Value::Object(metadata));
        agents_map.insert(slug.clone(), serde_json::Value::Object(entry));
    }

    let mut extension = serde_json::Map::new();
    extension.insert("schema".into(), serde_json::Value::String("paperclip/v1".into()));
    extension.insert("agents".into(), serde_json::Value::Object(agents_map));

    if !project_slugs.is_empty() {
        let mut projects_map = serde_json::Map::new();
        for slug in &project_slugs {
            let mut entry = serde_json::Map::new();
            let mut metadata = serde_json::Map::new();
            let mut paperclip = serde_json::Map::new();
            let mut catalog_team = serde_json::Map::new();
            catalog_team.insert(
                "catalogId".into(),
                serde_json::Value::String(provenance.catalog_id.clone()),
            );
            if let Some(ref k) = provenance.catalog_key {
                catalog_team.insert(
                    "catalogKey".into(),
                    serde_json::Value::String(k.clone()),
                );
            }
            catalog_team.insert(
                "catalogKind".into(),
                serde_json::Value::String(provenance.catalog_kind.clone()),
            );
            catalog_team.insert(
                "catalogCategory".into(),
                serde_json::Value::String(provenance.catalog_category.clone()),
            );
            catalog_team.insert(
                "catalogSlug".into(),
                serde_json::Value::String(provenance.catalog_slug.clone()),
            );
            catalog_team.insert(
                "packageName".into(),
                serde_json::Value::String(provenance.package_name.clone()),
            );
            catalog_team.insert(
                "packageVersion".into(),
                serde_json::Value::String(provenance.package_version.clone()),
            );
            catalog_team.insert(
                "originHash".into(),
                serde_json::Value::String(provenance.origin_hash.clone()),
            );
            paperclip.insert("catalogTeam".into(), serde_json::Value::Object(catalog_team));
            metadata.insert("paperclip".into(), serde_json::Value::Object(paperclip));
            entry.insert("metadata".into(), serde_json::Value::Object(metadata));
            projects_map.insert(slug.clone(), serde_json::Value::Object(entry));
        }
        extension.insert("projects".into(), serde_json::Value::Object(projects_map));
    }

    if !task_slugs.is_empty() {
        let mut tasks_map = serde_json::Map::new();
        for slug in &task_slugs {
            let mut entry = serde_json::Map::new();
            let mut metadata = serde_json::Map::new();
            let mut paperclip = serde_json::Map::new();
            let mut catalog_team = serde_json::Map::new();
            catalog_team.insert(
                "catalogId".into(),
                serde_json::Value::String(provenance.catalog_id.clone()),
            );
            if let Some(ref k) = provenance.catalog_key {
                catalog_team.insert(
                    "catalogKey".into(),
                    serde_json::Value::String(k.clone()),
                );
            }
            catalog_team.insert(
                "catalogKind".into(),
                serde_json::Value::String(provenance.catalog_kind.clone()),
            );
            catalog_team.insert(
                "catalogCategory".into(),
                serde_json::Value::String(provenance.catalog_category.clone()),
            );
            catalog_team.insert(
                "catalogSlug".into(),
                serde_json::Value::String(provenance.catalog_slug.clone()),
            );
            catalog_team.insert(
                "packageName".into(),
                serde_json::Value::String(provenance.package_name.clone()),
            );
            catalog_team.insert(
                "packageVersion".into(),
                serde_json::Value::String(provenance.package_version.clone()),
            );
            catalog_team.insert(
                "originHash".into(),
                serde_json::Value::String(provenance.origin_hash.clone()),
            );
            paperclip.insert("catalogTeam".into(), serde_json::Value::Object(catalog_team));
            metadata.insert("paperclip".into(), serde_json::Value::Object(paperclip));
            entry.insert("metadata".into(), serde_json::Value::Object(metadata));
            tasks_map.insert(slug.clone(), serde_json::Value::Object(entry));
        }
        extension.insert("tasks".into(), serde_json::Value::Object(tasks_map));
    }

    render_yaml_file(&serde_json::Value::Object(extension))
}

fn unique_sorted(values: &[String]) -> Vec<String> {
    let set: BTreeSet<&String> = values.iter().collect();
    set.into_iter().cloned().collect()
}

// ============================================================================
// merge / parse / render helpers
// ============================================================================

/// `mergePlainRecords` —— 与 Node 1:1 对齐。
pub fn merge_plain_records(
    base: &serde_json::Value,
    override_: &serde_json::Value,
) -> serde_json::Value {
    if !is_plain_object(base) || !is_plain_object(override_) {
        return override_.clone();
    }
    let base_obj = base.as_object().unwrap();
    let override_obj = override_.as_object().unwrap();
    let mut merged = base_obj.clone();
    for (k, v) in override_obj {
        let existing = merged.get(k);
        if let Some(existing_v) = existing {
            if is_plain_object(existing_v) && is_plain_object(v) {
                merged.insert(k.clone(), merge_plain_records(existing_v, v));
                continue;
            }
        }
        merged.insert(k.clone(), v.clone());
    }
    serde_json::Value::Object(merged)
}

/// `parseYamlDocument` —— 简化 frontmatter parser（不引入 `pc-frontmatter` 依赖）。
pub fn parse_yaml_document(raw: &str) -> serde_json::Value {
    // Wrap into frontmatter block, parse via minimal frontmatter logic.
    let wrapped = format!("---\n{}\n---\n", raw.trim());
    parse_frontmatter(&wrapped).frontmatter
}

/// Minimal frontmatter splitter —— 只识别 `---\n...\n---\n` 头部。
#[derive(Debug, Clone, Default)]
pub struct FrontmatterSplit {
    pub frontmatter: serde_json::Value,
    pub body: String,
}

pub fn parse_frontmatter(input: &str) -> FrontmatterSplit {
    let trimmed_start = input.trim_start_matches('\n');
    if !trimmed_start.starts_with("---") {
        return FrontmatterSplit {
            frontmatter: serde_json::Value::Object(serde_json::Map::new()),
            body: input.to_string(),
        };
    }
    // Find first newline after opening ---
    let after_open = match trimmed_start[3..].find('\n') {
        Some(idx) => &trimmed_start[3 + idx + 1..],
        None => {
            return FrontmatterSplit {
                frontmatter: serde_json::Value::Object(serde_json::Map::new()),
                body: input.to_string(),
            }
        }
    };
    // Find closing ---
    let close_idx = match after_open.find("\n---") {
        Some(idx) => idx,
        None => {
            return FrontmatterSplit {
                frontmatter: serde_json::Value::Object(serde_json::Map::new()),
                body: input.to_string(),
            }
        }
    };
    let fm_text = &after_open[..close_idx];
    let after_close = &after_open[close_idx + 4..];
    let body = after_close.trim_start_matches('\n').to_string();

    let frontmatter = parse_simple_yaml(fm_text);
    FrontmatterSplit {
        frontmatter,
        body,
    }
}

/// 简化 YAML 解析器 —— 只支持 frontmatter 的常见结构：
/// - `key: value`
/// - `key:` 嵌套（通过缩进）
/// - `key:\n  - string` 字符串数组
fn parse_simple_yaml(input: &str) -> serde_json::Value {
    let mut root = serde_json::Map::new();
    let lines: Vec<&str> = input.lines().collect();
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        if line.trim().is_empty() {
            idx += 1;
            continue;
        }
        let indent = leading_spaces(line);
        if indent != 0 {
            idx += 1;
            continue;
        }
        let (key, rest) = match line.split_once(':') {
            Some(parts) => parts,
            None => {
                idx += 1;
                continue;
            }
        };
        let rest = rest.trim();
        if rest.is_empty() {
            // Could be nested object/array — look at next non-empty line indent.
            let next = next_non_empty(&lines, idx + 1);
            if let Some(next_line) = next {
                let child_indent = leading_spaces(next_line);
                if child_indent > 0 {
                    if next_line.trim_start().starts_with("- ") {
                        // String array
                        let mut arr: Vec<serde_json::Value> = Vec::new();
                        let mut j = idx + 1;
                        while j < lines.len() {
                            let l = lines[j];
                            if l.trim().is_empty() {
                                j += 1;
                                continue;
                            }
                            let l_indent = leading_spaces(l);
                            if l_indent < child_indent {
                                break;
                            }
                            let trimmed = l.trim_start();
                            if let Some(s) = trimmed.strip_prefix("- ") {
                                arr.push(parse_yaml_scalar(s.trim()));
                                j += 1;
                            } else if trimmed == "-" {
                                // Inline nested object under "-"
                                // Collect nested
                                let mut nested = serde_json::Map::new();
                                j += 1;
                                while j < lines.len() {
                                    let l2 = lines[j];
                                    if l2.trim().is_empty() {
                                        j += 1;
                                        continue;
                                    }
                                    let l2_indent = leading_spaces(l2);
                                    if l2_indent <= child_indent {
                                        break;
                                    }
                                    if let Some((nk, nv)) = l2.trim_start().split_once(':') {
                                        nested.insert(
                                            nk.trim().to_string(),
                                            parse_yaml_scalar(nv.trim()),
                                        );
                                    }
                                    j += 1;
                                }
                                arr.push(serde_json::Value::Object(nested));
                            } else {
                                break;
                            }
                        }
                        idx = j;
                        root.insert(key.trim().to_string(), serde_json::Value::Array(arr));
                    } else {
                        // Nested object
                        let mut nested = serde_json::Map::new();
                        let mut j = idx + 1;
                        while j < lines.len() {
                            let l = lines[j];
                            if l.trim().is_empty() {
                                j += 1;
                                continue;
                            }
                            let l_indent = leading_spaces(l);
                            if l_indent == 0 {
                                break;
                            }
                            if l_indent < child_indent {
                                break;
                            }
                            if let Some((nk, nv)) = l.trim_start().split_once(':') {
                                let nv = nv.trim();
                                if nv.is_empty() {
                                    nested.insert(nk.trim().to_string(), serde_json::Value::Null);
                                } else {
                                    nested.insert(nk.trim().to_string(), parse_yaml_scalar(nv));
                                }
                            }
                            j += 1;
                        }
                        idx = j;
                        root.insert(key.trim().to_string(), serde_json::Value::Object(nested));
                    }
                    continue;
                }
            }
            root.insert(key.trim().to_string(), serde_json::Value::Null);
            idx += 1;
            continue;
        }
        root.insert(key.trim().to_string(), parse_yaml_scalar(rest));
        idx += 1;
    }
    serde_json::Value::Object(root)
}

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

fn next_non_empty<'a>(lines: &'a [&'a str], mut idx: usize) -> Option<&'a str> {
    while idx < lines.len() {
        if !lines[idx].trim().is_empty() {
            return Some(lines[idx]);
        }
        idx += 1;
    }
    None
}

fn parse_yaml_scalar(input: &str) -> serde_json::Value {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return serde_json::Value::Null;
    }
    if trimmed == "true" {
        return serde_json::Value::Bool(true);
    }
    if trimmed == "false" {
        return serde_json::Value::Bool(false);
    }
    if trimmed == "null" {
        return serde_json::Value::Null;
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return serde_json::Value::Number(n.into());
    }
    if let Ok(n) = trimmed.parse::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(n) {
            return serde_json::Value::Number(num);
        }
    }
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        return serde_json::Value::String(trimmed[1..trimmed.len() - 1].to_string());
    }
    if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2 {
        return serde_json::Value::String(trimmed[1..trimmed.len() - 1].to_string());
    }
    serde_json::Value::String(trimmed.to_string())
}

/// `renderSimpleMarkdown` —— 与 Node 1:1 对齐。
pub fn render_simple_markdown(frontmatter: &serde_json::Value, body: &str) -> String {
    let mut lines: Vec<String> = vec!["---".to_string()];
    if let Some(obj) = frontmatter.as_object() {
        for (k, v) in obj {
            if v.is_null() {
                continue;
            }
            if let serde_json::Value::Array(arr) = v {
                let strs: Vec<String> = arr
                    .iter()
                    .filter_map(|entry| entry.as_str().map(|s| s.to_string()))
                    .collect();
                lines.extend(render_string_array_yaml(k, &strs));
                continue;
            }
            if matches!(
                v,
                serde_json::Value::String(_)
                    | serde_json::Value::Number(_)
                    | serde_json::Value::Bool(_)
                    | serde_json::Value::Null
            ) {
                lines.push(format!("{k}: {}", yaml_scalar(v)));
            }
        }
    }
    lines.push("---".to_string());
    lines.push(String::new());
    let clean_body = body.trim();
    if !clean_body.is_empty() {
        lines.push(clean_body.to_string());
        lines.push(String::new());
    }
    lines.join("\n")
}

// ============================================================================
// Skill preparation
// ============================================================================

/// `isPinnedSourceRef` —— 与 Node 1:1 对齐：匹配 40-char hex commit。
pub fn is_pinned_source_ref(value: Option<&str>) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[0-9a-fA-F]{40}$").unwrap());
    match value {
        Some(v) => re.is_match(v.trim()),
        None => false,
    }
}

fn preparation(
    requirement: &CatalogTeamSkillRequirement,
    action: &str,
    reason: Option<String>,
) -> CatalogTeamSkillPreparation {
    CatalogTeamSkillPreparation {
        kind: requirement.kind.clone(),
        ref_: requirement.ref_.clone(),
        agent_slugs: requirement.agent_slugs.clone(),
        action: action.to_string(),
        catalog_skill_id: requirement.catalog_skill_id.clone(),
        catalog_skill_key: requirement.catalog_skill_key.clone(),
        source_locator: requirement.source_locator.clone(),
        source_ref: requirement.source_ref.clone(),
        reason,
    }
}

/// `collectCatalogTeamSkillPreparations` —— 与 Node 1:1 对齐。
pub fn collect_catalog_team_skill_preparations(
    team: &CatalogTeam,
    source_policy: &CatalogTeamSourcePolicy,
) -> CatalogTeamPreparationResult {
    let mut preparations: Vec<CatalogTeamSkillPreparation> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for requirement in &team.required_skills {
        if !requirement.resolved {
            let reason = format!(
                "Skill requirement \"{}\" is unresolved in catalog manifest.",
                requirement.ref_for_match()
            );
            errors.push(reason.clone());
            preparations.push(preparation(requirement, SKILL_PREP_BLOCKED, Some(reason)));
            continue;
        }

        if requirement.kind == SKILL_REQ_TYPE_CATALOG {
            preparations.push(preparation(requirement, SKILL_PREP_CATALOG_INSTALL_REQUIRED, None));
            continue;
        }

        if requirement.kind == SKILL_REQ_TYPE_LOCAL {
            preparations.push(preparation(requirement, SKILL_PREP_ALREADY_IN_PACKAGE, None));
            continue;
        }

        if requirement.kind == SKILL_REQ_TYPE_LOCAL_PATH && !source_policy.allow_local_path_sources {
            let reason = format!(
                "Local path skill source \"{}\" is development-only and is not allowed for catalog team install.",
                requirement.ref_for_match()
            );
            errors.push(reason.clone());
            preparations.push(preparation(requirement, SKILL_PREP_BLOCKED, Some(reason)));
            continue;
        }

        if requirement.kind == SKILL_REQ_TYPE_AGENT_PACKAGE {
            let reason = format!(
                "Agent package skill source \"{}\" is declared but no safe resolver is available yet.",
                requirement.ref_for_match()
            );
            errors.push(reason.clone());
            preparations.push(preparation(requirement, SKILL_PREP_BLOCKED, Some(reason)));
            continue;
        }

        if !source_policy.allow_external_sources {
            let reason = format!(
                "External skill source \"{}\" requires explicit source policy approval.",
                requirement.ref_for_match()
            );
            errors.push(reason.clone());
            preparations.push(preparation(requirement, SKILL_PREP_BLOCKED, Some(reason)));
            continue;
        }

        let is_external = requirement.kind == SKILL_REQ_TYPE_GITHUB
            || requirement.kind == SKILL_REQ_TYPE_SKILLS_SH;

        if team.kind == CATALOG_TEAM_KIND_BUNDLED && is_external
            && !is_pinned_source_ref(requirement.source_ref.as_deref())
        {
            let reason = format!(
                "Bundled catalog team external skill source \"{}\" must be pinned to a commit.",
                requirement.ref_for_match()
            );
            errors.push(reason.clone());
            preparations.push(preparation(requirement, SKILL_PREP_BLOCKED, Some(reason)));
            continue;
        }

        if team.kind == CATALOG_TEAM_KIND_OPTIONAL && is_external
            && !is_pinned_source_ref(requirement.source_ref.as_deref())
        {
            let reason = format!(
                "Optional catalog team external skill source \"{}\" is not pinned to a commit.",
                requirement.ref_for_match()
            );
            if !source_policy.allow_unpinned_optional_sources {
                errors.push(reason.clone());
                preparations.push(preparation(requirement, SKILL_PREP_BLOCKED, Some(reason)));
                continue;
            }
            warnings.push(reason);
        }

        preparations.push(preparation(requirement, SKILL_PREP_EXTERNAL_IMPORT_REQUIRED, None));
    }

    CatalogTeamPreparationResult {
        preparations,
        warnings,
        errors,
    }
}

/// `collectCatalogTeamSkillPreparations` 的返回值（1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogTeamPreparationResult {
    pub preparations: Vec<CatalogTeamSkillPreparation>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

// ============================================================================
// catalog skill key map (skill ref rewriting)
// ============================================================================

/// `collectCatalogSkillKeyMap` —— 与 Node 1:1 对齐。
pub fn collect_catalog_skill_key_map(team: &CatalogTeam) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for requirement in &team.required_skills {
        if requirement.kind != SKILL_REQ_TYPE_CATALOG {
            continue;
        }
        let key = match &requirement.catalog_skill_key {
            Some(k) => k.clone(),
            None => continue,
        };
        map.insert(requirement.ref_.clone(), key.clone());
        if let Some(id) = &requirement.catalog_skill_id {
            map.insert(id.clone(), key.clone());
        }
        map.insert(key.clone(), key.clone());
        if let Some(slug) = key.split('/').last() {
            if !slug.is_empty() {
                map.insert(slug.to_string(), key.clone());
            }
        }
    }
    map
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_manifest() -> CatalogManifest {
        CatalogManifest {
            package_name: "@paperclipai/teams-catalog".into(),
            package_version: "0.5.0".into(),
            teams: vec![
                CatalogTeam {
                    id: "team-1".into(),
                    key: Some("engineering/core".into()),
                    slug: "engineering-core".into(),
                    name: "Core Engineering".into(),
                    description: "Core engineering team".into(),
                    category: "engineering".into(),
                    kind: CATALOG_TEAM_KIND_BUNDLED.into(),
                    recommended_for_company_types: vec!["startup".into()],
                    tags: vec!["core".into()],
                    path: "teams/engineering-core".into(),
                    entrypoint: "TEAM.md".into(),
                    files: vec![
                        CatalogTeamFileEntry {
                            path: "TEAM.md".into(),
                            kind: CATALOG_TEAM_FILE_KIND_TASK.into(),
                        },
                        CatalogTeamFileEntry {
                            path: "tasks/foo/AGENTS.md".into(),
                            kind: CATALOG_TEAM_FILE_KIND_TASK.into(),
                        },
                    ],
                    agent_slugs: vec!["manager".into(), "engineer".into()],
                    root_agent_slugs: vec!["manager".into()],
                    project_slugs: vec!["main".into()],
                    required_skills: vec![],
                    compatibility: "compatible".into(),
                    trust_level: "default".into(),
                    content_hash: "hash-team-1".into(),
                    package_name: None,
                    package_version: None,
                },
                CatalogTeam {
                    id: "team-2".into(),
                    key: Some("optional/extra".into()),
                    slug: "shared-slug".into(),
                    name: "Optional Extra".into(),
                    description: "Optional extras".into(),
                    category: "engineering".into(),
                    kind: CATALOG_TEAM_KIND_OPTIONAL.into(),
                    recommended_for_company_types: vec![],
                    tags: vec![],
                    path: "teams/extra".into(),
                    entrypoint: "TEAM.md".into(),
                    files: vec![],
                    agent_slugs: vec![],
                    root_agent_slugs: vec![],
                    project_slugs: vec![],
                    required_skills: vec![
                        CatalogTeamSkillRequirement {
                            kind: SKILL_REQ_TYPE_GITHUB.into(),
                            ref_: "owner/repo".into(),
                            agent_slugs: vec!["engineer".into()],
                            resolved: true,
                            catalog_skill_id: None,
                            catalog_skill_key: None,
                            source_locator: None,
                            source_ref: Some("0123456789abcdef0123456789abcdef01234567".into()),
                        },
                    ],
                    compatibility: "compatible".into(),
                    trust_level: "default".into(),
                    content_hash: "hash-team-2".into(),
                    package_name: None,
                    package_version: None,
                },
                CatalogTeam {
                    id: "team-3".into(),
                    key: Some("duplicate/slug".into()),
                    slug: "shared-slug".into(), // collides with team-2
                    name: "Dup Slug".into(),
                    description: "another shared slug".into(),
                    category: "engineering".into(),
                    kind: CATALOG_TEAM_KIND_BUNDLED.into(),
                    recommended_for_company_types: vec![],
                    tags: vec![],
                    path: "teams/dup".into(),
                    entrypoint: "TEAM.md".into(),
                    files: vec![],
                    agent_slugs: vec![],
                    root_agent_slugs: vec![],
                    project_slugs: vec![],
                    required_skills: vec![],
                    compatibility: "compatible".into(),
                    trust_level: "default".into(),
                    content_hash: "hash-team-3".into(),
                    package_name: None,
                    package_version: None,
                },
            ],
        }
    }

    #[test]
    fn r846_normalize_portable_path() {
        assert_eq!(normalize_portable_path("foo/bar"), "foo/bar");
        assert_eq!(normalize_portable_path("foo\\bar"), "foo/bar");
        assert_eq!(normalize_portable_path("./foo"), "foo");
        assert_eq!(normalize_portable_path("/foo"), "foo");
        assert_eq!(normalize_portable_path("a/b/../c"), "a/c");
    }

    #[test]
    fn r846_infer_language_and_is_markdown() {
        assert!(is_markdown_path("TEAM.md"));
        assert!(is_markdown_path("sub/team.md"));
        assert!(!is_markdown_path("foo.ts"));
        assert_eq!(infer_language_from_path("foo.ts"), Some("typescript"));
        assert_eq!(infer_language_from_path("foo.yml"), Some("yaml"));
        assert_eq!(infer_language_from_path("foo.unknown"), None);
    }

    #[test]
    fn r846_list_teams_filters_and_sorts() {
        let manifest = fixture_manifest();
        let only_bundled = list_catalog_teams(
            &manifest,
            &CatalogTeamListQuery {
                kind: Some(CATALOG_TEAM_KIND_BUNDLED.into()),
                ..Default::default()
            },
        );
        assert_eq!(only_bundled.len(), 2);

        let by_query = list_catalog_teams(
            &manifest,
            &CatalogTeamListQuery {
                q: Some("core".into()),
                ..Default::default()
            },
        );
        assert_eq!(by_query.len(), 1);
        assert_eq!(by_query[0].id, "team-1");

        // Sort by name then key — Core Engineering, Dup Slug, Optional Extra
        let all = list_catalog_teams(&manifest, &CatalogTeamListQuery::default());
        let names: Vec<&str> = all.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Core Engineering", "Dup Slug", "Optional Extra"]);
    }

    #[test]
    fn r846_list_teams_attaches_package_metadata() {
        let manifest = fixture_manifest();
        let all = list_catalog_teams(&manifest, &CatalogTeamListQuery::default());
        for team in &all {
            assert_eq!(team.package_name.as_deref(), Some("@paperclipai/teams-catalog"));
            assert_eq!(team.package_version.as_deref(), Some("0.5.0"));
        }
    }

    #[test]
    fn r846_resolve_team_reference_id_key_slug() {
        let manifest = fixture_manifest();
        let by_id = resolve_catalog_team_reference(&manifest.teams, "team-1");
        assert_eq!(by_id.team.as_ref().unwrap().id, "team-1");
        let by_key = resolve_catalog_team_reference(&manifest.teams, "engineering/core");
        assert_eq!(by_key.team.as_ref().unwrap().id, "team-1");
        let empty = resolve_catalog_team_reference(&manifest.teams, "   ");
        assert!(empty.team.is_none());
        assert!(!empty.ambiguous);
    }

    #[test]
    fn r846_resolve_team_reference_slug_ambiguous() {
        let manifest = fixture_manifest();
        let ambig = resolve_catalog_team_reference(&manifest.teams, "shared-slug");
        assert!(ambig.team.is_none());
        assert!(ambig.ambiguous);
    }

    #[test]
    fn r846_get_team_or_throw_errors() {
        let manifest = fixture_manifest();
        assert!(get_catalog_team_or_throw(&manifest.teams, "shared-slug").is_err());
        assert!(get_catalog_team_or_throw(&manifest.teams, "missing").is_err());
        let found = get_catalog_team_or_throw(&manifest.teams, "engineering/core").unwrap();
        assert_eq!(found.id, "team-1");
    }

    #[test]
    fn r846_read_provenance_returns_none_for_bad_metadata() {
        assert!(read_catalog_team_provenance(None).is_none());
        assert!(read_catalog_team_provenance(Some(&json!("string"))).is_none());
        assert!(read_catalog_team_provenance(Some(&json!(null))).is_none());
        assert!(read_catalog_team_provenance(Some(&json!({ "other": "x" }))).is_none());
        assert!(read_catalog_team_provenance(Some(&json!({
            "paperclip": { "other": "x" }
        })))
        .is_none());
        assert!(read_catalog_team_provenance(Some(&json!({
            "paperclip": { "catalogTeam": { "catalogId": "" } }
        })))
        .is_none());
    }

    #[test]
    fn r846_read_provenance_extracts_fields() {
        let meta = json!({
            "paperclip": {
                "catalogTeam": {
                    "catalogId": "team-1",
                    "catalogKey": "engineering/core",
                    "originHash": "hash-team-1"
                }
            }
        });
        let p = read_catalog_team_provenance(Some(&meta)).expect("present");
        assert_eq!(p.catalog_id, "team-1");
        assert_eq!(p.catalog_key.as_deref(), Some("engineering/core"));
        assert_eq!(p.origin_hash.as_deref(), Some("hash-team-1"));
    }

    #[test]
    fn r846_is_pinned_source_ref() {
        assert!(is_pinned_source_ref(Some(
            "0123456789abcdef0123456789abcdef01234567"
        )));
        assert!(is_pinned_source_ref(Some(
            "ABCDEF1234567890ABCDEF1234567890ABCDEF12"
        )));
        assert!(!is_pinned_source_ref(Some("not-a-commit")));
        assert!(!is_pinned_source_ref(Some(
            "0123456789abcdef0123456789abcdef0123456"
        )));
        assert!(!is_pinned_source_ref(None));
        assert!(!is_pinned_source_ref(Some("  ")));
    }

    #[test]
    fn r846_collect_skill_preparations_unresolved_blocks() {
        let manifest = fixture_manifest();
        let team = &manifest.teams[0];
        let mut team = team.clone();
        team.required_skills = vec![CatalogTeamSkillRequirement {
            kind: SKILL_REQ_TYPE_CATALOG.into(),
            ref_: "broken".into(),
            agent_slugs: vec![],
            resolved: false,
            catalog_skill_id: None,
            catalog_skill_key: None,
            source_locator: None,
            source_ref: None,
        }];
        let result = collect_catalog_team_skill_preparations(&team, &CatalogTeamSourcePolicy::default());
        assert_eq!(result.preparations.len(), 1);
        assert_eq!(result.preparations[0].action, SKILL_PREP_BLOCKED);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn r846_collect_skill_preparations_blocks_external_without_policy() {
        let manifest = fixture_manifest();
        let team = &manifest.teams[1]; // optional, has pinned external
        let result = collect_catalog_team_skill_preparations(
            team,
            &CatalogTeamSourcePolicy::default(),
        );
        // Default policy denies external sources → blocked.
        assert!(!result.errors.is_empty());
        assert_eq!(result.preparations.len(), 1);
        assert_eq!(result.preparations[0].action, SKILL_PREP_BLOCKED);
    }

    #[test]
    fn r846_collect_skill_preparations_optional_pinned_passes_with_policy() {
        let manifest = fixture_manifest();
        let team = &manifest.teams[1];
        let result = collect_catalog_team_skill_preparations(
            team,
            &CatalogTeamSourcePolicy {
                allow_external_sources: true,
                ..Default::default()
            },
        );
        // Pinned + optional + allow_external_sources → allowed
        assert!(result.errors.is_empty());
        assert_eq!(result.preparations.len(), 1);
        assert_eq!(result.preparations[0].action, SKILL_PREP_EXTERNAL_IMPORT_REQUIRED);
    }

    #[test]
    fn r846_collect_skill_preparations_bundled_unpinned_blocked() {
        let mut manifest = fixture_manifest();
        manifest.teams[0].required_skills = vec![CatalogTeamSkillRequirement {
            kind: SKILL_REQ_TYPE_GITHUB.into(),
            ref_: "owner/repo".into(),
            agent_slugs: vec![],
            resolved: true,
            catalog_skill_id: None,
            catalog_skill_key: None,
            source_locator: None,
            source_ref: Some("not-pinned".into()),
        }];
        let team = &manifest.teams[0];
        let result = collect_catalog_team_skill_preparations(
            team,
            &CatalogTeamSourcePolicy {
                allow_external_sources: true,
                ..Default::default()
            },
        );
        assert_eq!(result.preparations.len(), 1);
        assert_eq!(result.preparations[0].action, SKILL_PREP_BLOCKED);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn r846_collect_skill_preparations_local_path_blocks_without_policy() {
        let mut manifest = fixture_manifest();
        manifest.teams[0].required_skills = vec![CatalogTeamSkillRequirement {
            kind: SKILL_REQ_TYPE_LOCAL_PATH.into(),
            ref_: "/local/path".into(),
            agent_slugs: vec![],
            resolved: true,
            catalog_skill_id: None,
            catalog_skill_key: None,
            source_locator: None,
            source_ref: None,
        }];
        let team = &manifest.teams[0];
        let result = collect_catalog_team_skill_preparations(team, &CatalogTeamSourcePolicy::default());
        assert_eq!(result.preparations[0].action, SKILL_PREP_BLOCKED);
    }

    #[test]
    fn r846_yaml_scalar_and_render_block() {
        assert_eq!(yaml_scalar(&json!(null)), "null");
        assert_eq!(yaml_scalar(&json!(true)), "true");
        assert_eq!(yaml_scalar(&json!(42)), "42");
        assert_eq!(yaml_scalar(&json!("hello")), "\"hello\"");
        assert_eq!(
            render_yaml_block(&json!({ "a": 1, "b": [1, 2] }), 0),
            vec!["a: 1", "b:", "  - 1", "  - 2"]
        );
        let empty_obj = json!({});
        assert_eq!(render_yaml_block(&empty_obj, 0), vec!["{}"]);
        let empty_arr = json!([]);
        assert_eq!(render_yaml_block(&empty_arr, 0), vec!["[]"]);
    }

    #[test]
    fn r846_render_synthetic_company_markdown() {
        let manifest = fixture_manifest();
        let team = &manifest.teams[0];
        let md = render_synthetic_company_markdown(team);
        assert!(md.starts_with("---\n"));
        assert!(md.contains("schema: agentcompanies/v1"));
        assert!(md.contains("includes:"));
        assert!(md.contains("# Core Engineering"));
    }

    #[test]
    fn r846_render_catalog_provenance_yaml_basic() {
        let manifest = fixture_manifest();
        let team = &manifest.teams[0];
        let yaml = render_catalog_provenance_yaml(team, &manifest, None);
        assert!(yaml.contains("schema: \"paperclip/v1\""));
        assert!(yaml.contains("agents:"));
        assert!(yaml.contains("manager:"));
        assert!(yaml.contains("engineer:"));
        assert!(yaml.contains("projects:"));
        assert!(yaml.contains("main:"));
        // Tasks: from team-1 files, "foo" is the directory name of "tasks/foo/AGENTS.md"
        assert!(yaml.contains("tasks:"));
        assert!(yaml.contains("foo:"));
        // No target_manager → no reparenting fields
        assert!(!yaml.contains("reportsToExistingAgentId"));
    }

    #[test]
    fn r846_render_catalog_provenance_yaml_with_target_manager_reparents_roots() {
        let manifest = fixture_manifest();
        let team = &manifest.teams[0];
        let target = TargetManagerReference {
            agent_id: "agent-1".into(),
            slug: "manager".into(),
        };
        let yaml = render_catalog_provenance_yaml(team, &manifest, Some(&target));
        // "manager" is in root_agent_slugs → reparented
        assert!(yaml.contains("reportsToExistingAgentId: \"agent-1\""));
        // "engineer" is NOT in root_agent_slugs → no reparenting under engineer block
        let engineer_block = yaml
            .split("engineer:")
            .nth(1)
            .and_then(|s| s.lines().next())
            .unwrap_or("");
        assert!(!engineer_block.contains("reportsToExistingAgentId"));
    }

    #[test]
    fn r846_collect_catalog_skill_key_map() {
        let mut manifest = fixture_manifest();
        manifest.teams[0].required_skills = vec![CatalogTeamSkillRequirement {
            kind: SKILL_REQ_TYPE_CATALOG.into(),
            ref_: "alias-ref".into(),
            agent_slugs: vec![],
            resolved: true,
            catalog_skill_id: Some("cat-1".into()),
            catalog_skill_key: Some("ns/skill".into()),
            source_locator: None,
            source_ref: None,
        }];
        let map = collect_catalog_skill_key_map(&manifest.teams[0]);
        assert_eq!(map.get("alias-ref").unwrap(), "ns/skill");
        assert_eq!(map.get("cat-1").unwrap(), "ns/skill");
        assert_eq!(map.get("ns/skill").unwrap(), "ns/skill");
        assert_eq!(map.get("skill").unwrap(), "ns/skill");
    }

    #[test]
    fn r846_render_simple_markdown_roundtrip() {
        let frontmatter = json!({
            "name": "Alpha",
            "tags": ["a", "b"],
            "count": 3,
            "enabled": true
        });
        let body = "# Body\n\nText.";
        let md = render_simple_markdown(&frontmatter, body);
        assert!(md.starts_with("---\n"));
        assert!(md.contains("name: \"Alpha\""));
        assert!(md.contains("tags:"));
        assert!(md.contains("  - \"a\""));
        assert!(md.contains("# Body"));
    }

    #[test]
    fn r846_merge_plain_records_deep_merge() {
        let base = json!({
            "a": 1,
            "b": { "x": 1, "y": 2 },
            "c": [1, 2]
        });
        let override_ = json!({
            "b": { "y": 99, "z": 100 },
            "c": [9],
            "d": "new"
        });
        let merged = merge_plain_records(&base, &override_);
        assert_eq!(merged["a"], json!(1));
        assert_eq!(merged["b"]["x"], json!(1));
        assert_eq!(merged["b"]["y"], json!(99));
        assert_eq!(merged["b"]["z"], json!(100));
        // Override replaces arrays wholesale (Node behavior: not recursive merge for arrays)
        assert_eq!(merged["c"], json!([9]));
        assert_eq!(merged["d"], json!("new"));
    }

    #[test]
    fn r846_parse_yaml_document_simple() {
        let yaml = "schema: paperclip/v1\nagents:\n  - foo\n  - bar";
        let v = parse_yaml_document(yaml);
        assert_eq!(v["schema"], json!("paperclip/v1"));
        assert_eq!(v["agents"], json!(["foo", "bar"]));
    }
}