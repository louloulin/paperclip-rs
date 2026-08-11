//! Hermes skills 扫描（对齐 Node `packages/adapters/hermes/src/server/skills.ts`）。
//!
//! Hermes 的 skills 来自两个源：
//! 1. **`~/.hermes/skills/<category>/<skill>/SKILL.md`** — 用户安装
//! 2. **Paperclip-managed runtime skills** — 由 `~/.paperclip-runtime/skills/**`
//!    物化（通过 `runtime_skills_dir` 注入）
//!
//! 扫描后合并：Paperclip-managed 优先（基于 `key` 去重），缺失 desired
//! skills 标 `state = "missing"`。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 单个 skill 条目（对齐 Node `AdapterSkillEntry`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdapterSkillEntry {
    pub key: String,
    pub runtime_name: Option<String>,
    pub desired: bool,
    pub managed: bool,
    pub state: SkillState,
    pub origin: SkillOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_label: Option<String>,
    pub read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location_label: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillState {
    Installed,
    Configured,
    Available,
    Missing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillOrigin {
    UserInstalled,
    CompanyManaged,
    ExternalUnknown,
}

/// 完整 skills 快照（对齐 Node `AdapterSkillSnapshot`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterSkillSnapshot {
    pub adapter_type: String,
    pub supported: bool,
    pub mode: &'static str,
    pub desired_skills: Vec<String>,
    pub entries: Vec<AdapterSkillEntry>,
    pub warnings: Vec<String>,
}

/// Frontmatter 解析（极简 YAML subset — 只支持 `key: value` 行）。
#[derive(Debug, Clone, Default, PartialEq)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    version: Option<String>,
    category: Option<String>,
}

fn parse_skill_frontmatter(content: &str) -> SkillFrontmatter {
    let mut fm = SkillFrontmatter::default();
    let Some(captures) = regex_lite::Regex::new(r"(?s)^---\s*\n([\s\S]*?)\n---")
        .ok()
        .and_then(|re| re.captures(content))
    else {
        return fm;
    };
    for line in captures.get(1).unwrap().as_str().lines() {
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            let value = if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                &value[1..value.len() - 1]
            } else {
                value
            };
            match key.trim() {
                "name" => fm.name = Some(value.to_string()),
                "description" => fm.description = Some(value.to_string()),
                "version" => fm.version = Some(value.to_string()),
                "category" => fm.category = Some(value.to_string()),
                _ => {}
            }
        }
    }
    fm
}

