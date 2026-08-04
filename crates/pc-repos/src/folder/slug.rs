//! Slug 规范化与 reserved key 常量。
//!
//! 对齐 Node `normalizeFolderSlug`：NFKD + 去重音 + 小写 + 非字母数字替换为 `-` +
//! 收尾压紧 + 默认 `folder`。

use uuid::Uuid;

/// 顶级 skill 文件夹保留名（与 Node `RESERVED_ROOT_SLUGS` 对齐）。
pub(crate) const RESERVED_ROOT_SLUGS: &[&str] = &["bundled", "my", "projects"];

/// "my" / "projects" 这些保留根下的子文件夹不能再被用户改（与 Node 对齐）。
pub(crate) const RESERVED_CHILD_ROOT_SYSTEM_KEYS: &[&str] = &["my", "projects"];

/// 最大嵌套层数。
pub(crate) const MAX_FOLDER_DEPTH: i32 = 4;

/// 把任意输入字符串规范化为 kebab-case slug。
/// 输入为空或纯符号时返回 `"folder"`。
pub fn normalize_folder_slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_dash = true;
    for ch in value.chars() {
        let normalized = ch.to_string().to_lowercase();
        for c in normalized.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c);
                last_dash = false;
            } else if !last_dash {
                out.push('-');
                last_dash = true;
            }
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "folder".to_string()
    } else {
        trimmed
    }
}

/// 顶级 skill 文件夹保留名判定。
pub(crate) fn is_reserved_root_slug(kind: crate::folder::FolderKind, parent_id: Option<Uuid>, slug: &str) -> bool {
    kind == crate::folder::FolderKind::Skill
        && parent_id.is_none()
        && RESERVED_ROOT_SLUGS.contains(&slug)
}
