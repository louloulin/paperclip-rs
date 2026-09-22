//! Personal Access Token (PAT) — 与 multica `personal_access_tokens` 表对应。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use thiserror::Error;

use mc_core::Id;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pat {
    pub id: Id,
    pub user_id: Id,
    pub name: String,
    /// sha256 hex of full token
    pub token_hash: String,
    /// Last 4 chars for display
    pub token_last4: String,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum PatError {
    #[error("pat not found")]
    NotFound,
    #[error("pat expired")]
    Expired,
}

#[async_trait]
pub trait PatStore: Send + Sync {
    async fn get_by_hash(&self, hash: &str) -> Result<Pat, PatError>;
    async fn put(&self, pat: Pat) -> Result<(), PatError>;
    async fn delete(&self, id: Id) -> Result<(), PatError>;
    async fn list_for_user(&self, user_id: Id) -> Result<Vec<Pat>, PatError>;
}

#[derive(Default, Clone)]
pub struct InMemoryPatStore {
    inner: Arc<RwLock<HashMap<Id, Pat>>>,
}

#[async_trait]
impl PatStore for InMemoryPatStore {
    async fn get_by_hash(&self, hash: &str) -> Result<Pat, PatError> {
        let guard = self.inner.read().unwrap();
        guard
            .values()
            .find(|p| p.token_hash == hash)
            .cloned()
            .ok_or(PatError::NotFound)
    }

    async fn put(&self, pat: Pat) -> Result<(), PatError> {
        let mut guard = self.inner.write().unwrap();
        guard.insert(pat.id, pat);
        Ok(())
    }

    async fn delete(&self, id: Id) -> Result<(), PatError> {
        let mut guard = self.inner.write().unwrap();
        guard.remove(&id);
        Ok(())
    }

    async fn list_for_user(&self, user_id: Id) -> Result<Vec<Pat>, PatError> {
        let guard = self.inner.read().unwrap();
        Ok(guard
            .values()
            .filter(|p| p.user_id == user_id)
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pat_round_trip() {
        let store = InMemoryPatStore::default();
        let pat = Pat {
            id: Id::new(),
            user_id: Id::new(),
            name: "ci".into(),
            token_hash: "abc".into(),
            token_last4: "wxyz".into(),
            expires_at: Utc::now() + chrono::Duration::days(30),
            last_used_at: None,
            scopes: vec!["read".into()],
            created_at: Utc::now(),
        };
        store.put(pat.clone()).await.unwrap();
        let got = store.get_by_hash("abc").await.unwrap();
        assert_eq!(got.id, pat.id);
    }
}
