//! Round 111 集成测试：验证 RoutineRepo 描述批注 (RoutineAnnotationThread/Comment) 子模块。
//! 覆盖 8 个新方法 + 1 个 bulk helper。

use pc_db::Db;
use pc_repos::routine::{
    NewRoutineAnnotationComment, NewRoutineAnnotationThread, RoutineAnnotationPatch,
    RoutineRepo,
};
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r111-{tag}-{id}"))
        .bind(format!("R111{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("insert company");
    id
}

async fn insert_routine(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routines (id, company_id, key, name, status) \
         VALUES ($1, $2, $3, 'r111', 'active')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r-{id}"))
    .execute(db.pool()).await.expect("insert routine");
    id
}

async fn insert_document(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, format, latest_body) \
         VALUES ($1, $2, 'markdown', '# test')",
    )
    .bind(id)
    .bind(company_id)
    .execute(db.pool()).await.expect("insert document");
    id
}

async fn insert_thread(
    db: &Db,
    company_id: Uuid,
    routine_id: Uuid,
    document_id: Uuid,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO document_annotation_threads \
            (id, company_id, routine_id, document_id, document_key, status, \
             original_revision_number, current_revision_number, selected_text, \
             prefix_text, suffix_text, normalized_start, normalized_end, \
             markdown_start, markdown_end, anchor_confidence, anchor_selector) \
         VALUES ($1, $2, $3, $4, 'description', $5, 1, 1, 'sel', '', '', 0, 3, 0, 3, 'exact', '{}'::jsonb)",
    )
    .bind(id)
    .bind(company_id)
    .bind(routine_id)
    .bind(document_id)
    .bind(status)
    .execute(db.pool()).await.expect("insert thread");
    id
}

async fn insert_comment(
    db: &Db,
    company_id: Uuid,
    routine_id: Uuid,
    thread_id: Uuid,
    document_id: Uuid,
    body: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO document_annotation_comments \
            (id, company_id, routine_id, thread_id, document_id, body, author_type) \
         VALUES ($1, $2, $3, $4, $5, $6, 'user')",
    )
    .bind(id)
    .bind(company_id)
    .bind(routine_id)
    .bind(thread_id)
    .bind(document_id)
    .bind(body)
    .execute(db.pool()).await.expect("insert comment");
    id
}

/// 1. get_company_id: 找到 / 找不到
#[tokio::test(flavor = "current_thread")]
async fn routine_get_company_id_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "cid").await;
    let rid = insert_routine(&db, cid).await;

    let repo = RoutineRepo::new(&db);
    let back = repo.get_company_id(rid).await.expect("get").expect("present");
    assert_eq!(back, cid);

    let none = repo.get_company_id(Uuid::new_v4()).await.expect("get");
    assert!(none.is_none());
}

/// 2. create_annotation_thread + get_annotation_thread
#[tokio::test(flavor = "current_thread")]
async fn annotation_thread_create_get_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "create").await;
    let rid = insert_routine(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let repo = RoutineRepo::new(&db);

    let input = NewRoutineAnnotationThread {
        company_id: cid,
        routine_id: rid,
        document_id: did,
        document_key: "description".to_owned(),
        status: Some("open".to_owned()),
        revision_number: 1,
        selected_text: "hello".to_owned(),
        prefix_text: None,
        suffix_text: None,
        normalized_start: 0,
        normalized_end: 5,
        markdown_start: 0,
        markdown_end: 5,
        anchor_confidence: Some("exact".to_owned()),
        anchor_selector: Some(json!({"type": "text"})),
    };
    let thread_id = repo.create_annotation_thread(&input).await.expect("create");
    let row = repo.get_annotation_thread(rid, thread_id).await.expect("get").expect("present");
    assert_eq!(row.id, thread_id);
    assert_eq!(row.company_id, cid);
    assert_eq!(row.routine_id, rid);
    assert_eq!(row.document_id, did);
    assert_eq!(row.document_key, "description");
    assert_eq!(row.status, "open");
    assert_eq!(row.selected_text, "hello");
}

/// 3. list_annotation_threads 状态过滤
#[tokio::test(flavor = "current_thread")]
async fn annotation_threads_list_filters_by_status() {
    let db = db().await;
    let cid = insert_company(&db, "list").await;
    let rid = insert_routine(&db, cid).await;
    let did = insert_document(&db, cid).await;
    insert_thread(&db, cid, rid, did, "open").await;
    insert_thread(&db, cid, rid, did, "open").await;
    insert_thread(&db, cid, rid, did, "resolved").await;

    let repo = RoutineRepo::new(&db);
    let all = repo.list_annotation_threads(rid, None, 200).await.expect("all");
    assert_eq!(all.len(), 3);
    let open = repo.list_annotation_threads(rid, Some("open"), 200).await.expect("open");
    assert_eq!(open.len(), 2);
    let resolved = repo.list_annotation_threads(rid, Some("resolved"), 200).await.expect("resolved");
    assert_eq!(resolved.len(), 1);
}

