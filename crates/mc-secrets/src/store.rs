//! Secret store trait + 内存实现。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use zeroize::Zeroize;

use crate::{Result, SecretError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretValue {
    pub plaintext: String,
}

impl SecretValue {
    pub fn new(plaintext: impl Into<String>) -> Self {
        Self {
            plaintext: plaintext.into(),
        }
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.plaintext.zeroize();
    }
}

impl From<&str> for SecretValue {
    fn from(s: &str) -> Self {
        Self::new(s.to_string())
    }
}

/// 后端类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretsBackend {
    Local,
    Aws,
}

#[async_trait]
pub trait SecretsStore: Send + Sync {
    async fn get(&self, name: &str) -> Result<SecretValue>;
    async fn put(&self, name: &str, value: SecretValue) -> Result<()>;
    async fn delete(&self, name: &str) -> Result<()>;
    async fn list(&self) -> Result<Vec<String>>;
}

#[derive(Default, Clone)]
pub struct InMemorySecretsStore {
    inner: Arc<RwLock<HashMap<String, SecretValue>>>,
}

#[async_trait]
impl SecretsStore for InMemorySecretsStore {
    async fn get(&self, name: &str) -> Result<SecretValue> {
        let guard = self.inner.read().unwrap();
        guard
            .get(name)
            .cloned()
            .ok_or_else(|| SecretError::NotFound(name.into()))
    }

    async fn put(&self, name: &str, value: SecretValue) -> Result<()> {
        let mut guard = self.inner.write().unwrap();
        guard.insert(name.to_string(), value);
        Ok(())
    }

    async fn delete(&self, name: &str) -> Result<()> {
        let mut guard = self.inner.write().unwrap();
        guard.remove(name);
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>> {
        let guard = self.inner.read().unwrap();
        Ok(guard.keys().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip() {
        let store = InMemorySecretsStore::default();
        store
            .put("api_key", SecretValue::new("secret"))
            .await
            .unwrap();
        let got = store.get("api_key").await.unwrap();
        assert_eq!(got.plaintext, "secret");
        store.delete("api_key").await.unwrap();
        assert!(store.get("api_key").await.is_err());
    }
}
