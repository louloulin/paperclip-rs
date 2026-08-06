//! Round 252 + 254: Realtime channel namespace + 客户端订阅过滤。
//!
//! 背景：
//! - R252：所有事件都发到同一个 broadcast bus；客户端无法按业务 channel
//!   过滤订阅（如 `issue.*` / `heartbeat.*` / `watchdog.*`）。
//! - R254：除了 event name 前缀外，还需要按 `resource_id` 过滤（例如只订阅某个
//!   `issue_id` 或 `watchdog_id` 的事件），避免客户端收到无关事件。
//!
//! 设计：
//! - `ChannelFilter` 接受三种匹配：
//!   - `Prefix(String)` 匹配所有以 `prefix` 开头的事件名。
//!   - `Exact(String)` 仅匹配该事件名。
//!   - `ResourceId { id, resource }` 匹配 `LiveEvent.resource_id == id` 且
//!     `LiveEvent.resource == resource`（resource 是 Optional，None 表示任意 resource 类型）。
//! - `parse_channels(s)` 解析逗号分隔字符串，去除空白与空项。
//! - `matches_any(filters, event)` 接受 `&LiveEvent`，同时检查 event name 与 resource_id。
//! - 提供 `default_channels()`：常用 channel 一键开启（issue / heartbeat / watchdog / task_watchdog）。

use uuid::Uuid;

/// 实时事件 channel 过滤规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelFilter {
    /// 前缀匹配：所有以 `prefix` 开头的事件。
    Prefix(String),
    /// 精确匹配：仅匹配该事件名。
    Exact(String),
    /// 资源 ID 匹配：仅匹配 `LiveEvent.resource_id == id` 的事件。
    /// - `resource`：可选的 resource 类型过滤（如 `"issue"` / `"issue_watchdog"`）；
    ///   传 `None` 表示匹配任意 resource 类型。
    ResourceId { id: Uuid, resource: Option<String> },
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
    /// - `issue_id:<uuid>` → ResourceId { id, resource: Some("issue") }
    /// - `watchdog_id:<uuid>` → ResourceId { id, resource: Some("issue_watchdog") }
    /// - `agent_id:<uuid>` → ResourceId { id, resource: Some("agent") }
    /// - `resource_id:<uuid>` → ResourceId { id, resource: None }（任意 resource 类型）
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
        // R254: resource_id 过滤器（按 UUID 前缀）
        if let Some((kind, uuid_str)) = s.split_once(':') {
            if let Ok(id) = Uuid::parse_str(uuid_str.trim()) {
                let resource = match kind.trim() {
                    "issue_id" => Some("issue".to_string()),
                    "watchdog_id" => Some("issue_watchdog".to_string()),
                    "agent_id" => Some("agent".to_string()),
                    "run_id" => Some("heartbeat_run".to_string()),
                    "resource_id" => None,
                    _ => return Some(ChannelFilter::Exact(s.to_string())),
                };
                return Some(ChannelFilter::ResourceId { id, resource });
            }
        }
        Some(ChannelFilter::Exact(s.to_string()))
    }
}

/// 把 `"issue.*,heartbeat.tick"` 解析为 `Vec<ChannelFilter>`。
pub fn parse_channels(s: &str) -> Vec<ChannelFilter> {
    s.split(',').filter_map(ChannelFilter::parse).collect()
}

/// 判定 LiveEvent 是否匹配任一 filter（filter 列表 OR 语义）。
///
/// 同时检查：
/// - event name 是否匹配 Prefix / Exact filter
/// - resource_id + resource 是否匹配 ResourceId filter
///
/// R252 兼容：旧版 `matches_any(&filters, &event.event)` 仍可工作（仅检查 event name）。
/// R254 新增：`matches_any(&filters, &live_event)` 同时检查 event name 与 resource_id。
pub fn matches_any(filters: &[ChannelFilter], event: &crate::LiveEvent) -> bool {
    if filters.is_empty() {
        return true; // 空列表 = 全部放行
    }
    filters.iter().any(|f| match f {
        ChannelFilter::Prefix(p) => event.event.starts_with(p.as_str()),
        ChannelFilter::Exact(e) => event.event == e.as_str(),
        ChannelFilter::ResourceId { id, resource } => {
            if event.resource_id != *id {
                return false;
            }
            match resource {
                None => true,
                Some(r) => event.resource == *r,
            }
        }
    })
}

