//! chat **生成面（quick-actions）**的纯领域规则（M4-4 / LUM-1475）：`regenerate` 的门与
//! 拒绝形状、以及「哪一轮回复才能长建议」的判据。
//!
//! 本模块**不做 I/O**：SQL 在 `mc_repos::chat_quick_action`，HTTP 在
//! `mc_http::routes::chat::task`。每条规则对回上游原文：
//!
//! | 规则 | 上游出处 |
//! | --- | --- |
//! | 只有 `message_kind == 'message'` **且** `task_id` 非空的 assistant 行才可刷新 | `task.go:2184` |
//! | 最近一轮 assistant 行不是可刷新轮 → 409 `no assistant reply to refresh yet` | `chat.go:1141` |
//! | 客户端锚定的消息已不是最新一轮 → 409 `a newer reply arrived — refresh it instead` | `chat.go:1136` |
//! | 会话已有在飞任务，或已有一次刷新在跑 → 409 `still working — try refreshing in a moment` | `chat.go:1140` |
//! | 部署没有 LLM 层 → 403 `suggestions_not_available`（`writeFeatureDisabled`） | `chat.go:1143` + `handler.go:578` |
//! | 成功是 **202** `{message_id}`（生成被 detach，结果走 ws `chat:quick_actions`） | `chat.go:1151` |
//!
//! ## 本片的两处刻意偏离（登记在 `docs/45`）
//!
//! **D-1（信封形状）**：上游 `writeFeatureDisabled` 写**扁平** `{"error": msg, "code": code}`
//!（`handler.go:578`）；本仓 `daemon/tasks.rs:567` 的 `plugin_api_disabled` 先例把同类响应收进
//! **嵌套**信封 `{"error":{"code","message"}}`。本片照后者：状态码（403）与机器可读的
//! `code`（`suggestions_not_available`）都不变，客户端仍按 `code` 分支 ⇒ 只有外层结构不同。
//!
//! **D-2（可达性）**：可用性检查是上游 service 的**第一句**（`task.go:2170`），而本部署没有
//! quick-actions provider（生成侧要走 daemon 的 suggest 往返，属 M6/M7）⇒ 它**恒失败**，
//! 于是后三个 409 与 202 成功在本仓的路由上**不可达**。本片不把它们塞进 handler 造一段
//! 跑不到的实现，而是分两处真做、由后续波次接起来：纯判据在本模块
//!（[`regenerable_target`] / [`is_regenerable_turn`] / [`RegenerateRefusal`]），落库判定在
//! `mc_repos::chat_quick_action` 的两条**真 SQL**（`#[ignore]` 的真库测试钉住）。
//! 生成本身（provider 调用 + `chat_message.quick_actions` 落库 + `chat:quick_actions` 广播）
//! 一并登记为后续波次的 `known_gap`（本片**不**造假实现）。

/// 可刷新轮的消息种类（`protocol.ChatMessageKindMessage`）。
pub const KIND_MESSAGE: &str = "message";

/// 上游 `ErrChatQuickActions*` 的四个拒绝形状（含状态码与文案）。
///
/// `Unavailable` 走的是 `writeFeatureDisabled`（**扁平** `{"error","code"}`，403）而不是
/// 普通错误信封 —— 上游注释写得很清楚：能力被关掉是**不可重试**的，
/// 用 503 会诱导客户端重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegenerateRefusal {
    /// 该会话还没有可刷新的 assistant 轮（没有回复 / 或不是普通消息轮）。
    #[error("chat quick actions: no assistant turn to regenerate")]
    NoTurn,
    /// 客户端想刷新的那一轮已经不是最新（有新回复落库）。
    #[error("chat quick actions: refresh target is stale")]
    Stale,
    /// 会话已有在飞任务（或已有一次刷新在跑）。
    #[error("chat quick actions: session busy")]
    Busy,
    /// 部署没有 LLM 层。
    #[error("chat quick actions: llm layer not configured")]
    Unavailable,
}

impl RegenerateRefusal {
    /// HTTP 状态码（上游 `RegenerateChatQuickActions` 的 `switch`）。
    pub fn status(self) -> u16 {
        match self {
            Self::NoTurn | Self::Stale | Self::Busy => 409,
            // `writeFeatureDisabled` → 403（**不是** 503）。
            Self::Unavailable => 403,
        }
    }

    /// 线上文案。
    pub fn message(self) -> &'static str {
        match self {
            Self::NoTurn => "no assistant reply to refresh yet",
            Self::Stale => "a newer reply arrived — refresh it instead",
            Self::Busy => "still working — try refreshing in a moment",
            Self::Unavailable => "suggestions are not available on this deployment",
        }
    }

    /// `writeFeatureDisabled` 的机器可读 `code`；只有 `Unavailable` 有。
    pub fn error_code(self) -> Option<&'static str> {
        match self {
            Self::Unavailable => Some("suggestions_not_available"),
            Self::NoTurn | Self::Stale | Self::Busy => None,
        }
    }
}

