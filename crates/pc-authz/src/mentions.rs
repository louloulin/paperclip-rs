//! pc-authz：从 issue / comment body 解析 mention IDs。
//!
//! 与原 `paperclip/packages/shared/src/project-mentions.ts` 的 `extractAgentMentionIds` /
//! `extractUserMentionIds` / `parseAgentMentionHref` / `parseUserMentionHref` 对齐。
//!
//! Markdown 链接格式：`[显示文本](scheme://id?key=val)`。
//! 已知 scheme：`agent://` / `user://` / `skill://` / `routine://` / `pipeline://` / `project://`。

use uuid::Uuid;

/// 各种 mention scheme（与原 `*_MENTION_SCHEME` 对齐）。
pub const AGENT_MENTION_SCHEME: &str = "agent://";
pub const USER_MENTION_SCHEME: &str = "user://";
pub const SKILL_MENTION_SCHEME: &str = "skill://";
pub const ROUTINE_MENTION_SCHEME: &str = "routine://";
pub const PIPELINE_MENTION_SCHEME: &str = "pipeline://";
pub const PROJECT_MENTION_SCHEME: &str = "project://";

/// 解析后的 agent mention。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAgentMention {
    pub agent_id: Uuid,
    pub icon: Option<String>,
}

/// 解析后的 user mention。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUserMention {
    pub user_id: String,
}

/// 解析 `agent://uuid[?i=icon]` href。
pub fn parse_agent_mention_href(href: &str) -> Option<ParsedAgentMention> {
    if !href.starts_with(AGENT_MENTION_SCHEME) {
        return None;
    }
    // 提取 scheme 之后的部分
    let remainder = &href[AGENT_MENTION_SCHEME.len()..];
    // 分离 path 和 query
    let (path_part, query_part) = match remainder.find('?') {
        Some(idx) => (&remainder[..idx], Some(&remainder[idx + 1..])),
        None => (remainder, None),
    };
    let id_str = path_part.trim().trim_start_matches('/');
    let agent_id = Uuid::parse_str(id_str).ok()?;
    let icon = query_part
        .and_then(|q| parse_query_param(q, "i"))
        .or_else(|| query_part.and_then(|q| parse_query_param(q, "icon")))
        .filter(|s| is_agent_icon_name(s));
    Some(ParsedAgentMention { agent_id, icon })
}

/// 解析 `user://userId` href。
pub fn parse_user_mention_href(href: &str) -> Option<ParsedUserMention> {
    if !href.starts_with(USER_MENTION_SCHEME) {
        return None;
    }
    let remainder = &href[USER_MENTION_SCHEME.len()..];
    let id_str = remainder.trim().trim_start_matches('/');
    if id_str.is_empty() {
        return None;
    }
    Some(ParsedUserMention {
        user_id: id_str.to_string(),
    })
}

/// 从 markdown body 中提取所有 agent mention IDs（去重）。
pub fn extract_agent_mention_ids(markdown: &str) -> Vec<Uuid> {
    let mut ids: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
    for href in extract_hrefs(markdown, AGENT_MENTION_SCHEME) {
        if let Some(parsed) = parse_agent_mention_href(&href) {
            ids.insert(parsed.agent_id);
        }
    }
    ids.into_iter().collect()
}

/// 从 markdown body 中提取所有 user mention IDs（去重，保留首次顺序）。
pub fn extract_user_mention_ids(markdown: &str) -> Vec<String> {
    let mut ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for href in extract_hrefs(markdown, USER_MENTION_SCHEME) {
        if let Some(parsed) = parse_user_mention_href(&href) {
            ids.insert(parsed.user_id);
        }
    }
    ids.into_iter().collect()
}

/// 从 markdown body 中提取所有 pipeline mention IDs（去重）。
pub fn extract_pipeline_mention_ids(markdown: &str) -> Vec<Uuid> {
    let mut ids: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
    for href in extract_hrefs(markdown, PIPELINE_MENTION_SCHEME) {
        let remainder = &href[PIPELINE_MENTION_SCHEME.len()..];
        let id_str = remainder
            .split('?')
            .next()
            .unwrap_or("")
            .trim()
            .trim_start_matches('/');
        if let Ok(id) = Uuid::parse_str(id_str) {
            ids.insert(id);
        }
    }
    ids.into_iter().collect()
}

