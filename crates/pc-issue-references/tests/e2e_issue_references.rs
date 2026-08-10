//! End-to-end tests for `pc-issue-references`.

use pc_issue_references::{
    extract_identifiers, extract_matches, parse_issue_href, strip_markdown_code, IssueReferenceService,
};
use pc_repos::{
    company::CompanyRepo,
    issue::{CreateIssueInput, IssueRepo},
    project::{NewProject, ProjectRepo, ProjectStatus},
    Db,
};
use uuid::Uuid;

const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(DB_URL, 5, 1).await.expect("connect to db")
}

async fn make_company(db: &Db, tag: &str) -> Uuid {
    let repo = CompanyRepo::new(db);
    let name = format!("REF Co {tag} {}", Uuid::new_v4());
    repo.create(&name, Some("e2e")).await.expect("create company").id
}

async fn make_project(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
    let repo = ProjectRepo::new(db);
    let name = format!("REF project {tag} {}", Uuid::new_v4());
    repo.create(&NewProject {
        company_id,
        goal_id: None,
        name,
        description: None,
        status: ProjectStatus::Active,
        lead_agent_id: None,
        target_date: None,
        color: None,
        icon: None,
        env: None,
    })
    .await
    .expect("create project")
    .id
}

async fn make_issue_with_identifier(
    db: &Db,
    company_id: Uuid,
    project_id: Uuid,
    identifier: &str,
    title: &str,
    description: Option<&str>,
) -> Uuid {
    let repo = IssueRepo::new(db);
    let input = CreateIssueInput {
        company_id,
        title,
        description,
        status: Some("todo"),
        work_mode: None,
        harness_kind: None,
        priority: Some("medium"),
        assignee_agent_id: None,
        assignee_user_id: None,
        project_id: Some(project_id),
        project_workspace_id: None,
        goal_id: None,
        parent_id: None,
        inherit_execution_workspace_from_issue_id: None,
        created_by_user_id: None,
        responsible_user_id: None,
        billing_code: None,
        request_depth: 0,
        assignee_adapter_overrides: None,
        execution_policy: None,
        execution_workspace_id: None,
        execution_workspace_preference: None,
        execution_workspace_settings: None,
        blocked_by_issue_ids: None,
        label_ids: None,
        unblock_descriptor: None,
    };
    let row = repo.create_full(&input).await.expect("create issue");
    // 强制设置 identifier（每个测试用 UUID 前缀避免 unique 冲突）
    let unique = format!("{}-{}", identifier, Uuid::new_v4().simple());
    sqlx::query("UPDATE issues SET identifier = $1 WHERE id = $2")
        .bind(&unique)
        .bind(row.id)
        .execute(db.pool())
        .await
        .expect("set identifier");
    row.id
}

/// 取一个 issue 的实际 identifier（用于 cross-test reference）。
async fn get_identifier(db: &Db, issue_id: Uuid) -> String {
    let row: (Option<String>,) = sqlx::query_as("SELECT identifier FROM issues WHERE id = $1")
        .bind(issue_id)
        .fetch_one(db.pool())
        .await
        .expect("get identifier");
    row.0.unwrap()
}

/// 替换 identifier 字符串：把所有的 "REF-1" / "REF-2" / ... 替换为实际 identifier
fn replace_idents(text: &str, mapping: &std::collections::HashMap<&str, String>) -> String {
    let mut out = text.to_string();
    // Sort by length descending so REF-10 is replaced before REF-1
    let mut keys: Vec<&str> = mapping.keys().copied().collect();
    keys.sort_by(|a, b| b.len().cmp(&a.len()));
    for k in keys {
        if let Some(v) = mapping.get(k) {
            out = out.replace(k, v);
        }
    }
    out
}

async fn reset_table(db: &Db, table: &str) {
    sqlx::query(&format!(
        "DELETE FROM {table} WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'REF Co %')"
    ))
    .execute(db.pool())
    .await
    .expect("reset table");
}

// ------------------------------------------------------------------
// 0. 纯单测：extractor
// ------------------------------------------------------------------

#[test]
fn r655_extractor_unit_basic() {
    assert_eq!(extract_identifiers("hello PAP-1 world"), vec!["PAP-1"]);
    assert_eq!(extract_identifiers("PAP-1 PAP-2"), vec!["PAP-1", "PAP-2"]);
    assert_eq!(extract_identifiers("nope"), Vec::<String>::new());
}

#[test]
fn r655_extractor_unit_dedup() {
    assert_eq!(
        extract_identifiers("PAP-1 [link](/issues/pap-1) PAP-1"),
        vec!["PAP-1"]
    );
}

