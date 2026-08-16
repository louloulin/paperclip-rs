//! Pure SQL safety validators for plugin database access.
//!
//! Mirrors Node `services/plugin-database.ts::validatePluginMigrationStatement`,
//! `validatePluginRuntimeQuery`, and `validatePluginRuntimeExecute`.
//!
//! These functions are pure: no IO, no DB. They accept a SQL string and
//! reject it (with a structured error) when it would let a plugin reach
//! outside its allowed namespace, run a banned statement, or skip identifier
//! qualification.

use crate::namespace::assert_identifier;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Reason codes for SQL validation rejections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlSafetyCode {
    BannedStatement,
    DestructiveMigration,
    MigrationDeletesData,
    NotDdlOrBackfill,
    MissingQualifiedObjectRef,
    SchemaOutsideNamespace,
    PublicTableNotWhitelisted,
    PublicMutation,
    RuntimeMutationInQuery,
    RuntimeNotSelect,
    RuntimeNotMutation,
    RuntimeDdlInExecute,
    RuntimeExecuteSchemaMismatch,
    RuntimeExecuteReferencesOtherSchema,
    NotSingleStatement,
}

#[derive(Debug, thiserror::Error)]
#[error("plugin SQL rejected ({code:?}): {message}")]
pub struct SqlSafetyError {
    pub code: SqlSafetyCode,
    pub message: String,
}
impl SqlSafetyError {
    fn new(code: SqlSafetyCode, message: impl Into<String>) -> Self { Self { code, message: message.into() } }
    pub fn code(&self) -> SqlSafetyCode { self.code }
}

/// A qualified reference extracted from a SQL statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedRef {
    pub keyword: String,
    pub schema: String,
    pub table: String,
}

static FROM_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)\bfrom\s+\"?(?P<s>[A-Za-z_][A-Za-z0-9_]*)\"?\s*\.\s*\"?(?P<t>[A-Za-z_][A-Za-z0-9_]*)"#).unwrap());
static JOIN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)\bjoin\s+\"?(?P<s>[A-Za-z_][A-Za-z0-9_]*)\"?\s*\.\s*\"?(?P<t>[A-Za-z_][A-Za-z0-9_]*)"#).unwrap());
static REFERENCES_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)\breferences\s+\"?(?P<s>[A-Za-z_][A-Za-z0-9_]*)\"?\s*\.\s*\"?(?P<t>[A-Za-z_][A-Za-z0-9_]*)"#).unwrap());
static INTO_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)\binto\s+\"?(?P<s>[A-Za-z_][A-Za-z0-9_]*)\"?\s*\.\s*\"?(?P<t>[A-Za-z_][A-Za-z0-9_]*)"#).unwrap());
static UPDATE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)\bupdate\s+\"?(?P<s>[A-Za-z_][A-Za-z0-9_]*)\"?\s*\.\s*\"?(?P<t>[A-Za-z_][A-Za-z0-9_]*)"#).unwrap());
static DDL_TABLE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)\b(?:alter\s+table|create\s+table|create\s+view|drop\s+table|truncate\s+table)\s+(?:ifs+(?:nots+)?existss+)?\"?(?P<s>[A-Za-z_][A-Za-z0-9_]*)\"?\s*\.\s*\"?(?P<t>[A-Za-z_][A-Za-z0-9_]*)"#).unwrap());
static CREATE_INDEX_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)\bcreate\s+(?:unique\s+)?index(?:\s+concurrently)?\s+(?:if\s+not\s+exists\s+)?\"?[A-Za-z_][A-Za-z0-9_]*\"?\s+on\s+\"?(?P<s>[A-Za-z_][A-Za-z0-9_]*)\"?\s*\.\s*\"?(?P<t>[A-Za-z_][A-Za-z0-9_]*)"#).unwrap());

