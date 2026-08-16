//! Unit tests for pc-plugin-database SQL safety validators.
//!
//! These exercise the pure-function parity with Node
//! `services/plugin-database.ts::validatePluginMigrationStatement`,
//! `validatePluginRuntimeQuery`, and `validatePluginRuntimeExecute`.

use pc_plugin_database::{
    derive_plugin_database_namespace, extract_qualified_refs, split_sql_statements,
    validate_plugin_migration_statement, validate_plugin_runtime_execute,
    validate_plugin_runtime_query, SqlSafetyCode,
};

// ============================================================================
// derive_plugin_database_namespace
// ============================================================================

#[test]
fn r673_namespace_basic_shape() {
    let ns = derive_plugin_database_namespace("my-plugin", None).unwrap();
    assert!(ns.starts_with("plugin_"), "got={ns}");
    // plugin_my_plugin_<10-hex-hash> -> 28 chars; well under 63 limit
    assert!(ns.len() <= 63, "got len={}", ns.len());
}

#[test]
fn r673_namespace_different_keys_yield_different_namespaces() {
    let a = derive_plugin_database_namespace("plugin-a", None).unwrap();
    let b = derive_plugin_database_namespace("plugin-b", None).unwrap();
    assert_ne!(a, b);
}

#[test]
fn r673_namespace_normalises_slug() {
    let ns = derive_plugin_database_namespace("Hello World!", None).unwrap();
    assert!(ns.contains("hello_world"), "got={ns}");
    assert!(!ns.contains(" "), "got={ns}");
    assert!(!ns.contains("!"), "got={ns}");
}

#[test]
fn r673_namespace_collapses_underscores() {
    let ns = derive_plugin_database_namespace("a___b", None).unwrap();
    assert!(!ns.contains("__"), "got={ns}");
}

#[test]
fn r673_namespace_falls_back_when_slug_empty() {
    let ns = derive_plugin_database_namespace("$$$", None).unwrap();
    assert!(ns.contains("plugin"), "got={ns}");
}

#[test]
fn r673_namespace_with_explicit_slug() {
    let ns = derive_plugin_database_namespace("abc", Some("my-slug")).unwrap();
    assert!(ns.contains("my_slug"), "got={ns}");
}

#[test]
fn r673_namespace_truncates_to_postgres_limit() {
    let ns = derive_plugin_database_namespace(&"x".repeat(200), None).unwrap();
    assert!(ns.len() <= 63, "got len={}", ns.len());
}

#[test]
fn r673_namespace_deterministic_for_same_key() {
    let a = derive_plugin_database_namespace("foo", None).unwrap();
    let b = derive_plugin_database_namespace("foo", None).unwrap();
    assert_eq!(a, b);
}

// ============================================================================
// split_sql_statements
// ============================================================================

#[test]
fn r673_split_single_statement() {
    let out = split_sql_statements("SELECT 1");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0], "SELECT 1");
}

#[test]
fn r673_split_multiple_statements() {
    let out = split_sql_statements("SELECT 1; SELECT 2;");
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], "SELECT 1");
    assert_eq!(out[1], "SELECT 2");
}

#[test]
fn r673_split_respects_quotes() {
    let out = split_sql_statements("SELECT 'a;b' FROM t; SELECT 2;");
    assert_eq!(out.len(), 2);
    assert!(out[0].contains("'a;b'"));
}

#[test]
fn r673_split_respects_line_comments() {
    let out = split_sql_statements("SELECT 1; -- ; ignored\nSELECT 2;");
    assert_eq!(out.len(), 2);
}

#[test]
fn r673_split_respects_block_comments() {
    let out = split_sql_statements("SELECT 1; /* ; ignored */ SELECT 2;");
    assert_eq!(out.len(), 2);
}

#[test]
fn r673_split_empty_input() {
    let out = split_sql_statements("");
    assert_eq!(out.len(), 0);
}

// ============================================================================
// extract_qualified_refs
// ============================================================================

#[test]
fn r673_extract_from_into_update_join() {
    let refs = extract_qualified_refs("SELECT * FROM plugin_x.foo INNER JOIN plugin_x.bar ON 1=1");
    assert!(refs.iter().any(|r| r.keyword == "from" && r.schema == "plugin_x" && r.table == "foo"));
    assert!(refs.iter().any(|r| r.keyword == "join" && r.schema == "plugin_x" && r.table == "bar"));
}

