//! Skill bundle 口径（M3-7 / LUM-1438；**M6-4 扩到三源**）。
//!
//! 两个面共用一个口径：
//!
//! 1. `POST /api/daemon/runtimes/:runtimeId/tasks/:taskId/skill-bundles/resolve` ——
//!    把 claim 里给出的 skill ref 解析成完整 bundle（upstream
//!    `ResolveTaskSkillBundles` / `service.LoadRequestedAgentSkillBundles` / `pkg/skillbundle`）。
//! 2. `POST /api/daemon/runtimes/:runtimeId/local-skills/import/:requestId/result` ——
//!    本地 skill 导入结果落库（upstream `ReportLocalSkillImportResult`）。
//!
//! ## hash 算法 —— **单一实现点**，本模块不再自带一份
//!
//! 上游 `pkg/skillbundle.BuildManifest` = 分节 sha256。本模块把三源都投影成
//! [`mc_core::skill::ManifestInput`] 后调 [`mc_core::skill::build_manifest`]
//! （唯一实现点，`docs/57` §9.1 的一次性豁免），**不再**保一份私有副本：
//! 两份「同结果的实现」早晚会漂，而 daemon 侧缓存校验一旦失配就是
//! 「所有 skill 都重发」或「错内容被当成完整缓存」。
//!
//! ## 三个源
//!
//! | 源 | 台账 | 本模块的入口 |
//! | --- | --- | --- |
//! | `workspace` | `skill`（`plugin_installation_id IS NULL`） | [`build_agent_bundle`] |
//! | `plugin` | 同一张表（`plugin_installation_id IS NOT NULL`） | [`build_agent_bundle`] |
//! | `builtin` | `mc-skill` 的编译期内联资产（不在库里） | [`build_builtin_bundle`] |
//!
//! ⚠️ 三源的**授权谓词都是 `agent_skill`**（`workspace` / `plugin` 走
//! `ListAgentSkillsByIDs`；`builtin` 不走库，因为 claim 已经决定了该 agent 收到哪些）。
//! 上游 `LoadRequestedAgentSkillBundles` 的 `switch` **没有** `plugin` 分支
//! ⇒ 上游的插件 ref 在此之前就 404，pinned-hash 的 409 不可达；
//! M6-4 让 plugin 可解析后，那道 409 才真正可达（见 `claims.rs` 的 ref 循环）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use mc_core::skill::{ManifestFile, ManifestInput, SkillSource};
use mc_repos::skill::binding::AgentSkillBundleRow;

/// bundle 源常量（上游 `skillbundle.Source*`，与 [`SkillSource::as_str`] 同源）。
pub(crate) const SOURCE_WORKSPACE: &str = "workspace";
/// 平台内置资产（编译期 embedding，不在库里）。
pub(crate) const SOURCE_BUILTIN: &str = "builtin";
/// 插件安装贡献（`skill.plugin_installation_id IS NOT NULL`）。
pub(crate) const SOURCE_PLUGIN: &str = "plugin";

/// wire 源字符串 → [`SkillSource`]（三个常量以外的源**没有**服务端生产者）。
#[must_use]
pub(crate) fn parse_source(raw: &str) -> Option<SkillSource> {
    match raw {
        SOURCE_WORKSPACE => Some(SkillSource::Workspace),
        SOURCE_BUILTIN => Some(SkillSource::Builtin),
        SOURCE_PLUGIN => Some(SkillSource::Plugin),
        _ => None,
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct SkillFileData {
    /// `path`。
    pub path: String,
    /// `content`。
    pub content: String,
    /// `sha256`（`sha256:<hex>` 形态）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    /// `size_bytes`。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub size_bytes: i64,
}

/// 完整 bundle 的 wire 形状（upstream `service.AgentSkillData`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct SkillBundleData {
    /// `id`。
    pub id: String,
    /// `source`（`omitempty`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// `name`。
    pub name: String,
    /// `description`（`omitempty`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// `hash`（`omitempty`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    /// `size_bytes`（`omitempty`）。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub size_bytes: i64,
    /// `content`。
    pub content: String,
    /// `files`（`omitempty`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SkillFileData>,
}

// `serde(skip_serializing_if = "…")` 只接受 `fn(&T) -> bool`，签名由 serde 定死。
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(value: &i64) -> bool {
    *value == 0
}

/// 把一行 `AgentSkillBundleRow` 投影成完整 bundle（`workspace` 或 `plugin` 源）。
///
/// 源由 `plugin_installation_id` 决定（上游没有源列，而是由 `service` 侧决定；
/// 本仓把它放在行上，`claims.rs` 也用它作 map key 的一半）。
#[must_use]
pub(crate) fn build_agent_bundle(row: &AgentSkillBundleRow) -> SkillBundleData {
    let source = if row.is_plugin() {
        SkillSource::Plugin
    } else {
        SkillSource::Workspace
    };
    let files: Vec<ManifestFile<'_>> = row
        .files
        .iter()
        .map(|(path, content)| ManifestFile { path, content })
        .collect();
    project(&Projection {
        id: &row.skill.id.to_string(),
        source,
        name: &row.skill.name,
        description: &row.skill.description,
        content: &row.skill.content,
        files: &files,
    })
}

/// 按 `"builtin:<name>"` 投影出内置 bundle；未知 id → `None`（调用方报 404）。
///
/// **全量**查找而不是 agent 作用域内查找：daemon 只能请求 claim 交给它的 ref，
/// 作用域在 claim 侧已经决定过（上游 `AllBuiltinSkills` 的注释）。
#[must_use]
pub(crate) fn build_builtin_bundle(id: &str) -> Option<SkillBundleData> {
    let skill = mc_skill::builtin::builtin_skill_by_id(id)?;
    let files = skill.manifest_files();
    Some(project(&Projection {
        id: &skill.id(),
        source: SkillSource::Builtin,
        name: skill.name,
        // 上游 `AgentSkillData`（内置物）没有 description 字段。
        description: "",
        content: skill.content,
        files: &files,
    }))
}

/// 三源共用的投影入参（都是借用：调用方手里的行/常量不需要再克隆一遍）。
struct Projection<'a> {
    id: &'a str,
    source: SkillSource,
    name: &'a str,
    description: &'a str,
    content: &'a str,
    files: &'a [ManifestFile<'a>],
}

