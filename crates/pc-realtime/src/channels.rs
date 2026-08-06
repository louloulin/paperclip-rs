//! Round 252: Realtime channel namespace + 客户端订阅过滤。
//!
//! 背景：
//! - 现存所有事件都发到同一个 broadcast bus；客户端无法按业务 channel
//!   过滤订阅（如 `issue.*` / `heartbeat.*` / `watchdog.*`）。
//! - Node paperclip 端通过 event 前缀（`issue.created` / `heartbeat.tick`）天然支持。
//! - 本模块提供 `ChannelFilter` + `parse_channels` 让客户端把 "issue.*,heartbeat.*"
//!   转成 `Vec<ChannelFilter>`，再喂给 `FilteredSubscriber`。
//!
//! 设计：
//! - `ChannelFilter` 接受「前缀」或「精确」匹配：
//!   - `Prefix("issue.")` 匹配所有以 `issue.` 开头的事件。
//!   - `Exact("issue.created")` 仅匹配该事件名。
//! - `parse_channels(s)` 解析逗号分隔字符串，去除空白与空项。
//! - `matches(filter, event)` 是核心判定函数。
//! - 提供 `default_channels()`：常用 channel 一键开启（issue / heartbeat / watchdog / task_watchdog）。

/// 实时事件 channel 过滤规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelFilter {
    /// 前缀匹配：所有以 `prefix` 开头的事件。
    Prefix(String),
    /// 精确匹配：仅匹配该事件名。
    Exact(String),
}

impl ChannelFilter {
    /// 构造「所有事件」通配（`*`）。
    pub fn all() -> Self {
        ChannelFilter::Prefix(String::new())
    }

    /// 解析单个 channel 字符串：
    /// - `*` → all
    /// - `xxx.*` → Prefix("xxx.")
    /// - `xxx.yyy` → Exact("xxx.yyy")
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if s == "*" {
            return Some(Self::all());
        }
        if let Some(prefix) = s.strip_suffix(".*") {
            if prefix.is_empty() {
                return None;
            }
            return Some(ChannelFilter::Prefix(format!("{prefix}.")));
        }
        Some(ChannelFilter::Exact(s.to_string()))
    }
}

/// 把 `"issue.*,heartbeat.tick"` 解析为 `Vec<ChannelFilter>`。
pub fn parse_channels(s: &str) -> Vec<ChannelFilter> {
    s.split(',')
        .filter_map(ChannelFilter::parse)
        .collect()
}

/// 判定 `event` 是否匹配任一 filter（filter 列表 OR 语义）。
pub fn matches_any(filters: &[ChannelFilter], event: &str) -> bool {
    if filters.is_empty() {
        return true; // 空列表 = 全部放行
    }
    filters.iter().any(|f| match f {
        ChannelFilter::Prefix(p) => event.starts_with(p.as_str()),
        ChannelFilter::Exact(e) => event == e.as_str(),
    })
}

/// 拼装「接受所有业务 channel」的默认订阅串。
pub fn default_channels() -> &'static str {
    "issue.*,heartbeat.*,watchdog.*,task_watchdog.*,recovery.*,wakeup.*,comment.*,plan.*"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wildcard() {
        assert_eq!(ChannelFilter::parse("*"), Some(ChannelFilter::all()));
    }

    #[test]
    fn parse_prefix() {
        assert_eq!(
            ChannelFilter::parse("issue.*"),
            Some(ChannelFilter::Prefix("issue.".into()))
        );
    }

    #[test]
    fn parse_exact() {
        assert_eq!(
            ChannelFilter::parse("issue.created"),
            Some(ChannelFilter::Exact("issue.created".into()))
        );
    }

    #[test]
    fn parse_rejects_empty() {
        assert_eq!(ChannelFilter::parse(""), None);
        assert_eq!(ChannelFilter::parse("   "), None);
        assert_eq!(ChannelFilter::parse(".*"), None);
    }

    #[test]
    fn parse_channels_splits_and_trims() {
        let v = parse_channels(" issue.* , heartbeat.tick , ");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], ChannelFilter::Prefix("issue.".into()));
        assert_eq!(v[1], ChannelFilter::Exact("heartbeat.tick".into()));
    }

    #[test]
    fn matches_any_returns_true_when_filters_empty() {
        assert!(matches_any(&[], "anything"));
    }

    #[test]
    fn matches_any_uses_or_semantics() {
        // 第一个 filter 是 prefix（issue.*），第二个是 exact（heartbeat.tick）。
        let filters = parse_channels("issue.*,heartbeat.tick");
        assert!(matches_any(&filters, "issue.created"));     // prefix match
        assert!(matches_any(&filters, "issue.tick"));        // prefix match
        assert!(matches_any(&filters, "heartbeat.tick"));    // exact match
        assert!(!matches_any(&filters, "heartbeat.other"));  // exact 不匹配其他
        assert!(!matches_any(&filters, "watchdog.tick"));    // 任一 filter 都不匹配
    }

    #[test]
    fn matches_any_with_only_prefixes_uses_or_semantics() {
        let filters = parse_channels("issue.*,heartbeat.*");
        assert!(matches_any(&filters, "issue.created"));
        assert!(matches_any(&filters, "heartbeat.other"));   // 两个 prefix 任一即可
        assert!(!matches_any(&filters, "watchdog.tick"));
    }

    #[test]
    fn default_channels_lists_all_business_prefixes() {
        let s = default_channels();
        assert!(s.contains("issue.*"));
        assert!(s.contains("heartbeat.*"));
        assert!(s.contains("watchdog.*"));
    }
}
