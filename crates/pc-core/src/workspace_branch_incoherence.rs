//! `workspace_branch_incoherence` 域（Round 277）。
//!
//! 与原 `paperclip/server/src/services/execution-workspaces.ts` 中
//! `fingerprintWorkspaceBranchIncoherence(input)` 1:1 对齐：
//! - 把 inspection 信息稳定序列化 + SHA-256 哈希
//! - 输出 `workspace_incoherence:v1:sha256:<hash>` 形式的 fingerprint
//! - 用作 reconcile audit comment 的 idempotency key
//!
//! 设计目标：高内聚低耦合。
//! - 高内聚：本模块只关心"branch incoherence fingerprint" 哈希逻辑。
//! - 低耦合：依赖 `stable_string` 模块（已实现 `stable_string_sha256_hex`）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::stable_string::versioned_sha256_fingerprint;

/// Git 工作区 cleanliness 字符串字面量（与 Node `cleanliness` union 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cleanliness {
    Clean,
    Dirty,
    Unknown,
}

impl Default for Cleanliness {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Fingerprint 输入：与 Node `fingerprintWorkspaceBranchIncoherence(input)` 1:1 对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchIncoherenceInput {
    pub source_issue_id: Option<String>,
    pub execution_workspace_id: Option<String>,
    pub worktree_path: String,
    pub expected_branch: String,
    pub actual_branch: Option<String>,
    pub cleanliness: Cleanliness,
    pub expected_head_sha: Option<String>,
    pub actual_head_sha: Option<String>,
}

/// `WORKSPACE_BRANCH_INCOHERENCE_REASON` 字符串字面量（与 Node 常量 1:1 对齐）。
pub const WORKSPACE_BRANCH_INCOHERENCE_REASON: &str = "git_worktree_branch_incoherence";

/// 计算 `workspace_incoherence:v1:sha256:<hash>` 形式的 fingerprint。
///
/// 与 Node `fingerprintWorkspaceBranchIncoherence(input)` 1:1 对齐：
/// 1) `path.resolve(input.worktreePath)` ——绝对规范化路径
/// 2) stableStringify 按 key 字典序输出整对象，固定顺序：
///    version=1, reason=WORKSPACE_BRANCH_INCOHERENCE_REASON,
///    sourceIssueId, executionWorkspaceId, worktreePath（绝对路径）,
///    expectedBranch, actualBranch, cleanliness,
///    expectedHeadSha, actualHeadSha
/// 3) sha256 + format `<reason_prefix>:v1:sha256:<hex>`
pub fn fingerprint_workspace_branch_incoherence(input: &BranchIncoherenceInput) -> String {
    let worktree_path = resolve_absolute_path(&input.worktree_path);
    let payload = json!({
        "version": 1,
        "reason": WORKSPACE_BRANCH_INCOHERENCE_REASON,
        "sourceIssueId": input.source_issue_id,
        "executionWorkspaceId": input.execution_workspace_id,
        "worktreePath": worktree_path.to_string_lossy(),
        "expectedBranch": input.expected_branch,
        "actualBranch": input.actual_branch,
        "cleanliness": input.cleanliness,
        "expectedHeadSha": input.expected_head_sha,
        "actualHeadSha": input.actual_head_sha,
    });
    versioned_sha256_fingerprint("workspace_incoherence", &payload)
}

/// `path.resolve`：Linux/macOS 简单实现——拼接 + clean_path。
/// 与 Node `path.resolve` 在 POSIX 平台表现一致：相对路径被相对 cwd 解析。
pub fn resolve_absolute_path(input: &str) -> PathBuf {
    if PathBuf::from(input).is_absolute() {
        return clean_path(&PathBuf::from(input));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    clean_path(&cwd.join(input))
}

fn clean_path(p: &std::path::Path) -> PathBuf {
    use std::path::Component;
    let mut cleaned = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(prefix) => cleaned.push(prefix.as_os_str()),
            Component::RootDir => cleaned.push(std::path::Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::Normal(seg) => cleaned.push(seg),
        }
    }
    cleaned
}

/// 把 inspection 对象直接转 fingerprint（便利：接受 JSON Value）。
///
/// 与 Node 函数同语义；调用方已有 inspection 对象时使用。
pub fn fingerprint_from_inspection_json(inspection: &serde_json::Value) -> String {
    let mut payload = serde_json::Map::new();
    payload.insert("version".to_string(), serde_json::Value::from(1));
    payload.insert(
        "reason".to_string(),
        serde_json::Value::from(WORKSPACE_BRANCH_INCOHERENCE_REASON),
    );
    if let Some(obj) = inspection.as_object() {
        for (k, v) in obj {
            payload.insert(k.clone(), v.clone());
        }
    } else {
        payload.insert("inspection".to_string(), inspection.clone());
    }
    versioned_sha256_fingerprint("workspace_incoherence", &serde_json::Value::Object(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_input() -> BranchIncoherenceInput {
        BranchIncoherenceInput {
            source_issue_id: Some("issue-1".to_string()),
            execution_workspace_id: Some("exec-1".to_string()),
            worktree_path: "/abs/path/worktree".to_string(),
            expected_branch: "feature/x".to_string(),
            actual_branch: Some("main".to_string()),
            cleanliness: Cleanliness::Dirty,
            expected_head_sha: Some("aaaa".to_string()),
            actual_head_sha: Some("bbbb".to_string()),
        }
    }

    #[test]
    fn fingerprint_deterministic_for_same_input() {
        let f1 = fingerprint_workspace_branch_incoherence(&base_input());
        let f2 = fingerprint_workspace_branch_incoherence(&base_input());
        assert_eq!(f1, f2);
    }

    #[test]
    fn fingerprint_format_prefix() {
        let f = fingerprint_workspace_branch_incoherence(&base_input());
        assert!(f.starts_with("workspace_incoherence:v1:sha256:"));
        // 64 hex chars
        assert_eq!(f.len(), "workspace_incoherence:v1:sha256:".len() + 64);
    }

    #[test]
    fn fingerprint_changes_with_source_issue_id() {
        let mut a = base_input();
        let mut b = base_input();
        b.source_issue_id = Some("issue-2".to_string());
        assert_ne!(
            fingerprint_workspace_branch_incoherence(&a),
            fingerprint_workspace_branch_incoherence(&b)
        );
    }

    #[test]
    fn fingerprint_changes_with_actual_branch() {
        let mut a = base_input();
        let mut b = base_input();
        b.actual_branch = Some("develop".to_string());
        assert_ne!(
            fingerprint_workspace_branch_incoherence(&a),
            fingerprint_workspace_branch_incoherence(&b)
        );
    }

    #[test]
    fn fingerprint_changes_with_cleanliness() {
        let mut a = base_input();
        let mut b = base_input();
        b.cleanliness = Cleanliness::Clean;
        assert_ne!(
            fingerprint_workspace_branch_incoherence(&a),
            fingerprint_workspace_branch_incoherence(&b)
        );
    }

    #[test]
    fn fingerprint_changes_with_expected_head_sha() {
        let mut a = base_input();
        let mut b = base_input();
        b.expected_head_sha = Some("cccc".to_string());
        assert_ne!(
            fingerprint_workspace_branch_incoherence(&a),
            fingerprint_workspace_branch_incoherence(&b)
        );
    }

    #[test]
    fn fingerprint_relative_worktree_resolves_to_absolute() {
        // Node: path.resolve("worktree") 应等于当前 cwd + "worktree"
        let mut a = base_input();
        let mut b = base_input();
        a.worktree_path = "worktree".to_string();
        b.worktree_path = format!(
            "{}/worktree",
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .to_string_lossy()
        );
        // Node `path.resolve` 在两个等价相对/绝对输入下应得到相同 fingerprint
        assert_eq!(
            fingerprint_workspace_branch_incoherence(&a),
            fingerprint_workspace_branch_incoherence(&b)
        );
    }

    #[test]
    fn fingerprint_null_actual_branch() {
        let mut a = base_input();
        a.actual_branch = None;
        let f = fingerprint_workspace_branch_incoherence(&a);
        // 即使空，hash 仍生成
        assert_eq!(f.len(), "workspace_incoherence:v1:sha256:".len() + 64);
    }

    #[test]
    fn fingerprint_cleanliness_serializes_as_lowercase() {
        let json = serde_json::to_string(&Cleanliness::Dirty).unwrap();
        assert_eq!(json, "\"dirty\"");
        let json = serde_json::to_string(&Cleanliness::Clean).unwrap();
        assert_eq!(json, "\"clean\"");
    }

    #[test]
    fn fingerprint_from_inspection_json_embeds_inspection() {
        let inspection = json!({"foo": "bar", "baz": 42});
        let f = fingerprint_from_inspection_json(&inspection);
        assert!(f.starts_with("workspace_incoherence:v1:sha256:"));
        // 与 versioned_sha256_fingerprint("workspace_incoherence", payload) 一致
        let mut payload = serde_json::Map::new();
        payload.insert("version".to_string(), serde_json::json!(1));
        payload.insert(
            "reason".to_string(),
            serde_json::Value::from(WORKSPACE_BRANCH_INCOHERENCE_REASON),
        );
        payload.insert("foo".to_string(), serde_json::json!("bar"));
        payload.insert("baz".to_string(), serde_json::json!(42));
        let expected = versioned_sha256_fingerprint(
            "workspace_incoherence",
            &serde_json::Value::Object(payload),
        );
        assert_eq!(f, expected);
    }

    #[test]
    fn fingerprint_from_inspection_accepts_non_object() {
        // 非对象值会包在 "inspection" key
        let f = fingerprint_from_inspection_json(&serde_json::json!("plain string"));
        assert!(f.starts_with("workspace_incoherence:v1:sha256:"));
    }
}
