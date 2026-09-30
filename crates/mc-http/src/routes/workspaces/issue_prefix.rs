//! `issue_prefix` 的入参校验（`BEHAVIOR_ISSUE_PREFIX_UNPORTED`，LUM-2592 / T1-6-G1）。
//!
//! 🔴 **为什么单独成文件**：`routes/workspaces.rs` 在本片起手时已是 **756 行**，加上本片
//! 的校验 + 双向单测会顶破门 ⑩ 的 **R7 单文件 800 行硬上限**（基线只许变短）。本仓
//! `agents.rs`+`agents/`、`auth.rs`+`auth/`、`issues.rs`+`issues/` 都是同一个 `foo.rs`
//! + `foo/` 子模块形状，这里沿用。
//!
//! 🔴 **本片只做到「拒」**：合法值**仍不落库** —— `mc_core::WorkspaceUpdate` 没有
//! `issue_prefix` 字段（列在 `migrations/upstream/020_issue_number.up.sql` 里**存在**，
//! `mc-repos/src/squad.rs:680` 在读，缺的是 DTO 字段）。补完条件与残留缺口见
//! `docs/37` §268，**不在本片范围**。

use mc_errors::Error;

/// 400 的**唯一**文本（上游 `issuePrefixFormatError`，`workspace.go:96` 逐字）。
const ISSUE_PREFIX_FORMAT_ERROR: &str = "issue prefix must be 1-10 uppercase letters or digits";

/// `issue_prefix` 的入参校验（上游 `normalizeIssuePrefix`，`workspace.go:84-94`）：
/// `ToUpper(TrimSpace(raw))`；空 ⇒ **「没给」**、回落默认、**不** 400；否则必须匹配
/// `^[A-Z0-9]{1,10}$`（`workspace.go:29`），否则 400 + 逐字文案。
///
/// ⚠️ 不用 `routes/workspaces.rs` 里那个 `validation(...)`：它返回该文件独有的
/// `ApiError` 且不转发 `http_status()`；本函数声明 `Error`，下面的双向单测才能
/// 直接断言状态码。
pub(super) fn normalize_issue_prefix(raw: &str) -> Result<String, Error> {
    let prefix = raw.trim().to_uppercase();
    // 上游的 ("", true)：空白 = 未提供，不 400
    let b = prefix.as_bytes();
    let ok = prefix.is_empty()
        || (prefix.len() <= 10
            && b.iter()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
    if ok {
        Ok(prefix)
    } else {
        Err(prefix_error())
    }
}

fn prefix_error() -> Error {
    Error::Validation {
        message: ISSUE_PREFIX_FORMAT_ERROR.to_string(),
        details: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 双向判据（`BEHAVIOR_ISSUE_PREFIX_UNPORTED`）：畸形值 ⇒ **400**；良构值 ⇒ **不**被拒。
    ///
    /// 🔴 双向不是形式主义：只补「畸形 → 400」而不测良构侧，一次把 `trim`/`upper`
    /// 写反就会让 `?issue_prefix= abc ` 这类合法请求变成假通过。
    #[test]
    fn issue_prefix_validation_is_bidirectional() {
        for raw in ["前端团队前端团队前端", "AB-C", "ABCDEFGHIJK", "abc def"] {
            let e = normalize_issue_prefix(raw).expect_err(raw);
            assert_eq!(e.http_status(), 400, "{raw}");
            assert_eq!(
                e.to_string(),
                format!("validation error: {ISSUE_PREFIX_FORMAT_ERROR}")
            );
        }
        for (raw, want) in [
            ("ABC", "ABC"),
            ("abc", "ABC"),
            (" ab1 ", "AB1"),             // trim 在 upper 之前 ⇒ 先 trim 再 upper
            ("A1B2C3D4E5", "A1B2C3D4E5"), // 上限 10 字符：恰好压线通过
            ("", ""),                     // 空 = 未提供 ⇒ 回落默认，不 400
            ("   ", ""),                  // 纯空白同上（上游 TrimSpace 之后判空）
        ] {
            assert_eq!(normalize_issue_prefix(raw).expect(raw), want, "{raw}");
        }
    }
}
