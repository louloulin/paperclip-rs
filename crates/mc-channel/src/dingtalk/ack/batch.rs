//! ACK 的**批次判据**（上游 `ack_batch.go` 里"这批输入算什么"那一半）与缺省输入读面。
//!
//! - **写者**：M7-8（`docs/32` §22 的 D1；门 ⑩ 的切分，边界取上游文件的边界）。
//! - **批次的判据是持久化的输入所有权，不是第二个去抖计时器**：同一个上下文里**未密封**的
//!   输入算一批；密封（`task_id` 落地）之后，`task_id` 把这一批和同一会话里的下一批分开。

use async_trait::async_trait;
use mc_core::id::Id;
use mc_repos::chat_message::ChatMessageRow;

use crate::dingtalk::ack::ReactionInputQueries;
use crate::engine::resolvers::{EngineError, EngineResult};

// =====================================================================
// 批次判据（上游 `ack_batch.go`）
// =====================================================================

/// 两条输入是否属于**同一批**（上游 `sameReactionBatch`）。
///
/// 会话必须相同、`b` 必须是已入库的 `user` 行；两边都还没密封 ⇒ 比上下文代际，否则比任务。
#[must_use]
pub fn same_reaction_batch(left: &ChatMessageRow, right: &ChatMessageRow) -> bool {
    if left.chat_session_id != right.chat_session_id
        || !right.channel_ingested
        || right.role != "user"
    {
        return false;
    }
    match (left.task_id, right.task_id) {
        (Some(left_task), Some(right_task)) => left_task == right_task,
        (None, None) => left.channel_context_revision == right.channel_context_revision,
        _ => false,
    }
}

/// 上游 `ackNotifier.inputs` 的默认缺省（没有输入读面 ⇒ 不做批次分类）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoReactionInputs;

#[async_trait]
impl ReactionInputQueries for NoReactionInputs {
    async fn get_chat_message(&self, _id: Id) -> EngineResult<Option<ChatMessageRow>> {
        Err(EngineError::infra(
            "dingtalk ack: no reaction input queries wired",
        ))
    }
}
