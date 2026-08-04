use super::{javascript_string_length, DecisionSigningError, MIN_DECISION_SIGNING_SECRET_LENGTH};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use pc_config::PaperclipHomePaths;
use rand::rngs::OsRng;
use rand::RngCore;
use std::fs::{self, Metadata, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionSigningKeyStore {
    key_path: PathBuf,
}

impl DecisionSigningKeyStore {
    pub fn new(key_path: impl Into<PathBuf>) -> Self {
        Self {
            key_path: key_path.into(),
        }
    }

    pub fn from_env() -> Result<Self, DecisionSigningError> {
        let paths = PaperclipHomePaths::from_env()?;
        Ok(Self::from_home_paths(&paths))
    }

    pub fn from_home_paths(paths: &PaperclipHomePaths) -> Self {
        Self::new(paths.secrets_dir().join("decision-signing.key"))
    }

    pub fn key_path(&self) -> &Path {
        &self.key_path
    }

    pub fn resolve_secret(
        &self,
        explicit_secret: Option<&str>,
    ) -> Result<String, DecisionSigningError> {
        if let Some(secret) = explicit_secret
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if javascript_string_length(secret) < MIN_DECISION_SIGNING_SECRET_LENGTH {
                return Err(DecisionSigningError::ExplicitSecretTooShort);
            }
            return Ok(secret.to_string());
        }
        self.load_or_create_generated_secret()
    }

    fn load_or_create_generated_secret(&self) -> Result<String, DecisionSigningError> {
        let directory =
            self.key_path
                .parent()
                .ok_or_else(|| DecisionSigningError::MissingParentDirectory {
                    path: self.key_path.clone(),
                })?;

        match enforce_secrets_directory_permissions(directory)
            .and_then(|()| read_generated_secret(&self.key_path))
        {
            Ok(existing) => return Ok(existing),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error),
        }

        fs::create_dir_all(directory)
            .map_err(|source| DecisionSigningError::io("creating directory", directory, source))?;
        enforce_secrets_directory_permissions(directory)?;

        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let generated = BASE64_STANDARD.encode(bytes);
        let temporary_path = temporary_key_path(&self.key_path);

        let publish_result = self.publish_generated_secret(&temporary_path, &generated);
        let cleanup_result = fs::remove_file(&temporary_path);
        match (publish_result, cleanup_result) {
            (Err(error), _) => Err(error),
            (Ok(secret), Ok(())) => Ok(secret),
            (Ok(secret), Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(secret),
            (Ok(_), Err(source)) => Err(DecisionSigningError::io(
                "removing temporary key",
                temporary_path,
                source,
            )),
        }
    }

    fn publish_generated_secret(
        &self,
        temporary_path: &Path,
        generated: &str,
    ) -> Result<String, DecisionSigningError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = options.open(temporary_path).map_err(|source| {
            DecisionSigningError::io("creating temporary key", temporary_path, source)
        })?;
        file.write_all(generated.as_bytes()).map_err(|source| {
            DecisionSigningError::io("writing temporary key", temporary_path, source)
        })?;
        file.sync_all().map_err(|source| {
            DecisionSigningError::io("syncing temporary key", temporary_path, source)
        })?;
        drop(file);
        enforce_key_file_permissions(temporary_path)?;

        match fs::hard_link(temporary_path, &self.key_path) {
            Ok(()) => {
                enforce_key_file_permissions(&self.key_path)?;
                Ok(generated.to_string())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                read_generated_secret(&self.key_path)
            }
            Err(source) => Err(DecisionSigningError::io(
                "publishing generated key",
                &self.key_path,
                source,
            )),
        }
    }
}

fn read_generated_secret(key_path: &Path) -> Result<String, DecisionSigningError> {
    enforce_key_file_permissions(key_path)?;
    let existing = fs::read_to_string(key_path)
        .map_err(|source| DecisionSigningError::io("reading key", key_path, source))?;
    let existing = existing.trim();
    if javascript_string_length(existing) < MIN_DECISION_SIGNING_SECRET_LENGTH {
        return Err(DecisionSigningError::GeneratedSecretTooShort {
            path: key_path.to_path_buf(),
        });
    }
    Ok(existing.to_string())
}

