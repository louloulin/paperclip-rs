//! Round 157 集成测试：PipelineRepo 仓储化扩展（routes/pipelines.rs 14 SQL 化 0）。
//!
//! 覆盖 12 个新方法：
//! 1.  count_cases_by_pipeline / count_cases_by_pipeline_grouped
//! 2.  get_pipeline_config
//! 3.  replace_transitions（事务）
//! 4.  list_attention_pipelines
//! 5.  insert_status_changed_event（bulk_review 用）
//! 6.  get_case_retry_plan（case_automation_retry_plan 用）
//! 7.  get_case_triple（case_automation_retry 用）
//! 8.  increment_case_version（case_automation_retry 用）
//! 9.  insert_fields_changed_event（case_automation_retry 用）
//! 10. get_case_company_id（case_automation_specific_retry 用）
//! 11. get_case_stage_version（case_automation_current_stage_rerun 用）
//! 12. get_stage（case_automation_retry_plan 用，复用现有方法）

use pc_db::Db;
use pc_repos::pipeline::PipelineRepo;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r157-{tag}-{id}"))
        .bind(format!("R157{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_pipeline(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipelines (id, company_id, key, name, enforce_transitions) \
         VALUES ($1, $2, $3, 'r157-p', false)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r157-pl-{id}"))
    .execute(db.pool())
    .await
    .expect("pipeline");
    id
}

async fn insert_stage(db: &Db, pipeline_id: Uuid, key: &str, kind: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_stages (id, pipeline_id, key, name, kind, position, config) \
         VALUES ($1, $2, $3, $3, $4, 0, '{}'::jsonb)",
    )
    .bind(id)
    .bind(pipeline_id)
    .bind(key)
    .bind(kind)
    .execute(db.pool())
    .await
    .expect("stage");
    id
}

async fn insert_pipeline_case(
    db: &Db,
    company_id: Uuid,
    pipeline_id: Uuid,
    stage_id: Uuid,
    status: &str,
    version: i32,
    pending_suggestion: Option<serde_json::Value>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_cases \
         (id, company_id, pipeline_id, stage_id, case_key, title, summary, fields, version, pending_suggestion, status) \
         VALUES ($1, $2, $3, $4, $5, 'r157-case', '', '{}'::jsonb, $6, $7, $8)",
    )
    .bind(id)
    .bind(company_id)
    .bind(pipeline_id)
    .bind(stage_id)
    .bind(format!("r157-ck-{id}"))
    .bind(version)
    .bind(pending_suggestion)
    .bind(status)
    .execute(db.pool())
    .await
    .expect("pipeline_case");
    id
}

async fn insert_case(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO cases (id, company_id, case_number, identifier, case_type, title, status, fields) \
         VALUES ($1, $2, 9999, $3, 'review', 'r157-bulk', $4, '{}'::jsonb)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r157-case-{id}"))
    .bind(status)
    .execute(db.pool())
    .await
    .expect("case");
    id
}

// ===== count_cases_by_pipeline / count_cases_by_pipeline_grouped =====

/// 1. count_cases_by_pipeline — 0 case 返 0，添加多个返 N。
#[tokio::test(flavor = "current_thread")]
async fn count_cases_basic() {
    let db = db().await;
    let cid = insert_company(&db, "cc1").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "open", "open").await;

    let repo = PipelineRepo::new(&db);
    // 空 → 0
    let n = repo
        .count_cases_by_pipeline(pid)
        .await
        .expect("count empty");
    assert_eq!(n, 0);

    // 加 2 个 case
    let _ = insert_pipeline_case(&db, cid, pid, sid, "open", 1, None).await;
    let _ = insert_pipeline_case(&db, cid, pid, sid, "working", 1, None).await;
    let n = repo.count_cases_by_pipeline(pid).await.expect("count 2");
    assert_eq!(n, 2);
}

/// 2. count_cases_by_pipeline_grouped — 按 status 分组。
#[tokio::test(flavor = "current_thread")]
async fn count_cases_grouped_basic() {
    let db = db().await;
    let cid = insert_company(&db, "cg1").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "open", "open").await;

    let _ = insert_pipeline_case(&db, cid, pid, sid, "open", 1, None).await;
    let _ = insert_pipeline_case(&db, cid, pid, sid, "open", 1, None).await;
    let _ = insert_pipeline_case(&db, cid, pid, sid, "working", 1, None).await;

    let repo = PipelineRepo::new(&db);
    let by_status = repo
        .count_cases_by_pipeline_grouped(pid)
        .await
        .expect("grouped");
    // 至少包含 ("open", 2) 和 ("working", 1)
    let open_n = by_status
        .iter()
        .find(|(s, _)| s == "open")
        .map(|(_, n)| *n)
        .unwrap_or(0);
    let working_n = by_status
        .iter()
        .find(|(s, _)| s == "working")
        .map(|(_, n)| *n)
        .unwrap_or(0);
    assert_eq!(open_n, 2);
    assert_eq!(working_n, 1);
}

