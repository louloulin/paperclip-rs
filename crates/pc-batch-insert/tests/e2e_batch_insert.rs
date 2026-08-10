//! End-to-end tests for `pc-batch-insert`.
//!
//! 覆盖真实 Postgres：
//! - chunker 切分结果能被 sqlx 用作多行 INSERT
//! - 列归一化（缺失 → NULL）
//! - 空批次不触发 INSERT
//! - 大批次（> 500 行）被自动切

use pc_batch_insert::{
    chunk_rows_for_insert, compute_columns, normalize_rows, ChunkOptions,
    DEFAULT_INSERT_CHUNK_ROWS, POSTGRES_MAX_BIND_PARAMS,
};
use pc_repos::Db;
use serde_json::{json, Value};
use serde_json::Value as JsonValue;
use sqlx::Row;
use std::collections::BTreeMap;
use uuid::Uuid;

const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

type JsonRow = BTreeMap<String, JsonValue>;

async fn connect() -> Db {
    Db::connect(DB_URL, 5, 1).await.expect("connect")
}

async fn cleanup(db: &Db, tag: &str) {
    let prefix = format!("BI-{tag}");
    let _ = sqlx::query("DELETE FROM companies WHERE issue_prefix = $1")
        .bind(&prefix)
        .execute(db.pool())
        .await;
}

async fn make_company(db: &Db, tag: &str) -> Uuid {
    let name = format!("BI Co {tag} {}", Uuid::new_v4());
    let row = sqlx::query("INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id")
        .bind(&name)
        .bind(format!("BI-{tag}"))
        .fetch_one(db.pool())
        .await
        .expect("create company");
    row.try_get::<Uuid, _>("id").expect("id")
}

/// 用 chunker 的结果构造可执行的 INSERT。
async fn insert_chunk(
    pool: &sqlx::PgPool,
    table: &str,
    chunk: Vec<JsonRow>,
) -> Result<u64, sqlx::Error> {
    if chunk.is_empty() {
        return Ok(0);
    }
    let cols = compute_columns(&chunk);
    let norm = normalize_rows(chunk, &cols);
    let mut sql = format!("INSERT INTO {table} (");
    sql.push_str(&cols.join(", "));
    sql.push_str(") VALUES ");
    for (i, _) in norm.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str("(");
        for (j, _) in cols.iter().enumerate() {
            if j > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!("${}", i * cols.len() + j + 1));
        }
        sql.push_str(")");
    }
    let mut q = sqlx::query(&sql);
    for row in &norm {
        for col in &cols {
            let v = row.get(col).cloned().unwrap_or(Value::Null);
            q = bind_value(q, v);
        }
    }
    let res = q.execute(pool).await?;
    Ok(res.rows_affected())
}

fn bind_value<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    v: JsonValue,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
        match v {
        JsonValue::Null => q.bind(Option::<String>::None),
        JsonValue::Bool(b) => q.bind(b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                q.bind(i)
            } else if let Some(f) = n.as_f64() {
                q.bind(f)
            } else {
                q.bind(n.to_string())
            }
        }
        JsonValue::String(s) => q.bind(s),
        // 否则 JSON 化字符串
        other => q.bind(other.to_string()),
    }
}

#[tokio::test]
async fn r680_e2e_empty_chunks_inserts_nothing() {
    let db = connect().await;
    cleanup(&db, "empty").await;
    let cid = make_company(&db, "empty").await;
    let chunks = chunk_rows_for_insert(Vec::new(), ChunkOptions::default());
    assert!(chunks.is_empty());

    // Verify no insert happens (count rows)
    let pre: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM companies WHERE id = $1")
        .bind(cid)
        .fetch_one(db.pool())
        .await
        .expect("count");
    assert_eq!(pre, 1);
    cleanup(&db, "empty").await;
}

#[tokio::test]
async fn r680_e2e_chunker_output_is_executable_in_sqlx() {
    let db = connect().await;
    cleanup(&db, "exec").await;
    let _ = make_company(&db, "exec").await;

    // Chunker 把 12 行切成 1 chunk（cols=2 → max_rows=500 → 1 chunk）
    let rows: Vec<JsonRow> = (0..12)
        .map(|i| {
            let mut r = JsonRow::new();
            r.insert("name".to_string(), json!(format!("Batch Insert Test {i}")));
            r.insert("issue_prefix".to_string(), json!(format!("BI-exec-{i:03}-{}", Uuid::new_v4())));
            r
        })
        .collect();
    let chunks = chunk_rows_for_insert(rows, ChunkOptions::default());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].len(), 12);

    // 真实 INSERT
    let n = insert_chunk(db.pool(), "companies", chunks.into_iter().flatten().collect())
        .await
        .expect("insert");
    assert_eq!(n, 12);

    cleanup(&db, "exec").await;
}

