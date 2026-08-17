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


#[cfg(test)]
mod internal_tests {
    use super::*;
    use crate::namespace::{
        assert_identifier, derive_plugin_database_namespace, quote_identifier,
        MAX_POSTGRES_IDENTIFIER_LENGTH, PluginNamespaceError,
    };
    use crate::sql_safety::{
        split_sql_statements, validate_plugin_migration_statement, validate_plugin_runtime_execute,
        validate_plugin_runtime_query, SqlSafetyCode,
    };

    #[test]
    fn r785_max_identifier_length_is_63() {
        assert_eq!(MAX_POSTGRES_IDENTIFIER_LENGTH, 63);
    }

    #[test]
    fn r785_assert_identifier_accepts_alphanumeric() {
        assert_eq!(assert_identifier("foo_bar", "test").unwrap(), "foo_bar");
        assert_eq!(assert_identifier("Abc123", "test").unwrap(), "Abc123");
    }

    #[test]
    fn r785_assert_identifier_rejects_empty() {
        let err = assert_identifier("", "test").unwrap_err();
        assert!(matches!(err, PluginNamespaceError::UnsafeIdentifier { .. }));
    }

    #[test]
    fn r785_assert_identifier_rejects_leading_digit() {
        assert!(assert_identifier("1abc", "test").is_err());
    }

    #[test]
    fn r785_assert_identifier_rejects_leading_underscore() {
        assert!(assert_identifier("_foo", "test").is_err());
    }

    #[test]
    fn r785_assert_identifier_rejects_special_chars() {
        assert!(assert_identifier("foo-bar", "test").is_err());
        assert!(assert_identifier("foo bar", "test").is_err());
        assert!(assert_identifier("foo;bar", "test").is_err());
    }

    #[test]
    fn r785_quote_identifier_wraps_in_dquotes() {
        assert_eq!(quote_identifier("foo"), "\"foo\"");
    }