// ===== get_pipeline_config =====

/// 3. get_pipeline_config — 存在 pipeline 返回 config，不存在返 None。
#[tokio::test(flavor = "current_thread")]
async fn get_pipeline_config_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "gpc1").await;
    let pid = insert_pipeline(&db, cid).await;
    // 给 pipeline 写一个 config
    let cfg = json!({"intakeForm": {"fields": [{"name": "title", "required": true}]}});
    sqlx::query("UPDATE pipelines SET config = $1::jsonb WHERE id = $2")
        .bind(&cfg)
        .bind(pid)
        .execute(db.pool())
        .await
        .expect("update config");

    let repo = PipelineRepo::new(&db);
    let back = repo
        .get_pipeline_config(pid)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(back, cfg);

    let miss = repo
        .get_pipeline_config(Uuid::new_v4())
        .await
        .expect("miss");
    assert!(miss.is_none());
}

// ===== replace_transitions =====

/// 4. replace_transitions — 事务化 DELETE + INSERT。
#[tokio::test(flavor = "current_thread")]
async fn replace_transitions_basic() {
    let db = db().await;
    let cid = insert_company(&db, "rt1").await;
    let pid = insert_pipeline(&db, cid).await;

    // 预存一个旧 transition
    sqlx::query(
        "INSERT INTO pipeline_transitions \
         (id, company_id, pipeline_id, from_stage_id, to_stage_id, label, from_stage_key, to_stage_key) \
         VALUES (gen_random_uuid(), $1, $2, gen_random_uuid(), gen_random_uuid(), 'old', 'old-from', 'old-to')",
    )
    .bind(cid)
    .bind(pid)
    .execute(db.pool())
    .await
    .expect("pre-existing transition");

    let repo = PipelineRepo::new(&db);
    let transitions = vec![
        ("from-a".to_string(), "to-a".to_string()),
        ("from-b".to_string(), "to-b".to_string()),
    ];
    let n = repo
        .replace_transitions(pid, &transitions)
        .await
        .expect("replace");
    assert_eq!(n, 2);
}

// ===== list_attention_pipelines =====

/// 5. list_attention_pipelines — 至少需要一个 pipeline，没有 in_review case 也算需关注。
#[tokio::test(flavor = "current_thread")]
async fn list_attention_pipelines_basic() {
    let db = db().await;
    let cid = insert_company(&db, "lap1").await;
    let pid = insert_pipeline(&db, cid).await;
    let repo = PipelineRepo::new(&db);
    // 无 cases 也会被列出（HAVING count(case_all.id) = 0）
    let rows = repo.list_attention_pipelines(cid, 20).await.expect("list");
    assert!(rows.iter().any(|(id, _, _, _, _, _)| *id == pid));
}

// ===== insert_status_changed_event =====

/// 6. insert_status_changed_event — 插入一条 event，不报错。
#[tokio::test(flavor = "current_thread")]
async fn insert_status_changed_event_basic() {
    let db = db().await;
    let cid = insert_company(&db, "isc1").await;
    let case_id = insert_case(&db, cid, "in_review").await;
    let repo = PipelineRepo::new(&db);
    repo.insert_status_changed_event(cid, case_id, "approved", "looks good")
        .await
        .expect("insert");
    let cnt: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM case_events WHERE case_id=$1 AND kind='status_changed'",
    )
    .bind(case_id)
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(cnt, 1);
}

// ===== get_case_retry_plan =====

/// 7. get_case_retry_plan — 返回 (company_id, pipeline_id, stage_id, version, pending_suggestion)。
#[tokio::test(flavor = "current_thread")]
async fn get_case_retry_plan_basic() {
    let db = db().await;
    let cid = insert_company(&db, "gcrp1").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "open", "open").await;
    let sugg = json!({"hint": "double-check ack"});
    let case_id = insert_pipeline_case(&db, cid, pid, sid, "working", 7, Some(sugg.clone())).await;

    let repo = PipelineRepo::new(&db);
    let row = repo
        .get_case_retry_plan(case_id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(row.0, cid);
    assert_eq!(row.1, pid);
    assert_eq!(row.2, sid);
    assert_eq!(row.3, 7);
    assert_eq!(row.4, Some(sugg));

    let miss = repo
        .get_case_retry_plan(Uuid::new_v4())
        .await
        .expect("miss");
    assert!(miss.is_none());
}