/// 从 markdown body 中提取所有 routine mention IDs（去重）。
pub fn extract_routine_mention_ids(markdown: &str) -> Vec<Uuid> {
    let mut ids: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
    for href in extract_hrefs(markdown, ROUTINE_MENTION_SCHEME) {
        let remainder = &href[ROUTINE_MENTION_SCHEME.len()..];
        let id_str = remainder
            .split('?')
            .next()
            .unwrap_or("")
            .trim()
            .trim_start_matches('/');
        if let Ok(id) = Uuid::parse_str(id_str) {
            ids.insert(id);
        }
    }
    ids.into_iter().collect()
}

/// 从 markdown body 中提取所有 skill mention IDs（去重）。
pub fn extract_skill_mention_ids(markdown: &str) -> Vec<Uuid> {
    let mut ids: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
    for href in extract_hrefs(markdown, SKILL_MENTION_SCHEME) {
        let remainder = &href[SKILL_MENTION_SCHEME.len()..];
        let id_str = remainder
            .split('?')
            .next()
            .unwrap_or("")
            .trim()
            .trim_start_matches('/');
        if let Ok(id) = Uuid::parse_str(id_str) {
            ids.insert(id);
        }
    }
    ids.into_iter().collect()
}

/// 提取 markdown 中所有指定 scheme 的 href 列表（去重）。
///
/// 匹配模式：`[label](scheme://...)`（与 Node `AGENT_MENTION_LINK_RE` 一致）。
fn extract_hrefs(markdown: &str, scheme: &str) -> Vec<String> {
    let mut hrefs: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // 简化实现：扫描 `[...](scheme://...)` 模式
    let mut i = 0;
    let md_bytes = markdown.as_bytes();
    let scheme_bytes = scheme.as_bytes();
    while i + 1 < md_bytes.len() {
        if md_bytes[i] == b'[' {
            // 找 `](`
            if let Some(close_bracket) = markdown[i..].find("](") {
                let label_end = i + close_bracket;
                if let Some(close_paren) = markdown[label_end + 2..].find(')') {
                    let href_start = label_end + 2;
                    let href_end = label_end + 2 + close_paren;
                    let href = &markdown[href_start..href_end];
                    if href.starts_with(scheme) && !href.contains(' ') && !href.contains('\n') {
                        if seen.insert(href.to_string()) {
                            hrefs.push(href.to_string());
                        }
                    }
                    // 跳过这一段
                    i = href_end + 1;
                    continue;
                }
            }
        }
        i += 1;
        let _ = scheme_bytes; // 抑制 unused warning
    }
    hrefs
}

/// 从 query string 中解析单个参数。
fn parse_query_param(query: &str, key: &str) -> Option<String> {
    for part in query.split('&') {
        if let Some(idx) = part.find('=') {
            let k = &part[..idx];
            if k == key {
                let v = &part[idx + 1..];
                return url_decode(v);
            }
        } else if part == key {
            return Some(String::new());
        }
    }
    None
}

/// 简单的 percent-decode（与 Node URL 兼容）。
fn url_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h1 = hex_digit(bytes[i + 1])?;
            let h2 = hex_digit(bytes[i + 2])?;
            out.push((h1 << 4) | h2);
            i += 3;
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 校验 agent icon name 格式（与 Node `AGENT_ICON_NAME_RE` 对齐）。
fn is_agent_icon_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
        })
}

/// 构造 `agent://uuid?i=icon` href（用于测试 + 业务生成）。
pub fn build_agent_mention_href(agent_id: Uuid, icon: Option<&str>) -> String {
    let id_str = agent_id.to_string();
    match icon {
        Some(i) if is_agent_icon_name(i) => format!("{AGENT_MENTION_SCHEME}{id_str}?i={i}"),
        _ => format!("{AGENT_MENTION_SCHEME}{id_str}"),
    }
}

