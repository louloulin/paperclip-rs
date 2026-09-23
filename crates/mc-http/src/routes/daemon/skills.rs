//! Skill bundle 口径（M3-7 / LUM-1438）。
//!
//! 两个面共用一个口径：
//!
//! 1. `POST /api/daemon/runtimes/:runtimeId/tasks/:taskId/skill-bundles/resolve` ——
//!    把 claim 里给出的 skill ref 解析成完整 bundle（upstream
//!    `ResolveTaskSkillBundles` / `pkg/skillbundle`）。
//! 2. `POST /api/daemon/runtimes/:runtimeId/local-skills/import/:requestId/result` ——
//!    本地 skill 导入结果落库（upstream `ReportLocalSkillImportResult`）。
//!
//! ## hash 算法逐字移植
//!
//! upstream `pkg/skillbundle.BuildManifest` 对 `"v1"` / `source` / `id` / `name` /
//! `description` / `content` 以及每个（按 `path` 升序）文件的 `path` / `sha256:<hex>` /
//! `content` 各调一次 `writeHashPart`（`"%d:%s\n"`，长度是**字节**数）。这里的
//! `write_hash_part` 与 `hex(sha256)` 的 digest 前缀都照抄，否则 daemon 侧缓存校验
//! （`daemon validates the returned bundle before writing it to cache`）会全部失配。
//!
//! ## 只实现 `workspace` 源
//!
//! upstream 有 `workspace` / `builtin` / `plugin` 三个源；本仓未建 builtin / plugin
//! skill 子系统，因此本模块只产出 `workspace` 源。缺失的 ref 一律 `404 skill bundle
//! not found`（与上游同一行为，见 `docs/32` 偏离表的插件面）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use mc_repos::daemon::SkillBundleRow;

/// bundle 源：workspace 内 skill —— **本切片唯一实现的源**。
///
/// 上游另有 `builtin`（内置 skill 台账）与 `plugin`（要额外校验 pinned hash）两个源，
/// 本地都没有台账面：请求里出现它们时按「查不到」处理（`not found`），偏离见 `docs/32`。
pub(crate) const SOURCE_WORKSPACE: &str = "workspace";

/// upstream `skillbundle.writeHashPart`：`fmt.Fprintf(h, "%d:%s\n", len(value), value)`。
fn write_hash_part(hasher: &mut Sha256, value: &str) {
    hasher.update(format!("{}:{}\n", value.len(), value).as_bytes());
}

/// 单个支持文件的 wire 形状（upstream `AgentSkillFileData`）。
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

/// `usize` 字节数 → `i64`（上游 `size_bytes` / `sizeBytes` 都是 int64）。
///
/// 只有恶意构造的 PB 级内容才会溢出 `i64`；饱和到 `i64::MAX` 比回绕成负数安全
/// （下游用它算配额）。
fn as_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// 把一行 `SkillBundleRow` 投影成完整 bundle（含 manifest hash）。
///
/// 文件按 `path` 升序参与 hash，但**输出顺序也按 `path` 升序** —— upstream 对
/// `skill.Files` 先 `sort.Slice` 再两处使用（hash + `refs`），所以两处顺序一致。
#[must_use]
pub(crate) fn build_bundle(row: &SkillBundleRow) -> SkillBundleData {
    let mut files: Vec<(&String, &String)> = row.files.iter().map(|(p, c)| (p, c)).collect();
    files.sort_by(|a, b| a.0.cmp(b.0));

    let mut hasher = Sha256::new();
    write_hash_part(&mut hasher, "v1");
    write_hash_part(&mut hasher, SOURCE_WORKSPACE);
    write_hash_part(&mut hasher, &row.skill.id.to_string());
    write_hash_part(&mut hasher, &row.skill.name);
    write_hash_part(&mut hasher, &row.skill.description);
    write_hash_part(&mut hasher, &row.skill.content);

    let mut size = as_i64(row.skill.content.len());
    let mut out_files = Vec::with_capacity(files.len());
    for (path, content) in files {
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(content.as_bytes())));
        write_hash_part(&mut hasher, path);
        write_hash_part(&mut hasher, &digest);
        write_hash_part(&mut hasher, content);
        size += as_i64(content.len());
        out_files.push(SkillFileData {
            path: path.clone(),
            content: content.clone(),
            sha256: digest,
            size_bytes: as_i64(content.len()),
        });
    }

    SkillBundleData {
        id: row.skill.id.to_string(),
        source: SOURCE_WORKSPACE.to_string(),
        name: row.skill.name.clone(),
        description: row.skill.description.clone(),
        hash: format!("sha256:{}", hex::encode(hasher.finalize())),
        size_bytes: size,
        content: row.skill.content.clone(),
        files: out_files,
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
    use uuid::Uuid;

    fn row(files: Vec<(&str, &str)>) -> SkillBundleRow {
        use chrono::Utc;
        use mc_repos::daemon::SkillRow;
        SkillBundleRow {
            skill: SkillRow {
                id: Uuid::new_v4(),
                workspace_id: Uuid::new_v4(),
                name: "deploy".into(),
                description: "deploy helper".into(),
                content: "main".into(),
                config: Value::Null,
                created_by: None,
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
        let a = build_bundle(&one);
        let b = build_bundle(&reversed);
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
        assert_ne!(build_bundle(&one).hash, build_bundle(&other).hash);
    }

    /// 手算一遍 `writeHashPart` 的字节口径（含多字节 UTF-8 是**字节**数而非字符数）。
    #[test]
    fn hash_part_uses_byte_length() {
        let mut hasher = Sha256::new();
        write_hash_part(&mut hasher, "部署");
        // "部署" = 6 字节 ⇒ 头部必须是 "6:" 而不是 "2:"。
        let expected = Sha256::digest("6:部署\n".as_bytes());
        assert_eq!(hasher.finalize().to_vec(), expected.to_vec());
    }

    #[test]
    fn size_bytes_counts_content_and_files() {
        let bundle = build_bundle(&row(vec![("a.md", "abc")]));
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