/// Extract every qualified `schema.table` reference from `statement`.
pub fn extract_qualified_refs(statement: &str) -> Vec<QualifiedRef> {
    let mut refs: Vec<QualifiedRef> = Vec::new();
    for (keyword, re) in &[
        ("from", &*FROM_RE),
        ("join", &*JOIN_RE),
        ("references", &*REFERENCES_RE),
        ("into", &*INTO_RE),
        ("update", &*UPDATE_RE),
    ] {
        for cap in re.captures_iter(statement) {
            if let (Some(s), Some(t)) = (cap.name("s"), cap.name("t")) {
                refs.push(QualifiedRef {
                    keyword: keyword.to_string(),
                    schema: s.as_str().to_string(),
                    table: t.as_str().to_string(),
                });
            }
        }
    }
    for cap in DDL_TABLE_RE.captures_iter(statement) {
        if let (Some(s), Some(t)) = (cap.name("s"), cap.name("t")) {
            let mat = cap.get(0).map(|m| m.as_str().to_lowercase()).unwrap_or_default();
            let kw = if mat.starts_with("alter table") {
                "alter table"
            } else if mat.starts_with("create table") {
                "create table"
            } else if mat.starts_with("create view") {
                "create view"
            } else if mat.starts_with("drop table") {
                "drop table"
            } else {
                "truncate table"
            };
            refs.push(QualifiedRef {
                keyword: kw.to_string(),
                schema: s.as_str().to_string(),
                table: t.as_str().to_string(),
            });
        }
    }
    for cap in CREATE_INDEX_RE.captures_iter(statement) {
        if let (Some(s), Some(t)) = (cap.name("s"), cap.name("t")) {
            refs.push(QualifiedRef {
                keyword: "create index".to_string(),
                schema: s.as_str().to_string(),
                table: t.as_str().to_string(),
            });
        }
    }
    refs
}

static BANNED_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)\bgrant\b").unwrap(),
        Regex::new(r"(?i)\brevoke\b").unwrap(),
        Regex::new(r"(?i)\btruncate\b").unwrap(),
        Regex::new(r"(?i)\bcopy\b").unwrap(),
        Regex::new(r"(?i)\bcall\b").unwrap(),
        Regex::new(r"(?i)\bdo\s+(?:\$\$|language\b)").unwrap(),
    ]
});

fn assert_no_banned_sql(statement: &str) -> Result<(), SqlSafetyError> {
    let normalised = normalise_sql(statement);
    for pat in BANNED_PATTERNS.iter() {
        if pat.is_match(&normalised) {
            return Err(SqlSafetyError::new(
                SqlSafetyCode::BannedStatement,
                "Plugin SQL contains a disallowed statement or clause",
            ));
        }
    }
    Ok(())
}

fn assert_allowed_public_read(
    r: &QualifiedRef,
    allowed_core_read_tables: &[String],
) -> Result<(), SqlSafetyError> {
    if r.schema != "public" {
        return Ok(());
    }
    if !allowed_core_read_tables.iter().any(|t| t == &r.table) {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::PublicTableNotWhitelisted,
            format!("Plugin SQL references public.{}, which is not whitelisted", r.table),
        ));
    }
    if !matches!(r.keyword.as_str(), "from" | "join" | "references") {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::PublicMutation,
            format!("Plugin SQL cannot mutate or define objects in public.{}", r.table),
        ));
    }
    Ok(())
}