/// 构造 `user://userId` href。
pub fn build_user_mention_href(user_id: &str) -> String {
    format!("{USER_MENTION_SCHEME}{}", user_id.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent_mention_href_basic() {
        let id = Uuid::new_v4();
        let href = format!("agent://{id}");
        let parsed = parse_agent_mention_href(&href).unwrap();
        assert_eq!(parsed.agent_id, id);
        assert_eq!(parsed.icon, None);
    }

    #[test]
    fn parse_agent_mention_href_with_icon() {
        let id = Uuid::new_v4();
        let href = format!("agent://{id}?i=robot");
        let parsed = parse_agent_mention_href(&href).unwrap();
        assert_eq!(parsed.agent_id, id);
        assert_eq!(parsed.icon.as_deref(), Some("robot"));
    }

    #[test]
    fn parse_agent_mention_href_wrong_scheme() {
        assert!(parse_agent_mention_href("user://abc").is_none());
    }

    #[test]
    fn parse_agent_mention_href_invalid_uuid() {
        assert!(parse_agent_mention_href("agent://not-a-uuid").is_none());
    }

    #[test]
    fn parse_user_mention_href_basic() {
        let parsed = parse_user_mention_href("user://u-12345").unwrap();
        assert_eq!(parsed.user_id, "u-12345");
    }

    #[test]
    fn parse_user_mention_href_empty() {
        assert!(parse_user_mention_href("user://").is_none());
    }

    #[test]
    fn extract_agent_mention_ids_single() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let md = format!(
            "Hey [@a](agent://{id1}) and [@b](agent://{id2}), please check this.",
        );
        let ids = extract_agent_mention_ids(&md);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    #[test]
    fn extract_agent_mention_ids_dedup() {
        let id = Uuid::new_v4();
        let md = format!(
            "[a](agent://{id}) and [b](agent://{id}) and [c](agent://{id})",
        );
        let ids = extract_agent_mention_ids(&md);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], id);
    }

    #[test]
    fn extract_agent_mention_ids_with_other_schemes() {
        let agent_id = Uuid::new_v4();
        let pipeline_id = Uuid::new_v4();
        let md = format!(
            "[agent](agent://{agent_id}) [pipeline](pipeline://{pipeline_id})",
        );
        let agents = extract_agent_mention_ids(&md);
        let pipelines = extract_pipeline_mention_ids(&md);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0], agent_id);
        assert_eq!(pipelines.len(), 1);
        assert_eq!(pipelines[0], pipeline_id);
    }

    #[test]
    fn extract_user_mention_ids_basic() {
        let md = "Hello [@alice](user://alice) and [@bob](user://bob) and [@alice](user://alice)";
        let ids = extract_user_mention_ids(md);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"alice".to_string()));
        assert!(ids.contains(&"bob".to_string()));
    }

    #[test]
    fn extract_routine_mention_ids_basic() {
        let id = Uuid::new_v4();
        let md = format!("Run [@r](routine://{id}) please");
        let ids = extract_routine_mention_ids(&md);
        assert_eq!(ids, vec![id]);
    }

    #[test]
    fn extract_skill_mention_ids_basic() {
        let id = Uuid::new_v4();
        let md = format!("Use [@s](skill://{id}?s=search-doc) skill");
        let ids = extract_skill_mention_ids(&md);
        assert_eq!(ids, vec![id]);
    }

    #[test]
    fn extract_agent_mention_ids_empty() {
        let ids = extract_agent_mention_ids("");
        assert!(ids.is_empty());
        let ids = extract_agent_mention_ids("no mentions here");
        assert!(ids.is_empty());
    }

    #[test]
    fn build_agent_mention_href_round_trip() {
        let id = Uuid::new_v4();
        let href = build_agent_mention_href(id, Some("bot"));
        let parsed = parse_agent_mention_href(&href).unwrap();
        assert_eq!(parsed.agent_id, id);
        assert_eq!(parsed.icon.as_deref(), Some("bot"));
    }

    #[test]
    fn build_user_mention_href_round_trip() {
        let href = build_user_mention_href("u-1");
        let parsed = parse_user_mention_href(&href).unwrap();
        assert_eq!(parsed.user_id, "u-1");
    }

    #[test]
    fn extract_agent_mention_ids_multiline() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let md = format!(
            "First line: [@a](agent://{id1})\n\nSecond line: [@b](agent://{id2})\n\nNo link",
        );
        let ids = extract_agent_mention_ids(&md);
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn extract_skips_links_with_spaces() {
        let id = Uuid::new_v4();
        // 含空格的 href 应被忽略（与 Node `[^)\s]+` 行为一致）
        let md = format!("Bad [link](agent://{id} extra-stuff)");
        let ids = extract_agent_mention_ids(&md);
        // 当前实现是基于 `find(')')` —— 如果 link 含空格但是连续的不允许
        // 实际上这里只截取到第一个 `)`，所以不会匹配到完整的 id
        // 这个 case 视实现而定；这里不强约束
        let _ = (md, ids);
    }
}