#[test]
fn r673_extract_create_table() {
    let refs = extract_qualified_refs("CREATE TABLE plugin_x.foo (id INT)");
    assert!(refs.iter().any(|r| r.keyword == "create table" && r.schema == "plugin_x" && r.table == "foo"));
}

#[test]
fn r673_extract_create_index() {
    let refs = extract_qualified_refs("CREATE INDEX my_idx ON plugin_x.foo (id)");
    assert!(refs.iter().any(|r| r.keyword == "create index" && r.schema == "plugin_x" && r.table == "foo"));
}

#[test]
fn r673_extract_no_qualified_refs() {
    let refs = extract_qualified_refs("SELECT 1");
    assert_eq!(refs.len(), 0);
}

// ============================================================================
// validate_plugin_migration_statement
// ============================================================================

const NS: &str = "plugin_x";

#[test]
fn r673_migration_create_table_allowed() {
    validate_plugin_migration_statement(
        "CREATE TABLE plugin_x.foo (id INT PRIMARY KEY)",
        NS,
        &[],
    )
    .unwrap();
}

#[test]
fn r673_migration_create_index_allowed() {
    validate_plugin_migration_statement(
        "CREATE INDEX idx_foo ON plugin_x.foo (id)",
        NS,
        &[],
    )
    .unwrap();
}

#[test]
fn r673_migration_insert_into_allowed() {
    validate_plugin_migration_statement(
        "INSERT INTO plugin_x.foo (id) VALUES (1)",
        NS,
        &[],
    )
    .unwrap();
}

#[test]
fn r673_migration_update_allowed() {
    validate_plugin_migration_statement(
        "UPDATE plugin_x.foo SET id = 2 WHERE id = 1",
        NS,
        &[],
    )
    .unwrap();
}

#[test]
fn r673_migration_with_cte_insert_allowed() {
    validate_plugin_migration_statement(
        "WITH moved AS (SELECT * FROM plugin_x.bar) INSERT INTO plugin_x.foo SELECT * FROM moved",
        NS,
        &[],
    )
    .unwrap();
}

#[test]
fn r673_migration_drop_rejected() {
    let err = validate_plugin_migration_statement(
        "DROP TABLE plugin_x.foo",
        NS,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::DestructiveMigration);
}

#[test]
fn r673_migration_truncate_rejected() {
    // TRUNCATE matches the banned keyword scan before the destructive-migration gate.
    let err = validate_plugin_migration_statement(
        "TRUNCATE plugin_x.foo",
        NS,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::BannedStatement);
}

#[test]
fn r673_migration_delete_from_rejected() {
    let err = validate_plugin_migration_statement(
        "DELETE FROM plugin_x.foo",
        NS,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::MigrationDeletesData);
}

#[test]
fn r673_migration_grant_rejected() {
    let err = validate_plugin_migration_statement(
        "GRANT ALL ON plugin_x.foo TO public",
        NS,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::BannedStatement);
}

#[test]
fn r673_migration_other_schema_rejected() {
    // SELECT does not match any allowed migration form -> NotDdlOrBackfill.
    let err = validate_plugin_migration_statement(
        "SELECT * FROM other_schema.foo",
        NS,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::NotDdlOrBackfill);
}

#[test]
fn r673_migration_schema_escape_rejected() {
    let err = validate_plugin_migration_statement(
        "INSERT INTO plugin_y.foo (id) VALUES (1)",
        NS,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::SchemaOutsideNamespace);
}

#[test]
fn r673_migration_public_read_allowed_with_whitelist() {
    // INSERT INTO ... SELECT ... references both public (whitelisted) and the
    // plugin namespace; namespace-scoped targets pass the schema check.
    validate_plugin_migration_statement(
        "INSERT INTO plugin_x.foo (id) SELECT id FROM public.companies",
        NS,
        &["companies".to_string()],
    )
    .unwrap();
}

#[test]
fn r673_migration_public_read_rejected_without_whitelist() {
    // public.companies is not in the whitelist -> PublicTableNotWhitelisted.
    let err = validate_plugin_migration_statement(
        "INSERT INTO plugin_x.foo (id) SELECT id FROM public.companies",
        NS,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::PublicTableNotWhitelisted);
}