/// Validate a plugin-authored migration statement.
pub fn validate_plugin_migration_statement(
    statement: &str,
    namespace: &str,
    core_read_tables: &[String],
) -> Result<(), SqlSafetyError> {
    assert_identifier(namespace, "namespace")
        .map_err(|e| SqlSafetyError::new(SqlSafetyCode::SchemaOutsideNamespace, e.to_string()))?;
    assert_no_banned_sql(statement)?;
    let normalized = normalise_sql(statement);
    if normalized.starts_with("drop ") || normalized.starts_with("truncate ") {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::DestructiveMigration,
            "Destructive plugin migrations are not allowed in Phase 1",
        ));
    }
    if normalized.contains("delete from") {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::MigrationDeletesData,
            "Plugin migrations cannot delete data",
        ));
    }
    let ddl_or_backfill_allowed = normalized.starts_with("create ")
        || normalized.starts_with("alter ")
        || normalized.starts_with("comment ")
        || normalized.starts_with("insert into ")
        || normalized.starts_with("update ")
        || (normalized.starts_with("with ")
            && (normalized.contains("insert into ") || normalized.contains("update ")));
    if !ddl_or_backfill_allowed {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::NotDdlOrBackfill,
            "Plugin migrations may contain DDL or namespace-scoped backfill statements only",
        ));
    }
    let refs = extract_qualified_refs(statement);
    if refs.is_empty() && !normalized.starts_with("comment ") {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::MissingQualifiedObjectRef,
            "Plugin migration objects must use fully qualified schema names",
        ));
    }
    let object_ref_keywords = [
        "alter table",
        "create index",
        "create table",
        "create view",
        "drop table",
        "into",
        "truncate table",
        "update",
    ];
    let has_qualified_object_ref = refs
        .iter()
        .any(|r| object_ref_keywords.contains(&r.keyword.as_str()));
    if !has_qualified_object_ref && !normalized.starts_with("comment ") {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::MissingQualifiedObjectRef,
            "Plugin migration objects must use fully qualified schema names",
        ));
    }
    for r in &refs {
        if r.schema == namespace {
            continue;
        }
        if r.schema == "public" {
            assert_allowed_public_read(r, core_read_tables)?;
            continue;
        }
        return Err(SqlSafetyError::new(
            SqlSafetyCode::SchemaOutsideNamespace,
            format!("Plugin SQL references schema {:?} outside namespace {:?}", r.schema, namespace),
        ));
    }
    Ok(())
}

/// Validate a plugin runtime `db.query` SELECT-style statement.
pub fn validate_plugin_runtime_query(
    query: &str,
    namespace: &str,
    core_read_tables: &[String],
) -> Result<(), SqlSafetyError> {
    assert_identifier(namespace, "namespace")
        .map_err(|e| SqlSafetyError::new(SqlSafetyCode::SchemaOutsideNamespace, e.to_string()))?;
    let statements = split_sql_statements(query);
    if statements.len() != 1 {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::NotSingleStatement,
            "Plugin runtime SQL must contain exactly one statement",
        ));
    }
    let statement = &statements[0];
    assert_no_banned_sql(statement)?;
    let normalized = normalise_sql(statement);
    if !normalized.starts_with("select ") && !normalized.starts_with("with ") {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::RuntimeNotSelect,
            "ctx.db.query only allows SELECT statements",
        ));
    }
    let mutation_keywords = ["insert ", "delete from", "alter ", "create ", "drop ", "truncate"];
    for kw in &mutation_keywords {
        if normalized.contains(kw) {
            return Err(SqlSafetyError::new(
                SqlSafetyCode::RuntimeMutationInQuery,
                "ctx.db.query cannot contain mutation or DDL keywords",
            ));
        }
    }
    for r in extract_qualified_refs(statement) {
        if r.schema == namespace {
            continue;
        }
        if r.schema == "public" {
            assert_allowed_public_read(&r, core_read_tables)?;
            continue;
        }
        return Err(SqlSafetyError::new(
            SqlSafetyCode::SchemaOutsideNamespace,
            format!("ctx.db.query cannot read schema {:?}", r.schema),
        ));
    }
    Ok(())
}