fn enforce_key_file_permissions(key_path: &Path) -> Result<(), DecisionSigningError> {
    let mut metadata = fs::symlink_metadata(key_path)
        .map_err(|source| DecisionSigningError::io("inspecting key", key_path, source))?;
    if !metadata.file_type().is_file() {
        return Err(DecisionSigningError::KeyNotRegularFile {
            path: key_path.to_path_buf(),
        });
    }
    assert_owned_by_current_user(
        &metadata,
        &format!("Decision signing key at {}", key_path.display()),
    )?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.mode() & 0o077 != 0 {
            fs::set_permissions(key_path, fs::Permissions::from_mode(0o600)).map_err(|source| {
                DecisionSigningError::io("setting key permissions", key_path, source)
            })?;
            metadata = fs::symlink_metadata(key_path).map_err(|source| {
                DecisionSigningError::io("re-inspecting key", key_path, source)
            })?;
            if !metadata.file_type().is_file() {
                return Err(DecisionSigningError::KeyNotRegularFile {
                    path: key_path.to_path_buf(),
                });
            }
            assert_owned_by_current_user(
                &metadata,
                &format!("Decision signing key at {}", key_path.display()),
            )?;
            if metadata.mode() & 0o077 != 0 {
                return Err(DecisionSigningError::KeyPermissions {
                    path: key_path.to_path_buf(),
                });
            }
        }
    }
    Ok(())
}

fn enforce_secrets_directory_permissions(directory: &Path) -> Result<(), DecisionSigningError> {
    let mut metadata = fs::symlink_metadata(directory).map_err(|source| {
        DecisionSigningError::io("inspecting secrets directory", directory, source)
    })?;
    if !metadata.file_type().is_dir() {
        return Err(DecisionSigningError::SecretsPathNotDirectory {
            path: directory.to_path_buf(),
        });
    }
    assert_owned_by_current_user(
        &metadata,
        &format!(
            "Decision signing secrets directory at {}",
            directory.display()
        ),
    )?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.mode() & 0o077 != 0 {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(
                |source| {
                    DecisionSigningError::io(
                        "setting secrets directory permissions",
                        directory,
                        source,
                    )
                },
            )?;
            metadata = fs::symlink_metadata(directory).map_err(|source| {
                DecisionSigningError::io("re-inspecting secrets directory", directory, source)
            })?;
            if !metadata.file_type().is_dir() {
                return Err(DecisionSigningError::SecretsPathNotDirectory {
                    path: directory.to_path_buf(),
                });
            }
            assert_owned_by_current_user(
                &metadata,
                &format!(
                    "Decision signing secrets directory at {}",
                    directory.display()
                ),
            )?;
            if metadata.mode() & 0o077 != 0 {
                return Err(DecisionSigningError::DirectoryPermissions {
                    path: directory.to_path_buf(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn assert_owned_by_current_user(
    metadata: &Metadata,
    description: &str,
) -> Result<(), DecisionSigningError> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(DecisionSigningError::WrongOwner {
            description: description.to_string(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn assert_owned_by_current_user(
    _metadata: &Metadata,
    _description: &str,
) -> Result<(), DecisionSigningError> {
    Ok(())
}

fn temporary_key_path(key_path: &Path) -> PathBuf {
    let mut random_suffix = [0_u8; 8];
    OsRng.fill_bytes(&mut random_suffix);
    let file_name = key_path.file_name().map_or_else(
        || "decision-signing.key".into(),
        |name| name.to_string_lossy(),
    );
    key_path.with_file_name(format!(
        "{file_name}.{}.{}.tmp",
        std::process::id(),
        hex::encode(random_suffix)
    ))
}
