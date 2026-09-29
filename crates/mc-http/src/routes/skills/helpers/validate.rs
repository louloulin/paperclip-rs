use super::{bad_request, Digest, Error, HashMap, Sha256, SkillFileInput, SkillFileInputDto};

/// 上游 `validateFilePath`：空、绝对路径、`Clean` 后以 `..` 开头都拒（`..foo` 的怪癖
/// 逐字复刻，见 `docs/32` §9.6）。`Clean` 走 `mc_skill::reserved` 的 Go 移植，
/// **不**用 `Path::clean`（两者对尾随 `/.`、重复分隔符处理不同）。
pub(crate) fn validate_file_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.starts_with('/') {
        return false;
    }
    !mc_skill::reserved::clean_path(path).starts_with("..")
}

/// 请求体里的文件清单 → 仓储入参，并**跳过**保留路径（上游 `IsReservedContentPath`）。
///
/// 保留路径（`SKILL.md`）是正文列 `skill.content` 的地盘：可以出现在请求体里、会被
/// `validate_file_path` 校验，但**不落 `skill_file`**；上游 create/update 各写一遍，
/// 这里收成一份。
pub(crate) fn supported_files(files: &[SkillFileInputDto]) -> Vec<SkillFileInput> {
    files
        .iter()
        .filter(|f| !mc_skill::reserved::is_reserved_content_path(&f.path))
        .map(|f| SkillFileInput {
            path: f.path.clone(),
            content: f.content.clone(),
        })
        .collect()
}

/// 上游 `contentHash`：裸十六进制 SHA-256（**不是** bundle 形态的 `sha256:…`），与 SQL 侧
/// `encode(sha256(convert_to(content,'UTF8')),'hex')` 同值 ⇒ 两处哈希可比较。
pub(crate) fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// 上游 `resolveSkillInclude`：`?include=` → 是否内联正文。缺省（与 `content`）都内联 ——
/// 已有客户端在读 `content`，翻默认值会让他们静默收到不同形状。
pub(crate) fn resolve_include(query: &HashMap<String, String>) -> Result<bool, Error> {
    match query.get("include").map(String::as_str).map(str::trim) {
        None | Some("" | "content") => Ok(true),
        Some("metadata") => Ok(false),
        Some(_) => Err(bad_request(
            r#"invalid include: expected "content" or "metadata""#,
        )),
    }
}
