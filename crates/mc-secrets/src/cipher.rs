//! AES-GCM 加密 / 解密（key = 32 bytes, nonce = 12 bytes）。
//!
//! 与 multica 端 `better-auth`/`server/internal/auth` 等加密流程兼容。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

#[derive(Debug, thiserror::Error)]
pub enum CipherError {
    #[error("encrypt failed: {0}")]
    Encrypt(String),
    #[error("decrypt failed: {0}")]
    Decrypt(String),
    #[error("invalid key length: {0}")]
    KeyLength(usize),
}

/// 加密 payload：base64(nonce) + base64(ciphertext+tag)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedPayload {
    pub nonce: String,
    pub ciphertext: String,
}

pub fn encrypt(plaintext: &[u8], key: &[u8]) -> Result<EncryptedPayload, CipherError> {
    if key.len() != 32 {
        return Err(CipherError::KeyLength(key.len()));
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| CipherError::Encrypt(e.to_string()))?;
    Ok(EncryptedPayload {
        nonce: base64::engine::general_purpose::STANDARD.encode(nonce_bytes),
        ciphertext: base64::engine::general_purpose::STANDARD.encode(ct),
    })
}

pub fn decrypt(payload: &EncryptedPayload, key: &[u8]) -> Result<Vec<u8>, CipherError> {
    if key.len() != 32 {
        return Err(CipherError::KeyLength(key.len()));
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce_bytes = base64::engine::general_purpose::STANDARD
        .decode(&payload.nonce)
        .map_err(|e| CipherError::Decrypt(e.to_string()))?;
    if nonce_bytes.len() != 12 {
        return Err(CipherError::Decrypt("invalid nonce length".into()));
    }
    let ct = base64::engine::general_purpose::STANDARD
        .decode(&payload.ciphertext)
        .map_err(|e| CipherError::Decrypt(e.to_string()))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let pt = cipher
        .decrypt(nonce, ct.as_ref())
        .map_err(|e| CipherError::Decrypt(e.to_string()))?;
    // 不在此处 zeroize：调用方负责；zeroize 会让借用冲突。
    let _ = pt; // suppress unused
    Ok(pt)
}

/// Helper：包装 key 使用后归零。
pub fn zeroize_key(key: &mut [u8]) {
    key.zeroize();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = [0u8; 32]; // 测试用固定 key
        let plaintext = b"hello world";
        let enc = encrypt(plaintext, &key).unwrap();
        let dec = decrypt(&enc, &key).unwrap();
        assert_eq!(dec, plaintext);
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let key1 = [1u8; 32];
        let key2 = [2u8; 32];
        let enc = encrypt(b"secret", &key1).unwrap();
        assert!(decrypt(&enc, &key2).is_err());
    }

    #[test]
    fn invalid_key_length_rejected() {
        let key = [0u8; 16];
        assert!(encrypt(b"x", &key).is_err());
    }

    #[test]
    fn different_nonce_per_call() {
        let key = [0u8; 32];
        let e1 = encrypt(b"abc", &key).unwrap();
        let e2 = encrypt(b"abc", &key).unwrap();
        assert_ne!(e1.nonce, e2.nonce);
        assert_ne!(e1.ciphertext, e2.ciphertext);
    }
}