// ===== get_case_triple =====

/// 8. get_case_triple — 返回 (company_id, pipeline_id, version)。
#[tokio::test(flavor = "current_thread")]
async fn get_case_triple_basic() {
    let db = db().await;
    let cid = insert_company(&db, "gct1").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "open", "open").await;
    let case_id = insert_pipeline_case(&db, cid, pid, sid, "working", 3, None).await;

    let repo = PipelineRepo::new(&db);
    let row = repo
        .get_case_triple(case_id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(row.0, cid);
    assert_eq!(row.1, pid);
    assert_eq!(row.2, 3);
}

// ===== increment_case_version =====

/// 9. increment_case_version — 每次调用 version +1 并返回新值。
#[tokio::test(flavor = "current_thread")]
async fn increment_case_version_basic() {
    let db = db().await;
    let cid = insert_company(&db, "icv1").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "open", "open").await;
    let case_id = insert_pipeline_case(&db, cid, pid, sid, "working", 1, None).await;
    let repo = PipelineRepo::new(&db);

    let v1 = repo.increment_case_version(case_id).await.expect("v1");
    assert_eq!(v1, 2);
    let v2 = repo.increment_case_version(case_id).await.expect("v2");
    assert_eq!(v2, 3);
}

// ===== insert_fields_changed_event =====

/// 10. insert_fields_changed_event — 插入一条 pipeline_case_event (kind='fields_changed', actor_type='system')。
#[tokio::test(flavor = "current_thread")]
async fn insert_fields_changed_event_basic() {
    let db = db().await;
    let cid = insert_company(&db, "ifce1").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "open", "open").await;
    let case_id = insert_pipeline_case(&db, cid, pid, sid, "working", 1, None).await;

    let repo = PipelineRepo::new(&db);
    let payload = json!({"action": "automation_retry_requested", "fromVersion": 1, "toVersion": 2});
    repo.insert_fields_changed_event(cid, case_id, &payload)
        .await
        .expect("insert");

    let cnt: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pipeline_case_events WHERE case_id=$1 AND kind='fields_changed'",
    )
    .bind(case_id)
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(cnt, 1);
}

// ===== get_case_company_id =====

/// 11. get_case_company_id — 返回 case 的 company_id。
#[tokio::test(flavor = "current_thread")]
async fn get_case_company_id_basic() {
    let db = db().await;
    let cid = insert_company(&db, "gcci1").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "open", "open").await;
    let case_id = insert_pipeline_case(&db, cid, pid, sid, "working", 1, None).await;

    let repo = PipelineRepo::new(&db);
    let back = repo
        .get_case_company_id(case_id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(back, cid);
}

// ===== get_case_stage_version =====

/// 12. get_case_stage_version — 返回 (company_id, stage_id, version)。
#[tokio::test(flavor = "current_thread")]
async fn get_case_stage_version_basic() {
    let db = db().await;
    let cid = insert_company(&db, "gcsv1").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "open", "open").await;
    let case_id = insert_pipeline_case(&db, cid, pid, sid, "working", 4, None).await;

    let repo = PipelineRepo::new(&db);
    let row = repo
        .get_case_stage_version(case_id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(row.0, cid);
    assert_eq!(row.1, sid);
    assert_eq!(row.2, 4);
}

// ===== get_stage（既有方法，本轮被 case_automation_retry_plan 复用）=====

/// 13. get_stage — 命中返回完整 PipelineStageRow，未命中 None。
#[tokio::test(flavor = "current_thread")]
async fn get_stage_basic() {
    let db = db().await;
    let cid = insert_company(&db, "gs1").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "review", "review").await;

    let repo = PipelineRepo::new(&db);
    let row = repo.get_stage(sid).await.expect("get").expect("present");
    assert_eq!(row.key, "review");
    assert_eq!(row.kind, "review");
    assert_eq!(row.pipeline_id, pid);

    let miss = repo.get_stage(Uuid::new_v4()).await.expect("miss");
    assert!(miss.is_none());
}

// ===== DTO smoke (sync) =====

/// 14. PipelineStageRow 类型 smoke — 验证 FromRow trait 实现。
#[test]
fn pipeline_stage_row_typecheck() {
    use pc_repos::pipeline::PipelineStageRow;
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<PipelineStageRow>();
}

/// 15. PipelineRow 类型 smoke。
#[test]
fn pipeline_row_typecheck() {
    use pc_repos::pipeline::PipelineRow;
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<PipelineRow>();
}