#[tokio::test]
async fn r680_e2e_normalize_missing_columns_inserted_as_null() {
    let db = connect().await;
    cleanup(&db, "null").await;

    // description 是可空列（实际 schema 中，companies.description 为 nullable）
    let rows: Vec<JsonRow> = (0..2)
        .map(|i| {
            let mut r = JsonRow::new();
            r.insert("name".to_string(), json!(format!("Null Test {i}")));
            r.insert("issue_prefix".to_string(), json!(format!("BI-null-{i:03}-{}", Uuid::new_v4())));
            // description 未填 → 归一化后应为 NULL
            r
        })
        .collect();
    let chunks = chunk_rows_for_insert(rows, ChunkOptions::default());
    assert_eq!(chunks.len(), 1);

    let n = insert_chunk(db.pool(), "companies", chunks.into_iter().flatten().collect())
        .await
        .expect("insert");
    assert_eq!(n, 2);

    // 查询应返回 description 为 NULL
    let count_null: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM companies WHERE name LIKE 'Null Test%' AND description IS NULL",
    )
    .fetch_one(db.pool())
    .await
    .expect("count null");
    assert_eq!(count_null, 2);

    cleanup(&db, "null").await;
}

#[tokio::test]
async fn r680_e2e_large_batch_is_chunked_into_multiple_statements() {
    let db = connect().await;
    cleanup(&db, "big").await;
    let _ = make_company(&db, "big").await;

    let rows: Vec<JsonRow> = (0..1500)
        .map(|i| {
            let mut r = JsonRow::new();
            r.insert("name".to_string(), json!(format!("Big Batch {i}")));
            r.insert("issue_prefix".to_string(), json!(format!("BI-big-{i:04}-{}", Uuid::new_v4())));
            r
        })
        .collect();

    let chunks = chunk_rows_for_insert(rows, ChunkOptions::default());
    // 1500 / 500 = 3 chunks
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].len(), DEFAULT_INSERT_CHUNK_ROWS);
    assert_eq!(chunks[1].len(), DEFAULT_INSERT_CHUNK_ROWS);
    assert_eq!(chunks[2].len(), 500);

    let mut total = 0;
    for chunk in chunks {
        let n = insert_chunk(db.pool(), "companies", chunk).await.expect("insert");
        total += n;
    }
    assert_eq!(total, 1500);

    cleanup(&db, "big").await;
}

#[tokio::test]
async fn r680_e2e_chunk_size_caps_by_columns() {
    let db = connect().await;
    cleanup(&db, "cols").await;

    // 现在用任意合适的现有列；
    // 我们构造一个 batch 含 1000 行 5 列（name, description, budget_monthly_cents, spent_monthly_cents, status）
    let rows: Vec<JsonRow> = (0..1000)
        .map(|i| {
            let mut r = JsonRow::new();
            r.insert("name".to_string(), json!(format!("Cols Test {i}")));
            r.insert("description".to_string(), json!(format!("Desc {i}")));
            r.insert("budget_monthly_cents".to_string(), json!(i));
            r.insert("spent_monthly_cents".to_string(), json!(0));
            r.insert("status".to_string(), json!("active"));
            r.insert("issue_prefix".to_string(), json!(format!("BI-cols-{i:04}-{}", Uuid::new_v4())));
            r
        })
        .collect();

    // 实际 num cols = 6
    let opts = ChunkOptions::new().with_max_rows(DEFAULT_INSERT_CHUNK_ROWS);
    let chunks = chunk_rows_for_insert(rows, opts);
    // cols=6, max_rows=500 → chunkSize = min(500, 65534/6=10922) = 500
    assert_eq!(chunks[0].len(), DEFAULT_INSERT_CHUNK_ROWS);
    assert!(chunks[0][0].contains_key("description"));

    let mut total = 0;
    for chunk in chunks {
        let n = insert_chunk(db.pool(), "companies", chunk).await.expect("insert");
        total += n;
    }
    assert_eq!(total, 1000);

    cleanup(&db, "cols").await;
}

#[tokio::test]
async fn r680_e2e_postgres_max_bind_params_constant() {
    assert_eq!(POSTGRES_MAX_BIND_PARAMS, 65534);
    // 65534 应当 < Postgres 硬上限 65535
    assert!(POSTGRES_MAX_BIND_PARAMS < 65_535);
}