/// 投影成 wire 形态的 bundle（hash / size / 每文件 sha256 都来自 manifest）。
///
/// 文件先按 `path` 升序排（`build_manifest` 内部也这么排），然后与
/// `manifest.files` **按位置**对齐 —— 两边的顺序因此必然一致，不必再查一次表。
fn project(input: &Projection<'_>) -> SkillBundleData {
    let mut ordered: Vec<ManifestFile<'_>> = input.files.to_vec();
    ordered.sort_by(|a, b| a.path.cmp(b.path));
    let manifest = mc_core::skill::build_manifest(&ManifestInput {
        id: input.id,
        source: input.source,
        name: input.name,
        description: input.description,
        content: input.content,
        files: &ordered,
    });
    let files = ordered
        .iter()
        .zip(manifest.files.iter())
        .map(|(file, reference)| SkillFileData {
            path: reference.path.clone(),
            content: (*file.content).to_string(),
            sha256: reference.sha256.clone(),
            size_bytes: reference.size_bytes,
        })
        .collect();
    SkillBundleData {
        id: input.id.to_string(),
        source: input.source.as_str().to_string(),
        name: input.name.to_string(),
        description: input.description.to_string(),
        hash: manifest.hash,
        size_bytes: manifest.size_bytes,
        content: input.content.to_string(),
        files,
    }
}

/// `config` 列里记录的导入来源（upstream `ReportLocalSkillImportResult` 逐字）。
#[must_use]
pub(crate) fn local_import_config(runtime_id: &str, provider: &str, source_path: &str) -> Value {
    json!({
        "origin": {
            "type": "runtime_local",
            "runtime_id": runtime_id,
            "provider": provider,
            "source_path": source_path,
        }
    })
}

