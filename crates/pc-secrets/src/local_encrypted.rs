//! 本地 AES-256-GCM 加密提供方。
//!
//! 与原 `server/src/secrets/local-encrypted-provider.ts` 等价：
//! - 主密钥来源：`PAPERCLIP_SECRETS_MASTER_KEY` 环境变量 → 密钥文件
//! - 密钥文件路径：`PAPERCLIP_SECRETS_MASTER_KEY_FILE` → `~/.paperclip/secrets/master.key`
//! - 加密方案：AES-256-GCM，随机 12-byte IV，认证 tag 16 bytes
//! - 材料格式：`{ scheme: "local_encrypted_v1", iv: "<hex>", tag: "<hex>", ciphertext: "<hex>" }`

use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, KeyInit, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

use crate::provider::{SecretProvider, SecretProviderRuntimeContext, SecretProviderWriteContext};
use crate::types::{
    LocalEncryptedMaterial, PreparedSecretVersion, ProviderHealthCheck, ProviderHealthStatus,
};

const KEY_BYTE_LEN: usize = 32; // 256 bits
const KEY_HEX_LEN: usize = 64;

/// 本地加密提供方。
pub struct LocalEncryptedProvider {
    master_key: [u8; KEY_BYTE_LEN],
}

impl LocalEncryptedProvider {
    /// 加载或创建主密钥。
    pub fn load() -> Result<Self, String> {
        let key_bytes = load_or_create_master_key()?;
        Ok(Self {
            master_key: key_bytes,
        })
    }

    /// 从字节创建（用于测试）。
    #[must_use]
    pub fn from_bytes(key: [u8; KEY_BYTE_LEN]) -> Self {
        Self { master_key: key }
    }

    fn encrypt_value(&self, plaintext: &str) -> LocalEncryptedMaterial {
        let key = Key::<Aes256Gcm>::from_slice(&self.master_key);
        let cipher = Aes256Gcm::new(key);
        let nonce_bytes = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce_bytes, plaintext.as_bytes())
            .expect("AES-256-GCM encryption should not fail");

        // Split ciphertext into actual data + 16-byte tag (last 16 bytes)
        let ct_len = ciphertext.len() - 16;
        let ct = &ciphertext[..ct_len];
        let tag = &ciphertext[ct_len..];

        LocalEncryptedMaterial {
            scheme: "local_encrypted_v1".into(),
            iv: hex::encode(nonce_bytes.as_slice()),
            tag: hex::encode(tag),
            ciphertext: hex::encode(ct),
        }
    }

    fn decrypt_value(&self, material: &LocalEncryptedMaterial) -> Result<String, String> {
        if material.scheme != "local_encrypted_v1" {
            return Err(format!(
                "unsupported encryption scheme: {}",
                material.scheme
            ));
        }

        let iv = hex::decode(&material.iv).map_err(|e| format!("invalid iv hex: {e}"))?;
        let ct = hex::decode(&material.ciphertext)
            .map_err(|e| format!("invalid ciphertext hex: {e}"))?;
        let tag = hex::decode(&material.tag).map_err(|e| format!("invalid tag hex: {e}"))?;

        let mut ciphertext = ct;
        ciphertext.extend_from_slice(&tag);

        let key = Key::<Aes256Gcm>::from_slice(&self.master_key);
        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(&iv);

        let plaintext = cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|_| "decryption failed: wrong key or corrupted data".to_string())?;

        String::from_utf8(plaintext).map_err(|e| format!("decrypted value is not valid UTF-8: {e}"))
    }

    fn sha256_hex(value: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(value.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn prepare_version(&self, value: &str) -> PreparedSecretVersion {
        let value_sha256 = Self::sha256_hex(value);
        let material = self.encrypt_value(value);
        PreparedSecretVersion {
            material: serde_json::to_value(&material).unwrap_or_default(),
            value_sha256: value_sha256.clone(),
            fingerprint_sha256: Some(value_sha256),
            external_ref: None,
            provider_version_ref: None,
        }
    }
}

#[async_trait::async_trait]
impl SecretProvider for LocalEncryptedProvider {
    fn provider_id(&self) -> &'static str {
        "local_encrypted"
    }

    async fn create_secret(
        &self,
        value: String,
        _context: &SecretProviderWriteContext,
    ) -> Result<PreparedSecretVersion, String> {
        Ok(self.prepare_version(&value))
    }

    async fn create_version(
        &self,
        value: String,
        _context: &SecretProviderWriteContext,
    ) -> Result<PreparedSecretVersion, String> {
        Ok(self.prepare_version(&value))
    }

    async fn resolve_version(
        &self,
        material: serde_json::Value,
        _context: &SecretProviderRuntimeContext,
    ) -> Result<String, String> {
        let mat: LocalEncryptedMaterial =
            serde_json::from_value(material).map_err(|e| format!("invalid material: {e}"))?;
        self.decrypt_value(&mat)
    }

    async fn health_check(
        &self,
        _deployment_mode: Option<String>,
        _provider_config: Option<serde_json::Value>,
    ) -> ProviderHealthCheck {
        let key_source = if std::env::var("PAPERCLIP_SECRETS_MASTER_KEY").is_ok() {
            "env"
        } else {
            "file"
        };

        ProviderHealthCheck {
            provider: "local_encrypted".into(),
            status: ProviderHealthStatus::Ok,
            message: format!("Local encrypted provider is active (key source: {key_source})"),
            warnings: None,
            backup_guidance: Some(vec![
                "Back up the master key separately from the database.".into(),
                "A restore needs both the database metadata and the same master key.".into(),
            ]),
            details: Some(serde_json::json!({ "keySource": key_source })),
        }
    }
}

// ── Master key management ───────────────────────────────

