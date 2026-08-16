//! Paperclip plugin SQL namespace derivation + pure SQL safety validators.
//!
//! This crate is the Rust 1:1 parity of Node `services/plugin-database.ts`
//! (the pure-function subset; the DB-coupled `pluginDatabaseService` factory
//! lives in `pc-plugin-host`). The functions here are all pure: no IO,
//! no DB. They are suitable for use by both the host and the install
//! guard at the API boundary.
//!
//! ## Modules
//!
//! - [`namespace`] - derive a stable, collision-safe schema name per plugin
//! - [`sql_safety`] - validate that plugin-authored SQL stays inside its
//!   allowed namespace, runs only permitted statements, and never escapes
//!   into the public schema.
//!
//! ## Usage
//!
//! ```rust
//! use pc_plugin_database::{
//!     derive_plugin_database_namespace,
//!     validate_plugin_migration_statement,
//! };
//!
//! let ns = derive_plugin_database_namespace("my-plugin", None).unwrap();
//! assert!(ns.starts_with("plugin_"));
//!
//! validate_plugin_migration_statement(
//!     "CREATE TABLE plugin_x.foo (id int)",
//!     "plugin_x",
//!     &[],
//! ).unwrap();
//! ```

pub mod namespace;
pub mod sql_safety;

pub use namespace::{
    assert_identifier, derive_plugin_database_namespace, quote_identifier, PluginNamespaceError,
    MAX_POSTGRES_IDENTIFIER_LENGTH,
};
pub use sql_safety::{
    extract_qualified_refs, split_sql_statements, validate_plugin_migration_statement,
    validate_plugin_runtime_execute, validate_plugin_runtime_query, QualifiedRef, SqlSafetyCode,
    SqlSafetyError,
};