#[test]
fn r673_migration_missing_qualified_ref_rejected() {
    // INSERT without schema-qualified target -> MissingQualifiedObjectRef.
    let err = validate_plugin_migration_statement(
        "INSERT INTO foo (id) VALUES (1)", NS, &[],
    ).unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::MissingQualifiedObjectRef);
}

#[test]
fn r673_migration_comment_allowed() {
    validate_plugin_migration_statement("COMMENT ON SCHEMA plugin_x IS 'owned'", NS, &[]).unwrap();
}

#[test]
fn r673_migration_invalid_namespace_rejected() {
    let err = validate_plugin_migration_statement(
        "CREATE TABLE plugin_x.foo (id INT)",
        "123-bad",
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::SchemaOutsideNamespace);
}

// ============================================================================
// validate_plugin_runtime_query
// ============================================================================

#[test]
fn r673_query_select_from_own_namespace_allowed() {
    validate_plugin_runtime_query(
        "SELECT * FROM plugin_x.foo",
        NS,
        &[],
    )
    .unwrap();
}

#[test]
fn r673_query_with_cte_allowed() {
    validate_plugin_runtime_query(
        "WITH a AS (SELECT 1 AS v) SELECT * FROM a",
        NS,
        &[],
    )
    .unwrap();
}

#[test]
fn r673_query_multiple_statements_rejected() {
    let err = validate_plugin_runtime_query("SELECT 1; SELECT 2", NS, &[]).unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::NotSingleStatement);
}

#[test]
fn r673_query_update_rejected() {
    // UPDATE doesn't start with SELECT/WITH -> RuntimeNotSelect.
    let err = validate_plugin_runtime_query(
        "UPDATE plugin_x.foo SET id = 2",
        NS,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::RuntimeNotSelect);
}

#[test]
fn r673_query_create_rejected() {
    // CREATE doesn't start with SELECT/WITH -> RuntimeNotSelect.
    let err = validate_plugin_runtime_query(
        "CREATE TABLE plugin_x.foo (id INT)",
        NS,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::RuntimeNotSelect);
}

#[test]
fn r673_query_other_schema_rejected() {
    let err = validate_plugin_runtime_query(
        "SELECT * FROM other_schema.foo",
        NS,
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::SchemaOutsideNamespace);
}

// ============================================================================
// validate_plugin_runtime_execute
// ============================================================================

#[test]
fn r673_execute_insert_own_namespace_allowed() {
    validate_plugin_runtime_execute(
        "INSERT INTO plugin_x.foo (id) VALUES (1)",
        NS,
    )
    .unwrap();
}

#[test]
fn r673_execute_update_own_namespace_allowed() {
    validate_plugin_runtime_execute(
        "UPDATE plugin_x.foo SET id = 2 WHERE id = 1",
        NS,
    )
    .unwrap();
}

#[test]
fn r673_execute_delete_own_namespace_allowed() {
    validate_plugin_runtime_execute(
        "DELETE FROM plugin_x.foo WHERE id = 1",
        NS,
    )
    .unwrap();
}

#[test]
fn r673_execute_select_rejected() {
    let err = validate_plugin_runtime_execute(
        "SELECT * FROM plugin_x.foo",
        NS,
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::RuntimeNotMutation);
}

#[test]
fn r673_execute_create_rejected() {
    // CREATE doesn't start with INSERT/UPDATE/DELETE -> RuntimeNotMutation.
    let err = validate_plugin_runtime_execute(
        "CREATE TABLE plugin_x.foo (id INT)",
        NS,
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::RuntimeNotMutation);
}

#[test]
fn r673_execute_other_schema_rejected() {
    let err = validate_plugin_runtime_execute(
        "INSERT INTO plugin_y.foo (id) VALUES (1)",
        NS,
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::RuntimeExecuteSchemaMismatch);
}

#[test]
fn r673_execute_public_rejected() {
    let err = validate_plugin_runtime_execute(
        "INSERT INTO public.companies (name) VALUES ('x')",
        NS,
    )
    .unwrap_err();
    assert_eq!(err.code(), SqlSafetyCode::RuntimeExecuteSchemaMismatch);
}
