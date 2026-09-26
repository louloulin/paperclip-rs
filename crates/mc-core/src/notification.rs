//! 通知偏好的**分组词表与形状**（上游 `internal/handler/notification_preference.go`，172 行；
//! `docs/62` §4.1 的 `M9-5`）。
//!
//! # 词表是**唯一判据**（逐字取自上游 `validNotifGroups` / `validNotifValues`）
//!
//! 7 个分组 × 2 个取值。分组不在词表里 ⇒ `400 invalid preference group: {k}`；
//! 取值不在词表里 ⇒ `400 invalid preference value: {v}`（上游逐字，含错误文本）。
//!
//! ⚠️ `system_notifications` 是一个**投递开关**（原生 OS 通知横幅），不是 inbox 事件分组
//! —— 上游注释逐字解释了为什么它仍在这个 map 里：「it shares the same preferences map so a
//! single endpoint covers all user notification preferences」。别把它当"第 8 个分组"删掉。
//!
//! # 本模块**只有** wire 形状（**没有**表）
//!
//! 偏好落在上游 `notification_preference` 表（`M9-5` 的 `crates/mc-repos/src/notification_preference.rs`
//! 负责读写，`preferences` 是一列 JSONB）。本模块只定「什么 key 合法、响应长什么样」。
//!
//! # 双形态键（`docs/62` §1.4 实测 `dual-form required: 3`）
//!
//! `/api/notification-preferences` 的 **3 个方法**（GET / PATCH / PUT）在上游都是
//! `Route(…) + Get/Patch/Put("/")` 形态 ⇒ **带尾斜杠与不带尾斜杠两种都必须注册**。
//! 本波只有这 3 条需要补形态（`docs/fixtures/upstream-routes.tsv` 里它们就以
//! `/api/notification-preferences/` 出现）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 合法的通知分组（上游 `validNotifGroups` 的 7 个 key，**字典序**）。
pub const NOTIFICATION_GROUPS: [&str; 7] = [
    "agent_activity",
    "assignments",
    "comments",
    "mentions",
    "status_changes",
    "system_notifications",
    "updates",
];

/// 合法取值（上游 `validNotifValues`）。
pub const NOTIFICATION_VALUES: [&str; 2] = ["all", "muted"];

/// 取值：全部通知。
pub const NOTIFICATION_VALUE_ALL: &str = "all";
/// 取值：静音。
pub const NOTIFICATION_VALUE_MUTED: &str = "muted";

/// 分组是否合法。
#[must_use]
pub fn is_valid_notification_group(group: &str) -> bool {
    NOTIFICATION_GROUPS.contains(&group)
}

/// 取值是否合法。
#[must_use]
pub fn is_valid_notification_value(value: &str) -> bool {
    NOTIFICATION_VALUES.contains(&value)
}

/// 校验一对 `(group, value)`。
///
/// 返回的 `Err` 文本**逐字**对齐上游（含 `invalid preference group: ` / `invalid preference
/// value: ` 前缀）—— 因为那两个串是客户端用来定位字段的。
///
/// # Errors
///
/// 分组或取值不在词表里。
pub fn validate_preference(group: &str, value: &str) -> Result<(), String> {
    if !is_valid_notification_group(group) {
        return Err(format!("invalid preference group: {group}"));
    }
    if !is_valid_notification_value(value) {
        return Err(format!("invalid preference value: {value}"));
    }
    Ok(())
}

/// `GET /api/notification-preferences/` 的响应（上游 `map[string]any{…}`）。
///
/// ⚠️ 未设置过偏好的用户拿到 **`preferences: {}`**（上游逐字：`writeJSON(w,
/// http.StatusOK, map[string]any{"workspace_id": workspaceID, "preferences": map[string]any{}})`）
/// —— 是**空对象**，不是全 `all` 的默认表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPreferencesResponse {
    /// 工作区（回显中间件解析出来的那个）。
    pub workspace_id: String,
    /// 已设置的分组 → 取值（只含**设过**的；`BTreeMap` 让输出确定性）。
    pub preferences: BTreeMap<String, String>,
}

/// `PATCH` / `PUT /api/notification-preferences/` 的请求体（上游 `req.Preferences`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateNotificationPreferencesRequest {
    /// 分组 → 取值。
    pub preferences: BTreeMap<String, String>,
}

impl UpdateNotificationPreferencesRequest {
    /// 逐对校验（第一个不合法的就报错；`BTreeMap` ⇒ 顺序确定，报错可复现）。
    ///
    /// # Errors
    ///
    /// 分组或取值不在词表里。
    pub fn validate(&self) -> Result<(), String> {
        for (group, value) in &self.preferences {
            validate_preference(group, value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_is_the_upstream_seven_by_two() {
        assert_eq!(NOTIFICATION_GROUPS.len(), 7);
        assert_eq!(NOTIFICATION_VALUES.len(), 2);
        for group in NOTIFICATION_GROUPS {
            assert!(is_valid_notification_group(group), "{group}");
        }
        // `system_notifications` 是投递开关，但**在**词表里（见模块头）。
        assert!(is_valid_notification_group("system_notifications"));
        // 词表外的分组被判非法（上游 400）。
        for unknown in [
            "",
            "System_Notifications",
            "inbox",
            "digest",
            "unsubscribed",
        ] {
            assert!(!is_valid_notification_group(unknown), "{unknown}");
        }
        assert!(is_valid_notification_value("all"));
        assert!(is_valid_notification_value("muted"));
        for unknown in ["", "ALL", "none", "off", "true"] {
            assert!(!is_valid_notification_value(unknown), "{unknown}");
        }
    }

    #[test]
    fn validation_errors_match_upstream_text() {
        assert_eq!(validate_preference("comments", "all"), Ok(()));
        assert_eq!(
            validate_preference("nope", "all"),
            Err("invalid preference group: nope".to_string())
        );
        assert_eq!(
            validate_preference("comments", "loud"),
            Err("invalid preference value: loud".to_string())
        );
        // 分组错**先于**取值错报出（上游同一个循环里的顺序）。
        assert_eq!(
            validate_preference("nope", "loud"),
            Err("invalid preference group: nope".to_string())
        );
    }

    #[test]
    fn empty_preferences_are_an_empty_object_not_a_default_table() {
        let response = NotificationPreferencesResponse {
            workspace_id: "ws".into(),
            preferences: BTreeMap::new(),
        };
        assert_eq!(
            serde_json::to_value(&response).expect("serialize"),
            serde_json::json!({"workspace_id": "ws", "preferences": {}})
        );
    }

    #[test]
    fn update_request_validates_every_pair_deterministically() {
        let mut preferences = BTreeMap::new();
        preferences.insert("assignments".to_string(), "all".to_string());
        preferences.insert("mentions".to_string(), "muted".to_string());
        let request = UpdateNotificationPreferencesRequest { preferences };
        assert_eq!(request.validate(), Ok(()));

        let mut bad = BTreeMap::new();
        bad.insert("assignments".to_string(), "loud".to_string());
        bad.insert("mentions".to_string(), "all".to_string());
        let request = UpdateNotificationPreferencesRequest { preferences: bad };
        assert_eq!(
            request.validate(),
            Err("invalid preference value: loud".to_string())
        );

        // 缺 `preferences` 字段 ⇒ 反序列化失败（调用方 400），不是静默空更新。
        assert!(serde_json::from_str::<UpdateNotificationPreferencesRequest>("{}").is_err());
    }
}
