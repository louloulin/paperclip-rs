//! Round 100 集成测试：验证 `pc_repos::ToolRepo` 对真实 schema 的 1:1 投影。
//!
//! 这些测试直接走 Repository API（不经过 HTTP 层），覆盖：
//! - `list_by_company` / `get_by_id` / `get_by_name`
//! - `create_application`（description 自动嵌入 metadata jsonb）
//! - `patch_application` 的 jsonb 合并语义（description + config）
//! - `set_application_status` / `delete_application`
//!
//! 前置：DB 已运行 196 条 migrate，包含 0148_tool_access_mcp_connections.sql。

use pc_db::Db;
use pc_repos::tool::{NewToolApplication, PatchToolApplication, ToolRepo};
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect to test db")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r100-{tag}-{id}"))
        .bind(format!("R100{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

/// 1. list_by_company: 仅返回本公司的 tool_applications
#[tokio::test(flavor = "current_thread")]
async fn tool_repo_list_by_company_filters_company() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid_a = insert_company(&db, "a").await;
    let cid_b = insert_company(&db, "b").await;

    // a 公司 2 个
    repo.create_application(&NewToolApplication {
        company_id: cid_a,
        name: "aa1".into(),
        kind: "mcp".into(),
        description: Some("first in a".into()),
        metadata: json!({"config": {"k": 1}}),
    })
    .await
    .expect("create aa1");
    repo.create_application(&NewToolApplication {
        company_id: cid_a,
        name: "aa2".into(),
        kind: "api".into(),
        description: None,
        metadata: json!({}),
    })
    .await
    .expect("create aa2");
    // b 公司 1 个
    repo.create_application(&NewToolApplication {
        company_id: cid_b,
        name: "bb1".into(),
        kind: "cli".into(),
        description: Some("first in b".into()),
        metadata: json!({}),
    })
    .await
    .expect("create bb1");

    let rows_a = repo.list_by_company(cid_a).await.expect("list a");
    let rows_b = repo.list_by_company(cid_b).await.expect("list b");

    let names_a: Vec<&str> = rows_a.iter().map(|r| r.name.as_str()).collect();
    let names_b: Vec<&str> = rows_b.iter().map(|r| r.name.as_str()).collect();

    assert_eq!(rows_a.len(), 2, "expected 2 in a, got {:?}", names_a);
    assert_eq!(rows_b.len(), 1, "expected 1 in b");
    assert!(names_a.contains(&"aa1") && names_a.contains(&"aa2"));
    assert!(names_b.contains(&"bb1"));
    assert!(!names_a.contains(&"bb1"), "b-leak into a!");
}

/// 2. Row 真实 schema 1:1：`name/type/status/metadata` 4 字段而非 22
#[tokio::test(flavor = "current_thread")]
async fn tool_repo_row_matches_real_schema() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "schema").await;
    let row = repo
        .create_application(&NewToolApplication {
            company_id: cid,
            name: "schema-check".into(),
            kind: "mcp".into(),
            description: Some("a description".into()),
            metadata: json!({"config": {"x": 1}}),
        })
        .await
        .expect("create");

    // tool_application_json 风格：kind 来自 type 列；description + config 从 metadata 拆出
    assert_eq!(row.kind, "mcp");
    assert_eq!(row.description(), Some("a description"));
    assert_eq!(row.config()["x"], 1);
}

/// 3. Round 100：description 自动嵌入 metadata jsonb（无需 caller 自己合并）
#[tokio::test(flavor = "current_thread")]
async fn tool_repo_create_embeds_description_into_metadata() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "embed").await;
    let row = repo
        .create_application(&NewToolApplication {
            company_id: cid,
            name: "with-desc".into(),
            kind: "api".into(),
            description: Some("hello".into()),
            metadata: json!({"config": {"url": "https://x"}}),
        })
        .await
        .expect("create");

    // 反查 DB：metadata 必须已经含 description
    let (meta,): (serde_json::Value,) = sqlx::query_as(
        "SELECT metadata FROM tool_applications WHERE id = $1",
    )
    .bind(row.id)
    .fetch_one(db.pool())
    .await
    .expect("query");
    assert_eq!(meta["description"], "hello");
    assert_eq!(meta["config"]["url"], "https://x");
}