/// 支持文件名白名单校验（upstream `validateFilePath`）：拒绝绝对路径与任何 `..` 段。
///
/// 上游实现细节不同（它按平台分隔符切段），但语义等价：不合法**静默丢弃该文件**
/// （调用方 `continue`），不报错。
#[must_use]
pub(crate) fn validate_file_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') {
        return false;
    }
    // Windows 盘符（`C:\…`）。
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return false;
    }
    !path
        .split(['/', '\\'])
        .any(|segment| segment == ".." || segment.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_repos::skill::read::SkillRow;
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    fn row(files: Vec<(&str, &str)>) -> AgentSkillBundleRow {
        use chrono::Utc;
        AgentSkillBundleRow {
            skill: SkillRow {
                id: Uuid::new_v4(),
                workspace_id: Uuid::new_v4(),
                name: "deploy".into(),
                description: "deploy helper".into(),
                content: "main".into(),
                config: Value::Null,
                created_by: None,
                plugin_installation_id: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            files: files
                .into_iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect(),
        }
    }

    /// 上游 `TestBuildManifestStableAcrossFileOrder` 的等价断言。
    #[test]
    fn hash_is_stable_across_file_order() {
        // 同一个 skill（含 id）两份，只把 files 的顺序倒过来 —— 一行 `row()` 内部
        // 会取新 uuid，所以必须 clone 而不能调两次 `row()`。
        let one = row(vec![("b.md", "b"), ("a.md", "a")]);
        let mut reversed = one.clone();
        reversed.files.reverse();
        let a = build_agent_bundle(&one);
        let b = build_agent_bundle(&reversed);
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.files[0].path, "a.md");
        assert_eq!(b.files[0].path, "a.md");
    }

    /// 上游 `TestBuildManifestChangesWhenContentChanges` 的等价断言。
    #[test]
    fn hash_changes_with_content() {
        let mut one = row(vec![]);
        let mut other = one.clone();
        one.skill.content = "main".into();
        other.skill.content = "changed".into();
        assert_ne!(
            build_agent_bundle(&one).hash,
            build_agent_bundle(&other).hash
        );
    }

    /// hash 口径只有一处实现：本模块（M6-4 起）调的就是 `mc_core` 里那一个 `fn`。
    ///
    /// 这条测试的用意不是「结果一样」，而是**把它钉死在同一个符号上**：如果谁在
    /// `daemon/skills.rs` 里又抄一份 `write_hash_part`，这里手算的字节口径会先红。
    #[test]
    fn hash_part_is_the_single_mc_core_implementation() {
        let mut hasher = Sha256::new();
        mc_core::skill::write_hash_part(&mut hasher, "部署");
        // "部署" = 6 字节 ⇒ 头部必须是 "6:" 而不是 "2:"。
        let expected = Sha256::digest("6:部署\n".as_bytes());
        assert_eq!(hasher.finalize().to_vec(), expected.to_vec());
    }

    /// workspace 与 plugin 只差 `source` 一节（同一张表、同一条授权谓词）。
    #[test]
    fn plugin_source_is_selected_by_installation_id() {
        let workspace = build_agent_bundle(&row(vec![]));
        assert_eq!(workspace.source, SOURCE_WORKSPACE);

        let mut installed = row(vec![]);
        installed.skill.plugin_installation_id = Some(Uuid::new_v4());
        let plugin = build_agent_bundle(&installed);
        assert_eq!(plugin.source, SOURCE_PLUGIN);
        // 源是 hash 的一节 ⇒ 同一行换个源必然换 digest。
        assert_ne!(workspace.hash, plugin.hash);
    }

    /// 内置资产：`builtin:<name>` 可解析、未知 id 落 `None`、`file_count`/files 与 manifest 对齐。
    #[test]
    fn builtin_bundle_matches_the_embedded_manifest() {
        let id = mc_skill::builtin::builtin_skill_id(mc_skill::builtin::PLATFORM_SKILL_NAME);
        let bundle = build_builtin_bundle(&id).expect("platform skill is embedded");
        assert_eq!(bundle.id, id);
        assert_eq!(bundle.source, SOURCE_BUILTIN);
        assert_eq!(bundle.name, mc_skill::builtin::PLATFORM_SKILL_NAME);
        assert!(bundle.description.is_empty());
        assert!(bundle.hash.starts_with("sha256:"));
        // 与 `mc-skill` 侧自算的 manifest 逐字一致（同一实现点的第二个视角）。
        let skill = mc_skill::builtin::builtin_skill_by_id(&id).unwrap();
        assert_eq!(bundle.hash, skill.manifest().hash);
        assert_eq!(bundle.size_bytes, skill.manifest().size_bytes);
        // `SKILL.md` 是正文（`content`），**不是**支持文件。
        assert_eq!(bundle.files.len(), skill.files.len());
        assert!(bundle.files.iter().all(|f| f.path != "SKILL.md"));
        assert_eq!(
            bundle
                .files
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>(),
            skill
                .file_refs()
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>()
        );
        assert!(!bundle.content.is_empty());

        assert!(build_builtin_bundle("builtin:nope").is_none());
        assert!(build_builtin_bundle("not-a-builtin").is_none());
    }

    #[test]
    fn only_the_three_sources_are_known() {
        assert_eq!(parse_source("workspace"), Some(SkillSource::Workspace));
        assert_eq!(parse_source("builtin"), Some(SkillSource::Builtin));
        assert_eq!(parse_source("plugin"), Some(SkillSource::Plugin));
        assert_eq!(parse_source(""), None);
        assert_eq!(parse_source("Workspace"), None);
        assert_eq!(parse_source("local"), None);
    }

    #[test]
    fn size_bytes_counts_content_and_files() {
        let bundle = build_agent_bundle(&row(vec![("a.md", "abc")]));
        assert_eq!(bundle.size_bytes, 4 + 3);
        assert_eq!(bundle.files[0].size_bytes, 3);
        assert_eq!(bundle.files[0].sha256.len(), "sha256:".len() + 64);
    }

    #[test]
    fn file_path_whitelist() {
        assert!(validate_file_path("a/b.md"));
        assert!(!validate_file_path("/etc/passwd"));
        assert!(!validate_file_path("../escape"));
        assert!(!validate_file_path("a/../b"));
        assert!(!validate_file_path("C:\\win"));
        assert!(!validate_file_path(""));
    }
}
