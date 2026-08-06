//! `git_status_paths` 域（Round 273）。
//!
//! 与原 `paperclip/server/src/services/workspace-file-resources.ts` 中
//! `parseGitStatusPaths(stdout)` 1:1 对齐：解析 `git status --porcelain=v1 -z`
//! NUL 分隔的输出。
//!
//! 设计目标：高内聚低耦合。
//! - **高内聚**：单一职责 — NUL 分隔 porcelain 输出解析。
//! - **低耦合**：输入 `&str`、输出 typed struct；零 IO。
//!
//! Git porcelain=v1 -z 输出格式（每条 entry）：
//! ```
//! XY PATH\0                 // 普通文件：前 2 字节是状态，后面跟路径
//! XY PATH\0ORIG_PATH\0      // renamed/copied：连续两条 NUL 分隔
//! ```
//! 其中：
//! - `X` 是 index 状态（staged）
//! - `Y` 是 worktree 状态（unstaged）
//! - 中间可能有空格分隔符（如 `" M"`）— 我们用 `slice(3)` 跳过前 3 个字符。
//!
//! 与 Node 版差异说明：
//! - Rust 用 `split('\0')` 对 NUL 分隔处理；过滤掉空 token。
//! - R / C 状态会消耗额外一条 token（下一条就是原路径）；与 Node `i += 1` 等价。

use serde::Serialize;

use crate::workspace_file_classify::WORKSPACE_FILE_LIST_MAX_SCANNED_ENTRIES;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitStatusPaths {
    pub paths: Vec<String>,
    pub hit_scan_cap: bool,
}

/// 解析 NUL 分隔的 `git status --porcelain=v1 -z --untracked-files=all` 输出。
///
/// 与 Node `parseGitStatusPaths(stdout)` 1:1 对齐：
/// - 用 `\0` 切分；过滤空 token
/// - 跳过长度 < 4 的 token（无 path）
/// - 从 token 中取 `status = token[0..2]`，`path = token[3..]`
/// - 如果 status 包含 `R` 或 `C`：跳过下一条 token（orig path）
/// - 路径数 >= WORKSPACE_FILE_LIST_MAX_SCANNED_ENTRIES 时截断（hit_scan_cap = true）
pub fn parse_git_status_paths(stdout: &str) -> GitStatusPaths {
    let tokens: Vec<&str> = stdout.split('\0').filter(|s| !s.is_empty()).collect();
    let mut paths = Vec::new();
    let mut hit_scan_cap = false;
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        if token.len() < 4 {
            i += 1;
            continue;
        }
        // status = token[0..2], skip 1 space at idx 2, path = token[3..]
        // status 中的字符可能 unicode (rename 等)，简单按 byte 处理。
        let status_bytes = &token.as_bytes()[..2.min(token.len())];
        let status = std::str::from_utf8(status_bytes).unwrap_or("");
        let file_path = &token[3..];
        if !file_path.is_empty() {
            paths.push(file_path.to_string());
        }
        // 跳过多余的 orig-path token
        if status.contains('R') || status.contains('C') {
            i += 2;
        } else {
            i += 1;
        }
        if paths.len() >= WORKSPACE_FILE_LIST_MAX_SCANNED_ENTRIES as usize {
            hit_scan_cap = true;
            break;
        }
    }
    GitStatusPaths {
        paths,
        hit_scan_cap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> GitStatusPaths {
        parse_git_status_paths(s)
    }

    #[test]
    fn empty_returns_empty() {
        let out = parse("");
        assert_eq!(out.paths, Vec::<String>::new());
        assert!(!out.hit_scan_cap);
    }

    #[test]
    fn only_nul_returns_empty() {
        let out = parse("\0\0\0");
        assert_eq!(out.paths, Vec::<String>::new());
        assert!(!out.hit_scan_cap);
    }

    #[test]
    fn single_modified_file() {
        // `git status --porcelain=v1 -z` 中，单文件状态形如 " M path/to/file\0"
        let out = parse(" M src/main.rs\0");
        assert_eq!(out.paths, vec!["src/main.rs".to_string()]);
        assert!(!out.hit_scan_cap);
    }

    #[test]
    fn multiple_files() {
        // 多个 NUL 分隔条目
        let out = parse(" M src/a.rs\0 M src/b.rs\0?? new.txt\0");
        assert_eq!(
            out.paths,
            vec![
                "src/a.rs".to_string(),
                "src/b.rs".to_string(),
                "new.txt".to_string()
            ]
        );
    }

    #[test]
    fn untracked_file() {
        // untracked 是 "??"
        let out = parse("?? new.txt\0");
        assert_eq!(out.paths, vec!["new.txt".to_string()]);
    }

    #[test]
    fn renamed_file_consumes_two_tokens() {
        // rename 时：第一个 token 是 "R  newpath"，第二个 token 是 "origpath"
        let out = parse("R  new/path\0orig/path\0");
        assert_eq!(out.paths, vec!["new/path".to_string()]);
    }

    #[test]
    fn copied_file_consumes_two_tokens() {
        let out = parse("C  copy/path\0orig/path\0");
        assert_eq!(out.paths, vec!["copy/path".to_string()]);
    }

    #[test]
    fn renamed_then_modified() {
        let out = parse("R  new.txt\0old.txt\0 M src/main.rs\0");
        assert_eq!(
            out.paths,
            vec!["new.txt".to_string(), "src/main.rs".to_string()]
        );
    }

    #[test]
    fn skip_short_tokens() {
        // token 长度 < 4 跳过
        let out = parse("M\0??\0XX\0 M valid.rs\0");
        assert_eq!(out.paths, vec!["valid.rs".to_string()]);
    }

    #[test]
    fn skip_empty_path_segments() {
        // status 后紧跟 NUL（path 为空）
        let out = parse(" M \0 M src/main.rs\0");
        assert_eq!(out.paths, vec!["src/main.rs".to_string()]);
    }

    #[test]
    fn hits_scan_cap_when_too_many_paths() {
        // 构造 > MAX_SCANNED_ENTRIES 的 entries
        let cap = WORKSPACE_FILE_LIST_MAX_SCANNED_ENTRIES as usize;
        let mut input = String::new();
        for i in 0..(cap + 5) {
            input.push_str(&format!(" M file_{i}.rs\0"));
        }
        let out = parse(&input);
        assert_eq!(out.paths.len(), cap);
        assert!(out.hit_scan_cap);
    }

    #[test]
    fn exactly_at_cap_triggers() {
        let cap = WORKSPACE_FILE_LIST_MAX_SCANNED_ENTRIES as usize;
        let mut input = String::new();
        for i in 0..cap {
            input.push_str(&format!(" M file_{i}.rs\0"));
        }
        let out = parse(&input);
        // 到达 cap 那一项触发 hit_scan_cap
        assert_eq!(out.paths.len(), cap);
        assert!(out.hit_scan_cap);
    }

    #[test]
    fn serialized_to_json_includes_both_fields() {
        let out = parse(" M a.rs\0");
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(v["paths"][0], "a.rs");
        assert_eq!(v["hit_scan_cap"], false);
    }

    #[test]
    fn path_with_spaces() {
        // 路径中可能包含空格（合法）
        let out = parse(" M src/sub dir/file name.rs\0");
        assert_eq!(out.paths, vec!["src/sub dir/file name.rs".to_string()]);
    }
}
