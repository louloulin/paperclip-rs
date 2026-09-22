//! Multica secrets: 本地加密 / AWS Secrets Manager / MCP / plugin。

pub mod cipher;
pub mod provider;
pub mod store;

#[cfg(feature = "aws")]
pub mod aws;

pub use cipher::{decrypt, encrypt, EncryptedPayload};
pub use store::{InMemorySecretsStore, SecretValue, SecretsBackend, SecretsStore};

use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret not found: {0}")]
    NotFound(String),

    #[error("decryption failed")]
    DecryptFailed,

    #[error("provider error: {0}")]
    Provider(String),

    #[error("io error: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, SecretError>;

/// 启动时确保密钥存在；不存在则生成并落盘。
pub fn ensure_root_key(root_key_path: &std::path::Path) -> Result<[u8; 32]> {
    use rand::RngCore;
    use std::io::Write;
    if root_key_path.exists() {
        let bytes = std::fs::read(root_key_path).map_err(|e| SecretError::Io(e.to_string()))?;
        if bytes.len() != 32 {
            return Err(SecretError::Io("root key invalid length".into()));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        return Ok(out);
    }
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    if let Some(parent) = root_key_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SecretError::Io(e.to_string()))?;
    }
    let mut f = std::fs::File::create(root_key_path).map_err(|e| SecretError::Io(e.to_string()))?;
    f.write_all(&key)
        .map_err(|e| SecretError::Io(e.to_string()))?;
    f.sync_all().map_err(|e| SecretError::Io(e.to_string()))?;
    Ok(key)
}

/// 顶层 Secrets facade：provider 路由（local / aws）。
#[derive(Clone)]
pub struct Secrets {
    pub store: Arc<dyn SecretsStore>,
}

impl Secrets {
    pub fn new(store: Arc<dyn SecretsStore>) -> Self {
        Self { store }
    }

    pub async fn get(&self, name: &str) -> Result<SecretValue> {
        self.store.get(name).await
    }

    pub async fn put(&self, name: &str, value: SecretValue) -> Result<()> {
        self.store.put(name, value).await
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        self.store.delete(name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_root_key_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("root.key");
        let k1 = ensure_root_key(&path).unwrap();
        let k2 = ensure_root_key(&path).unwrap();
        assert_eq!(k1, k2);
    }
}
