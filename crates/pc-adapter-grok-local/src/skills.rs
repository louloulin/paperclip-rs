//! Grok adapter skills 快照（对齐 Node
//! `packages/adapters/grok-local/src/server/skills.ts`）。
//!
//! Grok 不自己管理 skills — 所有 Paperclip-managed skills 复制到
//! `.claude/skills` 在下次 run 时生效。

use crate::grok_test::{info, AdapterEnvironmentCheck};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 单个 skill 条目（与 `pc-adapter-hermes::skills::AdapterSkillEntry` 同构）。
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
    Configured,
    Available,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillOrigin {
    CompanyManaged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterSkillSnapshot {
    pub adapter_type: String,
    pub supported: bool,
    pub mode: &'static str,
    pub desired_skills: Vec<String>,
    pub entries: Vec<AdapterSkillEntry>,
    pub warnings: Vec<String>,
}

/// 解析 `adapterConfig.desiredSkills`。
pub fn resolve_desired_skill_names(config: &serde_json::Value) -> Vec<String> {
    config
        .get("desiredSkills")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 扫描 Paperclip-managed runtime skills 目录。
pub async fn scan_runtime_skills(runtime_skills_dir: &Path) -> Vec<AdapterSkillEntry> {
    let mut entries = Vec::new();
    let mut items = match tokio::fs::read_dir(runtime_skills_dir).await {
        Ok(d) => d,
        Err(_) => return entries,
    };
    while let Ok(Some(item)) = items.next_entry().await {
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
            detail: Some(
                "Will be copied into `.claude/skills` in the execution workspace on the next run."
                    .to_string(),
            ),
            location_label: None,
        });
    }
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    entries
}

/// 构建 Grok skills 快照（对齐 Node `buildGrokSkillSnapshot`）。
pub async fn build_skill_snapshot(
    config: &serde_json::Value,
    runtime_skills_dir: Option<&Path>,
) -> AdapterSkillSnapshot {
    let desired = resolve_desired_skill_names(config);
    let desired_set: std::collections::HashSet<&str> = desired.iter().map(String::as_str).collect();

    let available: Vec<AdapterSkillEntry> = match runtime_skills_dir {
        Some(dir) => scan_runtime_skills(dir).await,
        None => Vec::new(),
    };
    let available_keys: std::collections::HashSet<String> =
        available.iter().map(|e| e.key.clone()).collect();

    let mut entries: Vec<AdapterSkillEntry> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for mut entry in available {
        let is_desired = desired_set.contains(entry.key.as_str());
        entry.desired = is_desired;
        entry.state = if is_desired {
            SkillState::Configured
        } else {
            SkillState::Available
        };
        entries.push(entry);
    }

    for desired_skill in &desired {
        if available_keys.contains(desired_skill) {
            continue;
        }
        warnings.push(format!(
            "Desired skill \"{desired_skill}\" is not available in Paperclip runtime skills."
        ));
    }

    AdapterSkillSnapshot {
        adapter_type: "grok_local".to_string(),
        supported: true,
        mode: "runtime_mounted",
        desired_skills: desired,
        entries,
        warnings,
    }
}

/// 把当前 snapshot 转成 Info check 列表（用于 UI）。
pub fn snapshot_to_checks(snapshot: &AdapterSkillSnapshot) -> Vec<AdapterEnvironmentCheck> {
    let configured = snapshot
        .entries
        .iter()
        .filter(|e| matches!(e.state, SkillState::Configured))
        .count();
    let available = snapshot
        .entries
        .iter()
        .filter(|e| matches!(e.state, SkillState::Available))
        .count();
    vec![info(
        "grok.skills.snapshot",
        &format!(
            "Grok skills snapshot: {configured} configured, {available} available, {} warnings",
            snapshot.warnings.len()
        ),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_skill(dir: &Path, name: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        std::fs::write(&path, format!("---\ndescription: {name}\n---")).unwrap();
        path
    }

    #[test]
    fn resolve_desired_skill_names_parses_array() {
        let config = json!({"desiredSkills": ["a", "b"]});
        assert_eq!(resolve_desired_skill_names(&config), vec!["a", "b"]);
        let empty = json!({});
        assert!(resolve_desired_skill_names(&empty).is_empty());
    }

    #[tokio::test]
    async fn scan_runtime_skills_finds_skill_dirs() {
        let dir = std::env::temp_dir().join(format!("grok-runtime-{}", uuid::Uuid::new_v4()));
        write_skill(&dir, "alpha");
        write_skill(&dir, "beta");
        let entries = scan_runtime_skills(&dir).await;
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.key == "alpha"));
        assert!(entries.iter().any(|e| e.key == "beta"));
        assert!(entries.iter().all(|e| e.managed));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn scan_returns_empty_when_no_dir() {
        let entries = scan_runtime_skills(Path::new("/nonexistent/path")).await;
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn build_snapshot_marks_desired_as_configured() {
        let dir = std::env::temp_dir().join(format!("grok-runtime-{}", uuid::Uuid::new_v4()));
        write_skill(&dir, "wanted");
        write_skill(&dir, "extra");
        let config = json!({"desiredSkills": ["wanted"]});
        let snapshot = build_skill_snapshot(&config, Some(&dir)).await;
        let wanted = snapshot
            .entries
            .iter()
            .find(|e| e.key == "wanted")
            .expect("wanted");
        assert!(wanted.desired);
        assert_eq!(wanted.state, SkillState::Configured);
        let extra = snapshot
            .entries
            .iter()
            .find(|e| e.key == "extra")
            .expect("extra");
        assert!(!extra.desired);
        assert_eq!(extra.state, SkillState::Available);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn build_snapshot_warns_about_missing_desired_skills() {
        let dir = std::env::temp_dir().join(format!("grok-runtime-{}", uuid::Uuid::new_v4()));
        write_skill(&dir, "available");
        let config = json!({"desiredSkills": ["missing-skill"]});
        let snapshot = build_skill_snapshot(&config, Some(&dir)).await;
        assert!(snapshot
            .warnings
            .iter()
            .any(|w| w.contains("missing-skill")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn snapshot_to_checks_returns_summary() {
        let snapshot = AdapterSkillSnapshot {
            adapter_type: "grok_local".to_string(),
            supported: true,
            mode: "runtime_mounted",
            desired_skills: vec!["x".to_string()],
            entries: vec![],
            warnings: vec![],
        };
        let checks = snapshot_to_checks(&snapshot);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].level, crate::grok_test::CheckLevel::Info);
    }
}