/// Hermes home（从 `adapter_config.env.HOME` 或 `$HOME` 环境变量）。
pub fn resolve_hermes_home(config: &Value) -> PathBuf {
    let home = config
        .get("env")
        .and_then(|v| v.get("HOME"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    home
}

/// 扫描 `~/.hermes/skills/<category>/[<skill>/]SKILL.md`。
///
/// 返回的 entries 按 `key` 升序排列（与 Node 一致）。
pub async fn scan_hermes_skills(skills_home: &Path) -> Vec<AdapterSkillEntry> {
    let mut entries = Vec::new();
    let mut categories = match tokio::fs::read_dir(skills_home).await {
        Ok(read_dir) => read_dir,
        Err(_) => return entries,
    };
    while let Ok(Some(cat)) = categories.next_entry().await {
        let cat_name = cat.file_name().to_string_lossy().into_owned();
        let Ok(cat_file_type) = cat.file_type().await else {
            continue;
        };
        if !cat_file_type.is_dir() {
            continue;
        }
        let cat_path = cat.path();

        // 1. category 目录自己的 SKILL.md（顶层 skill）
        let top_skill_md = cat_path.join("SKILL.md");
        if tokio::fs::metadata(&top_skill_md).await.is_ok() {
            entries.push(build_user_skill_entry(&cat_name, &top_skill_md, &cat_name).await);
        }

        // 2. 子目录的 SKILL.md
        if let Ok(mut items) = tokio::fs::read_dir(&cat_path).await {
            while let Ok(Some(item)) = items.next_entry().await {
                let Ok(item_type) = item.file_type().await else {
                    continue;
                };
                if !item_type.is_dir() {
                    continue;
                }
                let skill_name = item.file_name().to_string_lossy().into_owned();
                let skill_md = cat_path.join(&skill_name).join("SKILL.md");
                if tokio::fs::metadata(&skill_md).await.is_ok() {
                    let category_path = format!("{cat_name}/{skill_name}");
                    entries
                        .push(build_user_skill_entry(&skill_name, &skill_md, &category_path).await);
                }
            }
        }
    }
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    entries
}

async fn build_user_skill_entry(
    key: &str,
    skill_md_path: &Path,
    category_path: &str,
) -> AdapterSkillEntry {
    let description = tokio::fs::read_to_string(skill_md_path)
        .await
        .ok()
        .map(|content| {
            parse_skill_frontmatter(&content)
                .description
                .unwrap_or_default()
        })
        .filter(|s| !s.is_empty());
    AdapterSkillEntry {
        key: key.to_string(),
        runtime_name: Some(key.to_string()),
        desired: true,
        managed: false,
        state: SkillState::Installed,
        origin: SkillOrigin::UserInstalled,
        origin_label: Some("Hermes skill".to_string()),
        location_label: Some(format!("~/.hermes/skills/{category_path}")),
        read_only: true,
        source_path: Some(skill_md_path.to_path_buf()),
        target_path: None,
        detail: description,
    }
}

/// 读取 Paperclip-managed runtime skills 目录中的所有 entries。
///
/// runtime_skills_dir 一般是 `~/.paperclip-runtime/skills`，
/// 每个 subdir 含 `SKILL.md` + `manifest.json`（后者可选）。
pub async fn scan_runtime_skills(runtime_skills_dir: &Path) -> Vec<AdapterSkillEntry> {
    let mut entries = Vec::new();
    let mut categories = match tokio::fs::read_dir(runtime_skills_dir).await {
        Ok(read_dir) => read_dir,
        Err(_) => return entries,
    };
    while let Ok(Some(item)) = categories.next_entry().await {
        let Ok(file_type) = item.file_type().await else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let dir = item.path();
        let skill_md = dir.join("SKILL.md");
        if tokio::fs::metadata(&skill_md).await.is_err() {
            continue;
        }
        let key = item.file_name().to_string_lossy().into_owned();
        let description = tokio::fs::read_to_string(&skill_md)
            .await
            .ok()
            .map(|content| {
                parse_skill_frontmatter(&content)
                    .description
                    .unwrap_or_default()
            })
            .filter(|s| !s.is_empty());
        entries.push(AdapterSkillEntry {
            key,
            runtime_name: Some(dir.file_name().unwrap().to_string_lossy().into_owned()),
            desired: false,
            managed: true,
            state: SkillState::Available,
            origin: SkillOrigin::CompanyManaged,
            origin_label: Some("Managed by Paperclip".to_string()),
            read_only: false,
            source_path: Some(skill_md),
            target_path: None,
            detail: description,
            location_label: None,
        });
    }
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    entries
}

/// 解析 `adapter_config.desiredSkills`（数组形式）。
pub fn resolve_desired_skill_names(config: &Value) -> Vec<String> {
    config
        .get("desiredSkills")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 构建完整 snapshot（合并 Paperclip-managed + Hermes-installed）。
///
/// 行为对齐 Node `buildHermesSkillSnapshot`：
/// 1. 扫描 Hermes 自己的 `~/.hermes/skills`
/// 2. 扫描 Paperclip-managed runtime skills
/// 3. 合并：managed 优先，按 `key` 去重
/// 4. desired 集合中找不到的 → `state = Missing` + warning
pub async fn build_skill_snapshot(
    config: &Value,
    runtime_skills_dir: Option<&Path>,
) -> AdapterSkillSnapshot {
    let home = resolve_hermes_home(config);
    let hermes_skills_home = home.join(".hermes").join("skills");

    let hermes_entries = scan_hermes_skills(&hermes_skills_home).await;
    let hermes_keys: std::collections::HashSet<String> =
        hermes_entries.iter().map(|e| e.key.clone()).collect();

    let paperclip_entries: Vec<AdapterSkillEntry> = match runtime_skills_dir {
        Some(dir) => scan_runtime_skills(dir).await,
        None => Vec::new(),
    };
    let available_by_key: std::collections::HashMap<String, AdapterSkillEntry> = paperclip_entries
        .iter()
        .map(|e| (e.key.clone(), e.clone()))
        .collect();

    let desired = resolve_desired_skill_names(config);
    let desired_set: std::collections::HashSet<&str> = desired.iter().map(String::as_str).collect();

    let mut entries: Vec<AdapterSkillEntry> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Paperclip-managed skills
    for mut entry in paperclip_entries {
        let is_desired = desired_set.contains(entry.key.as_str());
        entry.desired = is_desired;
        entry.state = if is_desired {
            SkillState::Configured
        } else {
            SkillState::Available
        };
        entry.detail = if is_desired {
            Some("Will be available on the next run via Hermes skill loading.".to_string())
        } else {
            None
        };
        entries.push(entry);
    }

    // Hermes-installed skills (read-only, always loaded)
    for entry in hermes_entries {
        if available_by_key.contains_key(&entry.key) {
            continue; // skip if Paperclip already manages
        }
        entries.push(entry);
    }

    // Desired skills that don't exist anywhere
    for desired_skill in &desired {
        if available_by_key.contains_key(desired_skill) || hermes_keys.contains(desired_skill) {
            continue;
        }
        warnings.push(format!(
            "Desired skill \"{desired_skill}\" is not available in Paperclip or Hermes skills."
        ));
        entries.push(AdapterSkillEntry {
            key: desired_skill.clone(),
            runtime_name: None,
            desired: true,
            managed: true,
            state: SkillState::Missing,
            origin: SkillOrigin::ExternalUnknown,
            origin_label: Some("External or unavailable".to_string()),
            read_only: false,
            source_path: None,
            target_path: None,
            detail: Some("Cannot find this skill in Paperclip or ~/.hermes/skills/.".to_string()),
            location_label: None,
        });
    }

    AdapterSkillSnapshot {
        adapter_type: "hermes".to_string(),
        supported: true,
        mode: "persistent",
        desired_skills: desired,
        entries,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn write_skill(home: &Path, category: &str, skill_name: &str, frontmatter: &str) -> PathBuf {
        let dir = home
            .join(".hermes")
            .join("skills")
            .join(category)
            .join(skill_name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(&path, frontmatter).unwrap();
        path
    }

    #[test]
    fn parses_frontmatter_basic() {
        let content = "---\nname: test\ndescription: a test skill\n---\nbody";
        let fm = parse_skill_frontmatter(content);
        assert_eq!(fm.name.as_deref(), Some("test"));
        assert_eq!(fm.description.as_deref(), Some("a test skill"));
    }

    #[test]
    fn parses_frontmatter_with_quoted_value() {
        let content = "---\ndescription: \"a quoted skill\"\n---";
        let fm = parse_skill_frontmatter(content);
        assert_eq!(fm.description.as_deref(), Some("a quoted skill"));
    }

    #[test]
    fn parses_frontmatter_without_marker() {
        let fm = parse_skill_frontmatter("no frontmatter here");
        assert_eq!(fm, SkillFrontmatter::default());
    }

    #[test]
    fn resolve_hermes_home_uses_env_var() {
        let dir = std::env::temp_dir();
        let config = json!({"env": {"HOME": dir.to_string_lossy()}});
        let resolved = resolve_hermes_home(&config);
        assert_eq!(resolved, dir);
    }

    #[test]
    fn resolve_hermes_home_falls_back_to_actual_home() {
        let config = json!({});
        let resolved = resolve_hermes_home(&config);
        assert!(!resolved.as_os_str().is_empty());
    }

    #[tokio::test]
    async fn scan_finds_top_level_skill() {
        let home = std::env::temp_dir().join(format!("hermes-skills-{}", uuid::Uuid::new_v4()));
        let skills_home = home.join(".hermes").join("skills").join("terminal");
        std::fs::create_dir_all(&skills_home).unwrap();
        std::fs::write(
            skills_home.join("SKILL.md"),
            "---\ndescription: terminal skill\n---\n",
        )
        .unwrap();
        let entries = scan_hermes_skills(&home.join(".hermes").join("skills")).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "terminal");
        assert_eq!(entries[0].state, SkillState::Installed);
        assert!(entries[0].read_only);
        std::fs::remove_dir_all(&home).ok();
    }

    #[tokio::test]
    async fn scan_finds_sub_skills() {
        let home = std::env::temp_dir().join(format!("hermes-skills-{}", uuid::Uuid::new_v4()));
        write_skill(
            &home,
            "terminal",
            "bash",
            "---\ndescription: bash sub-skill\n---",
        );
        let skills_home = home.join(".hermes").join("skills");
        let entries = scan_hermes_skills(&skills_home).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "bash");
        assert!(entries[0]
            .location_label
            .as_deref()
            .unwrap()
            .contains("terminal/bash"));
        std::fs::remove_dir_all(&home).ok();
    }

    #[tokio::test]
    async fn scan_returns_empty_when_no_skills_dir() {
        let home = std::env::temp_dir().join(format!("hermes-skills-{}", uuid::Uuid::new_v4()));
        let entries = scan_hermes_skills(&home.join(".hermes").join("skills")).await;
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn build_snapshot_marks_missing_desired_skills() {
        let home = std::env::temp_dir().join(format!("hermes-skills-{}", uuid::Uuid::new_v4()));
        let config = json!({
            "desiredSkills": ["nonexistent-skill"],
            "env": {"HOME": home.to_string_lossy()}
        });
        let snapshot = build_skill_snapshot(&config, None).await;
        assert_eq!(snapshot.desired_skills, vec!["nonexistent-skill"]);
        assert!(snapshot
            .warnings
            .iter()
            .any(|w| w.contains("nonexistent-skill")));
        let missing = snapshot
            .entries
            .iter()
            .find(|e| e.key == "nonexistent-skill")
            .expect("missing entry");
        assert_eq!(missing.state, SkillState::Missing);
        std::fs::remove_dir_all(&home).ok();
    }

    #[tokio::test]
    async fn build_snapshot_dedupes_hermes_against_runtime() {
        let home = std::env::temp_dir().join(format!("hermes-skills-{}", uuid::Uuid::new_v4()));
        let hermes_home = home.join(".hermes").join("skills").join("shared");
        std::fs::create_dir_all(&hermes_home).unwrap();
        std::fs::write(
            hermes_home.join("SKILL.md"),
            "---\ndescription: user installed\n---",
        )
        .unwrap();

        let runtime = std::env::temp_dir().join(format!("hermes-runtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(runtime.join("shared")).unwrap();
        std::fs::write(
            runtime.join("shared").join("SKILL.md"),
            "---\ndescription: managed\n---",
        )
        .unwrap();

        let config = json!({"env": {"HOME": home.to_string_lossy()}});
        let snapshot = build_skill_snapshot(&config, Some(&runtime)).await;
        // Paperclip-managed wins (appears once, marked managed)
        let shared_entries: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|e| e.key == "shared")
            .collect();
        assert_eq!(
            shared_entries.len(),
            1,
            "duplicate shared entries: {snapshot:?}"
        );
        assert!(shared_entries[0].managed);

        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&runtime).ok();
    }
}