/// Validate a plugin runtime `db.execute` INSERT/UPDATE/DELETE statement.
pub fn validate_plugin_runtime_execute(
    query: &str,
    namespace: &str,
) -> Result<(), SqlSafetyError> {
    assert_identifier(namespace, "namespace")
        .map_err(|e| SqlSafetyError::new(SqlSafetyCode::SchemaOutsideNamespace, e.to_string()))?;
    let statements = split_sql_statements(query);
    if statements.len() != 1 {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::NotSingleStatement,
            "Plugin runtime SQL must contain exactly one statement",
        ));
    }
    let statement = &statements[0];
    assert_no_banned_sql(statement)?;
    let normalized = normalise_sql(statement);
    let starts_mutation = normalized.starts_with("insert into ")
        || normalized.starts_with("update ")
        || normalized.starts_with("delete from ");
    if !starts_mutation {
        return Err(SqlSafetyError::new(
            SqlSafetyCode::RuntimeNotMutation,
            "ctx.db.execute only allows INSERT, UPDATE, or DELETE",
        ));
    }
    let ddl_keywords = ["alter ", "create ", "drop ", "truncate"];
    for kw in &ddl_keywords {
        if normalized.contains(kw) {
            return Err(SqlSafetyError::new(
                SqlSafetyCode::RuntimeDdlInExecute,
                "ctx.db.execute cannot contain DDL keywords",
            ));
        }
    }
    let refs = extract_qualified_refs(statement);
    let target = refs
        .iter()
        .find(|r| matches!(r.keyword.as_str(), "into" | "update" | "from"));
    match target {
        Some(t) if t.schema == namespace => {}
        Some(t) => {
            return Err(SqlSafetyError::new(
                SqlSafetyCode::RuntimeExecuteSchemaMismatch,
                format!("ctx.db.execute target must be inside plugin namespace {:?}; got {:?}", namespace, t.schema),
            ));
        }
        None => {
            return Err(SqlSafetyError::new(
                SqlSafetyCode::RuntimeExecuteSchemaMismatch,
                format!("ctx.db.execute target must be inside plugin namespace {:?}", namespace),
            ));
        }
    }
    for r in &refs {
        if r.schema != namespace {
            return Err(SqlSafetyError::new(
                SqlSafetyCode::RuntimeExecuteReferencesOtherSchema,
                "ctx.db.execute cannot reference public or other non-plugin schemas",
            ));
        }
    }
    Ok(())
}

/// Split a multi-statement SQL input into its constituent statements,
/// respecting single/double quotes and `--` / `/* */` comments.
pub fn split_sql_statements(input: &str) -> Vec<String> {
    let mut statements: Vec<String> = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut start = 0usize;
    let mut quote: Option<char> = None;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if line_comment {
            if c == '\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_comment {
            if c == '*' && next == Some('/') {
                block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                if next == Some(q) {
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '-' && next == Some('-') {
            line_comment = true;
            i += 2;
            continue;
        }
        if c == '/' && next == Some('*') {
            block_comment = true;
            i += 2;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            i += 1;
            continue;
        }
        if c == ';' {
            let statement: String = chars[start..i].iter().collect();
            let trimmed = statement.trim();
            if !trimmed.is_empty() {
                statements.push(trimmed.to_string());
            }
            i += 1;
            start = i;
            continue;
        }
        i += 1;
    }
    let trailing: String = chars[start..].iter().collect();
    let trimmed = trailing.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }
    statements
}

/// Strip SQL string literals and comments so the result is safe to feed into
/// keyword/regex scanners.
fn strip_sql_for_keyword_scan(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '\'' {
            out.push_str("''");
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1).copied() == Some('\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == '"' {
            out.push_str("\"\"");
            i += 1;
            while i < chars.len() {
                if chars[i] == '"' {
                    if chars.get(i + 1).copied() == Some('"') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == '-' && next == Some('-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            if i + 1 < chars.len() {
                i += 2;
            } else {
                i = chars.len();
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Collapse whitespace and lowercase for keyword matching.
fn normalise_sql(input: &str) -> String {
    strip_sql_for_keyword_scan(input)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

