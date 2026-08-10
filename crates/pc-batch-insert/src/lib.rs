#![forbid(unsafe_code)]
//! `pc-batch-insert` —— Chunked multi-row SQL insert 助手。
//!
//! 对应 Node `server/src/services/batch-insert.ts`（69 行）。
//!
//! ## 设计目标
//!
//! - **Postgres 65535 bind-param 上限**：单条 INSERT 语句的 bind 数量必须低于此。
//!   公式：`(columnsPerRow × rowCount) < 65535`
//! - **统一的列归一化**：批次内不同行可能省略可选字段；归一化为"列并集 + 缺失/null 填充"。
//! - **空批次 no-op**：`rows.is_empty()` 时直接返回，不发 SQL。
//! - **chunker-only**：只做"如何把 `rows` 分块"的工作；下游 caller 自行决定如何 INSERT（sqlx / drizzle / 任意 SQL）。
//!
//! ## 公共 API
//!
//! - [`POSTGRES_MAX_BIND_PARAMS`] —— `65534`（比硬上限少 1，保证严格 <）
//! - [`DEFAULT_INSERT_CHUNK_ROWS`] —— `500`（保守上限，避免窄表语句过大）
//! - [`ChunkOptions`] —— `max_rows` 自定义
//! - [`chunk_rows_for_insert`] —— 纯函数，把 rows 按 chunk 切分
//! - [`normalize_rows`] —— 纯函数，把 rows 归一化为共享列
//! - [`compute_columns`] —— 提取列并集（保留首次出现顺序）
//!
//! ## 设计原则
//!
//! - **高内聚**：chunk / 归一化逻辑集中在本 crate。
//! - **无 DB 依赖**：所有函数纯函数，方便测试。
//! - **可测**：纯函数 + 边界用例全覆盖。

use std::collections::BTreeMap;

// ============================================================================
// Constants
// ============================================================================

/// Postgres 单条 SQL 的 bind-param 上限（保守少 1，避免落到边界）。
///
/// 与 Node `POSTGRES_MAX_BIND_PARAMS = 65534` 1:1 对齐。
pub const POSTGRES_MAX_BIND_PARAMS: usize = 65534;

/// 默认 chunk 行数上限（保守值，避免窄表语句过大）。
///
/// 与 Node `DEFAULT_INSERT_CHUNK_ROWS = 500` 1:1 对齐。
pub const DEFAULT_INSERT_CHUNK_ROWS: usize = 500;

// ============================================================================
// Types
// ============================================================================

/// 单行数据：`column → value`。
///
/// 使用 `BTreeMap` 而非 `HashMap`：保证列遍历顺序稳定（首次出现顺序），
/// chunk 输出可复现，方便测试断言。
pub type Row = BTreeMap<String, serde_json::Value>;

/// 一组 chunked 行。
pub type Chunks = Vec<Vec<Row>>;

/// Chunk 选项。
#[derive(Debug, Clone, Copy)]
pub struct ChunkOptions {
    /// 强制 chunk 行数上限（即使列很少也不超过此值）。
    pub max_rows: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self { max_rows: DEFAULT_INSERT_CHUNK_ROWS }
    }
}

impl ChunkOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_rows(mut self, max_rows: usize) -> Self {
        self.max_rows = max_rows.max(1);
        self
    }
}

// ============================================================================
// Pure helpers
// ============================================================================

/// 计算 row 集合中所有出现过的列（保留首次出现顺序）。
///
/// 与 Node `const columns = [...keys];` 1:1 对齐。
pub fn compute_columns(rows: &[Row]) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cols = Vec::new();
    for row in rows {
        for key in row.keys() {
            if seen.insert(key.clone()) {
                cols.push(key.clone());
            }
        }
    }
    cols
}

