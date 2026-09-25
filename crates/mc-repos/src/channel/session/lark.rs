//! lark **遗留**会话绑定面（`lark_chat_session_binding`）。
//!
//! ⚠️ 与泛化面**并存**、**不得**合并（R-M7-5）：本表没有代际列（泛化前的形态），列名也是
//! per-channel 口径（`lark_chat_id` / `lark_chat_type`）。
//!
//! 拆出本文件是**门 ⑩** 的要求（`session.rs` 一度 1,695 行 > 800 硬限）。

use super::{
    map_sqlx_err, ChatType, Db, Id, LarkChatSessionBindingRow, RepoWithDb, Result,
    LARK_BINDING_COLUMNS,
};

#[derive(Clone)]
pub struct LarkChatSessionBindingRepo {
    db: Db,
}

impl LarkChatSessionBindingRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 当前绑定（一个 `chat_session` 至多一行：`UNIQUE(chat_session_id)`）。
    pub async fn get_by_session(
        &self,
        session_id: Id,
    ) -> Result<Option<LarkChatSessionBindingRow>> {
        let sql = format!(
            "SELECT {LARK_BINDING_COLUMNS} FROM lark_chat_session_binding \
             WHERE chat_session_id = $1"
        );
        sqlx::query_as::<_, LarkChatSessionBindingRow>(&sql)
            .bind(session_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 按 `(installation, lark_chat_id)` 查（上游 lark 入站的会话查找口）。
    pub async fn get_by_chat(
        &self,
        installation_id: Id,
        lark_chat_id: &str,
    ) -> Result<Option<LarkChatSessionBindingRow>> {
        let sql = format!(
            "SELECT {LARK_BINDING_COLUMNS} FROM lark_chat_session_binding \
             WHERE installation_id = $1 AND lark_chat_id = $2"
        );
        sqlx::query_as::<_, LarkChatSessionBindingRow>(&sql)
            .bind(installation_id.0)
            .bind(lark_chat_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 建一行遗留绑定（`chat_session` 与 `lark_installation` 都必须已存在：两个外键）。
    pub async fn insert(
        &self,
        session_id: Id,
        installation_id: Id,
        lark_chat_id: &str,
        chat_type: ChatType,
    ) -> Result<LarkChatSessionBindingRow> {
        let sql = format!(
            "INSERT INTO lark_chat_session_binding \
             (chat_session_id, installation_id, lark_chat_id, lark_chat_type) \
             VALUES ($1, $2, $3, $4) RETURNING {LARK_BINDING_COLUMNS}"
        );
        sqlx::query_as::<_, LarkChatSessionBindingRow>(&sql)
            .bind(session_id.0)
            .bind(installation_id.0)
            .bind(lark_chat_id)
            .bind(chat_type.as_str())
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 推进"最近一条触发"游标（出站回复按它回帖）。
    pub async fn update_reply_target(
        &self,
        session_id: Id,
        last_message_id: Option<&str>,
        last_thread_id: Option<&str>,
    ) -> Result<u64> {
        sqlx::query(
            "UPDATE lark_chat_session_binding \
             SET last_lark_message_id = $2, last_lark_thread_id = $3 WHERE chat_session_id = $1",
        )
        .bind(session_id.0)
        .bind(last_message_id)
        .bind(last_thread_id)
        .execute(self.db.pool())
        .await
        .map(|done| done.rows_affected())
        .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for LarkChatSessionBindingRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
