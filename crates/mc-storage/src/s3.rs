//! S3 storage provider (feature = "s3").

#[cfg(feature = "s3")]
mod inner {
    use std::sync::Arc;

    use async_trait::async_trait;
    use aws_sdk_s3::primitives::ByteStream;
    use aws_sdk_s3::Client;

    use crate::{Object, Result, StorageError, StorageProvider};

    pub const PROVIDER_NAME: &str = "s3";

    pub struct S3Storage {
        client: Client,
        bucket: String,
    }

    impl S3Storage {
        pub fn new(client: Client, bucket: impl Into<String>) -> Self {
            Self {
                client,
                bucket: bucket.into(),
            }
        }

        pub fn from_env() -> anyhow::Result<Self> {
            let bucket = std::env::var("MULTICA_S3_BUCKET")
                .map_err(|_| anyhow::anyhow!("MULTICA_S3_BUCKET not set"))?;
            // Real client construction is out of scope for the initial scaffold.
            // The runtime configuration loader wires the client in mc-server.
            Err(anyhow::anyhow!(
                "S3Storage::from_env requires aws_config wiring in mc-server"
            ))
        }
    }

    #[async_trait]
    impl StorageProvider for S3Storage {
        fn name(&self) -> &str {
            PROVIDER_NAME
        }

        async fn put(
            &self,
            _bucket: &str,
            _key: &str,
            _data: bytes::Bytes,
            _content_type: Option<&str>,
        ) -> Result<Object> {
            Err(StorageError::Provider(
                "S3Storage::put not yet implemented (stub)".into(),
            ))
        }

        async fn get(&self, _bucket: &str, _key: &str) -> Result<bytes::Bytes> {
            Err(StorageError::Provider("not implemented".into()))
        }

        async fn delete(&self, _bucket: &str, _key: &str) -> Result<()> {
            Err(StorageError::Provider("not implemented".into()))
        }

        async fn head(&self, _bucket: &str, _key: &str) -> Result<Object> {
            Err(StorageError::Provider("not implemented".into()))
        }
    }
}

#[cfg(feature = "s3")]
pub use inner::*;
