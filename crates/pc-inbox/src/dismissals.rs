//! Inbox dismissal 纯逻辑 helper。
//!
//! 对应 Node `server/src/services/inbox-dismissals.ts`（75 行）1:1 复刻。
//! （原 `pc-inbox-dismissals` crate 已下沉到 `pc-inbox::dismissals`）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Inbox dismissal kind —— 与 Node `InboxDismissalKind` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InboxDismissalKind {
    Dismiss,
    Snooze,
}

impl InboxDismissalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dismiss => "dismiss",
            Self::Snooze => "snooze",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "dismiss" => Some(Self::Dismiss),
            "snooze" => Some(Self::Snooze),
            _ => None,
        }
    }
}

/// 计算最终 snoozedUntil 值。
///
/// 与 Node 1:1 对齐：
/// ```ts
/// const snoozedUntil = input.kind === "snooze" ? input.snoozedUntil ?? null : null;
/// ```
pub fn compute_snoozed_until(
    kind: InboxDismissalKind,
    snoozed_until: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match kind {
        InboxDismissalKind::Snooze => snoozed_until,
        InboxDismissalKind::Dismiss => None,
    }
}

/// Inbox dismissal row 形状（最小子集）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxDismissal {
    pub id: String,
    pub company_id: String,
    pub user_id: String,
    pub item_key: String,
    pub kind: InboxDismissalKind,
    pub dismissed_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()
    }

    #[test]
    fn r708_kind_round_trip() {
        for k in [InboxDismissalKind::Dismiss, InboxDismissalKind::Snooze] {
            assert_eq!(InboxDismissalKind::from_str(k.as_str()), Some(k));
        }
        assert_eq!(InboxDismissalKind::from_str("unknown"), None);
    }

    #[test]
    fn r708_compute_snooze_keeps_snoozed_until() {
        let r = compute_snoozed_until(InboxDismissalKind::Snooze, Some(ts()));
        assert_eq!(r, Some(ts()));
    }

    #[test]
    fn r708_compute_snooze_none() {
        // 即使输入 None，只要 kind 是 snooze 也透传 None
        let r = compute_snoozed_until(InboxDismissalKind::Snooze, None);
        assert!(r.is_none());
    }

    #[test]
    fn r708_compute_dismiss_always_none() {
        // 即使 snoozedUntil 有值，kind=dismiss 也返回 None
        let r = compute_snoozed_until(InboxDismissalKind::Dismiss, Some(ts()));
        assert!(r.is_none());
    }

    #[test]
    fn r708_compute_dismiss_with_none() {
        let r = compute_snoozed_until(InboxDismissalKind::Dismiss, None);
        assert!(r.is_none());
    }

    #[test]
    fn r708_serialization_camel_case() {
        let d = InboxDismissal {
            id: "d1".to_string(),
            company_id: "co1".to_string(),
            user_id: "u1".to_string(),
            item_key: "approval:123".to_string(),
            kind: InboxDismissalKind::Snooze,
            dismissed_at: ts(),
            snoozed_until: Some(ts()),
            updated_at: ts(),
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["companyId"], "co1");
        assert_eq!(v["userId"], "u1");
        assert_eq!(v["itemKey"], "approval:123");
        assert_eq!(v["kind"], "snooze");
        assert!(v.get("snoozedUntil").is_some());
    }

    #[test]
    fn r708_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InboxDismissal>();
    }
}