fn resolve_master_key_file_path() -> PathBuf {
    if let Ok(env_path) = std::env::var("PAPERCLIP_SECRETS_MASTER_KEY_FILE") {
        return PathBuf::from(env_path);
    }
    let home = dirs_next().unwrap_or_else(|| PathBuf::from("."));
    home.join(".paperclip").join("secrets").join("master.key")
}

fn dirs_next() -> Option<PathBuf> {
    dirs::home_dir()
}

fn decode_master_key(raw: &str) -> Option<[u8; KEY_BYTE_LEN]> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 64-char hex
    if trimmed.len() == KEY_HEX_LEN && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return hex::decode(trimmed).ok().and_then(|v| v.try_into().ok());
    }

    // base64
    if let Ok(decoded) = B64.decode(trimmed) {
        if decoded.len() == KEY_BYTE_LEN {
            return decoded.try_into().ok();
        }
    }

    // raw 32-byte UTF-8
    if trimmed.len() == KEY_BYTE_LEN {
        let mut key = [0u8; KEY_BYTE_LEN];
        key.copy_from_slice(trimmed.as_bytes());
        return Some(key);
    }

    None
}

fn load_or_create_master_key() -> Result<[u8; KEY_BYTE_LEN], String> {
    // 1. Try env var
    if let Ok(env_key) = std::env::var("PAPERCLIP_SECRETS_MASTER_KEY") {
        if let Some(key) = decode_master_key(&env_key) {
            return Ok(key);
        }
        return Err(
            "Invalid PAPERCLIP_SECRETS_MASTER_KEY (expected 32-byte base64, 64-char hex, or raw 32-char string)".into(),
        );
    }

    // 2. Try key file
    let key_path = resolve_master_key_file_path();
    if key_path.exists() {
        enforce_key_file_permissions_best_effort(&key_path);
        let raw = fs::read_to_string(&key_path)
            .map_err(|e| format!("cannot read key file {}: {e}", key_path.display()))?;
        if let Some(key) = decode_master_key(&raw) {
            return Ok(key);
        }
        return Err(format!(
            "Invalid secrets master key at {}",
            key_path.display()
        ));
    }

    // 3. Create new key file
    let dir = key_path.parent().ok_or("invalid key path")?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create dir {}: {e}", dir.display()))?;

    let mut key = [0u8; KEY_BYTE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut key);
    let encoded = B64.encode(key);

    fs::write(&key_path, encoded.as_bytes())
        .map_err(|e| format!("cannot write key file {}: {e}", key_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(key)
}

fn enforce_key_file_permissions_best_effort(key_path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(key_path) {
            let mode = meta.permissions().mode() & 0o777;
            if (mode & 0o077) != 0 {
                let _ = fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    let _ = key_path; // non-unix: no-op
}

// ── Tests ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        for (i, k) in key.iter_mut().enumerate() {
            *k = u8::try_from(i).unwrap();
        }
        key
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let provider = LocalEncryptedProvider::from_bytes(test_key());
        let plaintext = "my-secret-api-key-12345";
        let material = provider.encrypt_value(plaintext);
        let decrypted = provider.decrypt_value(&material).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails_decryption() {
        let p1 = LocalEncryptedProvider::from_bytes(test_key());
        let mut wrong_key = [0u8; 32];
        wrong_key[0] = 0xFF;
        let p2 = LocalEncryptedProvider::from_bytes(wrong_key);

        let material = p1.encrypt_value("secret");
        assert!(p2.decrypt_value(&material).is_err());
    }

    #[test]
    fn prepare_version_produces_valid_material() {
        let provider = LocalEncryptedProvider::from_bytes(test_key());
        let prep = provider.prepare_version("test-value");
        assert_eq!(prep.value_sha256.len(), 64);
        assert!(prep.fingerprint_sha256.is_some());
        assert!(prep.external_ref.is_none());

        // Should be decryptable
        let mat: LocalEncryptedMaterial = serde_json::from_value(prep.material).unwrap();
        let decrypted = provider.decrypt_value(&mat).unwrap();
        assert_eq!(decrypted, "test-value");
    }

    #[test]
    fn sha256_hex_deterministic() {
        let a = LocalEncryptedProvider::sha256_hex("hello");
        let b = LocalEncryptedProvider::sha256_hex("hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn decode_master_key_accepts_hex() {
        let hex_key = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let key = decode_master_key(hex_key).unwrap();
        assert_eq!(key[0], 0);
        assert_eq!(key[31], 0x1f);
    }

    #[test]
    fn decode_master_key_accepts_base64() {
        let b64_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
        let key = decode_master_key(b64_key).unwrap();
        assert_eq!(key[0], 0);
        assert_eq!(key[31], 0x1f);
    }

    #[test]
    fn decode_master_key_rejects_bad_input() {
        assert!(decode_master_key("short").is_none());
        assert!(decode_master_key("").is_none());
    }

    #[tokio::test]
    async fn provider_trait_methods_work() {
        let provider = LocalEncryptedProvider::from_bytes(test_key());

        let ctx = SecretProviderWriteContext {
            company_id: Uuid::nil(),
            secret_key: "MY_KEY".into(),
            secret_name: "test".into(),
            version: 1,
        };

        let prep = provider
            .create_secret("my-value".into(), &ctx)
            .await
            .unwrap();

        let runtime_ctx = SecretProviderRuntimeContext {
            company_id: Uuid::nil(),
            secret_id: Uuid::nil(),
            secret_key: "MY_KEY".into(),
            version: 1,
        };

        let resolved = provider
            .resolve_version(prep.material, &runtime_ctx)
            .await
            .unwrap();
        assert_eq!(resolved, "my-value");
    }
}
