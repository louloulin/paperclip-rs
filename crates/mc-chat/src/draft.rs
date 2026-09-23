//! 草稿恢复（draft-restore）的纯领域规则（M4-3 / LUM-1474）。
//!
//! 「用户切到别的会话/项目后，回来时把未发出的草稿还给他」这条链路的**读面**：
//! 列草稿（升序）+ 消费（删除）草稿。本模块**不做 I/O**：SQL 在
//! `mc_repos::chat_draft_restore`，HTTP 在 `mc_http::routes::chat::session`。
//!
//! | 规则 | 上游出处 |
//! | --- | --- |
//! | `GetChatDraftRestores` 在本 workspace 内按 `created_at ASC` 取全部（**无分页**） | `chat.go:1377 ListChatDraftRestores` |
//! | consume **幂等**：行不存在也回 204（「消费过了」与「从没存在」同形） | `chat.go:1439 ConsumeChatDraftRestore` |
//! | 草稿只对**本人**可见（`user_id` 参与过滤，不是 workspace 级资源） | 同上 + `chat_draft_restore` 的 `user_id` 列 |

/// 消费草稿的结果：**幂等**语义在此显式化（而不是靠 handler 里的 `if rows == 0` 分支）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeOutcome {
    /// 真删掉了一行（第一次消费）。
    Consumed,
    /// 行不存在或已被消费（重放）—— 上游同样回 204。
    AlreadyGone,
}

impl ConsumeOutcome {
    /// 两种结局都回 204；这里只用来断言 handler 不会分叉出 404。
    pub fn http_ok(self) -> bool {
        matches!(self, Self::Consumed | Self::AlreadyGone)
    }
}

/// 上游 `ConsumeChatDraftRestore`：`rows affected` 只决定日志，不决定响应。
pub fn consume_outcome(rows_affected: u64) -> ConsumeOutcome {
    if rows_affected == 0 {
        ConsumeOutcome::AlreadyGone
    } else {
        ConsumeOutcome::Consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consuming_a_missing_row_is_still_a_success() {
        assert_eq!(consume_outcome(0), ConsumeOutcome::AlreadyGone);
        assert_eq!(consume_outcome(1), ConsumeOutcome::Consumed);
        assert!(consume_outcome(0).http_ok());
        assert!(consume_outcome(1).http_ok());
    }
}