/// 4. list_thread_comments + list_thread_comments_bulk
#[tokio::test(flavor = "current_thread")]
async fn annotation_thread_comments_list_single_and_bulk() {
    let db = db().await;
    let cid = insert_company(&db, "cmt").await;
    let rid = insert_routine(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let t1 = insert_thread(&db, cid, rid, did, "open").await;
    let t2 = insert_thread(&db, cid, rid, did, "open").await;
    insert_comment(&db, cid, rid, t1, did, "c1").await;
    insert_comment(&db, cid, rid, t1, did, "c2").await;
    insert_comment(&db, cid, rid, t2, did, "c3").await;

    let repo = RoutineRepo::new(&db);
    let t1_comments = repo.list_thread_comments(t1).await.expect("list t1");
    assert_eq!(t1_comments.len(), 2);
    assert_eq!(t1_comments[0].body, "c1");
    assert_eq!(t1_comments[1].body, "c2");

    let bulk = repo.list_thread_comments_bulk(&[t1, t2]).await.expect("bulk");
    assert_eq!(bulk.len(), 3);
}

/// 5. create_thread_comment 写入
#[tokio::test(flavor = "current_thread")]
async fn annotation_thread_create_comment_writes() {
    let db = db().await;
    let cid = insert_company(&db, "cnew").await;
    let rid = insert_routine(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let tid = insert_thread(&db, cid, rid, did, "open").await;

    let repo = RoutineRepo::new(&db);
    let input = NewRoutineAnnotationComment {
        company_id: cid,
        routine_id: rid,
        thread_id: tid,
        document_id: did,
        body: "first comment".to_owned(),
        author_type: "user".to_owned(),
        author_user_id: Some("u1".to_owned()),
        author_agent_id: None,
    };
    let cid_new = repo.create_thread_comment(&input).await.expect("create");
    let row: (String, String) = sqlx::query_as(
        "SELECT body, author_type FROM document_annotation_comments WHERE id=$1",
    )
    .bind(cid_new)
    .fetch_one(db.pool())
    .await
    .expect("query");
    assert_eq!(row.0, "first comment");
    assert_eq!(row.1, "user");
}

/// 6. update_annotation_thread: status 切换 + resolved_at 触发
#[tokio::test(flavor = "current_thread")]
async fn annotation_thread_update_resolved_sets_timestamp() {
    let db = db().await;
    let cid = insert_company(&db, "upd").await;
    let rid = insert_routine(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let tid = insert_thread(&db, cid, rid, did, "open").await;
    let repo = RoutineRepo::new(&db);

    let patch = RoutineAnnotationPatch {
        status: Some("resolved".to_owned()),
        ..Default::default()
    };
    let n = repo.update_annotation_thread(rid, tid, &patch).await.expect("upd");
    assert_eq!(n, 1);

    let (status, resolved_at): (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as(
            "SELECT status, resolved_at FROM document_annotation_threads WHERE id=$1",
        )
        .bind(tid)
        .fetch_one(db.pool())
        .await
        .expect("query");
    assert_eq!(status, "resolved");
    assert!(resolved_at.is_some());
}

/// 7. update_annotation_thread: 切回 open 清除 resolved_at
#[tokio::test(flavor = "current_thread")]
async fn annotation_thread_update_open_clears_timestamp() {
    let db = db().await;
    let cid = insert_company(&db, "clr").await;
    let rid = insert_routine(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let tid = insert_thread(&db, cid, rid, did, "resolved").await;
    let repo = RoutineRepo::new(&db);

    let patch = RoutineAnnotationPatch {
        status: Some("open".to_owned()),
        ..Default::default()
    };
    let n = repo.update_annotation_thread(rid, tid, &patch).await.expect("upd");
    assert_eq!(n, 1);
    let resolved_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT resolved_at FROM document_annotation_threads WHERE id=$1",
    )
    .bind(tid)
    .fetch_one(db.pool())
    .await
    .expect("query");
    assert!(resolved_at.is_none());
}

/// 8. update_annotation_thread: 未知 thread 返 Ok(0)
#[tokio::test(flavor = "current_thread")]
async fn annotation_thread_update_missing_returns_zero() {
    let db = db().await;
    let cid = insert_company(&db, "miss").await;
    let rid = insert_routine(&db, cid).await;
    let repo = RoutineRepo::new(&db);

    let patch = RoutineAnnotationPatch {
        status: Some("open".to_owned()),
        ..Default::default()
    };
    let n = repo.update_annotation_thread(rid, Uuid::new_v4(), &patch).await.expect("upd");
    assert_eq!(n, 0);
}

/// 9. get_thread_document_id: 找到 / 找不到
#[tokio::test(flavor = "current_thread")]
async fn annotation_thread_document_id_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "did").await;
    let rid = insert_routine(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let tid = insert_thread(&db, cid, rid, did, "open").await;
    let repo = RoutineRepo::new(&db);

    let back = repo.get_thread_document_id(rid, tid).await.expect("get").expect("present");
    assert_eq!(back, did);

    let none = repo.get_thread_document_id(rid, Uuid::new_v4()).await.expect("get");
    assert!(none.is_none());
}
