//! `board_chat_threads` + `board_chat_messages` 域 — Board 董事会聊天持久化。
//!
//! 设计：
//! - 一次 board chat 创建一个 thread（绑定 issue）
//! - 每条 turn（user / assistant / system）作为 message 追加
//! - 任何状态变化都 update thread.last_message_at

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
    System,
    Tool,
}
impl ChatRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "system" => Some(Self::System),
            "tool" => Some(Self::Tool),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMessageStatus {
    Streaming,
    Complete,
    Failed,
    Cancelled,
}
impl ChatMessageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "streaming" => Some(Self::Streaming),
            "complete" => Some(Self::Complete),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

const THREAD_COLS: &str = "id, company_id, issue_id, title, status, created_by_user_id, \
    last_message_at, created_at, updated_at";

const MSG_COLS: &str = "id, thread_id, company_id, role, author_user_id, author_agent_id, \
    body, tool_uses, status, created_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardThreadRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub issue_id: Option<Uuid>,
    pub title: String,
    pub status: String,
    pub created_by_user_id: Option<Uuid>,
    pub last_message_at: Timestamp,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardMessageRow {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub company_id: Uuid,
    pub role: String,
    pub author_user_id: Option<Uuid>,
    pub author_agent_id: Option<Uuid>,
    pub body: String,
    pub tool_uses: Value,
    pub status: String,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewThread {
    pub company_id: Uuid,
    pub issue_id: Option<Uuid>,
    pub title: String,
    pub created_by_user_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMessage {
    pub thread_id: Uuid,
    pub company_id: Uuid,
    pub role: ChatRole,
    pub author_user_id: Option<Uuid>,
    pub author_agent_id: Option<Uuid>,
    pub body: String,
    pub tool_uses: Option<Value>,
    pub status: Option<ChatMessageStatus>,
}

pub struct BoardChatRepo<'a> {
    pub db: &'a Db,
}

impl<'a> BoardChatRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_threads(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> RepoResult<Vec<BoardThreadRow>> {
        let sql = format!(
            "SELECT {THREAD_COLS} FROM board_chat_threads \
             WHERE company_id=$1 ORDER BY last_message_at DESC LIMIT $2"
        );
        Ok(sqlx::query_as::<_, BoardThreadRow>(&sql)
            .bind(company_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get_thread(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<BoardThreadRow>> {
        let sql = format!(
            "SELECT {THREAD_COLS} FROM board_chat_threads \
             WHERE company_id=$1 AND id=$2"
        );
        Ok(sqlx::query_as::<_, BoardThreadRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn get_or_create_thread(&self, n: &NewThread) -> RepoResult<BoardThreadRow> {
        if let Some(issue_id) = n.issue_id {
            if let Some(existing) = sqlx::query_as::<_, BoardThreadRow>(&format!(
                "SELECT {THREAD_COLS} FROM board_chat_threads \
                 WHERE company_id=$1 AND issue_id=$2 LIMIT 1"
            ))
            .bind(n.company_id)
            .bind(issue_id)
            .fetch_optional(self.db.pool())
            .await?
            {
                return Ok(existing);
            }
        }
        let sql = format!(
            "INSERT INTO board_chat_threads (company_id, issue_id, title, created_by_user_id) \
             VALUES ($1, $2, $3, $4) RETURNING {THREAD_COLS}"
        );
        let row = sqlx::query_as::<_, BoardThreadRow>(&sql)
            .bind(n.company_id)
            .bind(n.issue_id)
            .bind(&n.title)
            .bind(n.created_by_user_id)
            .fetch_one(self.db.pool())
            .await?;
        Ok(row)
    }

    pub async fn list_messages(
        &self,
        thread_id: Uuid,
        limit: i64,
    ) -> RepoResult<Vec<BoardMessageRow>> {
        let sql = format!(
            "SELECT {MSG_COLS} FROM board_chat_messages \
             WHERE thread_id=$1 ORDER BY created_at ASC LIMIT $2"
        );
        Ok(sqlx::query_as::<_, BoardMessageRow>(&sql)
            .bind(thread_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn append_message(&self, n: &NewMessage) -> RepoResult<BoardMessageRow> {
        let status = n.status.unwrap_or(ChatMessageStatus::Complete);
        let mut tx = self.db.pool().begin().await?;
        let msg = sqlx::query_as::<_, BoardMessageRow>(&format!(
            "INSERT INTO board_chat_messages \
                (thread_id, company_id, role, author_user_id, author_agent_id, body, tool_uses, status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {MSG_COLS}",
        ))
        .bind(n.thread_id)
        .bind(n.company_id)
        .bind(n.role.as_str())
        .bind(n.author_user_id)
        .bind(n.author_agent_id)
        .bind(&n.body)
        .bind(n.tool_uses.clone().unwrap_or_else(|| serde_json::json!([])))
        .bind(status.as_str())
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE board_chat_threads SET last_message_at=now(), updated_at=now() WHERE id=$1",
        )
        .bind(n.thread_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(msg)
    }

    pub async fn set_message_status(
        &self,
        message_id: Uuid,
        status: ChatMessageStatus,
    ) -> RepoResult<Option<BoardMessageRow>> {
        let sql = format!(
            "UPDATE board_chat_messages SET status=$2 \
             WHERE id=$1 RETURNING {MSG_COLS}"
        );
        Ok(sqlx::query_as::<_, BoardMessageRow>(&sql)
            .bind(message_id)
            .bind(status.as_str())
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Ensure a board issue exists for a chat session.
    /// 优先按 (company_id, title) 查；找不到则创建；创建若遇 origin_fingerprint 唯一冲突则回查。
    pub async fn ensure_board_issue(&self, company_id: Uuid, title: &str) -> RepoResult<Uuid> {
        if let Some((id,)) = sqlx::query_as::<_, (Uuid,)>(
            "SELECT id FROM issues WHERE company_id=$1 AND title=$2 LIMIT 1",
        )
        .bind(company_id)
        .bind(title)
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(id);
        }
        match sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO issues (company_id, title, priority, status, origin_kind, origin_fingerprint) \
             VALUES ($1, $2, 'normal', 'open', 'board', 'board-chat') RETURNING id",
        )
        .bind(company_id)
        .bind(title)
        .fetch_one(self.db.pool())
        .await
        {
            Ok(id) => Ok(id),
            Err(sqlx::Error::Database(db_err)) if db_err.code().as_deref() == Some("23505") => {
                // 唯一约束冲突（极小概率 race）：再次查询并返回那条记录的 id。
                let (id,): (Uuid,) = sqlx::query_as(
                    "SELECT id FROM issues WHERE company_id=$1 AND title=$2 LIMIT 1",
                )
                .bind(company_id)
                .bind(title)
                .fetch_one(self.db.pool())
                .await?;
                Ok(id)
            }
            Err(other) => Err(RepoError::Sql(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_strings() {
        assert_eq!(ChatRole::User.as_str(), "user");
        assert_eq!(ChatRole::Assistant.as_str(), "assistant");
        assert_eq!(ChatRole::System.as_str(), "system");
        assert_eq!(ChatRole::Tool.as_str(), "tool");
    }

    #[test]
    fn status_strings() {
        assert_eq!(ChatMessageStatus::Complete.as_str(), "complete");
        assert_eq!(ChatMessageStatus::Streaming.as_str(), "streaming");
        assert_eq!(ChatMessageStatus::Failed.as_str(), "failed");
        assert_eq!(ChatMessageStatus::Cancelled.as_str(), "cancelled");
    }
}
