//! Session 模型 + 内存实现。
//!
//! 与 pc-auth `session.rs` 等价。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use thiserror::Error;
use uuid::Uuid;

use mc_core::Id;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub user_id: Id,
    pub workspace_id: Option<Id>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Session {
    pub fn new(user_id: Id, ttl_secs: u64) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            user_id,
            workspace_id: None,
            ip: None,
            user_agent: None,
            csrf_token: Uuid::new_v4().to_string(),
            expires_at: now
                + chrono::Duration::seconds(i64::try_from(ttl_secs).unwrap_or(i64::MAX)),
            created_at: now,
            last_seen_at: now,
            metadata: HashMap::new(),
        }
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session not found")]
    NotFound,
    #[error("session expired")]
    Expired,
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn get(&self, id: &str) -> Result<Session, SessionError>;
    async fn put(&self, session: Session) -> Result<(), SessionError>;
    async fn delete(&self, id: &str) -> Result<(), SessionError>;
    async fn touch(&self, id: &str) -> Result<(), SessionError>;
}

#[derive(Default, Clone)]
pub struct InMemorySessionStore {
    inner: Arc<RwLock<HashMap<String, Session>>>,
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn get(&self, id: &str) -> Result<Session, SessionError> {
        let guard = self.inner.read().unwrap();
        let session = guard.get(id).cloned().ok_or(SessionError::NotFound)?;
        if session.is_expired(Utc::now()) {
            return Err(SessionError::Expired);
        }
        Ok(session)
    }

    async fn put(&self, session: Session) -> Result<(), SessionError> {
        let mut guard = self.inner.write().unwrap();
        guard.insert(session.id.clone(), session);
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<(), SessionError> {
        let mut guard = self.inner.write().unwrap();
        guard.remove(id);
        Ok(())
    }

    async fn touch(&self, id: &str) -> Result<(), SessionError> {
        let mut guard = self.inner.write().unwrap();
        if let Some(s) = guard.get_mut(id) {
            s.last_seen_at = Utc::now();
            Ok(())
        } else {
            Err(SessionError::NotFound)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_session_round_trip() {
        let store = InMemorySessionStore::default();
        let session = Session::new(Id::new(), 60);
        let id = session.id.clone();
        store.put(session).await.unwrap();
        let got = store.get(&id).await.unwrap();
        assert_eq!(got.id, id);
    }

    #[tokio::test]
    async fn expired_session_returns_error() {
        let store = InMemorySessionStore::default();
        let mut session = Session::new(Id::new(), 60);
        session.expires_at = Utc::now() - chrono::Duration::seconds(1);
        let id = session.id.clone();
        store.put(session).await.unwrap();
        assert!(matches!(store.get(&id).await, Err(SessionError::Expired)));
    }
}
