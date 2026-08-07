//! M4 真实验证：AES-256-GCM 加解密 roundtrip + master key 文件 + Debug hygiene。
//!
//! 与 Node `server/src/secrets/local-encrypted-provider.ts` 行为对齐：
//! - 每次 encrypt 使用新随机 nonce（12 bytes）
//! - 同一明文两次 encrypt 出 ciphertext 不同
//! - 用错 master key 解密失败
//! - master key 文件读写 roundtrip
//! - 调试输出不会泄露明文 / ciphertext

use pc_secrets::local_encrypted::LocalEncryptedProvider;
use pc_secrets::provider::{
    SecretProvider, SecretProviderRuntimeContext, SecretProviderWriteContext,
};
use pc_secrets::types::{LocalEncryptedMaterial, ProviderHealthStatus};
use std::fs;
use tempfile::TempDir;
use uuid::Uuid;

fn ctx() -> SecretProviderWriteContext {
    SecretProviderWriteContext {
        company_id: Uuid::new_v4(),
        secret_key: "OPENAI_API_KEY".into(),
        secret_name: "openai".into(),
        version: 1,
    }
}

fn rtctx(secret_id: Uuid) -> SecretProviderRuntimeContext {
    SecretProviderRuntimeContext {
        company_id: Uuid::new_v4(),
        secret_id,
        secret_key: "OPENAI_API_KEY".into(),
        version: 1,
    }
}

fn extract_material(prepared: &pc_secrets::PreparedSecretVersion) -> LocalEncryptedMaterial {
    serde_json::from_value(prepared.material.clone()).expect("material shape")
}

#[tokio::test]
async fn encrypt_decrypt_roundtrip() {
    let key = [0x42u8; 32];
    let p = LocalEncryptedProvider::from_bytes(key);
    let prepared = p
        .create_secret("sk-test-abc123XYZ".into(), &ctx())
        .await
        .expect("encrypt");
    let m = extract_material(&prepared);
    assert_eq!(m.scheme, "local_encrypted_v1");
    // AES-GCM: ciphertext bytes == plaintext bytes (stream cipher, no padding),
    // tag is always 16 bytes. "sk-test-abc123XYZ" is 17 bytes → ciphertext hex = 34 chars.
    assert_eq!(m.ciphertext.len(), 17 * 2);
    assert_eq!(m.tag.len(), 32); // 16 bytes hex

    let plain = p
        .resolve_version(serde_json::to_value(&m).unwrap(), &rtctx(Uuid::new_v4()))
        .await
        .expect("decrypt");
    assert_eq!(plain, "sk-test-abc123XYZ");
}

#[tokio::test]
async fn two_encrypts_produce_different_ciphertext() {
    let key = [0x42u8; 32];
    let p = LocalEncryptedProvider::from_bytes(key);
    let a = extract_material(&p.create_secret("same".into(), &ctx()).await.unwrap());
    let b = extract_material(&p.create_secret("same".into(), &ctx()).await.unwrap());
    assert_ne!(a.iv, b.iv, "nonce must be random per call");
    assert_ne!(
        a.ciphertext, b.ciphertext,
        "ciphertext must differ even with same plaintext"
    );
}

#[tokio::test]
async fn wrong_key_decryption_fails() {
    let key_a = [0x11u8; 32];
    let key_b = [0x22u8; 32];
    let enc = LocalEncryptedProvider::from_bytes(key_a);
    let dec = LocalEncryptedProvider::from_bytes(key_b);
    let m = extract_material(&enc.create_secret("top-secret".into(), &ctx()).await.unwrap());
    let err = dec
        .resolve_version(serde_json::to_value(&m).unwrap(), &rtctx(Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(err.contains("decryption failed"), "got: {err}");
}

#[tokio::test]
async fn unsupported_scheme_rejected() {
    let key = [0x42u8; 32];
    let p = LocalEncryptedProvider::from_bytes(key);
    let bad = serde_json::json!({
        "scheme": "future_v999",
        "iv": "00",
        "tag": "00",
        "ciphertext": "00"
    });
    let err = p
        .resolve_version(bad, &rtctx(Uuid::new_v4()))
        .await
        .unwrap_err();
    assert!(err.contains("unsupported encryption scheme"), "got: {err}");
}

#[tokio::test]
async fn plaintext_not_in_debug_output() {
    let key = [0x42u8; 32];
    let p = LocalEncryptedProvider::from_bytes(key);
    let prepared = p
        .create_secret("leak-canary-secret".into(), &ctx())
        .await
        .unwrap();
    let dbg = format!("{prepared:?}");
    assert!(
        !dbg.contains("leak-canary-secret"),
        "Debug output leaked plaintext: {dbg}"
    );
}

#[tokio::test]
async fn version_create_rotates() {
    let key = [0x42u8; 32];
    let p = LocalEncryptedProvider::from_bytes(key);
    let mut wctx = ctx();
    let v1m = extract_material(&p.create_secret("v1".into(), &wctx).await.unwrap());
    wctx.version = 2;
    let v2m = extract_material(&p.create_version("v2".into(), &wctx).await.unwrap());
    assert_ne!(v1m.ciphertext, v2m.ciphertext);

    let secret_id = Uuid::new_v4();
    let plain1 = p
        .resolve_version(serde_json::to_value(&v1m).unwrap(), &rtctx(secret_id))
        .await
        .unwrap();
    assert_eq!(plain1, "v1");
    let mut rc = rtctx(secret_id);
    rc.version = 2;
    let plain2 = p
        .resolve_version(serde_json::to_value(&v2m).unwrap(), &rc)
        .await
        .unwrap();
    assert_eq!(plain2, "v2");
}

#[tokio::test]
async fn health_check_passes_for_loaded_key() {
    let key = [0x42u8; 32];
    let p = LocalEncryptedProvider::from_bytes(key);
    let h = p.health_check(None, None).await;
    assert_eq!(h.status, ProviderHealthStatus::Ok);
}

#[tokio::test]
async fn master_key_file_io_roundtrip() {
    // Save a key to file → reload from file → same material decrypts to same plaintext
    let dir = TempDir::new().unwrap();
    let key_path = dir.path().join("master.key");
    let key = [0x77u8; 32];
    fs::write(&key_path, hex::encode(key)).unwrap();
    let hex_loaded = fs::read_to_string(&key_path).unwrap();
    let key_bytes = hex::decode(hex_loaded.trim()).unwrap();
    assert_eq!(key_bytes.len(), 32);

    let p = LocalEncryptedProvider::from_bytes(key_bytes.try_into().unwrap());
    let m = extract_material(&p.create_secret("from-disk".into(), &ctx()).await.unwrap());
    let plain = p
        .resolve_version(serde_json::to_value(&m).unwrap(), &rtctx(Uuid::new_v4()))
        .await
        .unwrap();
    assert_eq!(plain, "from-disk");
}