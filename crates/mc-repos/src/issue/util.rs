use mc_core::issue::{AssigneeType, IssueOrigin};

// ---------------------------------------------------------------------------
// 工具函数
// ---------------------------------------------------------------------------

/// `workspace.slug` → issue identifier 前缀。
///
/// 上游把前缀存在 workspace 上（`getIssuePrefix`）；本仓 0001 没有该列，因此从
/// slug 派生：取字母数字、转大写、截断 8 位，空则 `ISS`。
pub fn issue_prefix_from_slug(slug: &str) -> String {
    let prefix: String = slug
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase();
    if prefix.is_empty() {
        "ISS".to_string()
    } else {
        prefix
    }
}

/// 拖拽排序的新 position。
///
/// `before` = 前一个锚点（更小 position），`after` = 后一个锚点（更大 position），
/// 与上游 `issueMovePosition` 语义一致；两者都 `None` 时保持原位。
/// 锚点顺序错乱 / 间距过小无法放值时返回 `None`（handler → 409）。
pub fn derive_move_position(before: Option<f64>, after: Option<f64>, current: f64) -> Option<f64> {
    let position = match (before, after) {
        (Some(b), Some(a)) => {
            if !matches!(b.partial_cmp(&a), Some(std::cmp::Ordering::Less)) {
                return None; // 锚点顺序错乱或不可比（NaN）
            }
            let mid = b + (a - b) / 2.0;
            if !mid.is_finite() || mid <= b || mid >= a {
                return None; // 间距太小，无法二分
            }
            mid
        }
        (Some(b), None) => b + 1.0,
        (None, Some(a)) => a - 1.0,
        (None, None) => current,
    };
    if position.is_finite() {
        Some(position)
    } else {
        None
    }
}

/// `assignee_type` 字符串 → 枚举。
pub fn parse_assignee_type(raw: &str) -> Option<AssigneeType> {
    match raw {
        "user" => Some(AssigneeType::User),
        "agent" => Some(AssigneeType::Agent),
        "squad" => Some(AssigneeType::Squad),
        "autopilot" => Some(AssigneeType::Autopilot),
        _ => None,
    }
}

/// origin 字符串 → 枚举。
pub fn parse_issue_origin(raw: &str) -> Option<IssueOrigin> {
    match raw {
        "manual" => Some(IssueOrigin::Manual),
        "quick_create" => Some(IssueOrigin::QuickCreate),
        "slack_chat" => Some(IssueOrigin::SlackChat),
        "lark_chat" => Some(IssueOrigin::LarkChat),
        "dingtalk_chat" => Some(IssueOrigin::DingTalkChat),
        "wecom_chat" => Some(IssueOrigin::WeComChat),
        "telegram_chat" => Some(IssueOrigin::TelegramChat),
        "agent_create" => Some(IssueOrigin::AgentCreate),
        "autopilot_run" => Some(IssueOrigin::AutopilotRun),
        "squad_handoff" => Some(IssueOrigin::SquadHandoff),
        _ => None,
    }
}

/// 逗号分隔参数 → 去空去重的 `Vec`（上游 `splitCommaParam`）。
pub fn split_comma_param(raw: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}