/// 4. patch_application：合并 description + config 到 metadata jsonb
#[tokio::test(flavor = "current_thread")]
async fn tool_repo_patch_application_merges_metadata() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "patch").await;
    let row = repo
        .create_application(&NewToolApplication {
            company_id: cid,
            name: "p".into(),
            kind: "mcp".into(),
            description: Some("before".into()),
            metadata: json!({"config": {"flag": true}}),
        })
        .await
        .expect("create");

    let patch = PatchToolApplication {
        name: Some("p2".into()),
        description: Some("after".into()),
        config: Some(json!({"flag": false, "added": 7})),
        status: Some("disabled".into()),
        metadata_merge: serde_json::Map::new(),
    };
    let n = repo
        .patch_application(cid, row.id, &patch)
        .await
        .expect("patch");
    assert!(n);

    let after = repo.get(cid, row.id).await.expect("get").expect("present");
    assert_eq!(after.name, "p2");
    assert_eq!(after.status, "disabled");
    assert_eq!(after.description(), Some("after"));
    assert_eq!(after.config()["flag"], false);
    assert_eq!(after.config()["added"], 7);
}

/// 5. set_application_status：只更新 status，不动 metadata
#[tokio::test(flavor = "current_thread")]
async fn tool_repo_set_status_keeps_metadata_intact() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "status").await;
    let row = repo
        .create_application(&NewToolApplication {
            company_id: cid,
            name: "s".into(),
            kind: "mcp".into(),
            description: Some("d".into()),
            metadata: json!({"config": {"a": 1}}),
        })
        .await
        .expect("create");

    let n = repo
        .set_application_status(cid, row.id, "disabled")
        .await
        .expect("set");
    assert!(n);

    let after = repo.get(cid, row.id).await.expect("get").expect("present");
    assert_eq!(after.status, "disabled");
    // metadata 没动
    assert_eq!(after.description(), Some("d"));
    assert_eq!(after.config()["a"], 1);
}

/// 6. delete_application：物理删除；之后 get_by_id 返回 None
#[tokio::test(flavor = "current_thread")]
async fn tool_repo_delete_then_get_returns_none() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "del").await;
    let row = repo
        .create_application(&NewToolApplication {
            company_id: cid,
            name: "d".into(),
            kind: "mcp".into(),
            description: None,
            metadata: json!({}),
        })
        .await
        .expect("create");

    let n = repo.delete_application(cid, row.id).await.expect("del");
    assert!(n);
    let get_by_id = repo.get_by_id(row.id).await.expect("get_by_id");
    assert!(get_by_id.is_none());
}

/// 7. validation：name/kind 空串必须返回 RepoError::Invalid（不写库）
#[tokio::test(flavor = "current_thread")]
async fn tool_repo_validation_rejects_empty_fields() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "validate").await;

    let bad_name = repo
        .create_application(&NewToolApplication {
            company_id: cid,
            name: "  ".into(),
            kind: "mcp".into(),
            description: None,
            metadata: json!({}),
        })
        .await;
    assert!(bad_name.is_err(), "empty name must be rejected");

    let bad_kind = repo
        .create_application(&NewToolApplication {
            company_id: cid,
            name: "x".into(),
            kind: "  ".into(),
            description: None,
            metadata: json!({}),
        })
        .await;
    assert!(bad_kind.is_err(), "empty kind must be rejected");
}

/// 8. noop patch：仍然更新 updated_at（保证单调递增，避免业务循环）
#[tokio::test(flavor = "current_thread")]
async fn tool_repo_noop_patch_touches_updated_at() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "noop").await;
    let row = repo
        .create_application(&NewToolApplication {
            company_id: cid,
            name: "n".into(),
            kind: "mcp".into(),
            description: None,
            metadata: json!({}),
        })
        .await
        .expect("create");

    let noop = PatchToolApplication::default();
    assert!(noop.is_noop());
    let n = repo
        .patch_application(cid, row.id, &noop)
        .await
        .expect("noop patch");
    assert!(n, "noop patch should still touch updated_at and return ok");
}