/// 把行归一化为共享列（缺失列 → `Value::Null`）。
///
/// 与 Node `normalized = rows.map(...)` 1:1 对齐。
///
/// 注意：`undefined` 也会转为 `null`（serde_json 默认 `Value::Null`）。
pub fn normalize_rows(rows: Vec<Row>, columns: &[String]) -> Vec<Row> {
    rows.into_iter()
        .map(|row| {
            let mut out: Row = Row::new();
            for col in columns {
                let v = row.get(col).cloned().unwrap_or(serde_json::Value::Null);
                out.insert(col.clone(), v);
            }
            out
        })
        .collect()
}

/// 把 rows 切成多个 chunk，每 chunk 不超过 `max_rows` 行且列×行不超 `POSTGRES_MAX_BIND_PARAMS`。
///
/// 与 Node `chunkSize = Math.max(1, Math.min(maxRows ?? DEFAULT, Math.floor(MAX / colsPerRow)))`
/// 1:1 对齐。
///
/// **空输入返回空 `Vec`**（no-op）。
pub fn chunk_rows_for_insert(rows: Vec<Row>, options: ChunkOptions) -> Chunks {
    if rows.is_empty() {
        return Vec::new();
    }

    let columns = compute_columns(&rows);
    let normalized = normalize_rows(rows, &columns);
    let columns_per_row = columns.len().max(1);

    // 与 Node 完全一致的 chunk size 计算
    let by_params = POSTGRES_MAX_BIND_PARAMS / columns_per_row;
    let chunk_size = options.max_rows.max(1).min(by_params.max(1));

    let mut chunks: Chunks = Vec::new();
    let mut start = 0;
    while start < normalized.len() {
        let end = (start + chunk_size).min(normalized.len());
        chunks.push(normalized[start..end].to_vec());
        start = end;
    }
    chunks
}

