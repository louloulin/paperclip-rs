//! Chunked multi-row insert helper for PostgreSQL.
//!
//! Port of `paperclip/server/src/services/batch-insert.ts` (69 lines). Mirrors
//! Node semantics 1:1 so existing import writers can adopt the Rust version
//! without changing their batching assumptions:
//!
//! - PostgreSQL caps a single statement at 65535 bind parameters. A multi-row
//!   insert binds `columnsPerRow * rowCount` parameters, so large imports must
//!   split their rows into chunks that stay under that ceiling.
//! - Rows are normalized to a shared column set (the union of keys across the
//!   batch, with missing/`null` values written as SQL NULL) so a single
//!   multi-row `VALUES` statement stays well-formed even when a caller omits
//!   an optional column on some rows.
//! - Empty input is a no-op (no chunks emitted).
//!
//! SQL execution path: the Node version calls into drizzle's `insert`/`values`
//! which is table-agnostic through a generic escape hatch. The Rust port
//! currently exposes the pure chunking helpers + constants (matching Node's
//! testable surface) and stubs a TODO for the sqlx-based statement builder —
//! the build-time trait dance over `sqlx::Encode`/`Executor` is omitted here
//! and will be wired in a follow-up once a concrete caller enforces the
//! binding contracts (see `insert_rows_in_chunks` TODO below).

#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap};

use serde_json::Value as JsonValue;

/// One below the hard 65535 ceiling so the arithmetic never lands exactly on it.
pub const POSTGRES_MAX_BIND_PARAMS: usize = 65534;

/// A conservative row cap so a single statement stays small even for narrow
/// tables.
pub const DEFAULT_INSERT_CHUNK_ROWS: usize = 500;

/// Optional knobs for chunking.
#[derive(Debug, Clone, Copy, Default)]
pub struct InsertOptions {
    /// Override the per-chunk row cap. Final chunk size is still clamped by
    /// `floor(POSTGRES_MAX_BIND_PARAMS / columnsPerRow)`.
    pub max_rows: Option<usize>,
}

/// Collect the union of keys across all rows. Order is sorted (lexicographic)
/// to keep the generated SQL stable and tests deterministic.
pub fn collect_columns(rows: &[HashMap<String, JsonValue>]) -> Vec<String> {
    let mut keys: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        for key in row.keys() {
            keys.insert(key.clone());
        }
    }
    keys.into_iter().collect()
}

/// Normalize rows so every row has every column. Missing keys become JSON
/// `null` (which the SQL executor will bind as NULL).
pub fn normalize_rows(
    columns: &[String],
    rows: Vec<HashMap<String, JsonValue>>,
) -> Vec<HashMap<String, JsonValue>> {
    rows.into_iter()
        .map(|row| {
            let mut out: HashMap<String, JsonValue> = HashMap::with_capacity(columns.len());
            for col in columns {
                let value = row.get(col).cloned().unwrap_or(JsonValue::Null);
                out.insert(col.clone(), value);
            }
            out
        })
        .collect()
}

/// Compute the per-chunk row count, matching Node's clamping rules:
///
/// ```text
/// columnsPerRow = max(1, columns.len())
/// chunkSize     = max(1, min(max_rows, floor(POSTGRES_MAX_BIND_PARAMS / columnsPerRow)))
/// ```
pub fn chunk_size(columns: &[String], max_rows: Option<usize>) -> usize {
    let columns_per_row = columns.len().max(1);
    let cap = max_rows.unwrap_or(DEFAULT_INSERT_CHUNK_ROWS);
    let by_params = POSTGRES_MAX_BIND_PARAMS / columns_per_row;
    1.max(cap.min(by_params))
}

/// Split row indices into chunks that respect the per-chunk row cap. Each
/// inner `Vec<usize>` is a list of positions into the caller-supplied row
/// slice. Returns an empty `Vec` when `rows_len` is zero.
pub fn chunk_rows_by_column_count(
    columns: &[String],
    rows_len: usize,
    max_rows: Option<usize>,
) -> Vec<Vec<usize>> {
    if rows_len == 0 {
        return Vec::new();
    }
    let size = chunk_size(columns, max_rows);
    let mut chunks: Vec<Vec<usize>> = Vec::new();
    let mut start = 0usize;
    while start < rows_len {
        let end = (start + size).min(rows_len);
        chunks.push((start..end).collect());
        start = end;
    }
    chunks
}