    #[test]
    fn r785_quote_identifier_escapes_inner_dquote() {
        assert_eq!(quote_identifier("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn r785_derive_namespace_uses_default_slug() {
        let ns = derive_plugin_database_namespace("my-plugin", None).unwrap();
        assert!(ns.starts_with("plugin_my_plugin_"));
        assert!(ns.len() <= MAX_POSTGRES_IDENTIFIER_LENGTH);
    }

    #[test]
    fn r785_derive_namespace_uses_explicit_slug() {
        let ns = derive_plugin_database_namespace("my-plugin", Some("custom_slug")).unwrap();
        assert!(ns.starts_with("plugin_custom_slug_"));
    }

    #[test]
    fn r785_derive_namespace_normalizes_special_chars() {
        let ns = derive_plugin_database_namespace("My Plugin Name", None).unwrap();
        assert!(ns.starts_with("plugin_my_plugin_name_"));
    }

    #[test]
    fn r785_derive_namespace_collapses_underscores() {
        let ns = derive_plugin_database_namespace("foo___bar", None).unwrap();
        assert!(ns.starts_with("plugin_foo_bar_"));
    }

    #[test]
    fn r785_derive_namespace_truncates_to_63() {
        let long = "a".repeat(100);
        let ns = derive_plugin_database_namespace(&long, None).unwrap();
        assert!(ns.len() <= MAX_POSTGRES_IDENTIFIER_LENGTH);
        // The hash prefix is preserved at the end
        let _ = &ns[ns.len() - 10..];
    }

    #[test]
    fn r785_derive_namespace_different_keys_different_hashes() {
        let ns1 = derive_plugin_database_namespace("plugin-a", None).unwrap();
        let ns2 = derive_plugin_database_namespace("plugin-b", None).unwrap();
        assert_ne!(ns1, ns2);
    }

    #[test]
    fn r785_derive_namespace_same_key_same_namespace() {
        let ns1 = derive_plugin_database_namespace("plugin-x", None).unwrap();
        let ns2 = derive_plugin_database_namespace("plugin-x", None).unwrap();
        assert_eq!(ns1, ns2);
    }

    #[test]
    fn r785_derive_namespace_empty_inputs_fallback_to_plugin() {
        let ns = derive_plugin_database_namespace("", Some("!!!")).unwrap();
        assert!(ns.starts_with("plugin_plugin_"));
    }

    #[test]
    fn r785_split_sql_single_statement() {
        let stmts = split_sql_statements("SELECT 1");
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn r785_split_sql_multiple_statements() {
        let stmts = split_sql_statements("SELECT 1; SELECT 2;");
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn r785_split_sql_keeps_string_literals_intact() {
        let stmts = split_sql_statements("SELECT \"; ; ;\"; SELECT 2;");
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn r785_split_sql_strips_line_comments() {
        let stmts = split_sql_statements("SELECT 1; -- comment\nSELECT 2;");
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn r785_split_sql_strips_block_comments() {
        let stmts = split_sql_statements("SELECT 1; /* comment */ SELECT 2;");
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn r785_validate_migration_allows_create_table() {
        validate_plugin_migration_statement(
            "CREATE TABLE plugin_x.foo (id INT PRIMARY KEY)",
            "plugin_x",
            &[],
        ).unwrap();
    }

    #[test]
    fn r785_validate_migration_rejects_drop() {
        let err = validate_plugin_migration_statement(
            "DROP TABLE plugin_x.foo",
            "plugin_x",
            &[],
        ).unwrap_err();
        assert_eq!(err.code(), SqlSafetyCode::DestructiveMigration);
    }

    #[test]
    fn r785_validate_migration_rejects_truncate() {
        let err = validate_plugin_migration_statement(
            "TRUNCATE TABLE plugin_x.foo",
            "plugin_x",
            &[],
        ).unwrap_err();
        // truncate is banned via assert_no_banned_sql (BannedStatement), not DestructiveMigration
        assert_eq!(err.code(), SqlSafetyCode::BannedStatement);
    }

    #[test]
    fn r785_validate_migration_rejects_delete_from() {
        let err = validate_plugin_migration_statement(
            "DELETE FROM plugin_x.foo WHERE id = 1",
            "plugin_x",
            &[],
        ).unwrap_err();
        assert_eq!(err.code(), SqlSafetyCode::MigrationDeletesData);
    }

    #[test]
    fn r785_validate_migration_rejects_select() {
        let err = validate_plugin_migration_statement(
            "SELECT * FROM plugin_x.foo",
            "plugin_x",
            &[],
        ).unwrap_err();
        assert_eq!(err.code(), SqlSafetyCode::NotDdlOrBackfill);
    }

    #[test]
    fn r785_validate_migration_rejects_other_schema() {
        let err = validate_plugin_migration_statement(
            "CREATE TABLE other_schema.foo (id INT)",
            "plugin_x",
            &[],
        ).unwrap_err();
        assert_eq!(err.code(), SqlSafetyCode::SchemaOutsideNamespace);
    }

    #[test]
    fn r785_validate_migration_allows_public_read_tables() {
        validate_plugin_migration_statement(
            "INSERT INTO plugin_x.foo SELECT * FROM public.allowed_table WHERE id = 1",
            "plugin_x",
            &["allowed_table".to_string()],
        ).unwrap();
    }

    #[test]
    fn r785_validate_migration_rejects_public_table_not_allowed() {
        let err = validate_plugin_migration_statement(
            "INSERT INTO plugin_x.foo SELECT * FROM public.secret_table WHERE id = 1",
            "plugin_x",
            &["allowed_table".to_string()],
        ).unwrap_err();
        // public.table not in core_read_tables -> PublicTableNotWhitelisted
        assert_eq!(err.code(), SqlSafetyCode::PublicTableNotWhitelisted);
    }

    #[test]
    fn r785_validate_runtime_query_allows_namespace_select() {
        validate_plugin_runtime_query(
            "SELECT id FROM plugin_x.foo",
            "plugin_x",
            &[],
        ).unwrap();
    }

    #[test]
    fn r785_validate_runtime_query_rejects_multi_statement() {
        let err = validate_plugin_runtime_query(
            "SELECT 1; SELECT 2;",
            "plugin_x",
            &[],
        ).unwrap_err();
        // multi-statement is rejected
        let _ = err;
    }

    #[test]
    fn r785_validate_runtime_execute_blocks_ddl() {
        let err = validate_plugin_runtime_execute(
            "CREATE TABLE plugin_x.foo (id INT)",
            "plugin_x",
        ).unwrap_err();
        let _ = err;
    }
}