/// 计算最优 chunk 行数（用于外部断言 / 调试）。
///
/// 返回值 = `min(options.max_rows, MAX_BIND_PARAMS / columns_per_row)`。
pub fn chunk_size(columns_per_row: usize, options: ChunkOptions) -> usize {
    let by_params = POSTGRES_MAX_BIND_PARAMS / columns_per_row.max(1);
    options.max_rows.max(1).min(by_params.max(1))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 构造 row：从 `(&str, Value)` 列表转换为 `Row`。
    fn row(pairs: Vec<(&str, serde_json::Value)>) -> Row {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn r680_empty_input_returns_no_chunks() {
        let chunks = chunk_rows_for_insert(Vec::new(), ChunkOptions::default());
        assert!(chunks.is_empty());
    }

    #[test]
    fn r680_single_row_single_chunk() {
        let rows = vec![row(vec![("a", json!(1)), ("b", json!("x"))])];
        let chunks = chunk_rows_for_insert(rows, ChunkOptions::default());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 1);
    }

    #[test]
    fn r680_compute_columns_preserves_first_occurrence_order() {
        let rows = vec![
            row(vec![("a", json!(1)), ("b", json!(2))]),
            row(vec![("b", json!(3)), ("c", json!(4)), ("a", json!(5))]),
        ];
        let cols = compute_columns(&rows);
        assert_eq!(cols, vec!["a", "b", "c"]);
    }

    #[test]
    fn r680_normalize_fills_missing_with_null() {
        let rows = vec![
            row(vec![("a", json!(1)), ("b", json!("x"))]),
            row(vec![("a", json!(2)), ("c", json!(true))]),
        ];
        let cols = compute_columns(&rows);
        assert_eq!(cols, vec!["a", "b", "c"]);
        let norm = normalize_rows(rows, &cols);
        assert_eq!(norm[0].get("c"), Some(&serde_json::Value::Null));
        assert_eq!(norm[1].get("b"), Some(&serde_json::Value::Null));
        assert_eq!(norm[1].get("a"), Some(&json!(2)));
    }

    #[test]
    fn r680_chunk_uses_smaller_of_two_bounds() {
        let rows = vec![row(vec![("a", json!(1))]); 2];
        let opts = ChunkOptions::new().with_max_rows(POSTGRES_MAX_BIND_PARAMS);
        let chunks = chunk_rows_for_insert(rows, opts);
        assert_eq!(chunks.len(), 1);

        let rows = vec![row(vec![("a", json!(1))]); 700];
        let opts = ChunkOptions::new().with_max_rows(500);
        let chunks = chunk_rows_for_insert(rows, opts);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 500);
        assert_eq!(chunks[1].len(), 200);
    }

    #[test]
    fn r680_chunk_size_caps_by_params_when_columns_high() {
        let cols: Vec<(&str, serde_json::Value)> =
            (0..200).map(|i| (Box::leak(format!("c{i}").into_boxed_str()) as &str, json!(i))).collect();
        let rows = vec![row(cols); 1000];
        let opts = ChunkOptions::new().with_max_rows(500);
        let chunks = chunk_rows_for_insert(rows, opts);
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].len(), 327);
        assert_eq!(chunks[1].len(), 327);
        assert_eq!(chunks[2].len(), 327);
        assert_eq!(chunks[3].len(), 19);
    }

    #[test]
    fn r680_chunk_size_at_least_one_when_columns_exceed_max() {
        let cols: Vec<(&str, serde_json::Value)> = (0..70_000)
            .map(|i| (Box::leak(format!("c{i}").into_boxed_str()) as &str, json!(i)))
            .collect();
        let rows = vec![row(cols); 3];
        let opts = ChunkOptions::new().with_max_rows(500);
        let chunks = chunk_rows_for_insert(rows, opts);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.len() == 1));
    }

    #[test]
    fn r680_max_rows_zero_or_less_is_clamped_to_one() {
        let rows = vec![row(vec![("a", json!(1))]); 5];
        let opts = ChunkOptions::new().with_max_rows(0);
        let chunks = chunk_rows_for_insert(rows, opts);
        assert_eq!(chunks.len(), 5);
    }

    #[test]
    fn r680_existing_columns_preserved_when_normalize_undefined() {
        let rows = vec![
            row(vec![("a", json!(1)), ("b", serde_json::Value::Null)]),
            row(vec![("a", json!(2))]),
        ];
        let cols = compute_columns(&rows);
        let norm = normalize_rows(rows, &cols);
        assert_eq!(norm[0].get("b"), Some(&serde_json::Value::Null));
        assert_eq!(norm[1].get("b"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn r680_helper_chunk_size_matches_logic() {
        assert_eq!(chunk_size(5, ChunkOptions::new().with_max_rows(1000)), 1000);
        assert_eq!(chunk_size(2000, ChunkOptions::new().with_max_rows(500)), 32);
        assert_eq!(chunk_size(70_000, ChunkOptions::new().with_max_rows(500)), 1);
    }

    #[test]
    fn r680_constants_match_node() {
        assert_eq!(POSTGRES_MAX_BIND_PARAMS, 65534);
        assert_eq!(DEFAULT_INSERT_CHUNK_ROWS, 500);
    }

    #[test]
    fn r680_distinct_value_types_preserved() {
        let rows = vec![
            row(vec![
                ("a", json!(1)),
                ("b", json!("text")),
                ("c", json!(true)),
                ("d", serde_json::Value::Null),
                ("e", json!({"nested": "obj"})),
                ("f", json!([1, 2, 3])),
            ]),
            row(vec![("a", json!(2))]),
        ];
        let cols = compute_columns(&rows);
        let norm = normalize_rows(rows, &cols);
        assert_eq!(norm[0]["a"], json!(1));
        assert_eq!(norm[0]["b"], json!("text"));
        assert_eq!(norm[0]["c"], json!(true));
        assert_eq!(norm[0]["d"], serde_json::Value::Null);
        assert_eq!(norm[0]["e"], json!({"nested": "obj"}));
        assert_eq!(norm[0]["f"], json!([1, 2, 3]));

        for k in ["b", "c", "d", "e", "f"] {
            assert_eq!(norm[1][k], serde_json::Value::Null);
        }
    }
}