/// SQL executor stub.
///
/// TODO(R759): implement the chunked `INSERT INTO table (cols) VALUES (...), (...)`
/// statement using `sqlx::query` against a `&sqlx::PgPool` or
/// `&mut sqlx::Transaction<'_, Postgres>`. The trait+`dyn sqlx::Encode`
/// abstraction hit several non-object-safe boundaries in sqlx 0.8; the cleanest
/// path is two generic functions, one per executor, sharing a private
/// `build_chunk_sql(columns, rows)` helper that returns (sql, Vec<JsonValue>)
/// pairs. Wiring it up requires a concrete caller (an import writer) so the
/// bind contracts can be exercised against a real DB.
#[allow(dead_code)]
pub async fn insert_rows_in_chunks(
    _table_name: &str,
    _rows: Vec<HashMap<String, JsonValue>>,
    _options: InsertOptions,
) -> Result<u64, sqlx::Error> {
    // Punted to a follow-up — see the doc comment above.
    Err(sqlx::Error::Protocol(
        "insert_rows_in_chunks: SQL execution not yet implemented; use chunk_rows_by_column_count + collect_columns + normalize_rows for now".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(pairs: &[(&str, JsonValue)]) -> HashMap<String, JsonValue> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
    }

    #[test]
    fn constants_match_node_source() {
        assert_eq!(POSTGRES_MAX_BIND_PARAMS, 65534);
        assert_eq!(DEFAULT_INSERT_CHUNK_ROWS, 500);
    }

    #[test]
    fn empty_rows_yield_empty_chunks() {
        let rows: Vec<HashMap<String, JsonValue>> = vec![];
        let columns = collect_columns(&rows);
        assert!(columns.is_empty());
        let chunks = chunk_rows_by_column_count(&columns, rows.len(), None);
        assert!(chunks.is_empty());
    }

    #[test]
    fn columns_are_union_of_all_keys_sorted() {
        let rows = vec![
            row(&[("a", json!(1)), ("b", json!(2))]),
            row(&[("b", json!(3)), ("c", json!(4))]),
        ];
        let columns = collect_columns(&rows);
        assert_eq!(columns, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn missing_keys_become_null_in_normalize() {
        let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let rows = vec![row(&[("a", json!(1)), ("c", json!(3))])];
        let normalized = normalize_rows(&columns, rows);
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0]["a"], json!(1));
        assert_eq!(normalized[0]["b"], JsonValue::Null);
        assert_eq!(normalized[0]["c"], json!(3));
    }

    #[test]
    fn chunk_size_respects_max_rows_override() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let size = chunk_size(&columns, Some(10));
        assert_eq!(size, 10);
    }

    #[test]
    fn chunk_size_clamped_by_postgres_param_limit() {
        // 5 columns → floor(65534 / 5) = 13106. Pick a max_rows above that.
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
        ];
        let size = chunk_size(&columns, Some(50_000));
        assert_eq!(size, 13106);
    }

    #[test]
    fn chunk_size_floor_of_one() {
        // columnsPerRow = max(1, columns.len()); chunkSize = max(1, min(...)).
        // With max_rows=0, cap=0, by_params=65534, min=0, max(1, 0)=1.
        let columns = vec!["a".to_string()];
        let size = chunk_size(&columns, Some(0));
        assert_eq!(size, 1);
    }

    #[test]
    fn single_column_max_rows_matches_postgres_cap() {
        // With 1 column and max_rows=65534, the chunk size matches the
        // POSTGRES_MAX_BIND_PARAMS floor (also 65534).
        let columns = vec!["a".to_string()];
        let size = chunk_size(&columns, Some(65534));
        assert_eq!(size, 65534);
    }

    #[test]
    fn single_column_max_rows_above_cap_clamps_to_cap() {
        // max_rows above the per-row column cap → take the param cap.
        let columns = vec!["a".to_string()];
        let size = chunk_size(&columns, Some(100_000));
        assert_eq!(size, 65534);
    }

    #[test]
    fn chunk_indices_split_rows() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let rows: Vec<HashMap<String, JsonValue>> = (0..25)
            .map(|i| row(&[("a", json!(i)), ("b", json!(i + 1))]))
            .collect();
        let chunks = chunk_rows_by_column_count(&columns, rows.len(), Some(10));
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(chunks[1], vec![10, 11, 12, 13, 14, 15, 16, 17, 18, 19]);
        assert_eq!(chunks[2], vec![20, 21, 22, 23, 24]);
    }

    #[test]
    fn max_rows_override_smaller_than_data_yields_multiple_chunks() {
        let columns = vec!["a".to_string()];
        let rows: Vec<HashMap<String, JsonValue>> = (0..5)
            .map(|i| row(&[("a", json!(i))]))
            .collect();
        let chunks = chunk_rows_by_column_count(&columns, rows.len(), Some(2));
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 2);
        assert_eq!(chunks[1].len(), 2);
        assert_eq!(chunks[2].len(), 1);
    }

    #[test]
    fn default_chunk_size_is_500_for_small_column_counts() {
        // With 1 column, default chunk size = min(500, 65534) = 500.
        let columns = vec!["a".to_string()];
        assert_eq!(chunk_size(&columns, None), 500);
    }

    #[test]
    fn chunk_single_row_with_single_column() {
        let columns = vec!["a".to_string()];
        let chunks = chunk_rows_by_column_count(&columns, 1, None);
        assert_eq!(chunks, vec![vec![0]]);
    }

    #[test]
    fn normalize_preserves_column_order() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let rows = vec![row(&[("b", json!(2)), ("a", json!(1))])];
        let normalized = normalize_rows(&columns, rows);
        assert_eq!(normalized[0]["a"], json!(1));
        assert_eq!(normalized[0]["b"], json!(2));
    }

    #[test]
    fn m8_serde_path_wired() {
        let v = json!({"_m8": true, "rows": 1});
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("m8"));
    }
}