#[test]
fn r655_extractor_unit_strip_code_fence() {
    let md = "PAP-1 \n```\nPAP-2\n```\nPAP-3";
    assert_eq!(extract_identifiers(md), vec!["PAP-1", "PAP-3"]);
}

#[test]
fn r655_extractor_unit_strip_inline_code() {
    let md = "PAP-1 `PAP-2` PAP-3";
    assert_eq!(extract_identifiers(md), vec!["PAP-1", "PAP-3"]);
}

#[test]
fn r655_extractor_unit_href() {
    assert_eq!(
        parse_issue_href("/issues/pap-1"),
        Some("PAP-1".to_string())
    );
    assert_eq!(
        parse_issue_href("https://x.com/issues/abc-99?foo=bar"),
        Some("ABC-99".to_string())
    );
    assert_eq!(parse_issue_href("/something/else"), None);
}

#[test]
fn r655_extractor_unit_strip_markdown_code_basic() {
    let out = strip_markdown_code("hello `world` bye");
    assert!(out.contains("hello"));
    assert!(out.contains("bye"));
    assert!(!out.contains("world"));
}

#[test]
fn r655_extractor_unit_extract_matches_preserves_index() {
    let text = "prefix PAP-1 suffix";
    let matches = extract_matches(text);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].identifier, "PAP-1");
    assert_eq!(matches[0].matched_text, "PAP-1");
}

// ------------------------------------------------------------------
// 1. replace_source_mentions：基础 + 自引用 + dedup
// ------------------------------------------------------------------

#[tokio::test]
async fn r655_replace_mentions_writes_persisted_rows() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "replace-basic").await;
    let project_id = make_project(&db, company_id, "replace-basic").await;
    let target = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique1}",
        "Target issue",
        None,
    )
    .await;
    let source = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique2}",
        "Source issue",
        Some("see REF-1 for details"),
    )
    .await;

    let svc = IssueReferenceService::new(db.clone());
    let inserted = svc
        .replace_source_mentions(
            company_id,
            source,
            "description",
            None,
            None,
            Some("see REF-1 for details"),
        )
        .await
        .expect("replace");
    assert_eq!(inserted, 1);

    let count = svc.count_for_source(company_id, source).await.expect("count");
    assert_eq!(count, 1);
    let inbound_count = svc.count_for_target(company_id, target).await.expect("in");
    assert_eq!(inbound_count, 1);
}

#[tokio::test]
async fn r655_replace_mentions_skips_self_reference() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "replace-self").await;
    let project_id = make_project(&db, company_id, "replace-self").await;
    let issue = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique3}",
        "Self",
        Some("see REF-3 itself"),
    )
    .await;

    let svc = IssueReferenceService::new(db.clone());
    let inserted = svc
        .replace_source_mentions(
            company_id,
            issue,
            "description",
            None,
            None,
            Some("see REF-3 itself"),
        )
        .await
        .expect("replace");
    assert_eq!(inserted, 0);

    let count = svc.count_for_source(company_id, issue).await.expect("count");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn r655_replace_mentions_dedups_within_source() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "replace-dedup").await;
    let project_id = make_project(&db, company_id, "replace-dedup").await;
    let target = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique4}",
        "Target",
        None,
    )
    .await;
    let source = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique5}",
        "Source",
        Some("REF-4 REF-4 [link](/issues/ref-4)"),
    )
    .await;

    let svc = IssueReferenceService::new(db.clone());
    let inserted = svc
        .replace_source_mentions(
            company_id,
            source,
            "description",
            None,
            None,
            Some("REF-4 REF-4 [link](/issues/ref-4)"),
        )
        .await
        .expect("replace");
    assert_eq!(inserted, 1);

    let count = svc.count_for_source(company_id, source).await.expect("count");
    assert_eq!(count, 1);
    let inbound_count = svc.count_for_target(company_id, target).await.expect("in");
    assert_eq!(inbound_count, 1);
}

#[tokio::test]
async fn r655_replace_mentions_replaces_old_with_new() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "replace-replace").await;
    let project_id = make_project(&db, company_id, "replace-replace").await;
    let t1 = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique6}",
        "T1",
        None,
    )
    .await;
    let t2 = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique7}",
        "T2",
        None,
    )
    .await;
    let source = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique8}",
        "Source",
        Some("REF-{unique6}"),
    )
    .await;

    let svc = IssueReferenceService::new(db.clone());
    svc.replace_source_mentions(
        company_id,
        source,
        "description",
        None,
        None,
        Some("REF-{unique6}"),
    )
    .await
    .expect("first replace");
    assert_eq!(svc.count_for_source(company_id, source).await.unwrap(), 1);
    assert_eq!(svc.count_for_target(company_id, t1).await.unwrap(), 1);
    assert_eq!(svc.count_for_target(company_id, t2).await.unwrap(), 0);

    svc.replace_source_mentions(
        company_id,
        source,
        "description",
        None,
        None,
        Some("REF-{unique7}"),
    )
    .await
    .expect("second replace");
    assert_eq!(svc.count_for_source(company_id, source).await.unwrap(), 1);
    assert_eq!(svc.count_for_target(company_id, t1).await.unwrap(), 0);
    assert_eq!(svc.count_for_target(company_id, t2).await.unwrap(), 1);
}

