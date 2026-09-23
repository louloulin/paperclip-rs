//! 快捷栏（pinned agents）的纯领域规则（M4-3 / LUM-1474）。
//!
//! 本模块**不做 I/O**：SQL 在 `mc_repos::chat_pinned_agent`，HTTP 在
//! `mc_http::routes::chat::bar`。规则逐条对回上游：
//!
//! | 规则 | 上游出处 |
//! | --- | --- |
//! | 每人最多 5 个置顶 agent | `chat_pinned_agent.go:34 maxChatPinnedAgents` |
//! | 已置顶的 agent 重复 pin → **幂等**（不算第 6 个、不改位置） | `PinChatAgent` 的 `already` 分支 + `ON CONFLICT` |
//! | 位置从 `max(position) + 1` 起（空栏 `COALESCE(MAX(position), 0) + 1` ⇒ **第一条是 `1.0`**） | `chat.sql` 的 `GetMaxChatPinnedAgentPosition` + `CreateChatPinnedAgent` |
//! | 列表按 `position ASC, created_at ASC` | `chat.sql` 的 `ListChatPinnedAgents` |
//! | unpin **幂等**：没这一行也回 204 | `UnpinChatAgent`（不查 rows affected） |

use uuid::Uuid;

/// 快捷栏上限（上游 `chat_pinned_agent.go:34` 的 `maxChatPinnedAgents`）。
pub const MAX_PINNED_AGENTS: usize = 5;

/// pin 被拒的原因。
///
/// ⚠️ 状态码是 **400**（上游 `chat_pinned_agent.go:120` 的 `writeError(w, http.StatusBadRequest,
/// "pinned agent limit reached")`），不是 409 —— 本仓早年把 `PinError` 写成 `too many pinned
/// agents` / 409 是**错的**，以本文件为准。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// 槽位已满且目标不在栏里 → 400 `pinned agent limit reached`。
    #[error("pinned agent limit reached")]
    LimitReached,
}

/// `PinChatAgent` 的容量门：**先判幂等**，再判上限。
///
/// 上游顺序不能反：已置顶的 agent 即使栏满也允许重放（否则前端重试会拿到假 409）。
pub fn check_capacity(existing_len: usize, already_pinned: bool) -> Result<(), PinError> {
    if already_pinned || existing_len < MAX_PINNED_AGENTS {
        Ok(())
    } else {
        Err(PinError::LimitReached)
    }
}

/// 当前栏里是否已有该 agent（幂等判定的唯一输入）。
pub fn already_pinned(existing_agent_ids: &[Uuid], agent_id: Uuid) -> bool {
    existing_agent_ids.contains(&agent_id)
}

/// 新 pin 的位置：`COALESCE(MAX(position), 0) + 1`。
///
/// 空栏 ⇒ `1.0`（上游 `GetMaxChatPinnedAgentPosition` 的兜底值是 `0` 而**不是 `-1`**；
/// 已 pin 的第一条 position 就是 `1.0`，别改成 0 起算 —— 那会让存量数据与新建数据的位置语义混用）。
pub fn next_position(max_position: Option<f64>) -> f64 {
    max_position.unwrap_or(0.0) + 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixth_distinct_agent_is_rejected_but_replays_are_not() {
        assert!(check_capacity(0, false).is_ok());
        assert!(check_capacity(MAX_PINNED_AGENTS - 1, false).is_ok());
        assert_eq!(
            check_capacity(MAX_PINNED_AGENTS, false).unwrap_err(),
            PinError::LimitReached
        );
        // 幂等重放：栏满也放行（前端重试、并发双写都不会假 409）。
        assert!(check_capacity(MAX_PINNED_AGENTS, true).is_ok());
    }

    #[test]
    fn already_pinned_matches_upstream_linear_scan() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        assert!(already_pinned(&[a, b], a));
        assert!(!already_pinned(&[a, b], Uuid::from_u128(3)));
        assert!(!already_pinned(&[], a));
    }

    #[test]
    fn positions_start_at_one_and_append() {
        // 空栏 `COALESCE(MAX(position), 0)` = 0 ⇒ 第一条 1.0。
        assert!((next_position(None) - 1.0).abs() < f64::EPSILON);
        assert!((next_position(Some(1.0)) - 2.0).abs() < f64::EPSILON);
        assert!((next_position(Some(4.0)) - 5.0).abs() < f64::EPSILON);
    }
}
