//! Node `redactCurrentUserText` + `redactWatchdogEvidenceText` 的纯函数 Rust 端口。
//!
//! 行为对齐：
//! - 屏蔽 home directory 路径段，将最后一段路径替换成 `firstChar + "*"` 形式
//! - 屏蔽用户名（按 word boundary），形式同上
//! - `enabled=false` 直接返回原文
//! - 输入为空时直接返回
//!
//! 与 Node 实现的差异：
//! - 用户名/家目录列表由调用方提供，避免在 recovery lib 中读环境
//! - 不实现 `redactSensitiveText`（关键词/命令/Token 脱敏）—— Node 在 redactor 之外组合，
//!   后续 Round 单独迁移以保持职责单一

use std::collections::HashSet;

pub const DEFAULT_REPLACEMENT: &str = "*";

#[derive(Debug, Clone)]
pub struct CurrentUserRedactionOptions {
    pub enabled: bool,
    pub user_names: Vec<String>,
    pub home_dirs: Vec<String>,
    /// 自定义 replacement；为空时使用首字母 + 重复 `*`。
    pub replacement: Option<String>,
}

impl CurrentUserRedactionOptions {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            user_names: vec![],
            home_dirs: vec![],
            replacement: None,
        }
    }
}

pub fn redact_watchdog_evidence_text(input: &str, opts: CurrentUserRedactionOptions) -> String {
    if input.is_empty() || !opts.enabled {
        return input.to_owned();
    }

    let mut result = input.to_owned();
    let mut seen: HashSet<String> = HashSet::new();

    let mut home_dirs = opts.home_dirs.clone();
    home_dirs.sort_by_key(|value| std::cmp::Reverse(value.len()));
    for home_dir in home_dirs {
        if home_dir.is_empty() || !result.contains(&home_dir) {
            continue;
        }
        let last_segment = split_path_last_segment(&home_dir);
        let replacement_dir = if let Some(segment) = last_segment {
            let masked = mask_user_name(&segment, opts.replacement.as_deref());
            replace_last_path_segment(&home_dir, &masked)
        } else {
            opts.replacement
                .clone()
                .unwrap_or_else(|| DEFAULT_REPLACEMENT.to_owned())
        };
        if seen.insert(home_dir.clone()) {
            result = result.replace(&home_dir, &replacement_dir);
        }
    }

    let mut user_names = opts.user_names.clone();
    user_names.sort_by_key(|value| std::cmp::Reverse(value.len()));
    for user_name in user_names {
        if user_name.is_empty() || !result.contains(&user_name) {
            continue;
        }
        if !seen.insert(user_name.clone()) {
            continue;
        }
        let masked = mask_user_name(&user_name, opts.replacement.as_deref());
        result = replace_with_word_boundaries(&result, &user_name, &masked);
    }

    result
}

fn mask_user_name(value: &str, fallback: Option<&str>) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return fallback.unwrap_or(DEFAULT_REPLACEMENT).to_owned();
    }
    let first = trimmed.chars().next().unwrap();
    let width = trimmed.chars().count();
    let stars = "*".repeat(width.max(1));
    format!("{first}{stars}")
}

fn split_path_last_segment(value: &str) -> Option<String> {
    let trimmed = value.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }
    let last_sep = trimmed
        .rfind('/')
        .into_iter()
        .chain(trimmed.rfind('\\'))
        .max();
    last_sep.map(|idx| trimmed[idx + 1..].to_owned())
}

fn replace_last_path_segment(path: &str, replacement: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if let Some(idx) = trimmed
        .rfind('/')
        .into_iter()
        .chain(trimmed.rfind('\\'))
        .max()
    {
        format!("{}{replacement}", &trimmed[..=idx])
    } else {
        replacement.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_returned_as_is() {
        let out = redact_watchdog_evidence_text("", CurrentUserRedactionOptions::disabled());
        assert_eq!(out, "");
    }
}

fn replace_with_word_boundaries(input: &str, target: &str, replacement: &str) -> String {
    let bytes = input.as_bytes();
    let target_bytes = target.as_bytes();
    if target_bytes.is_empty() {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut idx = 0;
    while idx + target_bytes.len() <= bytes.len() {
        if &bytes[idx..idx + target_bytes.len()] == target_bytes {
            let prev = if idx == 0 { None } else { Some(bytes[idx - 1]) };
            let next = bytes.get(idx + target_bytes.len()).copied();
            let prev_ok = prev.map_or(true, |value| !is_word_byte(value));
            let next_ok = next.map_or(true, |value| !is_word_byte(value));
            if prev_ok && next_ok {
                out.push_str(replacement);
                idx += target_bytes.len();
                continue;
            }
        }
        let ch = input[idx..].chars().next().unwrap();
        out.push(ch);
        idx += ch.len_utf8();
    }
    out.push_str(&input[idx..]);
    out
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-'
}