#[tokio::test]
async fn r655_replace_mentions_clears_when_text_empty() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "replace-clear").await;
    let project_id = make_project(&db, company_id, "replace-clear").await;
    let t1 = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique9}",
        "T1",
        None,
    )
    .await;
    let source = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique10}",
        "Source",
        Some("REF-{unique9}"),
    )
    .await;

    let svc = IssueReferenceService::new(db.clone());
    svc.replace_source_mentions(
        company_id,
        source,
        "description",
        None,
        None,
        Some("REF-{unique9}"),
    )
    .await
    .expect("first");
    assert_eq!(svc.count_for_source(company_id, source).await.unwrap(), 1);

    svc.replace_source_mentions(
        company_id,
        source,
        "description",
        None,
        None,
        None,
    )
    .await
    .expect("clear");
    assert_eq!(svc.count_for_source(company_id, source).await.unwrap(), 0);
    assert_eq!(svc.count_for_target(company_id, t1).await.unwrap(), 0);
}

// ------------------------------------------------------------------
// 2. sync_issue
// ------------------------------------------------------------------

#[tokio::test]
async fn r655_sync_issue_updates_title_and_description() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "sync-issue").await;
    let project_id = make_project(&db, company_id, "sync-issue").await;
    let t1 = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique11}",
        "T1",
        None,
    )
    .await;
    let t2 = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique12}",
        "T2",
        None,
    )
    .await;
    let source = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique13}",
        "Source REF-11",
        Some("description mentions REF-12"),
    )
    .await;

    let svc = IssueReferenceService::new(db.clone());
    let total = svc.sync_issue(source).await.expect("sync");
    assert_eq!(total, 2);

    let mentions = svc
        .list_for_source(company_id, source)
        .await
        .expect("list");
    assert_eq!(mentions.len(), 2);
    let target_ids: Vec<Uuid> = mentions.iter().map(|m| m.target_issue_id).collect();
    assert!(target_ids.contains(&t1));
    assert!(target_ids.contains(&t2));
}

#[tokio::test]
async fn r655_sync_issue_resolves_after_creation() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "sync-resolve").await;
    let project_id = make_project(&db, company_id, "sync-resolve").await;

    // 先创建一个 source（暂时无 target）
    let source = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique14}",
        "Source",
        Some("see REF-15"),
    )
    .await;
    let svc = IssueReferenceService::new(db.clone());
    let total = svc.sync_issue(source).await.expect("sync 1");
    assert_eq!(total, 0); // target 不存在

    // 之后创建 target
    let _target = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique15}",
        "Target",
        None,
    )
    .await;
    let total = svc.sync_issue(source).await.expect("sync 2");
    assert_eq!(total, 1);
}

// ------------------------------------------------------------------
// 3. related_work
// ------------------------------------------------------------------

#[tokio::test]
async fn r655_related_work_returns_outbound_and_inbound() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "related").await;
    let project_id = make_project(&db, company_id, "related").await;
    let outbound_target = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique16}",
        "Outbound target",
        None,
    )
    .await;
    let inbound_source = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique17}",
        "Inbound source",
        Some("see REF-18"),
    )
    .await;
    let center = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique18}",
        "Center",
        Some("see REF-16"),
    )
    .await;

    let svc = IssueReferenceService::new(db.clone());
    svc.sync_issue(center).await.expect("sync center");
    svc.sync_issue(inbound_source).await.expect("sync inbound");

    let work = svc
        .related_work_for_issue(company_id, center)
        .await
        .expect("work");
    assert_eq!(work.outbound.len(), 1);
    assert_eq!(work.outbound[0].issue.id, outbound_target);
    assert!(work.outbound[0].mention_count >= 1);
    assert_eq!(work.inbound.len(), 1);
    assert_eq!(work.inbound[0].issue.id, inbound_source);
}

