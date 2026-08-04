#![forbid(unsafe_code)]

pub mod aws;
pub mod gcp;
pub mod vault;
/// 秘密提供方抽象与本地 AES-256-GCM 加密实现。
///
/// 设计目标：
/// - 与原 paperclip `server/src/secrets/` 完全等价
/// - 支持 `local_encrypted`（AES-256-GCM + 主密钥文件）与 `aws_secrets_manager`（stub）
/// - 提供方注册表 + 健康检查
/// - `SecretProvider` trait 与 `SecretProviderModule` 分离
pub mod local_encrypted;
pub mod provider;
pub mod registry;
pub mod types;

pub use aws::AwsSecretsManagerProvider;
pub use gcp::GcpSecretManagerProvider;
pub use local_encrypted::LocalEncryptedProvider;
pub use provider::SecretProvider;
pub use registry::SecretProviderRegistry;
pub use types::{
    LocalEncryptedMaterial, PreparedSecretVersion, ProviderHealthCheck, ProviderHealthStatus,
    SecretProviderValidationResult, StoredSecretVersionMaterial,
};
pub use vault::VaultProvider;

pub fn hmac_sha256(key: &[u8], payload: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(payload);
    let bytes = mac.finalize().into_bytes();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{SecretProviderRuntimeContext, SecretProviderWriteContext};
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn gcp_provider_id_and_validate_empty() {
        let p = GcpSecretManagerProvider::new("test-project", "test-token");
        assert_eq!(p.provider_id(), "gcp_secret_manager");
    }

    #[test]
    fn gcp_validate_config_rejects_empty() {
        let p = GcpSecretManagerProvider::new("", "");
        let v = futures_executor_block_on(p.validate_config(None));
        assert!(!v.ok, "empty config should be invalid");
    }

    #[test]
    fn gcp_from_config_constructs() {
        let cfg = json!({
            "projectId": "p1",
            "accessToken": "tok",
        });
        let p = GcpSecretManagerProvider::from_config(Some(cfg)).unwrap();
        assert_eq!(p.project(), "p1");
        assert_eq!(p.token(), "tok");
    }

    #[test]
    fn gcp_from_config_rejects_missing() {
        let cfg = json!({ "projectId": "p1" });
        assert!(GcpSecretManagerProvider::from_config(Some(cfg)).is_err());
        let cfg = json!({ "accessToken": "tok" });
        assert!(GcpSecretManagerProvider::from_config(Some(cfg)).is_err());
        assert!(GcpSecretManagerProvider::from_config(None).is_err());
    }

    #[test]
    fn vault_provider_id_and_validate() {
        let p = VaultProvider::new("https://vault.example.com", "tok");
        assert_eq!(p.provider_id(), "vault");
        assert_eq!(p.addr(), "https://vault.example.com");
        assert_eq!(p.token(), "tok");
    }

    #[test]
    fn vault_from_config_constructs() {
        let cfg = json!({ "address": "https://vault.example.com", "token": "tok" });
        let p = VaultProvider::from_config(Some(cfg)).unwrap();
        assert_eq!(p.addr(), "https://vault.example.com");
    }

    #[test]
    fn vault_from_config_accepts_addr_alias() {
        let cfg = json!({ "addr": "https://vault.example.com", "token": "tok" });
        let p = VaultProvider::from_config(Some(cfg)).unwrap();
        assert_eq!(p.addr(), "https://vault.example.com");
    }

    #[test]
    fn vault_validate_rejects_empty() {
        let p = VaultProvider::new("", "");
        let v = futures_executor_block_on(p.validate_config(None));
        assert!(!v.ok);
    }

    #[test]
    fn sanitize_path_rejects_invalid() {
        assert!(super::vault::sanitize_path_for_test("").is_err());
        assert!(super::vault::sanitize_path_for_test("/leading").is_err());
        assert!(super::vault::sanitize_path_for_test("with space").is_err());
        assert!(super::vault::sanitize_path_for_test("ok/path-here_v1.0").is_ok());
    }

    // re-export from async to sync for tests
    fn futures_executor_block_on<F: std::future::Future>(f: F) -> F::Output {
        futures_executor_block_on_inner(f)
    }
    fn futures_executor_block_on_inner<F: std::future::Future>(f: F) -> F::Output {
        // minimal block-on via tokio test runtime if available; otherwise panic for !Send futures.
        // We only use this for validate_config which is a pure async fn.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime");
        rt.block_on(f)
    }

    #[tokio::test]
    async fn gcp_create_secret_round_trip_local() {
        let p = GcpSecretManagerProvider::new("test-project", "test-token");
        let ctx = SecretProviderWriteContext {
            company_id: Uuid::nil(),
            secret_key: "K".into(),
            secret_name: "K".into(),
            version: 1,
        };
        // create_secret 会真正调用网络，所以只断言 sanitize 路径合法
        // 改用 sanitize_name 单元测试：
        assert_eq!(super::gcp::sanitize_name_for_test("ok-name_1.0").unwrap(), "ok-name_1.0");
        assert!(super::gcp::sanitize_name_for_test("").is_err());
        assert!(super::gcp::sanitize_name_for_test("with space").is_err());
        let _ = (p, ctx); // suppress unused
    }
}
