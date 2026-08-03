pub type SecretRepositoryRef = dyn pc_secrets::provider::SecretProvider + Send + Sync;
