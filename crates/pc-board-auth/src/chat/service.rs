use async_trait::async_trait;
use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{
    board_chat::{
        BoardChatRepo, BoardMessageRow, BoardThreadRow, ChatMessageStatus, ChatRole, NewMessage,
        NewThread,
    },
    Db,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BoardChatHookEvent {
    ThreadOpened {
        company_id: Uuid,
        thread_id: Uuid,
        title: String,
    },
    MessagePosted {
        company_id: Uuid,
        thread_id: Uuid,
        message_id: Uuid,
        role: String,
    },
    MessageStatusChanged {
        message_id: Uuid,
        status: String,
    },
    BoardIssueEnsured {
        company_id: Uuid,
        issue_id: Uuid,
    },
}

#[async_trait]
pub trait BoardChatHook: Send + Sync {
    async fn on_board_chat_event(&self, _event: BoardChatHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopBoardChatHook;
#[async_trait]
impl BoardChatHook for NoopBoardChatHook {}

#[derive(Default)]
pub struct RecordingBoardChatHook {
    pub events: std::sync::Mutex<Vec<BoardChatHookEvent>>,
}
impl RecordingBoardChatHook {
    pub fn events_snapshot(&self) -> Vec<BoardChatHookEvent> {
        self.events.lock().expect("mutex").clone()
    }
    pub fn clear(&self) {
        self.events.lock().expect("mutex").clear()
    }
    pub fn len(&self) -> usize {
        self.events.lock().expect("mutex").len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
#[async_trait]
impl BoardChatHook for RecordingBoardChatHook {
    async fn on_board_chat_event(&self, e: BoardChatHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BoardChatError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("thread not found: {0}")]
    ThreadNotFound(Uuid),
    #[error("message not found: {0}")]
    MessageNotFound(Uuid),
    #[error("transient: {0}")]
    Transient(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for BoardChatError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}
pub type BoardChatResult<T> = std::result::Result<T, BoardChatError>;

fn require_non_nil(id: Uuid, field: &str) -> BoardChatResult<()> {
    if id.is_nil() {
        Err(BoardChatError::Validation(format!("{field} is required")))
    } else {
        Ok(())
    }
}

#[derive(Clone)]
pub struct BoardChatService {
    db: Db,
    hooks: Vec<Arc<dyn BoardChatHook>>,
}

impl BoardChatService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn BoardChatHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn BoardChatHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    fn repo(&self) -> BoardChatRepo<'_> {
        BoardChatRepo::new(&self.db)
    }
    async fn dispatch(&self, e: BoardChatHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_board_chat_event(e.clone()).await {
                tracing::warn!(?err, "board chat hook failed");
            }
        }
    }

    // ---- thread reads ----
    pub async fn list_threads(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> BoardChatResult<Vec<BoardThreadRow>> {
        require_non_nil(company_id, "companyId")?;
        Ok(self.repo().list_threads(company_id, limit).await?)
    }
    pub async fn get_thread(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> BoardChatResult<Option<BoardThreadRow>> {
        require_non_nil(company_id, "companyId")?;
        require_non_nil(id, "threadId")?;
        Ok(self.repo().get_thread(company_id, id).await?)
    }

    // ---- message reads ----
    pub async fn list_messages(
        &self,
        thread_id: Uuid,
        limit: i64,
    ) -> BoardChatResult<Vec<BoardMessageRow>> {
        require_non_nil(thread_id, "threadId")?;
        Ok(self.repo().list_messages(thread_id, limit).await?)
    }

    // ---- thread writes ----
    pub async fn get_or_create_thread(&self, input: NewThread) -> BoardChatResult<BoardThreadRow> {
        require_non_nil(input.company_id, "companyId")?;
        if input.title.trim().is_empty() {
            return Err(BoardChatError::Validation("title must not be empty".into()));
        }
        if self
            .repo()
            .get_thread(input.company_id, Uuid::nil())
            .await
            .is_ok()
        { /* no-op validation */ }
        let row = self.repo().get_or_create_thread(&input).await?;
        // emit only when this is a new thread (created_at == updated_at heuristic) or always as event
        self.dispatch(BoardChatHookEvent::ThreadOpened {
            company_id: row.company_id,
            thread_id: row.id,
            title: row.title.clone(),
        })
        .await;
        Ok(row)
    }

    // ---- message writes ----
    pub async fn append_message(&self, input: NewMessage) -> BoardChatResult<BoardMessageRow> {
        require_non_nil(input.company_id, "companyId")?;
        require_non_nil(input.thread_id, "threadId")?;
        if input.body.is_empty() {
            return Err(BoardChatError::Validation("body must not be empty".into()));
        }
        let row = self.repo().append_message(&input).await?;
        self.dispatch(BoardChatHookEvent::MessagePosted {
            company_id: row.company_id,
            thread_id: row.thread_id,
            message_id: row.id,
            role: row.role.clone(),
        })
        .await;
        Ok(row)
    }
    pub async fn set_message_status(
        &self,
        message_id: Uuid,
        status: ChatMessageStatus,
    ) -> BoardChatResult<Option<BoardMessageRow>> {
        require_non_nil(message_id, "messageId")?;
        let row = self.repo().set_message_status(message_id, status).await?;
        if let Some(r) = &row {
            self.dispatch(BoardChatHookEvent::MessageStatusChanged {
                message_id: r.id,
                status: status.as_str().to_string(),
            })
            .await;
        }
        Ok(row)
    }
    pub async fn ensure_board_issue(&self, company_id: Uuid, title: &str) -> BoardChatResult<Uuid> {
        require_non_nil(company_id, "companyId")?;
        if title.trim().is_empty() {
            return Err(BoardChatError::Validation("title must not be empty".into()));
        }
        let id = self.repo().ensure_board_issue(company_id, title).await?;
        self.dispatch(BoardChatHookEvent::BoardIssueEnsured {
            company_id,
            issue_id: id,
        })
        .await;
        Ok(id)
    }
}
