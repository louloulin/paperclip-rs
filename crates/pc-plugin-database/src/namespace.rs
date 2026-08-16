//! Plugin SQL namespace derivation.
//!
//! Mirrors Node `services/plugin-database.ts::derivePluginDatabaseNamespace`.
//! The plugin runtime asks the host for a per-plugin PostgreSQL schema where
//! it can freely create its own tables. We derive the namespace from the
//! plugin identifier (or an explicit slug) plus a short hash so two plugins
//! with similar names never collide and so a plugin cannot impersonate
//! another's schema.
//!
//! Pure: no IO, no DB.

use sha2::{Digest, Sha256};

/// Hard PostgreSQL identifier ceiling. NAMEDATALEN is 64 in modern Postgres
/// but the leading byte is reserved for length so the practical limit is 63.
pub const MAX_POSTGRES_IDENTIFIER_LENGTH: usize = 63;

/// Validate that `value` is a safe identifier that can be safely quoted and
/// embedded into a SQL identifier without escaping concerns.
///
/// Mirrors Node `assertIdentifier`.
pub fn assert_identifier(value: &str, label: &str) -> Result<String, PluginNamespaceError> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        || !value.chars().next().unwrap().is_ascii_alphabetic()
        || value.chars().next().unwrap() == '_'
    {
        return Err(PluginNamespaceError::UnsafeIdentifier {
            label: label.to_string(),
            value: value.to_string(),
        });
    }
    Ok(value.to_string())
}

/// Quote an already-validated identifier for safe embedding into a SQL string.
pub fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Derive a per-plugin PostgreSQL namespace name.
///
/// Returns `plugin_<slug>_<hash>` where `slug` is the lowercased,
/// alphanumeric+underscore-only form of `namespace_slug` (or `plugin_key`
/// when `namespace_slug` is `None`/empty) and `hash` is the first 10 hex
/// chars of the SHA-256 of `plugin_key`.
///
/// The result is truncated to fit within the 63-char Postgres identifier
/// limit. Two different plugin keys always produce different namespaces
/// (different hash prefix), and two similar names produce different hashes.
pub fn derive_plugin_database_namespace(
    plugin_key: &str,
    namespace_slug: Option<&str>,
) -> Result<String, PluginNamespaceError> {
    let slug_input = namespace_slug
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(plugin_key);

    let mut slug = slug_input.to_lowercase();
    slug = slug
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>();
    // Collapse runs of underscores.
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_us = false;
    for c in slug.chars() {
        if c == '_' {
            if !prev_us {
                collapsed.push(c);
            }
            prev_us = true;
        } else {
            collapsed.push(c);
            prev_us = false;
        }
    }
    // Strip leading/trailing underscores.
    let trimmed = collapsed.trim_matches('_').to_string();
    let slug = if trimmed.is_empty() {
        "plugin".to_string()
    } else {
        trimmed.chars().take(36).collect::<String>()
    };

    let mut hasher = Sha256::new();
    hasher.update(plugin_key.as_bytes());
    let hash_hex = format!("{:x}", hasher.finalize());
    let hash_prefix = &hash_hex[..10.min(hash_hex.len())];

    let mut namespace = format!("plugin_{}_{}", slug, hash_prefix);
    if namespace.len() > MAX_POSTGRES_IDENTIFIER_LENGTH {
        namespace.truncate(MAX_POSTGRES_IDENTIFIER_LENGTH);
    }
    Ok(namespace)
}

#[derive(Debug, thiserror::Error)]
pub enum PluginNamespaceError {
    #[error("Unsafe SQL identifier for {label}: {value:?}")]
    UnsafeIdentifier { label: String, value: String },
    #[error("Empty plugin key")]
    EmptyPluginKey,
}

impl PluginNamespaceError {
    pub fn empty_plugin_key() -> Self {
        PluginNamespaceError::EmptyPluginKey
    }
}
