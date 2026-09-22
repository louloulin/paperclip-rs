//! API key 模型。

use serde::{Deserialize, Serialize};

use mc_core::Id;

use super::DEFAULT_API_KEY_PREFIX;

/// API key（raw 形式，client 使用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// 完整 token：`mk_<prefix>_<random>`。
    pub raw: String,
    /// token 的 sha256 hex（server 端存储用于校验）。
    pub hash: String,
    pub user_id: Id,
    pub workspace_id: Option<Id>,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ApiKey {
    /// 生成新的 API key。
    pub fn generate(user_id: Id, name: impl Into<String>) -> Self {
        use rand::RngCore;
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        let raw = format!("{}{}", DEFAULT_API_KEY_PREFIX, hex::encode(buf));
        let hash = sha256_hex(raw.as_bytes());
        Self {
            raw,
            hash,
            user_id,
            workspace_id: None,
            name: name.into(),
            created_at: chrono::Utc::now(),
            expires_at: None,
        }
    }

    pub fn is_expired(&self) -> bool {
        if let Some(exp) = self.expires_at {
            exp <= chrono::Utc::now()
        } else {
            false
        }
    }
}

fn sha256_hex(input: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_unique_keys() {
        let k1 = ApiKey::generate(mc_core::Id::new(), "k1");
        let k2 = ApiKey::generate(mc_core::Id::new(), "k2");
        assert_ne!(k1.raw, k2.raw);
        assert!(k1.raw.starts_with(DEFAULT_API_KEY_PREFIX));
        assert_ne!(k1.hash, k2.hash);
    }
}
