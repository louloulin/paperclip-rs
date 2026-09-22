//! AWS Secrets Manager provider (feature = "aws").

#[cfg(feature = "aws")]
mod inner {
    use async_trait::async_trait;
    use aws_sdk_secretsmanager::Client;

    use crate::{Result, SecretError};
    use crate::store::{SecretValue, SecretsStore};

    pub struct AwsSecretsManagerStore {
        client: Client,
    }

    impl AwsSecretsManagerStore {
        pub fn new(client: Client) -> Self {
            Self { client }
        }
    }

    #[async_trait]
    impl SecretsStore for AwsSecretsManagerStore {
        async fn get(&self, _name: &str) -> Result<SecretValue> {
            Err(SecretError::Provider("AwsSecretsManagerStore::get not implemented".into()))
        }
        async fn put(&self, _name: &str, _value: SecretValue) -> Result<()> {
            Err(SecretError::Provider("AwsSecretsManagerStore::put not implemented".into()))
        }
        async fn delete(&self, _name: &str) -> Result<()> {
            Err(SecretError::Provider("AwsSecretsManagerStore::delete not implemented".into()))
        }
        async fn list(&self) -> Result<Vec<String>> {
            Err(SecretError::Provider("AwsSecretsManagerStore::list not implemented".into()))
        }
    }
}

#[cfg(feature = "aws")]
pub use inner::*;