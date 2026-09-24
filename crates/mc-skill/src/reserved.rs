//! 保留路径判定 ——「哪些路径不能当 `skill_file` 存」。
//!
//! - **写者**：M6-2（`docs/57` §3.2）。
//! - **上游**：`internal/skill/reserved.go` 的 `IsReservedContentPath`（27 行）。
//! - **语义（唯一实现点）**：`SKILL.md` 本身**不是**支持文件 —— 它的正文是
//!   `skill.content`（列），不是 `skill_file` 的一行。导入与手工写文件两条路径都必须
//!   在写库前用它挡一道，否则会出现「同一份正文两处存」且刷新时互相覆盖。
//! - **逐字对齐**：`filepath.Clean`（Unix）后做**大小写不敏感**比较，所以
//!   `./SKILL.md` / `sub/../SKILL.md` / `skills.md` 都算保留。
//!   `Clean` 是手工移植的（Rust `std` 没有等价函数），用例表见下。
//! - **已知偏差（登记 `docs/32` §9.6）**：上游 `strings.EqualFold` 是 Unicode 简单折叠，
//!   本仓用 `eq_ignore_ascii_case`。差异只在路径含非 ASCII 且能折叠成 `skill.md` 的字节
//!   上出现（ASCII 集内两者等价），本波不引入 Unicode 折叠表。
//! - **不做什么**：`validateFilePath`（路径合法性 / `..` 穿越）是 route 层的入参校验，
//!   归 M6-2 的 `routes/skills/helpers.rs`（`crud.rs` / `files.rs` 都调它），**不**搬到这里
//!   （一个在写库前、一个在解归档时，两者都要能独立复用）。

/// 上游 `skill.ContentFilename`。
pub const CONTENT_FILENAME: &str = "SKILL.md";

/// 上游 `IsReservedContentPath`。
pub fn is_reserved_content_path(path: &str) -> bool {
    clean_path(path).eq_ignore_ascii_case(CONTENT_FILENAME)
}

/// `filepath.Clean` 的 Unix 移植。
///
/// 规则：去掉空段与 `.`；`..` 弹出上一段，但弹不动时（已在根、或上一段本身就是 `..`）
/// 在**绝对路径**上丢弃、在**相对路径**上保留；全空 ⇒ 相对路径给 `.`、绝对路径给 `/`。
pub fn clean_path(path: &str) -> String {
    let rooted = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => match out.last() {
                Some(last) if *last != ".." => {
                    out.pop();
                }
                _ if rooted => {}
                _ => out.push(".."),
            },
            _ => out.push(part),
        }
    }
    let joined = out.join("/");
    if rooted {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `filepath.Clean`（Unix）的对照表。
    #[test]
    fn clean_path_matches_go_filepath_clean() {
        for (input, want) in [
            ("", "."),
            (".", "."),
            ("/", "/"),
            ("SKILL.md", "SKILL.md"),
            ("./SKILL.md", "SKILL.md"),
            ("SKILL.md/", "SKILL.md"),
            ("sub/../SKILL.md", "SKILL.md"),
            ("a//b", "a/b"),
            ("a/./b", "a/b"),
            ("//a/b", "/a/b"),
            ("a/b/..", "a"),
            ("a/..", "."),
            ("/a/../..", "/"),
            ("..", ".."),
            ("../SKILL.md", "../SKILL.md"),
            ("../../a", "../../a"),
            ("..hidden", "..hidden"),
            ("a/..b/c", "a/..b/c"),
            ("a/...b", "a/...b"),
            ("/SKILL.md", "/SKILL.md"),
        ] {
            assert_eq!(clean_path(input), want, "clean_path({input:?})");
        }
    }

    #[test]
    fn reserved_paths_are_caught_in_every_spelling() {
        for path in [
            "SKILL.md",
            "./SKILL.md",
            "sub/../SKILL.md",
            "SKILL.md/",
            "skill.md",
            "Skill.MD",
            "SKILL.Md",
            "a/../SKILL.md",
        ] {
            assert!(is_reserved_content_path(path), "{path} should be reserved");
        }
    }

    #[test]
    fn other_paths_are_not_reserved() {
        for path in [
            "",
            ".",
            "/",
            "README.md",
            "skills.md",
            "SKILL.md.bak",
            "SKILL.mdx",
            "skills/SKILL.md",
            "..hidden",
            "a/..b/SKILL.md",
        ] {
            assert!(
                !is_reserved_content_path(path),
                "{path} should not be reserved"
            );
        }
    }
}