#[tokio::test]
async fn r655_related_work_sorts_by_mention_count_then_identifier() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "related-sort").await;
    let project_id = make_project(&db, company_id, "related-sort").await;
    let _ = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique19}",
        "Low",
        None,
    )
    .await;
    let high = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique20}",
        "High",
        None,
    )
    .await;
    let center = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique21}",
        "Center",
        Some("see REF-19 and REF-20 REF-20"),
    )
    .await;

    let svc = IssueReferenceService::new(db.clone());
    svc.sync_issue(center).await.expect("sync");

    let work = svc
        .related_work_for_issue(company_id, center)
        .await
        .expect("work");
    assert_eq!(work.outbound.len(), 2);
    // 高 mention_count 排前面
    let first = &work.outbound[0];
    let second = &work.outbound[1];
    assert!(first.mention_count >= second.mention_count);
    assert_eq!(first.issue.id, high);
}

#[tokio::test]
async fn r655_related_work_rejects_cross_company() {
    let db = connect().await;
    let c1 = make_company(&db, "related-cc1").await;
    let c2 = make_company(&db, "related-cc2").await;
    let p1 = make_project(&db, c1, "related-cc1").await;
    let issue = make_issue_with_identifier(
        &db,
        c1,
        p1,
        "REF-{unique22}",
        "X",
        None,
    )
    .await;
    let svc = IssueReferenceService::new(db.clone());
    let err = svc
        .related_work_for_issue(c2, issue)
        .await
        .expect_err("cross company");
    assert!(matches!(
        err,
        pc_issue_references::IssueReferenceError::CompanyMismatch { .. }
    ));
}

// ------------------------------------------------------------------
// 4. list / count / delete
// ------------------------------------------------------------------

#[tokio::test]
async fn r655_list_for_source_returns_views() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "list-source").await;
    let project_id = make_project(&db, company_id, "list-source").await;
    let t = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique23}",
        "T",
        None,
    )
    .await;
    let source = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique24}",
        "Source",
        Some("see REF-23"),
    )
    .await;

    let svc = IssueReferenceService::new(db.clone());
    svc.sync_issue(source).await.expect("sync");
    let mentions = svc
        .list_for_source(company_id, source)
        .await
        .expect("list");
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].target_issue_id, t);
    assert_eq!(mentions[0].source_kind, "description");
    assert_eq!(mentions[0].matched_text.as_deref(), Some("REF-{unique23}"));
}

#[tokio::test]
async fn r655_list_for_target_returns_inbound() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "list-target").await;
    let project_id = make_project(&db, company_id, "list-target").await;
    let t = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique25}",
        "T",
        None,
    )
    .await;
    let source = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique26}",
        "Source",
        Some("see REF-25"),
    )
    .await;
    let svc = IssueReferenceService::new(db.clone());
    svc.sync_issue(source).await.expect("sync");
    let inbound = svc
        .list_for_target(company_id, t)
        .await
        .expect("in");
    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0].source_issue_id, source);
}

#[tokio::test]
async fn r655_delete_for_source_removes_rows() {
    let db = connect().await;
    reset_table(&db, "issue_reference_mentions").await;
    let company_id = make_company(&db, "delete").await;
    let project_id = make_project(&db, company_id, "delete").await;
    let _ = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique27}",
        "T",
        None,
    )
    .await;
    let source = make_issue_with_identifier(
        &db,
        company_id,
        project_id,
        "REF-{unique28}",
        "Source",
        Some("see REF-27"),
    )
    .await;

    let svc = IssueReferenceService::new(db.clone());
    svc.sync_issue(source).await.expect("sync");
    assert_eq!(svc.count_for_source(company_id, source).await.unwrap(), 1);

    let deleted = svc
        .delete_for_source(company_id, source, "description", None)
        .await
        .expect("del");
    assert_eq!(deleted, 1);
    assert_eq!(svc.count_for_source(company_id, source).await.unwrap(), 0);
}

#[tokio::test]
async fn r655_validation_rejects_nil_uuids() {
    let db = connect().await;
    let svc = IssueReferenceService::new(db.clone());
    let err = svc
        .replace_source_mentions(Uuid::nil(), Uuid::new_v4(), "title", None, None, None)
        .await
        .expect_err("nil company");
    assert!(matches!(
        err,
        pc_issue_references::IssueReferenceError::Validation(_)
    ));
}

#[tokio::test]
async fn r655_validation_rejects_empty_source_kind() {
    let db = connect().await;
    let svc = IssueReferenceService::new(db.clone());
    let err = svc
        .replace_source_mentions(Uuid::new_v4(), Uuid::new_v4(), "", None, None, None)
        .await
        .expect_err("empty kind");
    assert!(matches!(
        err,
        pc_issue_references::IssueReferenceError::Validation(_)
    ));
}