/// R252 兼容便捷函数：仅检查 event name（不检查 resource_id）。
pub fn matches_any_event_name(filters: &[ChannelFilter], event_name: &str) -> bool {
    if filters.is_empty() {
        return true;
    }
    filters.iter().any(|f| match f {
        ChannelFilter::Prefix(p) => event_name.starts_with(p.as_str()),
        ChannelFilter::Exact(e) => event_name == e.as_str(),
        ChannelFilter::ResourceId { .. } => false, // ResourceId 永远不在 event_name-only 检查中匹配
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
        use crate::LiveEvent;
        let evt = LiveEvent::new("anything", "x", Uuid::new_v4());
        assert!(matches_any(&[], &evt));
    }

    #[test]
    fn matches_any_uses_or_semantics() {
        use crate::LiveEvent;
        // 第一个 filter 是 prefix（issue.*），第二个是 exact（heartbeat.tick）。
        let filters = parse_channels("issue.*,heartbeat.tick");
        assert!(matches_any(
            &filters,
            &LiveEvent::new("issue.created", "issue", Uuid::new_v4())
        ));
        assert!(matches_any(
            &filters,
            &LiveEvent::new("issue.tick", "issue", Uuid::new_v4())
        ));
        assert!(matches_any(
            &filters,
            &LiveEvent::new("heartbeat.tick", "heartbeat", Uuid::new_v4())
        ));
        assert!(!matches_any(
            &filters,
            &LiveEvent::new("heartbeat.other", "heartbeat", Uuid::new_v4())
        ));
        assert!(!matches_any(
            &filters,
            &LiveEvent::new("watchdog.tick", "watchdog", Uuid::new_v4())
        ));
    }

    #[test]
    fn matches_any_with_only_prefixes_uses_or_semantics() {
        use crate::LiveEvent;
        let filters = parse_channels("issue.*,heartbeat.*");
        assert!(matches_any(
            &filters,
            &LiveEvent::new("issue.created", "issue", Uuid::new_v4())
        ));
        assert!(matches_any(
            &filters,
            &LiveEvent::new("heartbeat.other", "heartbeat", Uuid::new_v4())
        ));
        assert!(!matches_any(
            &filters,
            &LiveEvent::new("watchdog.tick", "watchdog", Uuid::new_v4())
        ));
    }

    #[test]
    fn default_channels_lists_all_business_prefixes() {
        let s = default_channels();
        assert!(s.contains("issue.*"));
        assert!(s.contains("heartbeat.*"));
        assert!(s.contains("watchdog.*"));
    }

    /// R254: ChannelFilter::parse 支持 `issue_id:<uuid>` 形式。
    #[test]
    fn parse_issue_id_resource_filter() {
        let id = Uuid::new_v4();
        let filter = ChannelFilter::parse(&format!("issue_id:{id}")).expect("must parse");
        assert_eq!(
            filter,
            ChannelFilter::ResourceId {
                id,
                resource: Some("issue".to_string()),
            }
        );
    }

    /// R254: ChannelFilter::parse 支持 `watchdog_id:<uuid>` 形式。
    #[test]
    fn parse_watchdog_id_resource_filter() {
        let id = Uuid::new_v4();
        let filter = ChannelFilter::parse(&format!("watchdog_id:{id}")).expect("must parse");
        assert_eq!(
            filter,
            ChannelFilter::ResourceId {
                id,
                resource: Some("issue_watchdog".to_string()),
            }
        );
    }

    /// R254: ChannelFilter::parse 支持 `agent_id:<uuid>` / `run_id:<uuid>` / `resource_id:<uuid>` 形式。
    #[test]
    fn parse_agent_run_resource_id_filters() {
        let id = Uuid::new_v4();
        let agent_filter = ChannelFilter::parse(&format!("agent_id:{id}")).unwrap();
        assert_eq!(
            agent_filter,
            ChannelFilter::ResourceId {
                id,
                resource: Some("agent".to_string()),
            }
        );
        let run_filter = ChannelFilter::parse(&format!("run_id:{id}")).unwrap();
        assert_eq!(
            run_filter,
            ChannelFilter::ResourceId {
                id,
                resource: Some("heartbeat_run".to_string()),
            }
        );
        let generic_filter = ChannelFilter::parse(&format!("resource_id:{id}")).unwrap();
        assert_eq!(
            generic_filter,
            ChannelFilter::ResourceId { id, resource: None }
        );
    }

    /// R254: ChannelFilter::parse 无效 UUID 时 fallback 到 Exact。
    #[test]
    fn parse_invalid_uuid_falls_back_to_exact() {
        let filter = ChannelFilter::parse("issue_id:not-a-uuid").unwrap();
        assert_eq!(
            filter,
            ChannelFilter::Exact("issue_id:not-a-uuid".to_string())
        );
    }

    /// R254: matches_any 在 LiveEvent 上同时检查 event name + resource_id。
    #[test]
    fn matches_any_with_live_event_checks_resource_id() {
        use crate::LiveEvent;
        let target_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let evt_match = LiveEvent::new("issue.created", "issue", target_id);
        let evt_other = LiveEvent::new("issue.created", "issue", other_id);
        let evt_wrong_resource = LiveEvent::new("watchdog.tick", "issue_watchdog", target_id);
        let filters = vec![ChannelFilter::ResourceId {
            id: target_id,
            resource: Some("issue".to_string()),
        }];
        assert!(matches_any(&filters, &evt_match));
        assert!(!matches_any(&filters, &evt_other)); // resource_id 不同
        assert!(!matches_any(&filters, &evt_wrong_resource)); // resource 类型不同
    }

    /// R254: matches_any ResourceId::None 匹配任意 resource 类型。
    #[test]
    fn matches_any_resource_id_none_matches_any_resource_type() {
        use crate::LiveEvent;
        let id = Uuid::new_v4();
        let evt = LiveEvent::new("something.happened", "issue_watchdog", id);
        let filters = vec![ChannelFilter::ResourceId { id, resource: None }];
        assert!(matches_any(&filters, &evt));
    }

    /// R254: matches_any 混合 Prefix + ResourceId filter（OR 语义）。
    #[test]
    fn matches_any_combines_prefix_and_resource_id() {
        use crate::LiveEvent;
        let id = Uuid::new_v4();
        let evt_issue = LiveEvent::new("issue.created", "issue", Uuid::new_v4());
        let evt_target_resource = LiveEvent::new("watchdog.tick", "issue_watchdog", id);
        let evt_unrelated = LiveEvent::new("heartbeat.tick", "heartbeat_run", Uuid::new_v4());
        let filters = vec![
            ChannelFilter::Prefix("issue.".to_string()),
            ChannelFilter::ResourceId {
                id,
                resource: Some("issue_watchdog".to_string()),
            },
        ];
        assert!(matches_any(&filters, &evt_issue));
        assert!(matches_any(&filters, &evt_target_resource));
        assert!(!matches_any(&filters, &evt_unrelated));
    }

    /// R254: matches_any_event_name 兼容函数不匹配 ResourceId filter。
    #[test]
    fn matches_any_event_name_ignores_resource_id_filters() {
        let id = Uuid::new_v4();
        let filters = vec![ChannelFilter::ResourceId {
            id,
            resource: Some("issue".to_string()),
        }];
        // event_name-only 模式下，ResourceId 永远不匹配
        assert!(!matches_any_event_name(&filters, "issue.created"));
        // 但 Prefix filter 仍能匹配
        let filters2 = vec![ChannelFilter::Prefix("issue.".to_string())];
        assert!(matches_any_event_name(&filters2, "issue.created"));
    }
}