/// 上游 `task.go:2184` 的判据：这一轮 assistant 行能不能当刷新目标。
///
/// 两个条件缺一不可：`message_kind == 'message'`（`no_response` / 失败轮没有可长建议的内容），
/// `task_id` 非空（生成器要按 turn 的 task id 去写 `chat_message.quick_actions`）。
pub fn is_regenerable_turn(message_kind: &str, has_task_id: bool) -> bool {
    message_kind == KIND_MESSAGE && has_task_id
}

/// 上游 service 里「最近一轮」判据的**判定输入**：
/// `GetLatestAssistantChatMessageForSession` 的行（SQL 已保证 `role='assistant' AND
/// task_id IS NOT NULL`，故这里只需 `message_kind` 与 `id`）。
///
/// 抽成一层是为了让 `None`（一行都没有）与 `Some(不可刷新轮)` 都落到同一个 409。
pub fn regenerable_target(
    latest: Option<(&str, uuid::Uuid)>,
    expected_message_id: uuid::Uuid,
) -> Result<uuid::Uuid, RegenerateRefusal> {
    let Some((kind, id)) = latest else {
        return Err(RegenerateRefusal::NoTurn);
    };
    if !is_regenerable_turn(kind, true) {
        return Err(RegenerateRefusal::NoTurn);
    }
    if id != expected_message_id {
        return Err(RegenerateRefusal::Stale);
    }
    Ok(id)
}

/// 上游成功状态码：**202**（生成 detach，结果走 ws `chat:quick_actions`）。
pub const REGENERATE_ACCEPTED: u16 = 202;

/// 自动生成与手工刷新的共用开关名（上游 `chatQuickActionsEnabled` 的 per-device 开关只拦
/// 自动档；手工刷新**刻意**忽略它 —— `task.go:2168` 注释）。
pub const QUICK_ACTIONS_TOGGLE_BYPASSED_BY_MANUAL: bool = true;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_shapes_match_upstream_statuses_and_wording() {
        assert_eq!(RegenerateRefusal::NoTurn.status(), 409);
        assert_eq!(RegenerateRefusal::Stale.status(), 409);
        assert_eq!(RegenerateRefusal::Busy.status(), 409);
        // 能力被关掉 → 403（不是 503：503 会诱导重试）。
        assert_eq!(RegenerateRefusal::Unavailable.status(), 403);
        assert_eq!(
            RegenerateRefusal::NoTurn.message(),
            "no assistant reply to refresh yet"
        );
        assert_eq!(
            RegenerateRefusal::Stale.message(),
            "a newer reply arrived — refresh it instead"
        );
        assert_eq!(
            RegenerateRefusal::Busy.message(),
            "still working — try refreshing in a moment"
        );
        assert_eq!(
            RegenerateRefusal::Unavailable.message(),
            "suggestions are not available on this deployment"
        );
        assert_eq!(RegenerateRefusal::NoTurn.error_code(), None);
        assert_eq!(
            RegenerateRefusal::Unavailable.error_code(),
            Some("suggestions_not_available")
        );
    }

    #[test]
    fn only_an_ordinary_message_turn_can_seed_suggestions() {
        assert!(is_regenerable_turn("message", true));
        // `no_response` / 失败轮没有可长建议的内容。
        assert!(!is_regenerable_turn("no_response", true));
        // 生成器要按 turn 的 task id 写回，没 task 就没锚点。
        assert!(!is_regenerable_turn("message", false));
    }

    #[test]
    fn regenerable_target_distinguishes_no_turn_from_stale() {
        let expected = uuid::Uuid::from_u128(9);
        assert_eq!(
            regenerable_target(None, expected).unwrap_err(),
            RegenerateRefusal::NoTurn
        );
        assert_eq!(
            regenerable_target(Some(("no_response", expected)), expected).unwrap_err(),
            RegenerateRefusal::NoTurn
        );
        assert_eq!(
            regenerable_target(Some(("message", uuid::Uuid::from_u128(1))), expected).unwrap_err(),
            RegenerateRefusal::Stale
        );
        assert_eq!(
            regenerable_target(Some(("message", expected)), expected).unwrap(),
            expected
        );
    }

    #[test]
    fn accepted_status_is_202() {
        assert_eq!(REGENERATE_ACCEPTED, 202);
        // 同 `task.rs`：绕开常量断言口径（门 ③）。
        let bypassed: bool = QUICK_ACTIONS_TOGGLE_BYPASSED_BY_MANUAL;
        assert!(bypassed);
    }
}
